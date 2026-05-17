# Outpost MDM — Deployment Guide

Production-target: **Ubuntu 24.04 droplet, 1 vCPU / 512 MB RAM** (e.g. `mdm.secondf8n.tech`).

## TL;DR

```bash
ssh -i ~/.ssh/awscalifornia ubuntu@mdm.secondf8n.tech
sudo apt update && sudo apt install -y docker.io docker-compose-plugin nginx certbot python3-certbot-nginx

# pull the latest image from GHCR
docker pull ghcr.io/daphate/outpost-mdm-rs:latest

# create the production .env
sudo mkdir -p /opt/outpost && cd /opt/outpost
sudo tee .env >/dev/null <<EOF
JWT_SECRET=$(openssl rand -base64 48)
RUST_LOG=info
EOF
sudo chmod 600 .env

# write a compose file (or copy from this repo's docker-compose.yml)
sudo tee docker-compose.yml >/dev/null <<'EOF'
services:
  outpost-server:
    image: ghcr.io/daphate/outpost-mdm-rs:latest
    ports: ["127.0.0.1:8080:8080"]
    env_file: .env
    environment:
      BIND_ADDR: 0.0.0.0:8080
      DB_PATH: /var/lib/outpost/outpost.db
      APP_FILES_DIR: /var/lib/outpost/files
    volumes: ["outpost-data:/var/lib/outpost"]
    restart: unless-stopped
volumes: { outpost-data: {} }
EOF

sudo docker compose up -d
sudo docker compose logs -f | head -40  # capture BOOTSTRAP admin password (one-shot)

# TLS terminator
sudo certbot --nginx -d mdm.secondf8n.tech
# /etc/nginx/sites-available/default proxies to 127.0.0.1:8080
```

## Capture the bootstrap admin password

On first boot, `outpost-server` detects the seed admin row whose `password_hash IS NULL` and prints a one-shot password to stderr:

```
==============================================================
  BOOTSTRAP: initial password for 'admin' (user_id=1)

  XXXXXXXXXXXXXXXXXXXX

  Capture this NOW — it is not recoverable after this boot.
==============================================================
```

Copy it from `docker compose logs` and store securely. The server marks `must_change_password = 1`, so the first login at the API forces a password change.

## Required environment variables

| Var | Default | Required | Description |
|-----|---------|----------|-------------|
| `JWT_SECRET`     | — | **yes** | ≥32 bytes, used for HS512 JWT + HMAC-signed download URLs. Generate with `openssl rand -base64 48`. |
| `BIND_ADDR`      | `0.0.0.0:8080` | no | Listen socket. |
| `DB_PATH`        | `/var/lib/outpost/outpost.db` | no | SQLite file path. |
| `APP_FILES_DIR`  | `/var/lib/outpost/files` | no | Storage root for uploaded APKs / models. |
| `RUST_LOG`       | `info` | no | `tracing_subscriber::EnvFilter` directive. |
| `JWT_TTL_SECS`   | `86400` | no | Session token lifetime (24 h default). |

The server **refuses to start** if `JWT_SECRET` is missing or shorter than 32 bytes.

## nginx reverse proxy snippet

```nginx
server {
    listen 443 ssl http2;
    server_name mdm.secondf8n.tech;

    ssl_certificate     /etc/letsencrypt/live/mdm.secondf8n.tech/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/mdm.secondf8n.tech/privkey.pem;

    client_max_body_size 200M;   # APK uploads

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host              $host;
        proxy_set_header X-Real-IP         $remote_addr;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Request-Id      $request_id;
        proxy_http_version 1.1;
        proxy_read_timeout 120s;   # long-poll friendly
    }
}
```

## Health probes

- `GET /healthz` — process is up (does not touch the DB). 200 OK always when the binary is reachable.
- `GET /readyz` — process AND DB are reachable. Runs `SELECT 1` against SQLite. 200 OK or 503 SERVICE_UNAVAILABLE.

Configure your orchestrator's liveness + readiness probes accordingly.

## Backups

The single source of truth is the SQLite database at `$DB_PATH`. WAL mode is enabled, so the consistent-read snapshot pattern is:

```bash
sudo docker compose exec outpost-server sh -c 'sqlite3 /var/lib/outpost/outpost.db ".backup /var/lib/outpost/backup-$(date +%Y%m%d-%H%M%S).db"'
```

For continuous replication, deploy [Litestream](https://litestream.io/) alongside the server pointing at any S3-compatible target (Cloud.ru, R2, B2, etc.). Configuration is out of scope of this initial deploy doc.

## Footprint

Designed-target on a 1 vCPU / 512 MB droplet, alongside SQLite and nginx:

- Server process — ≤50 MB RSS under nominal load (measure with `docker stats outpost-server` post-deploy)
- SQLite DB — proportional to fleet size; tens of MB for hundreds of devices
- nginx + Ubuntu base — ~100-150 MB

These are design targets, not measurements. Confirm with `docker stats` and `free -h` after deployment.

## Hardening checklist

- [x] Static musl binary (Chainguard `static` image is glibc-free, no shell)
- [x] Runs as `USER nonroot`
- [x] All secrets via `.env` (mode `600`), never baked into the image
- [x] Trivy scan in CI gates HIGH/CRITICAL CVEs
- [x] `cargo audit` + `cargo deny check` in CI
- [x] WAL mode + foreign keys enforced
- [x] argon2id password hashing (no MD5 carryover from upstream)
- [x] HMAC-signed download URLs (no anonymous public access)
- [x] Multi-tenant scoping on every read/write
- [ ] TLS via certbot/nginx (operator-installed, per droplet)
- [ ] Off-host backups via Litestream (optional)
- [ ] Log shipping (operator-installed, e.g. promtail → Loki)
