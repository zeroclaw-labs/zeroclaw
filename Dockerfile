<<<<<<< HEAD
# Build backend
FROM rust:1.83-slim-bookworm as builder
WORKDIR /app
COPY . .
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin zeroclaw
=======
# syntax=docker/dockerfile:1.7

# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.92-slim@sha256:bf3368a992915f128293ac76917ab6e561e4dda883273c8f5c9f6f8ea37a378e AS builder

WORKDIR /app

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
# Create dummy main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked
RUN rm -rf src

# 2. Copy source code
COPY . .
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw
>>>>>>> origin/main

# Build frontend
FROM node:20-slim as frontend-builder
WORKDIR /app/frontend
COPY frontend/package*.json ./
RUN npm install
COPY frontend/ .
RUN npm run build

<<<<<<< HEAD
# Final image
FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
=======
# Create minimal config for PRODUCTION (allows binding to public interfaces)
# NOTE: Provider configuration must be done via environment variables at runtime
RUN cat > /zeroclaw-data/.zeroclaw/config.toml <<EOF
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = ""
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7
>>>>>>> origin/main

COPY --from=builder /app/target/release/zeroclaw /app/zeroclaw
COPY --from=builder /app/migrations /app/migrations
COPY --from=frontend-builder /app/frontend/dist /app/public

ENV ZEROCLAW_CONFIG_PATH=/app/config.toml
ENV PUBLIC_DIR=/app/public

<<<<<<< HEAD
CMD ["/app/zeroclaw", "daemon", "--host", "0.0.0.0", "--port", "8080"]
=======
# ── Stage 3: Development Runtime (Debian) ────────────────────
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS dev

# Install runtime dependencies + basic debug tools
RUN apt-get update && apt-get install -y \
    ca-certificates \
    openssl \
    curl \
    git \
    iputils-ping \
    vim \
    && rm -rf /var/lib/apt/lists/*

COPY --from=permissions /zeroclaw-data /zeroclaw-data
COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw

# Overwrite minimal config with DEV template (Ollama defaults)
COPY dev/config.template.toml /zeroclaw-data/.zeroclaw/config.toml
RUN chown 65534:65534 /zeroclaw-data/.zeroclaw/config.toml

# Environment setup
# Use consistent workspace path
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
# Defaults for local dev (Ollama) - matches config.template.toml
ENV PROVIDER="ollama"
ENV ZEROCLAW_MODEL="llama3.2"
ENV ZEROCLAW_GATEWAY_PORT=3000

# Note: API_KEY is intentionally NOT set here to avoid confusion.
# It is set in config.toml as the Ollama URL.

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 3000
ENTRYPOINT ["zeroclaw"]
CMD ["gateway", "--port", "3000", "--host", "[::]"]

# ── Stage 4: Production Runtime (Distroless) ─────────────────
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:84fcd3c223b144b0cb6edc5ecc75641819842a9679a3a58fd6294bec47532bf7 AS release

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=permissions /zeroclaw-data /zeroclaw-data

# Environment setup
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
# Default provider (model is set in config.toml, not here,
# so config file edits are not silently overridden)
ENV PROVIDER="openrouter"
ENV ZEROCLAW_GATEWAY_PORT=3000

# API_KEY must be provided at runtime!

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 3000
ENTRYPOINT ["zeroclaw"]
CMD ["gateway", "--port", "3000", "--host", "[::]"]
>>>>>>> origin/main
