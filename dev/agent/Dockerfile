# Development Dockerfile for ZeroClaw Agent
# Builds from source but runs on Debian (with shell) instead of distroless.
#
# Build context MUST be the repository root (..).

# ── Stage 1: Build ──
FROM rust:1.83-slim AS builder

WORKDIR /app

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
# Create dummy main.rs to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release --locked
RUN rm -rf src

# 2. Copy source code
COPY . .
# Touch main.rs to force rebuild
RUN touch src/main.rs
RUN cargo build --release --locked && \
    strip target/release/zeroclaw

# ── Stage 2: Development Runtime (Debian - includes shell, debug tools) ──
FROM debian:bookworm-slim AS dev

# Install runtime dependencies + basic debug tools (curl, ping, git)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    openssl \
    curl \
    git \
    iputils-ping \
    vim \
    && rm -rf /var/lib/apt/lists/*

# Setup permissions and directories
RUN mkdir -p /zeroclaw-data/.zeroclaw/workspace && \
    chown -R 65534:65534 /zeroclaw-data

# Minimal config for docker networking
# NOTE: api_key here is overloaded to store the Ollama Base URL
RUN cat > /zeroclaw-data/.zeroclaw/config.toml << 'EOF'
workspace_dir = "/zeroclaw-data/.zeroclaw/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
# This is the Ollama Base URL, not a secret key
api_key = "http://host.docker.internal:11434"
default_provider = "ollama"
default_model = "llama3.2"
default_temperature = 0.7

[gateway]
port = 3000
host = "0.0.0.0"
allow_public_bind = true
EOF
RUN chown -R 65534:65534 /zeroclaw-data

# Copy binary
COPY --from=builder /app/target/release/zeroclaw /usr/local/bin/zeroclaw

# Environment setup
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/.zeroclaw/workspace
ENV HOME=/zeroclaw-data
# Default to Ollama configuration
ENV API_KEY=${API_KEY:-http://host.docker.internal:11434}
ENV PROVIDER=${PROVIDER:-ollama}
ENV ZEROCLAW_MODEL=${ZEROCLAW_MODEL:-llama3.2}
ENV ZEROCLAW_GATEWAY_PORT=3000

WORKDIR /zeroclaw-data


# Run as non-root user (nobody)
USER 65534:65534

EXPOSE 3000

# Flexible entrypoint
ENTRYPOINT ["zeroclaw"]
CMD ["gateway", "--port", "3000", "--host", "[::]"]
