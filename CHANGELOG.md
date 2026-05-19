# Changelog

Notable changes to Outpost MDM. Format loosely follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.18.15] — 2026-05-19

### Phase 27 — «Настроить устройство»: structured update-config + install-apk push

**Переименование** — раздел `/devices/{id}/edit` теперь называется
«Настроить устройство» (был «Изменить устройство»). Это не косметика —
страница теперь делает много больше чем edit row в БД.

**Structured update-config form** — новый раздел «Быстрая настройка» с
типизированными dropdown'ами вместо raw-JSON textarea, который требовал
от admin'а помнить точные filename'ы моделей и enum-значения:

- **Модели (4 dropdown'а)** — `preferred_llm` / `preferred_translator_llm`
  / `preferred_vlm` / `preferred_stt`. Hardcoded в Rust список
  ([`web.rs llm_options()`](crates/outpost-server/src/routes/web.rs)) с
  filename + label + описанием сценария применения («Soldier V25 4B —
  рекомендуется», «Whisper tiny — для T0 устройств», etc.).
- **Режимы (6 dropdown'ов)** — `tts_mode`, `answer_mode`, `translator_mode`,
  `translator_audio_mode`, `log_level`, `cpu_thread_count`. Все enum-варианты
  и числа из контракта.
- **Tri-state переключатели (4)** — `wake_word_enabled`,
  `translator_cloud_enabled`, `show_build_badge`, `telemetry_enabled`.
  Состояния: «не менять» (по умолчанию), «вкл», «выкл».
- **Рядом с каждым dropdown'ом** — текущее значение из
  `current_state_json` (что сейчас на устройстве по последнему /sync
  snapshot'у), pretty-печать через `current_settings` map в template'е.

Новый handler [`device_config_structured_form`](crates/outpost-server/src/routes/web.rs)
собирает form fields → JSON object → ставит в очередь push_messages с
`command='update-config'`. Пустые поля не попадают в payload (idempotent —
не отправляем «не менять» как явное присвоение).

**Raw JSON форма не удалена** — переехала под `<details>` «Расширенно —
сырой JSON patch». Для случаев когда нужен key, которого ещё нет в Quick
Setup (например свежедобавленный в контракт).

**Push новой версии приложения (install-apk)** — новый раздел в форме с
тремя способами доставки APK:

1. **Pin версии** через `pinned_version_id` (был и раньше) — pull-on-sync.
2. **Раскатка через rollouts** (был и раньше) — fleet/canary policy.
3. **install-apk push (новое)** — admin выбирает версию из dropdown'а,
   нажимает «Поставить в очередь install-apk», server создаёт
   `push_messages.command='install-apk'` с payload `{version_code,
   version_name, sha256, size_bytes, url}`.

Handler [`device_install_apk_form`](crates/outpost-server/src/routes/web.rs) валидирует:
- Версия принадлежит этому customer'у.
- `source_url` не null (иначе устройство не сможет скачать APK).
- Гейт `versionCode >= 178 (rc42 b37)` — устройство должно поддерживать
  push commands.

⚠ **Client-side install-apk handler не реализован** — это AR Hud team's
scope. До его реализации команда сохраняется в `applied_commands`
с unknown-command status, без вреда. Контракт `install-apk` задокументирован
как §3.4 в [tactical-ar-hud `MDM-DEVICE-CONTROL-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/MDM-DEVICE-CONTROL-CONTRACT.md).

**Файлы:**
- [`crates/outpost-server/templates/device_edit.html`](crates/outpost-server/templates/device_edit.html) — полная переработка, ~430 строк (было ~190).
- [`crates/outpost-server/src/routes/web.rs`](crates/outpost-server/src/routes/web.rs):
  - +10 helper functions для dropdown options (`llm_options`, `tts_mode_options`, и т.д.).
  - +`DeviceEditTemplate` расширен 10 fields для options + `current_settings`.
  - +`device_config_structured_form` handler.
  - +`device_install_apk_form` handler.
  - +2 routes: `/devices/{id}/config-structured`, `/devices/{id}/install-apk-form`.

**Tests:** 97/97 passing. Новые tests **не добавлены** (UI form roundtrip
без headless-browser-теста сложно покрыть; structured handler — simple
form-parsing + delegation в существующий `queue_device_command_form`,
который уже test'ируется через push command routes).

## [Unreleased] — Ops

### Observability stack: fix Prometheus / node-exporter boot race against tailscaled

После hard-reset `mdm.secondf8n.tech` (provisioning телефонов, host
стал неотвечающим) Grafana показывала "No data" на всех панелях.
Root cause: `prometheus.service` и `prometheus-node-exporter.service`
bind'ятся на tailscale IP `100.68.41.91:9090` / `:9100`, но на boot
стартуют **раньше** чем `tailscaled` поднимает интерфейс →
`bind: cannot assign requested address` → exit 1. Vendor unit
`prometheus.service` имеет `Restart=on-abnormal` который **не**
покрывает exit-code-1 → сервис стоит мёртвый до manual restart.

**Fix:** systemd drop-in'ы в
[`deploy/systemd-drop-ins/`](deploy/systemd-drop-ins/) — добавляют
`After=tailscaled.service` + переключают на `Restart=on-failure`
с 5 s back-off и burst до 20–30 попыток. На следующих reboot'ах
сервисы будут retry'ить пока tailscale не поднимет интерфейс
(обычно <10 сек). Никакого ручного вмешательства не нужно.

Документировано:
- [`docs/DEPLOY.md → Observability stack`](docs/DEPLOY.md#observability-stack-grafana--prometheus--node-exporter) — incident write-up + install steps + verify
- [`deploy/systemd-drop-ins/README.md`](deploy/systemd-drop-ins/README.md) — copy/paste install commands
- [sophia-soul `insights/outpost-mdm-patterns.md` §16](https://github.com/daphate/sophia-soul/blob/main/insights/outpost-mdm-patterns.md) — generic pattern для **любого** сервиса который bind'ится на tailscale IP

Caveat: TSDB content за период `boot → first successful Prometheus
bind` отсутствует (на инциденте 2026-05-19 — ≈15 мин). WAL
восстановился полностью, исторические данные сохранены, но на
timeseries-графиках спанящих это окно будет видимый gap. Это
expected, не bug.

Code-changes: 0 (operational config-only commit). Прода не требует
redeploy outpost-server'а.

## [0.18.13] — 2026-05-19

### Phase 26 — UX polish: timezone, groups UI, bulk file ops

**Default configuration (v0.18.8)** — каждый customer получает seed-конфиг
«По умолчанию» через migration 0019, при создании нового устройства она
автоматически назначается через `customers.default_configuration_id`.
В `/configurations` отображается с амбер-бейджем + кнопка «Сделать
дефолтной». Также — `build.rs` для outpost-migrations crate
(`cargo:rerun-if-changed=migrations`), без него incremental cargo
не пересобирал embedded migrations при изменении только SQL-файлов
(silent-skip bug, который мог прятать миграции в ранних deploy'ах).

**Server timezone (v0.18.9)** — admin UI рендерит все timestamps в
configurable IANA timezone (default `Europe/Moscow`, MSK). Migration
0020 seed'ит ключ `settings.server.timezone`. AppState.fmt_ts метод
конвертит UTC из БД → server_tz через chrono-tz. /settings page имеет
dropdown с 596 IANA timezone'ами, settings_save обновляет
Arc<RwLock<Tz>> в AppState — hot-reload без restart'а сервера.

**Regression tests + deploy gate (v0.18.10, v0.18.11)** — добавлены
13 тестов: 4 на `trim_to` (regression на v0.18.7 byte-slice panic с
кириллицей), 9 на timezone (fmt_ts корректность UTC→MSK, fallback на
malformed input, atomic set_tz, load_server_tz с default'ами,
migration 0020 sanity). deploy.ps1 теперь делает `cargo test
--release --lib` после build и до scp — broken тесты блокируют deploy.

**Multi-file upload (v0.18.12)** — `/files` имеет drag-drop dropzone +
HTML5 `<input multiple webkitdirectory>`, можно перетащить целую папку.
files_upload handler итерирует по всем `file` parts multipart запроса
и создаёт N row'ов в `uploaded_files` за один HTTP-запрос. Flash-message
отчитывается о количестве загруженных файлов.

**Multi-file distribute (v0.18.12)** — checkboxes на `/files` rows
+ bulk-action bar (появляется когда выбран ≥1 файл). Submit form'ы на
`POST /files/bulk-distribute` с file_ids[] + target_type + target_id
итерирует через `do_distribute_file` для каждого выбранного, накапливая
total_commands + per-file failures. Flash-message содержит сводку.

**Groups UI (v0.18.6)** — на `/devices` колонка «Группы» (chip badges),
на `/groups` группы стали раскрывающимися с табличкой членов и формой
«Добавить устройство». Routes POST `/groups/{id}/members` + DELETE с
проверкой ownership на двойной JOIN (group AND device должны
принадлежать одному customer'у).

**Grafana SSO (v0.18.2)** — Grafana под `auth_request /__mdm_auth_check`,
запросы без cookie или с pending_2fa session → 401 → redirect на
/login. Grafana ini переведена на `anonymous = Admin` +
`disable_login_form = true` (полное доверие nginx-слою). Один логин
с 2FA даёт доступ ко всему admin UI + Grafana + Prometheus.

**Embedded static assets (v0.18.0)** — tailwind.js + htmx.min.js вшиты
в release-binary через `include_bytes!` (раньше тянулись с
cdn.tailwindcss.com / unpkg.com — admin UI ломался под VPN с медленными
CDN-маршрутами). Сейчас `<script src="/static/*">` ходит на тот же
hostname, без внешних зависимостей.

**Adaptive width (v0.18.5, v0.18.13)** — стандартизованы max-width
классы:
- Listing pages (`/devices`, `/groups`, `/configurations`, `/files`,
  `/applications`, `/users`, `/telemetry`): `max-w-7xl`
- Edit pages с tables (`/devices/{id}/edit`, `/configurations/{id}/edit`,
  `/file/{id}/distribute`): `max-w-5xl`
- Pure forms (`/profile`, `/me/2fa`, `/me/password`, group_edit):
  `max-w-3xl` / `max-w-md` для compactness
- Login/signup: `max-w-sm/md` (specifically narrow)
- Settings (`/settings` с raw_entries table): `max-w-7xl`

## [Unreleased]

### Phase 24 — Русский язык по умолчанию + i18n framework

**Why:** Outpost MDM admin'ы — операторы парка устройств, которые
работают в русскоязычной среде. Английский UI был техдолгом первой
итерации.

**i18n framework (`src/i18n.rs`)**
- `Locale` enum: `Ru` (default) + `En`.
- `Strings` struct — single-file canonical bundle of ~250 keys with
  Russian and English translations side-by-side. Compile-time check:
  adding a key forces every locale to provide a translation.
- `from_request()` resolves locale from `outpost_lang` cookie → falls
  back to `Accept-Language` header → falls back to `Locale::DEFAULT`
  (Russian).
- `WebUser.locale` populated at extract time; available to handlers via
  `user.s()` for future per-template wiring.

**Translation policy (per user, 2026-05-17)**
- **Default: Russian.** Maximally Russian UI; technical terms left in
  English (`API`, `OTLP`, `TOTP`, `JSON`, `SHA-256`, `JWT`, `Bearer`,
  package names, HTTP method names).
- 30 admin-UI templates rewritten with hardcoded Russian text in v0.8.
- Backend-emitted error strings (login failures, form validations,
  password change errors, signup errors) localized to Russian.

**Language switcher (`/settings`)**
- POST `/settings/language` writes `outpost_lang` cookie (1-year TTL).
- Settings page has a `<select>` for Русский / English with a clear
  banner: English support is partial in v0.8 — full template
  parameterization (each Template struct gets `s: &'static Strings`,
  every literal becomes `{{ s.X }}`) lands in v0.9.

**Honest scope statement**
- v0.8 ships **Russian-only UI** with the i18n module fully wired
  (cookie parsing, language switcher, both locale bundles populated).
- Future work: replace hardcoded Russian literals in templates with
  `{{ s.X }}` references so the English bundle becomes active when the
  switcher is set to English. This is a 30-template × 49-render-call
  refactor that we deliberately deferred for v0.9 to ship Russian fast.

**Tests**
- All 161 existing tests updated to match Russian UI strings.
- TOTP unit tests + render assertions unchanged from v0.7.
- Suite: 161 passing, 0 failed.

**Deploy**
- Compiled in WSL Ubuntu 24.04 → scp'd to `mdm.secondf8n.tech` as
  preview-24. All 15 admin pages render in Russian on first paint.

### Phase 23 — Multi-tenant + 2FA + Signup

The three features that v0.5's status table listed as "dropped per plan"
land in one phase: super-admins manage tenants, admins protect their
account with TOTP, and a public signup form lets new tenants sign up
without operator action (gated by a settings kill-switch).

**Schema (migration 0013_customers_active.sql — one migration covers
all of 23a + 23b)**
- `customers.is_active` + `customers.kind` (production / demo / test).
- `customers.read` + `customers.write` permissions; granted only to the
  super-admin role (id=1). Regular admin (id=2) stays per-tenant.
- `users.totp_secret` + `users.totp_enabled` columns.
- `totp_recovery_codes` table — argon2id-hashed single-use codes; 10
  are generated at enrollment and shown once.

**23a — Customer (multi-tenant) management**
- `/customers` (super-admin only): list every tenant with device + user
  counts, status badge (active / disabled), kind tag. New-customer
  inline form.
- `/customers/{id}/edit` — rename, change description, change kind, edit
  metadata_json. Plus a "Switch into" button for super-admins.
- `/customers/{id}/toggle-active` — soft-disable (keeps data, blocks new
  logins / push for the tenant). Cannot disable your own home tenant.
- `/customers/{id}/switch` — sets `outpost_acting` cookie; for the next
  24 h the super-admin's queries are scoped to that tenant just like a
  regular admin. Original `home_customer_id` is preserved on `WebUser`.

**23b — 2FA (TOTP)**
- New module `src/totp.rs` — RFC 6238, SHA-1 HMAC, 6-digit code, 30-s
  step, ±1-step skew. ~120 LOC + 5 unit tests. Compatible with Google
  Authenticator, Authy, 1Password, Bitwarden.
- New deps: `totp-lite 2` (one file, depends on sha1 already in tree)
  and `base32 0.5` for the otpauth URI.
- `/me/2fa` — enable / disable / regenerate setup. QR-code SVG via the
  existing `qrcode` crate (same one we use for device enrollment).
- Login flow now branches: after correct password,
  `users.totp_enabled = 1` → issue a short-lived (5 min) `pending_2fa`
  session and redirect to `/login/2fa`. The pending session's `kind`
  prevents it from reaching any protected page until upgraded.
- `/login/2fa` accepts either the 6-digit code or a one-time recovery
  code. Verifies, issues full session, revokes pending.
- 10 recovery codes (formatted `xxxx-xxxx-xxxx-xxxx`) generated at
  enrollment; shown once, stored as argon2id hashes.

**23c — Signup (self-service)**
- `/signup` — public page. Disabled by default; super-admin flips
  `settings.signup.enabled = "true"` to turn it on.
- Atomic in a single tx: create `customers` row + admin user (role
  `admin`, not super-admin). Login fails → tx rolls back; tenant rolls
  back too.
- Rate-limited via the existing login limiter (signup is brute-forceable
  the same way).
- On success: session cookie issued, redirect to `/dashboard`. New
  admin can change password / enable 2FA immediately.

**WebUser extractor enhancements**
- New `is_super_admin: bool` field (computed from
  `user_roles.is_super_admin` once at extract time).
- `home_customer_id` preserved separately from `customer_id` so the
  customer-switch overlay doesn't lose membership info.
- `WebUser::require_super_admin()` helper for handlers that need
  cross-tenant access.

**Tests**
- 5 new TOTP unit tests in `src/totp.rs` (secret format, otpauth URI,
  current-step verify, previous-step skew, garbage rejection).
- 7 new web integration tests: customers list / create / toggle /
  duplicate-name reject, /me/2fa setup + verify-with-correct-code path,
  signup disabled banner + signup enabled-flow auto-login.
- Suite total: **161 passing**, 0 failed (up from 149 at v0.6.1).

**Deployed** to `mdm.secondf8n.tech` as `preview-23`; migration 0013
applied on top of existing data; browser smoke OK on /customers,
/me/2fa, /signup. Test customer cleaned.

### Phase 22 — Device telemetry: OTLP/HTTP-JSON receiver + Prometheus + Grafana

**Why:** Phones go to clients as demo units. We have to see — without
intruding — whether the client opens the app, what features they touch,
what errors they hit. The telemetry stream is the only loopback that
tells us if a demo is alive or sitting in a drawer.

**Schema (migration 0012_telemetry.sql)**
- `device_logs` (severity-tagged events): timestamp, severity_number /
  severity_text, body, attrs_json, resource_json, trace_id, span_id.
- `device_metrics` (counters/gauges/histograms reduced to count): name,
  kind, value, attrs_json, resource_json, unit.
- `device_traces`: trace_id, span_id, parent_span_id, name, kind,
  start_ts/end_ts, duration_ms, status_code, attrs_json, resource_json.
- `device_activity_daily` rollup table (precomputed nightly for cheap
  dashboard queries).
- All four tables have indices on `(device_id, ts)` and `(customer_id,
  ts)` plus a `received_at` index for the Prometheus 24-h windows.

**Server-side OTLP receiver (`routes/otel.rs`)**
- `POST /v1/traces`, `POST /v1/metrics`, `POST /v1/logs` — OTLP/HTTP-JSON
  spec. The spec's protobuf wire format is deferred (JSON is fine on the
  512 MB box; protobuf needs `prost` + build-time codegen).
- Authentication: `Authorization: Bearer <device_token>` — the same
  long-lived JWT issued at `/api/v1/enroll`. The receiver rejects
  user-issued tokens (401) so admins can't masquerade as devices.
- Robust parsing: tolerates int / int-as-string / float for both
  `timeUnixNano` and numeric metric values; flattens OTLP `KeyValue[]`
  → JSON; handles gauge / sum / histogram / summary instrument kinds.

**Prometheus exposition (`routes/prom.rs`)**
- `GET /metrics` (no auth — nginx site does not expose it publicly).
- Server-self metrics: `outpost_build_info`, `outpost_devices_*_total`,
  `outpost_push_pending_total` / `_failed_total`, `outpost_otlp_*_24h`.
- `outpost_metric_latest{name="…"}` exposes the latest sample across
  the fleet for a hard-coded set of 10 common Outpost metric names
  (battery.pct, app.session_seconds, ml.inference_ms, etc.). This
  bounds cardinality — Prometheus stays performant.

**Admin UI**
- New page `/telemetry`: 6 KPI cards (active devices 24 h / logs /
  errors / metrics / traces / last ingest), top-10 most-active devices
  table, recent-errors table, top-metric-names table. 30-s auto-refresh.
- New page `/devices/{id}/telemetry`: 5 KPI cards + latest metric
  samples + recent spans + recent log events.
- New page `/devices/{id}/logs`: full log stream with min-severity
  filter, body-substring search, since-window selector (1h / 6h / 24h /
  7d / 30d), limit selector (10..1000).
- `_nav.html` got a Telemetry link.

**Host stack (mdm.secondf8n.tech)**
- Prometheus installed via apt; scrapes `localhost:8080/metrics` every
  30 s. 14-day retention, 200 MB cap. RSS ~36 MB.
- Grafana installed via official apt; pre-provisioned Prometheus
  datasource; pre-provisioned starter dashboard
  (`deploy/grafana-dashboards/outpost-fleet.json`).
  Behind nginx at `/grafana/`. RSS ~290 MB.
- nginx site config gained `/grafana/` (with WebSocket upgrade) and
  `/prometheus/` reverse-proxy locations. Canonical version-controlled
  copy at `deploy/nginx-mdm.secondf8n.tech.conf`.
- **Operational note:** the 512 MB droplet OOM-killed on first Grafana
  start. User power-cycled and resized to 1 GB; current free ≈ 400 MB.
  Phase 22 deploy depends on the 1 GB droplet.

**Tests**
- 8 new OTLP integration tests in `tests/otel.rs`: logs ingest persists
  records; rejects user-token / no-token / malformed JSON; gauge + sum
  metrics ingested; traces compute duration_ms; `/metrics` emits
  Prometheus-format; empty batches return 200 with `inserted:0`.
- Suite total: **149 passing**, 0 failed.

**Contract for the device sender** (other session writing OTLP sender
in Outpost-Android): see `docs/OTEL-CONTRACT.md`. Stable for v0.6.x;
breaking changes will tag the server release as v0.7.

**Bringing back from the "dropped" list (per user, 2026-05-17 evening)**
- **Customer / multi-tenant management** — admin UI for managing tenants
  on shared server.
- **2FA (TOTP)** — second factor for admin web login.
- **Signup** — self-service tenant creation.

All three are now on the active roadmap; previously listed as "dropped
per plan" in v0.5 status table. Memory updated:
`project_outpost_mdm_rs.md`.

### Phase 21 — Edit / delete + Headwind feature parity (files, roles, settings, profile)

**Why:** v0.4.0 closed creation but every list page was a one-way street —
no rename, no reassignment, no deletion, no group/configuration linking.
Headwind's UI exposes the full per-resource edit modal. Bringing parity.

**Added — schema**
- Migration `0011_devices_configuration.sql`: adds `configuration_id`
  pointer on `devices` (so each device can claim its active configuration),
  plus `description`, `custom1`, `custom2`, `phone` free-form fields
  that Headwind operators rely on.

**Added — pages (10 new)**
- `/devices/{id}/edit` (rename, set configuration, toggle active, assign
  to multiple groups via checkbox list) + `/devices/{id}/delete`
- `/groups/{id}/edit` (rename, edit description) + `/groups/{id}/delete`
- `/applications/{id}/edit` (rename, kind, description) + `/delete`
- `/applications/{id}/versions` (list + upload-new-version multipart
  form) + per-version `/delete`
- `/configurations/{id}/edit` (full edit incl. settings_json) +
  `/delete` + `/apps` (assign apps with install/show/remove mode) +
  `/apps/{app_id}/delete` (unassign)
- `/users/{id}/delete` (with self-protection) +
  `/users/{id}/reset-password` (admin mints a 16-char one-time, flash
  message displays it once, `must_change_password` flag set on user)
- `/files` — generic uploaded-files browser independent of application
  versions, with kind tagging (apk / llm-model / mmproj / whisper / tts /
  knowledge-db / mbtiles / config / generic / icon), multipart upload,
  per-row delete
- `/roles` — read-only role + permission inventory across seed roles
  (super-admin, admin, operator, viewer) with permission set per role
  and current user count
- `/settings` — server-wide settings UI: enrollment_base_url,
  default_sync_interval, max_upload_mb, branding_display_name; raw
  key/value table for everything else. Upserts via single transaction.
- `/profile` — current-user self-edit (email); links to existing
  `/me/password` for password change

**Added — infrastructure**
- Manual multi-value form parser (`parse_form` + `ParsedForm`) since
  axum's `serde_urlencoded`-backed `Form` extractor rejects `Vec<_>` —
  needed for the multi-checkbox group assignment on device edit.
- `format_size()` helper for human-readable byte counts (KiB / MiB /
  GiB).
- `_nav.html` reshuffled: 10 top-level links including new Files,
  Roles, Settings; user login chip links to /profile.

**Tests**
- 11 new web integration tests; suite now totals **141 passing**, up
  from 130 at v0.4.0.
- Coverage adds: device edit (single + multi-group assignment + delete
  + 404-after-delete), group edit + rename + delete, admin password
  reset (verifies flash cookie carries the new one-time), user delete
  with self-protection, configuration edit + app-assignment lifecycle,
  /roles renders seed roles with permission badges, /settings round-trip
  (save → re-render with new defaults), /profile email round-trip,
  /files multipart upload + delete.

**Deployed** to `mdm.secondf8n.tech` as `preview-21`; verified
end-to-end in browser (Chrome tabs open). Migration 0011 ran cleanly
against the existing prod DB (had to force `cargo` to recompile the
migrations crate after adding the SQL file — Rust's incremental build
doesn't watch the `migrations/` directory).

### Phase 20 — Full admin UI: create-forms + enrollment QR + push scheduling + password change

**Why:** v0.3.0 added read-only HTMX pages for all resources, but every
mutation still required a `curl` JSON call. Operators in the field can't
provision a fleet from the terminal — the UI has to do everything.

**Added — templates**
- New: `device_enroll.html`, `device_push.html`, `me_password.html`
- All 5 existing list pages (`devices`, `groups`, `applications`,
  `configurations`, `push`, `users`) gained inline `<details>` "+ New X"
  forms that re-open with an error message on validation failure.
- `_nav.html` now links the logged-in user's login to `/me/password`.

**Added — handlers (`routes/web.rs`)**
- `POST /devices/new` — create a device record by serial.
- `GET /devices/{id}/enroll` + `POST` — show / generate single-use
  enrollment secret + render a 285×285 QR-SVG embedding the
  `{server_url, customer_id, device_id, enrollment_secret}` payload.
- `GET /devices/{id}/push` + `POST` — schedule a per-device push command
  (reboot / install-apk / update-config / sync-models / sync-knowledge /
  sync-maps / remote-wipe) with optional `due_at` and JSON payload.
- `POST /groups/new`, `POST /configurations/new` — straightforward
  inserts with unique-violation handling.
- `POST /applications/upload` — single-multipart hop that creates the
  application row (find-or-create by `package_name`), writes the file
  under `APP_FILES_DIR`, hashes SHA-256, and creates the
  `application_version` row in one transaction.
- `POST /push/new` — schedule push targeting either a device or a group
  (the dropdown encodes the target as `device:N` / `group:N`).
- `POST /users/new`, `POST /users/{id}/toggle-active` — admin can mint
  operators/viewers/admins and toggle account active state. Cannot
  deactivate self.
- `GET /me/password` + `POST` — verifies current password before
  hashing the new one with argon2id; clears the
  `must_change_password` flag on success.

**Added — infrastructure**
- `FlashCookie` extractor + `set_flash_cookie` / `clear_flash_cookie`
  helpers — single-shot success banners across POST→303→GET. URL-encoded
  values via a small inline percent-encoder; no new dep.
- `redirect_with_flash(target, msg)` helper packages 303 + Set-Cookie
  for happy-path POST handlers.
- `qrcode 0.14` dep with `svg` feature only; `qrcode_svg` helper renders
  payload to SVG with 285×285 min dimensions + quiet zone.

**Tests**
- 10 new web integration tests in `tests/web.rs`. Suite now totals
  **130 passing, 0 failing** (up from 120 at v0.3.0).
- Coverage: device create + serial validation, group create, user
  create + role + short-pwd rejection, configuration create + invalid
  JSON rejection, enrollment view+generate+QR-SVG presence, password
  change happy path + relogin verification, password mismatch.

**Production state on `mdm.secondf8n.tech`**
- New binary deployed via WSL2 Ubuntu 24.04 → systemd
  (`/usr/local/bin/outpost-server.preview-ui`). RSS 1.7 MB on cold
  start. All 8 UI pages and 6 form-POST flows verified end-to-end over
  HTTPS.

### Phase 19 — Drop Docker from production, ship as systemd unit

**Why:** On the 1 vCPU / 512 MB box the Docker daemon costs ~50-80 MB
RSS without giving us any isolation value (single service, single
tenant). Production became simpler: cross-compile to
`x86_64-unknown-linux-musl` on the maintainer's workstation, `scp` the
~12 MB static ELF, supervise with systemd. Volume-permission cirque-du-
chown disappears, and the binary's `/etc/outpost/env` reads via
`EnvironmentFile=` like any well-behaved unit.

**Added**
- `deploy/outpost-server.service` — hardened unit (`NoNewPrivileges`,
  `ProtectSystem=strict`, `PrivateTmp`, `SystemCallFilter=
  @system-service`, empty `CapabilityBoundingSet`, `MemoryMax=256M`).
- `deploy/deploy.ps1` — one-shot Windows-host build+ship script:
  `cargo zigbuild` → `scp` → install + `ln -sfn` symlink swap →
  `systemctl restart` → poll `/healthz`. Keeps N=3 previous binaries
  on the host for one-symlink rollback.
- `docs/DEPLOY.md` — rewritten end-to-end for the systemd path
  (workstation toolchain setup, unit deployment, health checks,
  hardening checklist, rollback).

**Removed**
- `Dockerfile` and `docker-compose.yml` from the repo. The image stays
  reachable via `ghcr.io/daphate/outpost-mdm-rs:<sha>` for archival
  purposes but is no longer the production deploy artifact.
- Docker-related guidance from README and DEPLOY.md.

**Phase 18 — Pre-flight data-dir writability check**

`main.rs` now calls `ensure_dir_writable` for both the DB parent dir
and `APP_FILES_DIR` before opening the SQLite pool. On
`PermissionDenied`, the server exits with an explicit message naming
UID 65532 and pointing at the chown/bind-mount fix — instead of
bouncing for 20 seconds in a `restart: unless-stopped` loop that emits
`os error 13` and nothing else. Less surface to debug at 2 a.m.

### Phase 17 — Per-IP login rate limit (brute-force protection)

**Why:** Failed-credential brute force against `/login` is a textbook
attack; relying on the upstream nginx `limit_req_zone` is correct
defense-in-depth but the binary itself should not be defenceless when
nginx is misconfigured or absent (direct-Docker-port deployments).

**Added**
- `crate::rate_limit::LoginRateLimiter` — hand-rolled token-bucket map,
  per-IP, no new external deps. Defaults: **10-attempt burst, refilling
  at 1 token / 30 s** (10 attempts per 5 minutes per IP).
- `crate::client_ip::ClientIp` extractor — resolves IP from
  `X-Forwarded-For` (rightmost entry, set by trusted upstream nginx),
  falling back to `ConnectInfo<SocketAddr>` for direct connections.
- `ApiError::TooManyRequests` → 429 with `code: "too_many_requests"`.
- Both API login (`POST /api/v1/auth/login`) and HTML login
  (`POST /login`) check the rate limiter first; on hit, the API
  returns 429 JSON, the HTML page re-renders with a friendly error.
- `AppState::login_limiter` field; lives for the process lifetime.
- `main.rs` + `tests/common/mod.rs` now serve with
  `into_make_service_with_connect_info::<SocketAddr>()` so the
  extractor sees the peer address.
- 3 new unit tests in `rate_limit::tests` (first burst allowed,
  buckets-are-per-IP, refill-over-time).
- 1 new integration test in `tests/security.rs` —
  `login_rate_limit_kicks_in_after_burst` — drives 10 wrong-password
  attempts and asserts the next one returns 429 with the expected
  error code.

**Stats**
- Test count: **114 passing, 0 failing** (was 110 at v0.2.0; +4)

## [0.2.0] — 2026-05-17

## [0.2.0] — 2026-05-17

Second release. Two big changes since v0.1.0:

1. **HTMX/Askama admin UI** (Phase 15) — browser sign-in / dashboard /
   devices table, no npm or build pipeline, just Askama templates +
   Tailwind via CDN + HTMX 2.0.4. Cookie session piggybacks on the
   existing auth model.
2. **JWT → opaque DB-backed sessions** (Phase 16) — instant revocation,
   smaller wire footprint, zero JWT-library CVE surface. `jsonwebtoken`
   crate removed entirely. New `POST /api/v1/auth/logout` endpoint.
   Env var renamed `JWT_SECRET` → `APP_SECRET` (legacy alias accepted
   for one release).

Test count: 110 passing (was 96 at v0.1.0).

### Phase 16 — Replace JWT with opaque DB-backed sessions

**Why:** JWT is stateless — revocation requires rotating the signing key
(invalidates _everything_). For a fleet where a stolen device must be
locked out _now_, that's the wrong primitive. Opaque session tokens
stored server-side give instant revocation, smaller wire (~64 bytes vs
~400), no `alg=none`/algorithm-confusion attack surface, and ~0.1 ms
per-request DB hit over WAL'd SQLite — well within budget.

**Added**
- New migration `0010_sessions.sql` — `sessions` table keyed by
  **sha256 of the bearer token** (DB-file leak does not expose live
  tokens), with `kind`/`subject_id`/`customer_id`/`role_id`/`login`/
  `issued_at`/`expires_at`/`revoked_at`
- New module `crate::session` — `create_user_session` /
  `create_device_session` / `verify` / `revoke` /
  `revoke_all_for_subject` / `cleanup`
- New endpoint `POST /api/v1/auth/logout` — revokes the caller's
  current session (the capability JWT couldn't offer)
- Scheduler tick now opportunistically GCs sessions expired or revoked
  more than 30 days ago
- 7 new unit tests in `session::tests`: round-trip, revoked fails,
  expired fails, unknown fails, DB never stores raw token,
  revoke-all-for-subject, device session, cleanup

**Changed**
- `jsonwebtoken` crate dependency **removed** (~40 transitive crates gone)
- `crate::auth` trimmed to just argon2id helpers + `generate_password`
- `KIND_USER`/`KIND_DEVICE` moved to `crate::session`
- `AuthUser` / `AuthDevice` / `WebUser` extractors look up sessions
  instead of verifying JWT claims (signature mismatch / kind mismatch
  → 401 / Redirect, same as before)
- Env var **renamed:** `JWT_SECRET` → `APP_SECRET`. The legacy
  `JWT_SECRET` name still works for one release as a fallback —
  `Config::from_env` tries `APP_SECRET` first
- `Config::jwt_secret` → `Config::app_secret`, `Config::jwt_ttl_secs`
  → `Config::session_ttl_secs`; `AppState` likewise
- `signed_url::{sign, verify}` continues to use `app_secret` for HMAC
  (this was always the only thing the secret actually signed)

**Migration note for operators**
- Set `APP_SECRET` instead of `JWT_SECRET` (both still accepted in this
  release; the deprecated alias goes away in v0.3.0)
- No DB downtime: migration `0010_sessions.sql` is additive
- All existing v0.1.0 JWT tokens stop working — clients re-login (this
  is the right behaviour: the secret format and storage both changed)

**Stats**
- Test count: **110 passing, 0 failing** (was 104 at P15)

### Phase 15 — HTMX/Askama admin UI (sign-in + dashboard + devices)

**Added**
- Browser-facing routes alongside the JSON API:
  - `GET /` → 303 to `/dashboard` (cookie auth resolves)
  - `GET /login` — rendered sign-in form (Tailwind via CDN, HTMX 2.0.4)
  - `POST /login` — verifies credentials, issues HS512 JWT, sets `outpost_session` cookie (HttpOnly + SameSite=Lax + Secure-when-prod), 303 → `/dashboard`
  - `GET /logout` — clears cookie, 303 → `/login`
  - `GET /dashboard` — fleet stats overview (7 metric cards: devices total / online / enrolled, applications, configurations, push pending, push 24h)
  - `GET /devices` — devices table with online/offline indicator, battery %, app version, last-seen timestamp
- `WebUser` axum extractor — cookie-based; rejection is `Redirect::to("/login")` instead of JSON 401
- `auth_extract::extract_token` — shared Bearer-or-cookie token reader
- `AuthUser` (API extractor) now accepts the same cookie session as a fallback — admin can drive `/api/v1/*` from a browser dev console with the cookie that the HTMX UI already set
- `Config::secure_cookies` (env `SECURE_COOKIES`, default `true`; tests default `false`)
- Askama 0.13 added as a workspace dep
- 5 Askama templates under `crates/outpost-server/templates/`: `base.html`, `_nav.html`, `login.html`, `dashboard.html`, `devices.html`
- 8 new integration tests in `tests/web.rs`:
  - `/login` GET renders HTML with form
  - `/dashboard` without cookie → 303 to `/login`
  - `/` → redirect
  - Full browser flow: POST /login → 303 + Set-Cookie → GET /dashboard with cookie → 200 with stats
  - Wrong password → 200 with error banner + no cookie set
  - `/logout` → Set-Cookie with `Max-Age=0`
  - `/devices` after login shows the newly-created device in the table
  - The session cookie issued by the UI also works for `/api/v1/auth/me` (cookie fallback in the API extractor)

**Stats**
- Test count: **104 passing, 0 failing** (was 96 at v0.1.0 + 8 web tests)

## [0.1.0] — 2026-05-17

## [0.1.0] — 2026-05-17

First production-ready release: API-complete server with 96 passing
tests, end-to-end device enrollment + sync + push, multipart uploads
with HMAC-signed downloads, OWASP-style hardening headers, body size
limit, per-request timeout, container hardening, deploy runbook,
CI security scans, and full project hygiene docs.

Designed and tested to fit a **1 vCPU / 512 MB RAM Ubuntu 24.04
droplet** (`mdm.secondf8n.tech`) alongside SQLite and nginx.

The HTMX/Askama admin UI is intentionally deferred to a follow-up
phase. Operators drive the server via curl/Postman / the OpenAPI
surface in the meantime.

### Phase 14 — Production-readiness docs + GitHub project hygiene

**Added**
- `SECURITY.md` — vulnerability disclosure policy, scope matrix, cryptographic posture table, hardening checklist
- `docs/ARCHITECTURE.md` — module map, request lifecycle, persistence layer, auth model, push pipeline, file pipeline, out-of-scope list
- `CONTRIBUTING.md` — dev setup, coding conventions, testing conventions, migration rules, commit/PR template
- `.github/ISSUE_TEMPLATE/{bug_report.yml,feature_request.yml,config.yml}` (blank issues disabled; security contact link)
- `.github/PULL_REQUEST_TEMPLATE.md` with verification checklist
- README extended with a Documentation table linking each doc

### Phase 13 — Transport hardening: body size limit, request timeout, security headers

**Added**
- `Config::max_body_bytes` (env `MAX_BODY_BYTES`, default **200 MiB** — fits APK + ML-model uploads on the 1 vCPU droplet)
- `Config::request_timeout_secs` (env `REQUEST_TIMEOUT_SECS`, default **120 s** — long enough for the largest upload on the constrained host)
- `axum::extract::DefaultBodyLimit` layer enforcing `max_body_bytes`; oversized requests return 413
- `tower_http::timeout::TimeoutLayer` enforcing `request_timeout_secs`; slow handlers cap out with 503
- OWASP-style hardening response headers via `tower_http::set_header::SetResponseHeaderLayer::if_not_present`:
  - `X-Content-Type-Options: nosniff`
  - `X-Frame-Options: DENY`
  - `Referrer-Policy: no-referrer`
  - `Strict-Transport-Security: max-age=31536000; includeSubDomains`
  - `X-Robots-Tag: noindex, nofollow`
  - `Permissions-Policy: camera=(), microphone=(), geolocation=()`
- `tower-http` features extended: `timeout`, `set-header`, `limit`
- New unit test `app::tests::security_headers_are_set` (all 6 headers present on `/healthz`)
- New `tests/security.rs` integration suite (2 tests): oversized body → 413; security headers reach the wire including `x-request-id`
- Startup logs now emit `max_body_bytes` and `request_timeout_secs` so the deployed limits are auditable in tracing

**Changed**
- `AppState` carries `max_body_bytes` and `request_timeout_secs`
- `AppState::new` signature gains the two new fields; `test_state()` populates them with sensible defaults
- `main.rs` propagates them into `AppState`

**Stats**
- Test count: **96 passing, 0 failing** (was 92 at P12; +4 across unit + new security suite)

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

[Unreleased]: https://github.com/daphate/outpost-mdm-rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/daphate/outpost-mdm-rs/releases/tag/v0.2.0
[0.1.0]: https://github.com/daphate/outpost-mdm-rs/releases/tag/v0.1.0
