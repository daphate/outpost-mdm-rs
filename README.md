# Outpost MDM (Rust)

Outpost MDM is a field-grade device management server for managing fleets of [Outpost-Android](https://github.com/daphate/tactical-ar-hud) tactical devices (Ulefone Armor 28 Ultra and analogs).

> **Статус:** Действующий сервер, версия 0.18.22 (на 17 июня 2026). Активно дорабатывается: харденинг безопасности, наблюдаемость (Grafana/OTLP), device-facing API раздачи комплектов (bundles), шифрованная раздача. План производственного развёртывания — в [`docs/PRODUCTION-ROLLOUT-PLAN.md`](docs/PRODUCTION-ROLLOUT-PLAN.md). The architecture, schema, and feature surface draw inspiration from [Headwind MDM](https://github.com/h-mdm/hmdm-server) (Apache 2.0). An earlier Java fork at [daphate/outpost-mdm](https://github.com/daphate/outpost-mdm) is preserved as an archival reference; this Rust project is the active codebase.

## Stack

- **Language:** Rust stable (edition 2024)
- **HTTP framework:** [axum](https://github.com/tokio-rs/axum) 0.8
- **Persistence:** SQLite via [sqlx](https://github.com/launchbadge/sqlx) (WAL mode)
- **Auth:** opaque DB-backed session tokens (sha256-hashed PK, HttpOnly cookies) + argon2id password hashing
- **Templating:** [Askama](https://github.com/askama-rs/askama) with HTMX 2.x + Tailwind v4 (CDN in admin UI)
- **OpenAPI:** [utoipa](https://github.com/juhaku/utoipa)
- **Deployment:** single static musl ELF (~12 MB) supervised by systemd; no container runtime in prod

## Design constraints

Designed to fit in **1 vCPU / 512 MB RAM Ubuntu 24.04** alongside SQLite and nginx. Target ≤50 MB RSS for the server process under nominal load.

## Workspace layout

```
crates/
  outpost-server/      # axum HTTP binary
  outpost-core/        # domain types and services
  outpost-migrations/  # sqlx SQLite migrations
deploy/
  outpost-server.service   # systemd unit
  deploy.ps1               # Windows-host cross-compile + scp + restart
.github/workflows/ci.yml
```

## Quick start

```sh
# Local development — set a long secret first
export APP_SECRET="$(openssl rand -base64 48)"
cargo run -p outpost-server

# Health
curl http://localhost:8080/healthz   # liveness
curl http://localhost:8080/readyz    # readiness (touches DB)
```

On first boot the server prints the bootstrap admin password to stderr
exactly once. In dev: read from your terminal. In prod (systemd):
```sh
sudo journalctl -u outpost-server | grep -A 2 BOOTSTRAP
```

## Deployment

Production runs on `mdm.secondf8n.tech` as a systemd service. The deploy
loop is intentionally tiny:

```powershell
# From a Windows dev box (cargo-zigbuild + Zig 0.13 + musl target installed)
.\deploy\deploy.ps1
```

The script cross-compiles `outpost-server` to `x86_64-unknown-linux-musl`,
`scp`s the binary into `/usr/local/bin/outpost-server.<sha>`, atomically
flips the `/usr/local/bin/outpost-server` symlink, and `systemctl restart`s
the service. N-1 revisions stay on the host for one-symlink rollback.

See [`docs/DEPLOY.md`](docs/DEPLOY.md) for the full runbook
(Ubuntu droplet + nginx + certbot, sizing, env vars, backups, hardening
checklist).

## Documentation

| Doc | What's in it |
| --- | --- |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Module map, request lifecycle, persistence, auth model, push pipeline |
| [`docs/DEPLOY.md`](docs/DEPLOY.md)             | Production deploy runbook (Ubuntu droplet + nginx + certbot, env vars, backups) |
| [`CHANGELOG.md`](CHANGELOG.md)                 | Per-phase narrative of what changed and why |
| [`SECURITY.md`](SECURITY.md)                   | Vulnerability disclosure policy + cryptographic posture |
| [`CONTRIBUTING.md`](CONTRIBUTING.md)           | Dev setup, coding conventions, PR checklist |

## License

Apache License 2.0. See `LICENSE` for full text.

Acknowledgement: this project draws design inspiration from [Headwind MDM](https://h-mdm.com) (Apache 2.0), which is independently licensed by Headwind Solutions LLC.
