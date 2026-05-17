# Changelog

Notable changes to Outpost MDM. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Phase 12 — Comprehensive integration test coverage

**Added**
- `tests/common/mod.rs` — shared `TestApp` test harness (boots in-memory pool + bootstrapped admin + real TCP listener, abort on drop), plus `http_get` / `http_json` / `http_request` / `build_multipart` helpers
- `TestApp::token_for_role` — convenience for tests that need a non-admin token (operator/viewer)
- 7 new integration test files covering every CRUD resource:
  - `tests/applications.rs` (4 tests) — CRUD happy path, version lifecycle, duplicate package_name, viewer 403
  - `tests/groups.rs` (5 tests) — CRUD, duplicate name, device membership add/list/remove, unknown device, missing group 404
  - `tests/configurations.rs` (4 tests) — CRUD, invalid `settings_json` on create + update, application attachment lifecycle with duplicate-attach 400
  - `tests/users.rs` (7 tests) — CRUD, duplicate login, weak password, unknown role, self-delete prevention, self-password-change without `users.write`, viewer cannot change others' password
  - `tests/settings.rs` (6 tests) — list seeded, get specific, upsert, invalid JSON, unknown key 404, viewer read-only
  - `tests/push.rs` (7 tests) — schedule create/cancel with status transitions, missing due_at+cron 400, multiple targets 400, invalid payload_json 400, empty command 400, message list empty
  - `tests/enrollment.rs` (4 tests) — **full end-to-end device lifecycle:** admin creates → generates enrollment payload → device enrolls → admin schedules reboot → scheduler tick fans out → device syncs and receives → device acks → message delivered; wrong-secret 401; user token rejected for `/sync`; device token rejected for `/auth/me`

**Stats**
- Test count: **92 passing, 0 failing** (was 55 at P11)
- New tests: 37 across 7 new files
- Existing tests untouched (no churn)

### Phase 10 — Container hardening, deploy docs, CI security scans

**Added**
- `Dockerfile` rewritten as three-stage build:
  - planner stage (cargo-chef recipe extraction)
  - builder stage (cargo-zigbuild → x86_64-unknown-linux-musl static binary; Zig 0.13 installed for the linker)
  - runtime stage on `cgr.dev/chainguard/static:latest` (~few MB, no shell, glibc-free, `USER nonroot` by default)
  - `LABEL` annotations for OCI image metadata
- `docker-compose.yml` requires `JWT_SECRET` in `.env` (fail-fast via `${JWT_SECRET:?…}`)
- `deny.toml` allow-list for licenses (MIT / Apache-2.0 / BSD / ISC / Unicode-3.0 / etc.) and blocked unknown registries/git sources
- `.github/workflows/ci.yml` extended with new jobs:
  - `cargo-deny` (advisories + licenses)
  - `cargo-audit` (RustSec CVE database)
  - Trivy scan of the built image (HIGH/CRITICAL severities reported as SARIF)
- `docs/DEPLOY.md` — full production guide: Ubuntu prep, image pull, `.env` generation, compose file, nginx + certbot, backup pattern via Litestream, footprint expectations, hardening checklist
- README extended with `/readyz` mention + reference to `docs/DEPLOY.md`

### Phase 6 — Device enrollment + long-polling sync + push scheduler

**Added**
- `outpost-server::auth` — two-kind JWTs (`kind: "user" | "device"`); `issue_device_token` + verification helpers
- `outpost-server::auth_extract::AuthDevice` — extractor that yields the authenticated device identity after verifying the JWT kind matches `"device"` and the device is `is_enrolled = 1`
- `outpost-server::routes::enrollment` — three endpoints:
  - `POST /api/v1/devices/{id}/enrollment` (admin, `devices.enroll` permission): rotate the device's `enrollment_secret`, return enrollment payload `{server_url, customer_id, device_id, enrollment_secret}` (the admin / web UI renders this as a QR)
  - `POST /api/v1/enroll` (device-facing, no auth): exchange `(device_id, enrollment_secret, os_version, app_version)` for a 90-day device JWT; secret is consumed (set NULL) on success
  - `POST /api/v1/sync` (device JWT): per-tick check-in — device sends telemetry + acks, server returns up to 50 pending commands and marks them as `sent`
