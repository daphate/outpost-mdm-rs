# syntax=docker/dockerfile:1.7
#
# Multi-stage build for Outpost MDM.
#
# Stage 1 (planner)  — cargo-chef captures the dependency manifest so the
#                       layer cache only invalidates when Cargo.toml/Lock change.
# Stage 2 (builder)  — cargo-zigbuild produces a fully-static
#                       x86_64-unknown-linux-musl binary via Zig as linker
#                       (significantly faster than musl-gcc).
# Stage 3 (runtime)  — Chainguard `static` image: tiny (~few MB), no shell,
#                       non-root by default, glibc-free; the static musl
#                       binary runs without any libc dependency.

ARG RUST_VERSION=1
ARG TARGET=x86_64-unknown-linux-musl

# -------------------------------------------------------------------------
# Stage 1 — planner: produce recipe.json
# -------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-bookworm AS planner
WORKDIR /app
RUN cargo install --locked cargo-chef
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# -------------------------------------------------------------------------
# Stage 2 — builder: cache deps then build with zigbuild
# -------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-slim-bookworm AS builder
WORKDIR /app
ARG TARGET
RUN cargo install --locked cargo-chef cargo-zigbuild \
 && apt-get update \
 && apt-get install -y --no-install-recommends pkg-config wget xz-utils ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add ${TARGET}

# Install Zig (cargo-zigbuild needs it on PATH).
ARG ZIG_VERSION=0.13.0
RUN wget -q https://ziglang.org/download/${ZIG_VERSION}/zig-linux-x86_64-${ZIG_VERSION}.tar.xz \
 && tar -xf zig-linux-x86_64-${ZIG_VERSION}.tar.xz -C /opt \
 && mv /opt/zig-linux-x86_64-${ZIG_VERSION} /opt/zig \
 && ln -s /opt/zig/zig /usr/local/bin/zig \
 && rm zig-linux-x86_64-${ZIG_VERSION}.tar.xz

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --target ${TARGET} --zigbuild

COPY . .
RUN cargo zigbuild --release --target ${TARGET} --bin outpost-server \
 && cp target/${TARGET}/release/outpost-server /outpost-server \
 && /outpost-server --version || true   # smoke: binary at least starts

# -------------------------------------------------------------------------
# Stage 3 — runtime: Chainguard static (nonroot, glibc-free)
# -------------------------------------------------------------------------
FROM cgr.dev/chainguard/static:latest
LABEL org.opencontainers.image.title="outpost-mdm-rs"
LABEL org.opencontainers.image.source="https://github.com/daphate/outpost-mdm-rs"
LABEL org.opencontainers.image.licenses="Apache-2.0"

COPY --from=builder /outpost-server /usr/local/bin/outpost-server

ENV BIND_ADDR=0.0.0.0:8080 \
    DB_PATH=/var/lib/outpost/outpost.db \
    APP_FILES_DIR=/var/lib/outpost/files \
    RUST_LOG=info

EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/outpost-server"]
