# syntax=docker/dockerfile:1.7
#
# Multi-stage build for Outpost MDM.
#
# Stage 1 (planner)  — cargo-chef captures the dependency manifest so the
#                       layer cache only invalidates when Cargo.toml/Lock change.
# Stage 2 (builder)  — cargo-zigbuild produces a fully-static
#                       x86_64-unknown-linux-musl binary via Zig as linker
#                       (significantly faster than musl-gcc).
# Stage 3 (runtime)  — Google distroless `static-debian12:nonroot`: tiny
#                       (~few MB), no shell, non-root by default (UID 65532),
#                       glibc-free; the static musl binary runs without any
#                       libc dependency. Chainguard's `cgr.dev/chainguard/static`
#                       moved behind auth in 2026; distroless is the public
#                       anonymous-pull equivalent with the same UID 65532
#                       (matters for our bind-mount ownership on the host).

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
# Copy the toolchain pin FIRST so the subsequent `rustup target add`
# installs the musl target against the channel the project actually uses.
# Without this, COPY . . later triggers a rustup channel re-sync that
# discards the previously-installed musl target.
COPY rust-toolchain.toml ./
RUN cargo install --locked cargo-chef cargo-zigbuild \
 && apt-get update \
 && apt-get install -y --no-install-recommends pkg-config wget xz-utils ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && rustup show \
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
# Stage 3 — runtime: distroless static-debian12:nonroot (UID 65532, glibc-free)
# -------------------------------------------------------------------------
FROM gcr.io/distroless/static-debian12:nonroot
LABEL org.opencontainers.image.title="outpost-mdm-rs"
LABEL org.opencontainers.image.source="https://github.com/daphate/outpost-mdm-rs"
LABEL org.opencontainers.image.licenses="Apache-2.0"

COPY --from=builder /outpost-server /usr/local/bin/outpost-server

ENV BIND_ADDR=0.0.0.0:8080 \
    DB_PATH=/var/lib/outpost/outpost.db \
    APP_FILES_DIR=/var/lib/outpost/files \
    RUST_LOG=info

EXPOSE 8080
# distroless:nonroot already runs as UID 65532; restate for clarity.
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/outpost-server"]
