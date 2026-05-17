# syntax=docker/dockerfile:1.7
# Multi-stage build: Rust + musl-tools build, Chainguard static runtime.
# Target: minimal footprint for 1 vCPU / 512 MB Ubuntu droplet.

FROM rust:1-slim-bookworm AS builder
RUN rustup target add x86_64-unknown-linux-musl \
    && apt-get update \
    && apt-get install -y --no-install-recommends musl-tools pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependencies separately from source — Cargo.toml first
COPY Cargo.toml Cargo.lock* ./
COPY crates/outpost-server/Cargo.toml crates/outpost-server/
COPY crates/outpost-core/Cargo.toml crates/outpost-core/
COPY crates/outpost-migrations/Cargo.toml crates/outpost-migrations/
RUN mkdir -p crates/outpost-server/src crates/outpost-core/src crates/outpost-migrations/src \
    && echo "fn main(){}" > crates/outpost-server/src/main.rs \
    && echo "" > crates/outpost-core/src/lib.rs \
    && echo "" > crates/outpost-migrations/src/lib.rs \
    && cargo build --release --target x86_64-unknown-linux-musl --bin outpost-server

# Real source
COPY . .
RUN touch crates/outpost-server/src/main.rs \
    && cargo build --release --target x86_64-unknown-linux-musl --bin outpost-server

# Runtime: Chainguard static (no shell, no glibc, nonroot by default)
FROM cgr.dev/chainguard/static:latest
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/outpost-server /usr/local/bin/outpost-server
ENV BIND_ADDR=0.0.0.0:8080 \
    RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/outpost-server"]
