# Outpost MDM — Architecture

This document describes the layout of the Rust server as of June 2026 (outpost-server v0.18.22).
It is the canonical map for a new contributor; pair it with the
phase-by-phase narrative in [`CHANGELOG.md`](../CHANGELOG.md).

## Workspace shape

```text
outpost-mdm-rs/
├── Cargo.toml                       # workspace deps + release profile
├── Cargo.lock
├── deny.toml                        # cargo-deny license/advisory policy
├── rust-toolchain.toml              # pinned Rust toolchain
├── README.md  CHANGELOG.md  CONTRIBUTING.md  SECURITY.md  LICENSE
├── docs/                            # design + ops docs
│   ├── ARCHITECTURE.md              # ← this file
│   ├── DEPLOY.md
│   ├── PRODUCTION-ROLLOUT-PLAN.md
│   ├── PROVISION-NEW-DEVICE.md
│   ├── OFFLINE-RESILIENCE.md
│   ├── OTEL-CONTRACT.md
│   ├── CLIENT-TELEMETRY-CONTRACT.md
│   ├── LIBRARY-ARCHIVES.md
│   ├── BALLISTICS-CRYPTO-DESIGN.md
│   ├── BALLISTICS-IMPLEMENTATION-SPEC.md
│   └── V25-SOLDIER-ROLLOUT.md
├── deploy/                          # production deploy assets (musl ELF + systemd; no container in prod)
│   ├── outpost-server.service       # systemd unit
│   ├── nginx-mdm.secondf8n.tech.conf
│   ├── deploy.ps1
│   ├── grafana-dashboards/          # outpost-device.json, outpost-fleet.json
│   ├── grafana-alerting/            # rules.yml
│   └── systemd-drop-ins/            # prometheus + node-exporter (tailscale-bind)
├── notes/   tools/                  # internal notes + helper scripts
└── crates/
    ├── outpost-core/                # shared domain types/services (populated incrementally)
    │   └── src/lib.rs
    ├── outpost-migrations/          # sqlx migrations + integration tests
    │   ├── migrations/              # append-only — never edit a shipped .sql, add a new file
    │   │   ├── 0001_customers.sql
    │   │   ├── 0002_users_auth.sql
    │   │   ├── 0003_devices.sql
    │   │   ├── 0004_applications.sql
    │   │   ├── 0005_configurations.sql
    │   │   ├── 0006_uploaded_files.sql
    │   │   ├── 0007_push.sql
    │   │   ├── 0008_settings.sql
    │   │   ├── 0009_seed_admin.sql
    │   │   ├── 0010_sessions.sql                  # P16: opaque DB-backed sessions
    │   │   ├── 0011_devices_configuration.sql
    │   │   ├── 0012_telemetry.sql
    │   │   ├── 0013_customers_active.sql
    │   │   ├── 0014_application_source_url.sql
    │   │   ├── 0015_application_rollouts.sql
    │   │   ├── 0016_device_state_snapshot.sql
    │   │   ├── 0017_device_keys.sql               # P-256 device pubkeys from enroll
    │   │   ├── 0018_encrypted_distributions.sql   # per-recipient encrypted file mapping
    │   │   ├── 0019_default_configuration.sql
    │   │   ├── 0020_server_timezone.sql
    │   │   ├── 0021_default_config_settings.sql
    │   │   ├── 0022_server_datetime_format.sql
    │   │   ├── 0023_user_profile_fields.sql
    │   │   ├── 0024_ballistics_schema.sql
    │   │   ├── 0025_ballistics_permissions.sql
    │   │   ├── 0026_bundle_assignments.sql
    │   │   ├── 0027_bundle_permissions_seed.sql
    │   │   └── 0028_ballistics_entities_tenant_pk.sql
    │   └── src/lib.rs               # `MIGRATOR: sqlx::migrate::Migrator`
    └── outpost-server/              # the binary + library
        ├── src/
        │   ├── main.rs              # boot: config → pool → bootstrap → scheduler → serve
        │   ├── lib.rs               # public module surface (shared with integration tests)
        │   ├── config.rs            # `Config::from_env` (typed env → struct)
        │   ├── state.rs             # `AppState` + `test_state` helper
        │   ├── app.rs               # HTTP application factory + global middleware stack
        │   ├── shutdown.rs          # graceful shutdown (ctrl-c / SIGTERM)
        │   ├── db.rs                # SQLite pool with Outpost-tuned PRAGMAs (WAL)
        │   ├── auth.rs              # argon2id password hashing (no JWT since P16)
        │   ├── auth_extract.rs      # bearer token → `AuthUser` / `AuthDevice`
        │   ├── session.rs           # opaque DB-backed session tokens (sha256 in `sessions`)
        │   ├── totp.rs              # RFC 6238 TOTP — admin 2FA on web login
        │   ├── bootstrap.rs         # first-boot admin password generation
        │   ├── permission.rs        # `require_permission` (user_role_permissions)
        │   ├── rate_limit.rs        # per-IP token-bucket limiter
        │   ├── client_ip.rs         # resolve originating client IP
        │   ├── error.rs             # unified `ApiError` + `IntoResponse`
        │   ├── page.rs              # pagination envelope + query params
        │   ├── scheduler.rs         # push scheduler task + `tick_once`
        │   ├── storage.rs           # content-addressed disk writer + traversal guard
        │   ├── signed_url.rs        # HMAC-SHA256 signed download URLs (APP_SECRET)
        │   ├── cloudru_signer.rs    # Cloud.ru SigV4 presigned-URL generator
        │   ├── distribution.rs      # encrypt-for-recipient pipeline (per-device)
        │   ├── distribute_gc.rs     # GC task for encrypted_distributions blobs
        │   ├── apk_watcher.rs       # Outpost-Android APK upstream watcher
        │   ├── rollout_monitor.rs   # phased-rollout auto-promote / auto-rollback
        │   ├── i18n.rs              # typed translations for the admin UI
        │   └── routes/              # one module per resource family
        │       ├── mod.rs           # router assembly
        │       ├── auth.rs          # /api/v1/auth/* — login / logout / me
        │       ├── devices.rs       # /api/v1/devices — CRUD + telemetry
        │       ├── groups.rs        # /api/v1/groups — groups + membership
        │       ├── applications.rs  # /api/v1/applications — APK catalog + versions
        │       ├── configurations.rs# /api/v1/configurations — config bundles + app assignment
        │       ├── users.rs         # /api/v1/users — account management
        │       ├── settings.rs      # /api/v1/settings — installation key/value
        │       ├── stats.rs         # /api/v1/stats — fleet rollups
        │       ├── push.rs          # /api/v1/push — schedule / inspect commands
        │       ├── files.rs         # /api/v1/files (admin) + /files/signed/{token} (public)
        │       ├── enrollment.rs    # device-facing /enroll + /sync (long-polling)
        │       ├── bundles.rs       # /api/v1/bundles — bootstrap bundle assignment
        │       ├── distribute.rs    # per-device encrypted file distribution
        │       ├── ballistics.rs    # BALLISTICS-MDM-CONTRACT v1 endpoints
        │       ├── otel.rs          # OTLP/HTTP-JSON receiver (spans / metrics / logs)
        │       ├── prom.rs          # Prometheus exposition (`GET /metrics`)
        │       ├── internal.rs      # nginx `auth_request`-only internal endpoints
        │       └── web.rs           # HTML admin UI (Askama + cookie session)
        ├── templates/               # 33 Askama HTML templates (admin UI)
        └── tests/
            ├── common/              # shared TestApp + HTTP helpers
            └── {auth,devices,applications,configurations,groups,users,settings,
                  push,files,enrollment,security,healthz,otel,web}.rs
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
        → AuthUser / AuthDevice       Bearer opaque session token (sha256 lookup in `sessions`, checks kind, revoked_at, expiry)
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

After Phase 16 (May 2026), tokens are **opaque 256-bit random hex
values, stored server-side as sha256 in the `sessions` table**. JWT was
removed entirely — stateless tokens were a poor fit for a system where
stolen-device revocation matters.

```
Login                             POST /api/v1/auth/login
  user submits {login, password}
  server verifies argon2id PHC
  server: token = hex(rand_32_bytes())
  server: INSERT sessions (id_hash = sha256(token), kind='user', subject_id, …)
  ← {access_token: <token>, token_type: "Bearer", expires_in: 86400}

