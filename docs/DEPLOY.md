# Outpost MDM — Deployment Guide

Production target: **Ubuntu 24.04 droplet, 1 vCPU / 512 MB RAM** (e.g. `mdm.secondf8n.tech`).

Production runs the server as a single static musl ELF supervised by **systemd**, in front of an **nginx** TLS reverse proxy. There is no container runtime in production — the binary is cross-compiled on the maintainer's workstation and `scp`'d to the host. This keeps RSS well under 100 MB on the 512 MB box and removes a layer of operational surface (docker daemon, volume permissions, image-tag drift).

---

## TL;DR (first-time deploy)

```bash
# On the droplet, as root:
apt update && apt install -y nginx certbot python3-certbot-nginx sqlite3

useradd --system --home /var/lib/outpost --shell /usr/sbin/nologin outpost
mkdir -p /var/lib/outpost/files /etc/outpost
chown -R outpost:outpost /var/lib/outpost && chmod 750 /var/lib/outpost
chown root:outpost /etc/outpost && chmod 750 /etc/outpost

# Production env — APP_SECRET must be a strong random 32+ byte value.
cat > /etc/outpost/env <<EOF
APP_SECRET=$(openssl rand -base64 48)
RUST_LOG=info
SECURE_COOKIES=true
SESSION_TTL_SECS=86400

# Cloud.ru read-only IAM (optional). Если все три заданы — на странице
# /devices/{id}/enroll будет рендериться APK-QR с presigned URL на 7 дней.
# Если хотя бы одно отсутствует — APK-QR блок скрыт, страница содержит
# только enrollment QR. Half-set комбинация — server start fail.
# CLOUDRU_TENANT_ID=00000000-0000-0000-0000-000000000000
# CLOUDRU_KEY_ID=00000000000000000000000000000000
# CLOUDRU_SECRET=00000000000000000000000000000000
# CLOUDRU_BUCKET=outpost                       # default
# CLOUDRU_APK_KEY=apks/latest/app-debug.apk    # default
EOF
chown root:outpost /etc/outpost/env && chmod 640 /etc/outpost/env

# systemd unit (copy deploy/outpost-server.service from the repo)
cp /tmp/outpost-server.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable outpost-server

# nginx site (see "nginx reverse proxy" section below)
# certbot TLS
certbot --nginx -d mdm.secondf8n.tech --agree-tos --email <your-email> --redirect

# First binary deploy from the dev workstation:
# (on Windows host) F:\projects\outpost-mdm-rs> .\deploy\deploy.ps1
```

On the **dev workstation** (Windows here, but works equivalently from Linux/macOS), one-time setup:

```powershell
rustup target add x86_64-unknown-linux-musl
# Download Zig 0.13.0 to a stable path, e.g. F:\tools\zig-0.13.0\
cargo install --locked cargo-zigbuild
```

After that, every deploy is `.\deploy\deploy.ps1` — the script cross-compiles, scp's the binary, atomically swaps `/usr/local/bin/outpost-server`, and `systemctl restart`s.

---

## Capture the bootstrap admin password

On first boot, `outpost-server` detects the seed admin row whose `password_hash IS NULL` and prints a one-shot password to stderr (which systemd forwards to the journal):

```
==============================================================
  BOOTSTRAP: initial password for 'admin' (user_id=1)

  XXXXXXXXXXXXXXXXXXXX

  Capture this NOW — it is not recoverable after this boot.
==============================================================
```

Grab it:

```bash
sudo journalctl -u outpost-server | grep -A 4 BOOTSTRAP
```

The server marks `must_change_password = 1`, so first login forces a password change.

---

## Required environment variables (`/etc/outpost/env`)

