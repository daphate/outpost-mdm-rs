# Outpost MDM — Architecture

This document describes the layout of the Rust server as of May 2026.
It is the canonical map for a new contributor; pair it with the
phase-by-phase narrative in [`CHANGELOG.md`](../CHANGELOG.md).

## Workspace shape

```text
outpost-mdm-rs/
├── Cargo.toml                       # workspace deps + release profile
├── Dockerfile                       # 3-stage: planner / builder / Chainguard runtime
├── docker-compose.yml               # local + production compose
├── deny.toml                        # cargo-deny license/advisory policy
├── docs/
│   ├── ARCHITECTURE.md              # ← this file
│   └── DEPLOY.md
└── crates/
    ├── outpost-core/                # shared domain types (currently empty stub)
    ├── outpost-migrations/          # sqlx-migrate SQL + integration tests
    │   ├── migrations/
    │   │   ├── 0001_customers.sql
    │   │   ├── 0002_users_auth.sql
    │   │   ├── 0003_devices.sql
    │   │   ├── 0004_applications.sql
    │   │   ├── 0005_configurations.sql
    │   │   ├── 0006_uploaded_files.sql
    │   │   ├── 0007_push.sql
    │   │   ├── 0008_settings.sql
    │   │   └── 0009_seed_admin.sql
    │   └── src/lib.rs               # `MIGRATOR: sqlx::migrate::Migrator`
    └── outpost-server/              # the binary + library
        ├── src/
        │   ├── main.rs              # boot: load config, open pool, bootstrap, spawn scheduler, serve
        │   ├── lib.rs               # public module surface (so integration tests can share)
        │   ├── config.rs            # `Config::from_env` (typed env → struct)
        │   ├── state.rs             # `AppState` + `test_state` helper
        │   ├── app.rs               # `build_router(state) -> Router` + global middleware stack
        │   ├── shutdown.rs          # `signal()` future: ctrl-c (any OS), SIGTERM (Unix)
        │   ├── db.rs                # `open_pool(path) -> SqlitePool` with WAL pragmas
        │   ├── auth.rs              # argon2id + HS512 JWT primitives, `KIND_USER` / `KIND_DEVICE`
        │   ├── auth_extract.rs      # `AuthUser` / `AuthDevice` axum extractors
        │   ├── bootstrap.rs         # first-boot admin password generation
        │   ├── error.rs             # `ApiError` enum + `IntoResponse`
        │   ├── page.rs              # `Page<T>`, `PageParams`, clamp helpers
        │   ├── permission.rs        # `require_permission(db, role_id, "x.y")`
        │   ├── scheduler.rs         # push scheduler tokio task + `tick_once`
        │   ├── signed_url.rs        # HMAC-SHA256 signed download tokens
        │   ├── storage.rs           # content-addressed disk writer + path-traversal guard
        │   └── routes/              # one module per resource family
        │       ├── mod.rs           # `api_v1(state) -> Router` merges everything
        │       ├── auth.rs          # POST /api/v1/auth/login, GET /api/v1/auth/me
        │       ├── devices.rs       # CRUD + telemetry
        │       ├── groups.rs        # CRUD + membership
        │       ├── applications.rs  # CRUD + versions
        │       ├── configurations.rs# CRUD + app assignment
        │       ├── users.rs         # CRUD + /password
        │       ├── settings.rs      # GET/PUT key-value
        │       ├── stats.rs         # /fleet rollup
        │       ├── push.rs          # messages + schedule
        │       ├── files.rs         # multipart upload + signed-URL download
        │       └── enrollment.rs    # /enroll, /sync, scheduler glue
        └── tests/
            ├── common/mod.rs        # shared TestApp + HTTP helpers
            └── {auth,devices,applications,configurations,groups,users,
                  settings,push,files,enrollment,security,healthz}.rs
```

## Lifecycle of one HTTP request

```
TCP accept
  → axum::serve
    → DefaultBodyLimit               413 if body too large
    → CompressionLayer                gzip on the way out
    → CorsLayer                       OPTIONS preflight handling
    → SetRequestIdLayer               injects x-request-id (UUID v4) if absent
    → PropagateRequestIdLayer         copies x-request-id to response
    → TraceLayer                      spans + structured log per request
    → SetResponseHeaderLayer × 6      OWASP hardening headers
    → TimeoutLayer                    503 if handler exceeds REQUEST_TIMEOUT_SECS
    → Router::route
      → handler() async fn
        → AuthUser / AuthDevice       Bearer JWT extraction (parses kind, verifies sig, hits DB)
        → require_permission(...)     looks up `user_role_permissions`
        → sqlx::query{,_as,_scalar}   bound queries against SqlitePool
        → returns Result<Json<T>, ApiError>
      ← Response
    ← Response (with x-request-id back to client)
  ← TCP close
```

A failed handler that returns `ApiError` lands in `error.rs::IntoResponse`,
which renders a stable JSON envelope:

```json
{ "error": { "code": "invalid_credentials", "message": "invalid credentials" } }
```

## Persistence

- **SQLite WAL mode** for the production database. Connection-level
  pragmas applied by `db::open_pool`:
  - `journal_mode = WAL`
  - `synchronous = NORMAL`
  - `foreign_keys = ON`
  - `busy_timeout = 5 s`
