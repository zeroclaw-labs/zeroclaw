# ── Stage 1: Build ────────────────────────────────────────────
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

# ── Stage 2: Create data directories with correct permissions ──
FROM busybox:latest AS permissions
# Create the directory structure that the app expects
# This includes a minimal config.toml that allows Docker deployments to work
RUN mkdir -p /zeroclaw-data/.zeroclaw/workspace && \
    chown -R 65534:65534 /zeroclaw-data

# Create minimal config.toml required for Docker (allows binding to public interfaces)
# NOTE: Provider configuration should be done via environment variables at runtime
# These are placeholder values that will be overridden by environment variables
RUN cat > /zeroclaw-data/.zeroclaw/config.toml << 'EOF'
workspace_dir = "/zeroclaw-data/.zeroclaw/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = ""
default_provider = "openrouter"
default_model = ""
default_temperature = 0.7

[gateway]
port = 3000
host = "0.0.0.0"
allow_public_bind = true
EOF
RUN chown -R 65534:65534 /zeroclaw-data

# ── Stage 3: Runtime (distroless nonroot — no shell, no OS, tiny, UID 65534) ──
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/zeroclaw /usr/local/bin/zeroclaw
COPY --from=permissions /zeroclaw-data /zeroclaw-data

# Environment variables with sensible defaults (overridable at runtime)
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/.zeroclaw/workspace
ENV HOME=/zeroclaw-data
ENV API_KEY=${API_KEY:-}
ENV PROVIDER=${PROVIDER:-openrouter}
ENV ZEROCLAW_MODEL=${ZEROCLAW_MODEL:-anthropic/claude-sonnet-4-20250514}
ENV ZEROCLAW_GATEWAY_PORT=${ZEROCLAW_GATEWAY_PORT:-3000}

# Set working directory so that relative paths resolve correctly
WORKDIR /zeroclaw-data

# ── Environment variable configuration (Docker-native setup) ──
# These can be overridden at runtime via docker run -e or docker-compose
#
# Required:
#   API_KEY or ZEROCLAW_API_KEY     - Your LLM provider API key
#
# Optional:
#   PROVIDER or ZEROCLAW_PROVIDER   - LLM provider (default: openrouter)
#                                     Options: openrouter, openai, anthropic, ollama
#   ZEROCLAW_MODEL                  - Model to use (provider-specific)
#   ZEROCLAW_GATEWAY_PORT           - Gateway port (default: 3000)
#   ZEROCLAW_WORKSPACE              - Workspace directory (default: /zeroclaw-data/.zeroclaw/workspace)
#
# For Ollama:
#   API_KEY=http://host.docker.internal:11434  (Ollama base URL)
#
# Example:
#   docker run -e API_KEY=sk-... -e PROVIDER=openrouter zeroclaw/zeroclaw
#   docker run -e API_KEY=http://host.docker.internal:11434 -e PROVIDER=ollama -e ZEROCLAW_MODEL=llama3.2:latest zeroclaw/zeroclaw

# Explicitly set non-root user (distroless:nonroot defaults to 65534, but be explicit)
USER 65534:65534

EXPOSE 3000

ENTRYPOINT ["zeroclaw"]
CMD ["gateway", "--port", "3000", "--host", "[::]"]
