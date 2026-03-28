# syntax=docker/dockerfile:1.7
#
# MoA (ZeroClaw) Railway Deployment Dockerfile
# Multi-stage build optimized for Railway hosting.
#
# Stage 1 (builder): Compiles the ZeroClaw binary from source.
# Stage 2 (runtime): Minimal Debian image with only runtime dependencies.
#
# Usage:
#   Railway auto-detects this Dockerfile via railway.toml.
#   For local testing:
#     docker build -f deploy/railway/Dockerfile -t zeroclaw-railway .

# ── Stage 0: Build Web Frontend ──────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --ignore-scripts
COPY web/ ./
RUN npm run build

# ── Stage 1: Builder ─────────────────────────────────────────
FROM rust:1.87-slim-bookworm AS builder

WORKDIR /build

# Install build-time dependencies required by native crates.
# - pkg-config: Locates system libraries for Rust build scripts.
# - libssl-dev: OpenSSL headers (required by reqwest/rustls fallback paths).
# - build-essential: C toolchain for native crate compilation (rusqlite bundled, etc.).
# - cmake: Required by some transitive native dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        build-essential \
        cmake \
    && rm -rf /var/lib/apt/lists/*

# Copy dependency manifests first to maximize Docker layer caching.
# A dummy main.rs triggers a dependency-only build so source changes
# do not invalidate the (slow) dependency download/compile layer.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release --locked -p zeroclaw 2>/dev/null || true
RUN rm -rf src

# Copy the full source tree and build the real binary.
COPY . .
# Use freshly built frontend assets from web-builder stage
COPY --from=web-builder /web/dist/ web/dist/
# Touch main.rs so cargo detects the source change and rebuilds.
RUN touch src/main.rs
RUN cargo build --release --locked -p zeroclaw \
    && cp target/release/zeroclaw /build/zeroclaw \
    && strip /build/zeroclaw

# ── Stage 2: Runtime ─────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install only the minimal runtime dependencies.
# - ca-certificates: TLS root certificates for HTTPS provider/channel calls.
# - libssl3: OpenSSL runtime library.
# - gosu: Secure privilege dropping (entrypoint runs as root to fix volume
#   permissions, then drops to zeroclaw user via gosu).
# - git: Required by the skills module to clone open-skills repositories.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        libssl3 \
        gosu \
        git \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user for the runtime process.
RUN groupadd --system zeroclaw \
    && useradd --system --gid zeroclaw --create-home --home-dir /app zeroclaw

WORKDIR /app

# Create workspace and config directories with gateway config.
# allow_public_bind is required because Railway routes external traffic
# through its reverse proxy to 0.0.0.0 inside the container.
# NOTE: These directories are created as a fallback for non-volume deploys.
# When a Railway volume is mounted at /app/.zeroclaw, the entrypoint script
# re-creates subdirectories and fixes ownership at runtime.
RUN mkdir -p /app/.zeroclaw /app/workspace \
    && printf 'default_temperature = 0.7\n\n[gateway]\nallow_public_bind = true\n' > /app/.zeroclaw/config.toml \
    && chmod 600 /app/.zeroclaw/config.toml \
    && chown -R zeroclaw:zeroclaw /app

# Copy the compiled binary from the builder stage.
COPY --from=builder /build/zeroclaw /usr/local/bin/zeroclaw

# Copy the entrypoint script that fixes volume mount permissions.
COPY deploy/railway/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Environment variables for Railway.
# ZEROCLAW_HOST: Bind to all interfaces (required for Railway port forwarding).
# ZEROCLAW_PORT: Default gateway port; Railway maps its internal PORT to this.
# HOME: Ensures config discovery works under the non-root user.
ENV ZEROCLAW_HOST=0.0.0.0
ENV ZEROCLAW_PORT=8080
ENV ZEROCLAW_ALLOW_PUBLIC_BIND=true
ENV ZEROCLAW_REQUIRE_PAIRING=false
ENV HOME=/app

# Expose the gateway port.
EXPOSE 8080

# Start as root so entrypoint.sh can fix volume permissions.
# The entrypoint drops to the zeroclaw user via gosu before exec'ing the CMD.
ENTRYPOINT ["entrypoint.sh"]

# Start the ZeroClaw gateway server.
# exec form required with ENTRYPOINT; PORT expansion handled via shell wrapper.
CMD ["sh", "-c", "zeroclaw gateway --host 0.0.0.0 --port ${PORT:-8080}"]