- **`SqlitePoolOptions`** sized to **8 connections** for file-backed
  databases, **1** for `:memory:` so tests don't get unrelated empty
  databases on each connection check-out.
- **Migrations** are embedded into the binary via the
  `sqlx::migrate!()` macro pointed at
  `crates/outpost-migrations/migrations/`. They apply at startup
  inside `db::open_pool`. The migrations are append-only — never edit a
  shipped `.sql`; add a new numbered file.
- **Multi-tenancy** is enforced application-side via `WHERE customer_id = ?`
  on every read and write. The schema retains the column even though
  the initial deployment is single-tenant — a future deployment adds
  rows to `customers` without schema surgery.

## Auth model

- One **JWT secret** (`JWT_SECRET`, ≥ 32 bytes) signs both user and
  device tokens with HS512.
- The `kind` claim distinguishes principals:
  - `"user"` — recovered by the `AuthUser` extractor, must match a
    `users.is_active = 1` row.
  - `"device"` — recovered by `AuthDevice`, must match a
    `devices.is_enrolled = 1` row.
- Rotating `JWT_SECRET` invalidates every outstanding session (devices
  must re-enroll). This is the **single rotation point**; no token
  revocation list.
- Passwords use **argon2id** (RustCrypto) with the crate's default
  parameters (tuned for interactive auth). PHC-encoded hashes live in
  `users.password_hash`. First boot detects `password_hash IS NULL`
  and generates a 20-character alphanumeric password, prints it once to
  stderr, sets `must_change_password = 1`.

## Push pipeline

```
Admin              PushSchedule row (pending, due_at?)
                              │
                              │  scheduler::spawn  (tokio task, ticks every push.scheduler_tick_secs)
                              ▼
                   scheduler::tick_once(pool)
                              │  resolve_targets() (device | group | configuration | tenant)
                              │
                              │  INSERT INTO push_messages (one per target, status='pending')
                              │  UPDATE push_schedule SET status='done'
                              ▼
                   push_messages rows (pending)
                              │
                              │   POST /api/v1/sync from device (Bearer device JWT)
                              │     drain pending → mark 'sent', return to device
                              ▼
                   push_messages rows (sent)
                              │
                              │   next POST /api/v1/sync with acks: [...]
                              │     mark 'delivered'
                              ▼
                   push_messages rows (delivered)
```

The transport is **HTTPS long-polling** (no MQTT broker), chosen to
minimise process count on the 1 vCPU droplet. The `scheduler` tick
interval is read from `settings.push.scheduler_tick_secs` at server
start; tighten it for low-latency fleets, loosen for battery-friendly
devices.

## File pipeline

```
Admin POST /api/v1/files/upload (multipart "file" + "kind")
   → storage::write_bytes($APP_FILES_DIR, bytes, ext)   sha256 + fan-out aa/bb/aabb...
   → INSERT INTO uploaded_files                         metadata only
   ← {id, sha256, size, ...}

Admin GET /api/v1/files/{id}/signed-url?expires_in=300
   → signed_url::sign(file_id, ttl, jwt_secret)
   ← {url: "/files/signed/v1.42.1683893760.UUID.HEX", expires_in: 300}

Device GET /files/signed/{token}    (no Authorization header)
   → signed_url::verify(token, jwt_secret)              constant-time, expiry-checked
   → storage::resolve_under_root(...)                   path-traversal guard
   → stream the bytes back with original_name + content_type
```

The same pipeline serves APK installs, ML model bundles, knowledge-base
snapshots, and MBTiles packs — the type is annotated via
`uploaded_files.kind`.

## Where things are decided

- **Configuration**: every knob via `Config::from_env`. `JWT_SECRET` is
  the only required env var; everything else has a sensible default
  documented in `Config`'s field comments.
- **Tracing**: JSON via `tracing-subscriber::fmt().json()`, level
  filtered by `RUST_LOG`. Each request gets a `request_id` span.
- **Shutdown**: `axum::serve(...).with_graceful_shutdown(shutdown::signal())`
  drains in-flight requests on Ctrl+C / SIGTERM, exits ≤ 5 s under
  nominal load.
- **Permissions**: `permission::require_permission` is the gate; no
  decentralised checks. Adding a new endpoint means adding the
  permission name to `0002_users_auth.sql` and inserting the
  appropriate `user_role_permissions` rows.
- **Errors**: every fallible handler returns `Result<_, ApiError>`. Any
  `From<E>` impl required for ergonomic `?` lives in `error.rs`.

## Out-of-scope (intentionally)

- **Frontend admin UI** — planned as a follow-up (HTMX + Askama +
  Tailwind v4); for now operators drive the server via curl/Postman.
- **MQTT push transport** — HTTPS long-polling is the only transport;
  `rumqttd`/`rumqttc` are kept on the radar but not enabled.
- **Multi-server clustering** — single-process / single-SQLite. If the
  fleet outgrows that, `libsql` or migrating to Postgres are the
  obvious next steps.
- **Distributed file storage** — files live on the local volume; a
  `litestream`-backed replica + S3-compatible object store are
  documented in `docs/DEPLOY.md` but not built-in.
- **Cron expressions in `push_schedule`** — only one-shot `due_at` is
  honoured by the scheduler today; the column is reserved.
