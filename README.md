# Outpost MDM (Rust)

Outpost MDM is a field-grade device management server for managing fleets of [Outpost-Android](https://github.com/daphate/tactical-ar-hud) tactical devices (Ulefone Armor 28 Ultra and analogs).

> **Status:** Early greenfield work, May 2026. The architecture, schema, and feature surface draw inspiration from [Headwind MDM](https://github.com/h-mdm/hmdm-server) (Apache 2.0). An earlier Java fork at [daphate/outpost-mdm](https://github.com/daphate/outpost-mdm) is preserved as an archival reference; this Rust project is the active codebase.

## Stack

- **Language:** Rust stable (edition 2024)
- **HTTP framework:** [axum](https://github.com/tokio-rs/axum) 0.8
- **Persistence:** SQLite via [sqlx](https://github.com/launchbadge/sqlx) (WAL mode)
- **Auth:** JWT (jsonwebtoken) + argon2id password hashing
- **Templating:** [Askama](https://github.com/askama-rs/askama) with HTMX 2.x + Tailwind v4 standalone
- **OpenAPI:** [utoipa](https://github.com/juhaku/utoipa)
- **Container:** static musl binary on [Chainguard Wolfi](https://images.chainguard.dev/) base image

## Design constraints

Designed to fit in **1 vCPU / 512 MB RAM Ubuntu 24.04** alongside SQLite and nginx. Target ≤50 MB RSS for the server process under nominal load.

## Workspace layout

```
crates/
  outpost-server/      # axum HTTP binary
  outpost-core/        # domain types and services
  outpost-migrations/  # sqlx SQLite migrations
.github/workflows/ci.yml
Dockerfile
docker-compose.yml
```

## Quick start

```sh
# Local development — set a long secret first
export JWT_SECRET="$(openssl rand -base64 48)"
cargo run -p outpost-server

# With Docker (Compose) — needs .env file with JWT_SECRET
echo "JWT_SECRET=$(openssl rand -base64 48)" > .env
docker compose up --build

# Health
curl http://localhost:8080/healthz   # liveness
curl http://localhost:8080/readyz    # readiness (touches DB)
```

The server prints the bootstrap admin password to stderr exactly once on
first boot — capture from `docker compose logs` before the container
exits or restarts.

## Deployment

See [`docs/DEPLOY.md`](docs/DEPLOY.md) for the production deploy guide
(Ubuntu droplet + nginx + certbot, sizing, env vars, backups,
hardening checklist).

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