- `outpost-server::scheduler` — tokio task that wakes every N seconds (read from `settings.push.scheduler_tick_secs`, default 60, clamped to 5..=3600), drains `push_schedule` rows whose `due_at` is past, and fans them out per-device:
  - `device_id` targeting → single-device push
  - `group_id` → all enrolled devices in the group
  - `configuration_id` → all enrolled devices in the tenant (no device→config FK in v1; treated as broadcast)
  - Null target → tenant-wide broadcast
  - One-shot only: cron expressions reserved for a follow-up
- 3 new unit tests in `scheduler::tests`: direct-device emit, future `due_at` skipped, group fan-out (with un-enrolled device filtered out)

**Changed**
- `main.rs` clones the pool and spawns `scheduler::spawn(pool)` after building state
- `Claims` struct gains a `kind` field (default `"user"` for backwards-compatibility with existing tokens)

### Phase 5 — File uploads + HMAC-signed download URLs

**Added**
- `outpost-server::storage` — content-addressed disk writer (`write_bytes`) with sha256 fan-out (`aa/bb/aabb…ext`); `resolve_under_root` path-traversal guard
- `outpost-server::signed_url` — HMAC-SHA256 signed tokens for public downloads, format `v1.{file_id}.{expires}.{nonce}.{hex_tag}`; constant-time verification via `subtle`
- `outpost-server::routes::files` — admin endpoints (`GET /api/v1/files`, `POST /api/v1/files/upload` multipart, `GET /api/v1/files/{id}`, `DELETE /api/v1/files/{id}`, `GET /api/v1/files/{id}/signed-url`) plus **public** `GET /files/signed/{token}` that requires no Authorization header
- `Config::app_files_dir` (env var `APP_FILES_DIR`, default `/var/lib/outpost/files`)
- 3 new unit tests in `storage::tests` (round-trip, content-address determinism, traversal block)
- 5 new unit tests in `signed_url::tests` (round-trip, wrong key, expired, tampered file_id, garbage)
- 2 new integration tests in `tests/files.rs` (full upload → signed URL → public download flow with tamper rejection; auth-required upload)

**Changed**
- `AppState` carries `Arc<PathBuf>` for the files directory
- `main.rs` creates `app_files_dir` on disk before serving
- Workspace deps added: `hmac 0.12`, `sha2 0.10`, `subtle 2`, `hex 0.4`
- `outpost-server` enables `axum` feature `multipart`
- Dev-deps: `tempfile 3`

### Phase 4 — Core CRUD endpoints

**Added**
- `outpost-server::permission::require_permission` — DB-backed permission checker against `user_role_permissions`
- `outpost-server::page::{Page, PageParams}` — paginated list envelope with `MAX_LIMIT=200` cap and `clamp()` safety
- 8 route sub-modules under `crates/outpost-server/src/routes/`:
  - **devices** — list / get / create / update / delete + `/devices/{id}/telemetry`
  - **groups** — CRUD + `/groups/{id}/devices` membership management
  - **applications** — CRUD + `/applications/{id}/versions` (sub-resource for versioned releases)
  - **configurations** — CRUD + `/configurations/{id}/applications` assignment
  - **users** — CRUD + `/users/{id}/password` (self-service + admin override)
  - **settings** — list / get / set key-value system settings
  - **stats** — `/stats/fleet` rollup (device counts, push counters)
  - **push** — list / get / cancel push messages + create / cancel scheduled tasks
- `routes::api_v1()` composes all sub-routers and applies shared state
- Every CRUD path enforces multi-tenant scoping (`WHERE customer_id = ?`) and a per-permission check before mutation
- 5 new unit tests: page param clamp, 3 permission role checks; new `devices_without_token` route guard
- 4 new integration tests in `tests/devices.rs`: full CRUD happy path, duplicate-serial 400, viewer role 403 on create, empty-tenant fleet stats

**Notes**
- `applications` upload-the-binary path is deferred to P5; this commit lands the metadata surface only
- The push scheduler tick that drains `push_schedule` → `push_messages` is deferred to P6; this commit lands the REST surface only

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
