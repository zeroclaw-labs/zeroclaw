# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.83-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked && \
    strip target/release/zeroclaw

# ── Stage 2: Runtime (distroless nonroot — no shell, no OS, tiny, UID 65534) ──
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/zeroclaw /usr/local/bin/zeroclaw

# Default workspace and data directory (owned by nonroot user)
VOLUME ["/data"]
ENV ZEROCLAW_WORKSPACE=/data/workspace

# ── Environment variable configuration (Docker-native setup) ──
# These can be overridden at runtime via docker run -e or docker-compose
#
# Required:
#   API_KEY or ZEROCLAW_API_KEY     - Your LLM provider API key
#
# Optional:
#   PROVIDER or ZEROCLAW_PROVIDER   - LLM provider (default: openrouter)
#                                     Options: openrouter, openai, anthropic, ollama
#   ZEROCLAW_MODEL                  - Model to use (default: anthropic/claude-sonnet-4-20250514)
#   PORT or ZEROCLAW_GATEWAY_PORT   - Gateway port (default: 3000)
#
# Example:
#   docker run -e API_KEY=sk-... -e PROVIDER=openrouter zeroclaw/zeroclaw

# Explicitly set non-root user (distroless:nonroot defaults to 65534, but be explicit)
USER 65534:65534

EXPOSE 3000

ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
