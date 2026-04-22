# syntax=docker/dockerfile:1.7
#
# deploy/zeroclaw/Dockerfile — Tailscale-equipped zeroclaw image for
# adi-zeroclaw-shane and adi-zeroclaw-meg. Builds on the root Dockerfile's
# builder stage (Rust 1.93), then produces a Debian-based runtime with
# tailscaled bundled so the machine joins the adi tailnet.
#
# Build context: repo root. Invoke via:
#   fly deploy --config deploy/zeroclaw/fly.shane.toml
# which sets the correct context.

# ── Stage 0: Frontend build ─────────────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN npm ci --ignore-scripts 2>/dev/null || npm install --ignore-scripts
COPY web/ .
RUN npm run build

# ── Stage 1: Build zeroclaw binary ───────────────────────────────
FROM rust:1.93-slim AS builder

WORKDIR /app
ARG ZEROCLAW_CARGO_FEATURES="channel-telegram,channel-slack,backend-postgres"

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY src/ src/
COPY benches/ benches/
COPY apps/tauri/Cargo.toml apps/tauri/Cargo.toml
RUN mkdir -p apps/tauri/src \
    && echo "fn main() {}" > apps/tauri/src/main.rs \
    && echo "fn main() {}" > apps/tauri/build.rs

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked --features "$ZEROCLAW_CARGO_FEATURES" && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw

# ── Stage 2: Runtime (Debian + Tailscale) ───────────────────────
# trixie to match rust:1.93-slim's GLIBC (bookworm is too old).
FROM debian:trixie-slim AS runtime

ARG TAILSCALE_VERSION=1.80.2

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl iproute2 procps tini \
    && curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE_VERSION}_amd64.tgz" -o /tmp/tailscale.tgz \
    && tar -xzf /tmp/tailscale.tgz -C /tmp \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscaled" /usr/local/bin/tailscaled \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscale"  /usr/local/bin/tailscale \
    && rm -rf /tmp/tailscale* /var/lib/apt/lists/*

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=web-builder /web/dist /app/web/dist

# Runtime layout: /zeroclaw-data is the persistent volume mount.
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace

COPY deploy/zeroclaw/config.seed.toml /opt/zeroclaw.config.seed.toml
COPY deploy/zeroclaw/entrypoint.sh    /entrypoint.sh
RUN chmod 0755 /entrypoint.sh

ENV LANG=C.UTF-8
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV ZEROCLAW_GATEWAY_PORT=42617

EXPOSE 42617

# Runs as ROOT so tailscaled can open /dev/net/tun. entrypoint.sh
# starts tailscaled, seeds config, then execs zeroclaw.
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
