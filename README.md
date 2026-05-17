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
# Local development
cargo run -p outpost-server

# With Docker
docker compose up --build

# Health check
curl http://localhost:8080/healthz
```

## License

Apache License 2.0. See `LICENSE` for full text.

Acknowledgement: this project draws design inspiration from [Headwind MDM](https://h-mdm.com) (Apache 2.0), which is independently licensed by Headwind Solutions LLC.
