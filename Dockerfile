# syntax=docker/dockerfile:1.7

# ── Stage 0: Frontend build ─────────────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN npm ci --ignore-scripts 2>/dev/null || npm install --ignore-scripts
COPY web/ .
RUN npm run build

# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.94-slim@sha256:da9dab7a6b8dd428e71718402e97207bb3e54167d37b5708616050b1e8f60ed6 AS builder

WORKDIR /app
ARG ZEROCLAW_CARGO_FEATURES=""

# Install build dependencies
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml
# Create dummy targets declared in Cargo.toml so manifest parsing succeeds.
RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    if [ -n "$ZEROCLAW_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$ZEROCLAW_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi
RUN rm -rf src benches crates/robot-kit/src

# 2. Copy only build-relevant source paths (avoid cache-busting on docs/tests/scripts)
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY --from=web-builder /web/dist web/dist
COPY *.rs .
RUN touch src/main.rs
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    rm -rf target/release/.fingerprint/zeroclawlabs-* \
           target/release/deps/zeroclawlabs-* \
           target/release/incremental/zeroclawlabs-* && \
    if [ -n "$ZEROCLAW_CARGO_FEATURES" ]; then \
      cargo build --release --locked --features "$ZEROCLAW_CARGO_FEATURES"; \
    else \
      cargo build --release --locked; \
    fi && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw
RUN size=$(stat -c%s /app/zeroclaw 2>/dev/null || stat -f%z /app/zeroclaw) && \
    if [ "$size" -lt 1000000 ]; then echo "ERROR: binary too small (${size} bytes), likely dummy build artifact" && exit 1; fi

# Prepare runtime directory structure and default config inline (no extra stage)
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace && \
    printf '%s\n' \
        'workspace_dir = "/zeroclaw-data/workspace"' \
        'config_path = "/zeroclaw-data/.zeroclaw/config.toml"' \
        'api_key = ""' \
        'default_provider = "openrouter"' \
        'default_model = "anthropic/claude-sonnet-4-20250514"' \
        'default_temperature = 0.7' \
        '' \
        '[gateway]' \
        'port = 42617' \
        'host = "[::]"' \
        'allow_public_bind = true' \
        > /zeroclaw-data/.zeroclaw/config.toml && \
    chown -R 65534:65534 /zeroclaw-data

# ── Stage 2: Development Runtime (Debian) ────────────────────
FROM debian:trixie-slim@sha256:f6e2cfac5cf956ea044b4bd75e6397b4372ad88fe00908045e9a0d21712ae3ba AS dev

# Install essential runtime dependencies only (use docker-compose.override.yml for dev tools)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /zeroclaw-data /zeroclaw-data
COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw

# Overwrite minimal config with DEV template (Ollama defaults)
COPY dev/config.template.toml /zeroclaw-data/.zeroclaw/config.toml
RUN chown 65534:65534 /zeroclaw-data/.zeroclaw/config.toml

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
# Use consistent workspace path
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
# Defaults for local dev (Ollama) - matches config.template.toml
ENV PROVIDER="ollama"
ENV ZEROCLAW_MODEL="llama3.2"
ENV ZEROCLAW_GATEWAY_PORT=42617

# Note: API_KEY is intentionally NOT set here to avoid confusion.
# It is set in config.toml as the Ollama URL.

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617
HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["zeroclaw", "status", "--format=exit-code"]
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]

# ── Stage 3: Production Runtime (Distroless) ─────────────────
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:84fcd3c223b144b0cb6edc5ecc75641819842a9679a3a58fd6294bec47532bf7 AS release

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /zeroclaw-data /zeroclaw-data

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
# Default provider and model are set in config.toml, not here,
# so config file edits are not silently overridden
#ENV PROVIDER=
ENV ZEROCLAW_GATEWAY_PORT=42617

# API_KEY must be provided at runtime!

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617
HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["zeroclaw", "status", "--format=exit-code"]
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
