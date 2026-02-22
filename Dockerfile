# syntax=docker/dockerfile:1.7

# ── Stage 1: Build Frontend (Node.js) ─────────────────────────
FROM node:20-slim AS frontend-builder
WORKDIR /app/web
COPY web/package*.json ./
RUN npm install
COPY web/ .
RUN npm run build

# ── Stage 2: Build Backend (Rust) ─────────────────────────────
FROM rust:1.93-slim@sha256:9663b80a1621253d30b146454f903de48f0af925c967be48c84745537cd35d8b AS builder

WORKDIR /app

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY --from=frontend-builder /app/web/dist ./web/dist
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml

RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked

RUN rm -rf src benches crates/robot-kit/src
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY firmware/ firmware/

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw

# 准备运行时配置：确保 [channels_config] 结构完整
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace && \
    cat > /zeroclaw-data/.zeroclaw/config.toml <<EOF
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = "AIzaSyBJrfIw8nf_D7QCokVLQkq3HPw87XjG3_M"
default_provider = "google"
default_model = "gemini-2.5-flash"
default_temperature = 0.7

[reliability]
api_keys = ["AIzaSyBJrfIw8nf_D7QCokVLQkq3HPw87XjG3_M"]

[channels_config]
cli = true
message_timeout_secs = 300

[channels_config.telegram]
enabled = true
bot_token = "8543592134:AAGHmulHpg89eVl_Lns_VIFfrt5cXnvbP5c"
chat_id = "8519504418"
allowed_users = ["8519504418"]

[gateway]
port = 8080
host = "0.0.0.0"
allow_public_bind = true
EOF

RUN chmod 600 /zeroclaw-data/.zeroclaw/config.toml && \
    chown -R 65534:65534 /zeroclaw-data

# ── Stage 3: Production Runtime ──────────────────────────────
FROM debian:trixie-slim AS release

RUN apt-get update && apt-get install -y ca-certificates libssl3 curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /zeroclaw-data /zeroclaw-data

ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_GATEWAY_PORT=8080

USER 65534:65534
WORKDIR /zeroclaw-data
EXPOSE 8080

# 核心修正：使用程序要求的 --config-dir 参数，指向文件夹路径
ENTRYPOINT ["zeroclaw"]
CMD ["gateway", "--config-dir", "/zeroclaw-data/.zeroclaw"]