| Var | Default | Required | Description |
|-----|---------|----------|-------------|
| `APP_SECRET`         | — | **yes** | ≥32 bytes; HMAC-SHA256 secret for signed download URLs **and** session cookie binding. Rotating invalidates all sessions. Generate: `openssl rand -base64 48`. Legacy alias: `JWT_SECRET` (deprecated). |
| `BIND_ADDR`          | `127.0.0.1:8080` (set by unit) | no | Loopback only — nginx terminates TLS and proxies. |
| `DB_PATH`            | `/var/lib/outpost/outpost.db` (set by unit) | no | SQLite file path. |
| `APP_FILES_DIR`      | `/var/lib/outpost/files` (set by unit) | no | Storage root for uploaded APKs / models. |
| `RUST_LOG`           | `info` | no | `tracing_subscriber::EnvFilter` directive. |
| `SESSION_TTL_SECS`   | `86400` | no | User session lifetime (24 h). Devices use a fixed 90-day TTL. |
| `MAX_BODY_BYTES`     | `209715200` (200 MiB) | no | Request body cap → 413 on overflow. |
| `REQUEST_TIMEOUT_SECS` | `120` | no | Per-request wall-clock timeout → 503. |
| `SECURE_COOKIES`     | `true` | no | `Secure` flag on the `outpost_session` cookie. Disable only for plain-HTTP local dev. |
| `CLOUDRU_TENANT_ID`  | — | conditional | Cloud.ru tenant UUID (read-only IAM). Если задан — сервер генерирует SigV4 presigned URL для APK на странице `/devices/{id}/enroll` и рендерит QR-код «Шаг 1 — установить приложение». См. ниже. |
| `CLOUDRU_KEY_ID`     | — | conditional | Cloud.ru access key ID, read-only. All-or-nothing с `CLOUDRU_TENANT_ID` + `CLOUDRU_SECRET`. |
| `CLOUDRU_SECRET`     | — | conditional | Cloud.ru secret access key, read-only. |
| `CLOUDRU_BUCKET`     | `outpost` | no | S3 bucket с APK / моделями. |
| `CLOUDRU_APK_KEY`    | `apks/latest/app-debug.apk` | no | Object key для latest APK pointer. QR на enrollment-странице ведёт на этот key с TTL 7 дней. |

The server **refuses to start** if:
- `APP_SECRET` is missing or shorter than 32 bytes.
- Cloud.ru creds are partially set — `CLOUDRU_TENANT_ID/KEY_ID/SECRET` must be **all three** or **none**. Half-set комбинация → `bail!` со списком какие именно полей не хватает.

### Cloud.ru read-only IAM creds (для APK-QR на странице enrollment)

Когда `CLOUDRU_TENANT_ID/KEY_ID/SECRET` заданы, на странице `/devices/{id}/enroll` рендерится дополнительный QR со SigV4 presigned URL на `apks/latest/app-debug.apk` (или на key из `CLOUDRU_APK_KEY`). TTL — 7 дней (SigV4 max). Юзер открывает страницу с админ-машины, оператор показывает QR оператору телефона, тот сканирует — браузер сразу скачивает APK без ручного копи-паста ссылок через Telegram.

Если переменные не заданы — APK-блок на странице **не отображается**, страница содержит только enrollment QR. Логи на старте:

```
INFO Cloud.ru presigner enabled tenant_id=… key_id_prefix=… bucket=outpost apk_key=apks/latest/app-debug.apk
# или
INFO Cloud.ru presigner disabled — set CLOUDRU_TENANT_ID, CLOUDRU_KEY_ID, CLOUDRU_SECRET to enable APK-QR на странице enrollment
```

Creds должны быть **read-only**: scope только `GET object` / `HEAD object` / `LIST bucket`. Сервер сам никогда не делает `PUT`/`DELETE` через них, но если ключи скомпрометируются (например leak через misconfigured logging) — read-only minimises blast radius. Cloud.ru console → IAM service account → отдельная роль с одной только `s3:GetObject` permission на `arn:s3:::outpost/*`.

Per-device персонализированные creds (план на будущее) — пока не реализованы. Сейчас один shared read-only ключ на весь fleet.