Every API request          Bearer <token> OR Cookie outpost_session=<token>
  AuthUser extractor:
    SELECT * FROM sessions
     WHERE id_hash = sha256(presented_token)
       AND revoked_at IS NULL
       AND expires_at > now
    AND users.is_active = 1
  → AuthUser { id, customer_id, role_id, login }

Logout                            POST /api/v1/auth/logout
  UPDATE sessions SET revoked_at = now() WHERE id_hash = sha256(token)
  Takes effect on the NEXT request — no key rotation needed.

Device enrollment                 POST /api/v1/enroll
  same as user login, but kind='device', 90-day TTL
```

Key properties:
- **A DB-file leak does not expose live tokens** — we only store
  `sha256(token)`, not the token itself
- **Instant revocation** — single row UPDATE, no global rekey
- **No JWT-library CVE category** — no parser, no `alg=none`
- `APP_SECRET` is reserved for HMAC-SHA256 on signed download URLs
  (`/files/signed/<token>` for devices). It does not sign session
  tokens (those are random bytes).
- Passwords use **argon2id** (RustCrypto) with default parameters.
  PHC-encoded hashes live in `users.password_hash`. First boot detects
  `password_hash IS NULL`, generates a 20-character alphanumeric
  password, prints once to stderr, sets `must_change_password = 1`.

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
                              │   POST /api/v1/sync from device (Bearer device session token)
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
   → signed_url::sign(file_id, ttl, app_secret)
   ← {url: "/files/signed/v1.42.1683893760.UUID.HEX", expires_in: 300}

Device GET /files/signed/{token}    (no Authorization header)
   → signed_url::verify(token, app_secret)              constant-time, expiry-checked
   → storage::resolve_under_root(...)                   path-traversal guard
   → stream the bytes back with original_name + content_type
```

The same pipeline serves APK installs, ML model bundles, knowledge-base
snapshots, and MBTiles packs — the type is annotated via
`uploaded_files.kind`.

## Where things are decided

- **Configuration**: every knob via `Config::from_env`. `APP_SECRET` is
  the only required env var (the legacy name `JWT_SECRET` is still
  accepted as a deprecated alias); everything else has a sensible default
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

- ~~**Frontend admin UI**~~ — *shipped.* The HTMX + Askama admin UI is
  implemented (templates under `crates/outpost-server/templates/`, served
  by `routes/web.rs`); the JSON API remains available for curl/Postman.
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
