# Changelog

Notable changes to Outpost MDM. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Phase 3 — Auth: JWT + argon2id + bootstrap

**Added**
- `outpost-server::auth` — argon2id password hashing (`hash_password`, `verify_password`), HS512 JWT (`issue_token`, `verify_token`, `Claims`), and a cryptographically-strong `generate_password` helper
- `outpost-server::bootstrap::bootstrap_pending_passwords` — on every startup, scans for `users.password_hash IS NULL`, generates a 20-char random password, hashes it with argon2id, persists the hash, and prints the cleartext password to stderr exactly once
- `outpost-server::error::ApiError` — unified HTTP error type with stable JSON code/message and `IntoResponse` impl
- `outpost-server::auth_extract::AuthUser` extractor — verifies the Bearer token, checks the user is still active in the DB, and yields a typed identity
- `outpost-server::routes::auth` module with `POST /api/v1/auth/login` and `GET /api/v1/auth/me`
- `Config::jwt_secret` (required at startup, fail-fast if missing or shorter than 32 bytes) and `Config::jwt_ttl_secs` (default 24h)
- 8 new unit tests in `auth::tests`: hash/verify round-trip, fresh salt per hash, JWT round-trip, tampered-signature reject, expired reject, password generator length/charset
- 2 new unit tests in `bootstrap::tests`: bootstraps seed admin, idempotent
- 1 new unit test in `app::tests`: `/api/v1/auth/me` without token → 401
- 3 new integration tests in `outpost-server/tests/auth.rs`: full login → JWT → /me flow, wrong-password 401 with `invalid_credentials` code, invalid-token 401

**Changed**
- `AppState` now carries `Arc<String>` jwt secret and `i64` ttl
- `state::test_state` now also runs bootstrap so tests have a usable admin account
- `main.rs` runs bootstrap after migrations, before serving
- `Config::from_env` returns `Result<Self>`; fails on missing or short `JWT_SECRET`
- Workspace deps: added `jsonwebtoken 9`, `argon2 0.5`, `uuid 1` (v4 + serde features), `rand 0.8`

### Phase 2 — SQLite schema & migrations

**Added**
- `outpost-migrations` crate: `MIGRATOR` static compiled in via `sqlx::migrate!()`, plus thin `run(&pool)` helper
- 9 SQL migration files under `crates/outpost-migrations/migrations/`:
  - `0001_customers.sql` — single-row `customers` (tenancy root)
  - `0002_users_auth.sql` — `user_roles`, `permissions`, `user_role_permissions`, `users` + seeded roles/permissions
  - `0003_devices.sql` — `groups`, `devices` (with folded deviceinfo telemetry columns), `device_groups`
  - `0004_applications.sql` — `applications` (with `kind` tag for APK / ML model / mbtiles / etc.), `application_versions`
  - `0005_configurations.sql` — `configurations`, `configuration_applications`
  - `0006_uploaded_files.sql` — generic uploaded-file catalog
  - `0007_push.sql` — `push_messages`, `push_schedule` (folded from upstream push plugin)
  - `0008_settings.sql` — key/value `settings` table with 5 seeded defaults
  - `0009_seed_admin.sql` — bootstrap super-admin user with NULL `password_hash` (first-boot generation in P3)
- `outpost-server::db::open_pool` — WAL mode, `synchronous = NORMAL`, `foreign_keys = ON`, busy-timeout 5 s, pool size 8 (1 for in-memory)
- `outpost-server::state::AppState` shared across handlers via `with_state` + `axum::extract::State`
- `outpost-server::state::test_state` helper for integration tests
- `/readyz` readiness probe with SQL `SELECT 1` — returns 200/`ok` or 503/`degraded`
- 8 migration integration tests in `outpost-migrations/tests/migrate.rs`: clean apply, idempotency, full table list, seeded customers/roles/permissions/admin, FK enforcement, settings seeds
- 3 `db` module unit tests (in-memory pool + seeded customer + FK violation + WAL/memory pragma)
- 2 new integration tests in `outpost-server/tests/healthz.rs` for `/readyz` real-TCP

**Changed**
- Workspace deps: added `sqlx 0.8` with `runtime-tokio`, `sqlite`, `migrate`, `chrono`, `macros`
- `outpost-server` depends on `outpost-core` and `outpost-migrations`
- `build_router` signature: now takes `AppState`

### Phase 1 — HTTP server core

**Added**
- Environment-driven typed [`Config`](crates/outpost-server/src/config.rs): `BIND_ADDR`, `DB_PATH`, `RUST_LOG`
- tower-http middleware stack: request-id (UUID), structured tracing, gzip compression, permissive CORS
- Graceful shutdown on Ctrl+C (cross-platform) and SIGTERM (Unix)
- `outpost-server` re-organised into `lib` + `bin` so integration tests can build the same router the binary serves
- Unit tests for `Config` defaults and env fallback
- Unit tests for the router (`/healthz` returns 200 + JSON, `x-request-id` header present, unknown route 404)
- Real-TCP integration test (`tests/healthz.rs`) that boots `axum::serve` and hits `/healthz` over the wire

### Phase 0 — Repository bootstrap

**Added**
- Cargo workspace: `outpost-server` (binary), `outpost-core` (domain stub), `outpost-migrations` (sqlx-migrate stub)
- `/healthz` returning `{"status":"ok","version":"…"}`
- Multi-stage Dockerfile: musl static binary on `cgr.dev/chainguard/static`
- `docker-compose.yml` for local development
- GitHub Actions CI (`.github/workflows/ci.yml`): `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, Docker build (push to `ghcr.io/daphate/outpost-mdm-rs` on `main`)
- Apache License 2.0 (canonical text from apache.org)
- README, `.editorconfig`, `.gitattributes` (LF), `.dockerignore`
- Release profile tuned for size (`opt-level = "z"`, `lto = "fat"`, `strip = "symbols"`, `panic = "abort"`)

[Unreleased]: https://github.com/daphate/outpost-mdm-rs