**v0.17:** при `POST /api/v1/enroll` server **прокидывает** эти же creds в response — поле `cloudru_credentials: {tenant_id, key_id, secret}`. Android-клиент сохраняет их в `ModelPreferences.cloudruCreds` и подсовывает в `CloudRuSigner` через override-flow (см. `MDM-DEPLOY-CONTRACT §1.5`). Если CLOUDRU_* env'ы на сервере не заданы — поле отсутствует, клиент работает на встроенных в APK fallback-creds. Это безболезненный switch — оба варианта совместимы.

### Long-polling `/api/v1/sync` (v0.17)

Клиент (b39+ если AR Hud команда поддержит) может опционально передавать query-param `?wait_for_command_ms=30000` при `POST /api/v1/sync`. Если по результату обычного drain'а нет pending command'ов, server держит соединение до 30 секунд (либо до появления push'а), потом возвращает обычный response. Polling tick внутри loop'а — 2 секунды.

Без параметра — старое immediate-return поведение (для legacy клиентов).

`REQUEST_TIMEOUT_SECS` должен быть ≥ `LONG_POLL_MAX_MS / 1000` (по умолчанию 120 ≥ 30, OK). nginx `proxy_read_timeout` тоже должен быть достаточно большим — по умолчанию 60s, тоже OK.

### Sliding session refresh (v0.17)

При каждом `/api/v1/sync` server проверяет remaining TTL session'а; если < 50% (т.е. < 45 дней при 90-дневном TTL) — продлевает до полного 90-дневного TTL от now. Эффект: устройство online хотя бы раз в 45 дней → session **никогда** не истекает. Подробнее — `docs/OFFLINE-RESILIENCE.md`.

---

## systemd unit (`/etc/systemd/system/outpost-server.service`)

The canonical unit is checked in at [`deploy/outpost-server.service`](../deploy/outpost-server.service). Key properties:

- `User=outpost`, `Group=outpost` — unprivileged.
- `EnvironmentFile=/etc/outpost/env` — secrets never touch the unit file or the repo.
- `BIND_ADDR=127.0.0.1:8080` — accessible only via nginx.
- `Restart=on-failure`, `RestartSec=5s`, `StartLimitBurst=5/60s` — auto-restart on crash but bail out of a tight loop.
- `ReadWritePaths=/var/lib/outpost` — strict filesystem confinement; everything else is read-only.
- `MemoryMax=256M` — hard ceiling to keep the OOM killer pointed at this process if it leaks, not at sshd/nginx.
- `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `PrivateDevices`, `SystemCallFilter=@system-service`, `CapabilityBoundingSet=` — defence in depth, mirrors the hardening that the Chainguard runtime image used to provide automatically.

Operate it like any other systemd unit:

```bash
sudo systemctl status outpost-server
sudo systemctl restart outpost-server
sudo journalctl -u outpost-server -f
sudo journalctl -u outpost-server -o json | jq '.MESSAGE'  # structured logs
```

---

## Build & deploy from the workstation

### One-time setup

```powershell
# Cross-compile target
rustup target add x86_64-unknown-linux-musl

# Zig (cargo-zigbuild needs it as the C/musl linker)
# Download zig-windows-x86_64-0.13.0.zip from https://ziglang.org/download/
# Extract to F:\tools\zig-0.13.0\  (or update deploy.ps1's $env:Path)

cargo install --locked cargo-zigbuild
```

### Every deploy

```powershell
F:\projects\outpost-mdm-rs> .\deploy\deploy.ps1
```

The script:

1. Reads the short git SHA.
2. Runs `cargo zigbuild --release --target x86_64-unknown-linux-musl --bin outpost-server`.
3. `scp`s the binary to `/tmp/outpost-server.new` on the host.
4. `sudo install -m 0755`s it as `/usr/local/bin/outpost-server.<sha>`.
5. Atomically swaps `/usr/local/bin/outpost-server` (a symlink) to point at the new copy.
6. `sudo systemctl restart outpost-server`.
7. Polls `https://mdm.secondf8n.tech/healthz` for up to 20 s.
8. Prunes old `outpost-server.<sha>` copies, keeping the most recent 3.

### Rollback

```bash
# On the host:
ls -t /usr/local/bin/outpost-server.* | head
# Pick the previous one and re-point the symlink:
sudo ln -sfn /usr/local/bin/outpost-server.<previous-sha> /usr/local/bin/outpost-server
sudo systemctl restart outpost-server
```

---

## nginx reverse proxy

The canonical site config is checked in at [`.tmp/mdm.secondf8n.tech.nginx`](../.tmp/mdm.secondf8n.tech.nginx). Certbot rewrites parts of it to add TLS; the post-certbot version on the host is the source of truth.

```nginx
server {
    listen 443 ssl http2;
    server_name mdm.secondf8n.tech;

    ssl_certificate     /etc/letsencrypt/live/mdm.secondf8n.tech/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mdm.secondf8n.tech/privkey.pem;

    client_max_body_size 250M;             # APK + ML-model uploads
    add_header X-Content-Type-Options nosniff always;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host              $host;
        proxy_set_header X-Real-IP         $remote_addr;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Request-Id      $request_id;
        proxy_http_version 1.1;
        proxy_read_timeout 120s;
        proxy_buffering off;               # /api/v1/sync long-poll friendly
    }

    location = /healthz { access_log off; proxy_pass http://127.0.0.1:8080; }
    location = /readyz  { access_log off; proxy_pass http://127.0.0.1:8080; }
}
```

---

## Health probes

- `GET /healthz` — process is up (does not touch DB). 200 OK whenever the binary is reachable.
- `GET /readyz` — process **and** DB are reachable. Runs `SELECT 1`. 200 OK or 503.

systemd does not poll these — `Restart=on-failure` plus the journal are enough on a single-service box. If you front this with HAProxy/k8s in the future, point liveness at `/healthz` and readiness at `/readyz`.

---

## Backups

The only mutable state is `/var/lib/outpost/`. WAL mode is enabled, so use the SQLite `.backup` command (it's snapshot-consistent without blocking writers):

```bash
sudo -u outpost sqlite3 /var/lib/outpost/outpost.db \
  ".backup '/var/lib/outpost/backup-$(date +%Y%m%d-%H%M%S).db'"
```

For off-host continuous replication, deploy [Litestream](https://litestream.io/) as a sibling systemd unit pointing at any S3-compatible target (Cloud.ru, R2, B2). Out of scope of this initial deploy doc; the schematic is one systemd unit + one YAML config.

---

## Footprint

Designed-target on a 1 vCPU / 512 MB droplet, alongside SQLite, nginx, and Ubuntu base:

- `outpost-server` — ≤50 MB RSS under nominal load. Capped at 256 MB by `MemoryMax=` in the unit; the OOM killer reaps the server (not sshd or nginx) if a leak ever blows past it.
- SQLite DB — proportional to fleet size; tens of MB for hundreds of devices.
- nginx + Ubuntu base — ~100-150 MB.

Measure with `systemd-cgtop` (live) and `systemctl status outpost-server` (the `Memory:` line is the cgroup-reported RSS for the service).

---

## Hardening checklist

- [x] Static musl binary; no runtime libc / loader dependency.
- [x] Runs as system user `outpost` (UID assigned by useradd), shell `/usr/sbin/nologin`.
- [x] Secrets in `/etc/outpost/env` (root:outpost, mode 640), never in the binary or in the repo.
- [x] systemd hardening: `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `PrivateDevices`, `SystemCallFilter=@system-service`, empty `CapabilityBoundingSet` and `AmbientCapabilities`.
- [x] `MemoryMax=256M` caps RSS at the cgroup level.
- [x] `cargo audit` + `cargo deny check` in CI.
- [x] WAL mode + foreign keys enforced at connection open.
- [x] argon2id password hashing.
- [x] HMAC-signed download URLs (no anonymous public access).
- [x] Multi-tenant scoping on every read/write.
- [x] TLS via certbot/nginx, auto-renew armed.
- [ ] Off-host backups via Litestream (optional).
- [ ] Log shipping (optional, e.g. journalctl → Vector → Loki).
