# syntax=docker/dockerfile:1.7

# ── Stage 1: Build Frontend (Node.js) ─────────────────────────
FROM node:20-slim AS frontend-builder
WORKDIR /app/web
COPY web/package*.json ./
RUN npm install
COPY web/ .
RUN npm run build [cite: 1, 2]

# ── Stage 2: Build Backend (Rust) ─────────────────────────────
FROM rust:1.93-slim@sha256:9663b80a1621253d30b146454f903de48f0af925c967be48c84745537cd35d8b AS builder

WORKDIR /app

# 安装构建所需的系统依赖
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/* [cite: 2]

# 1. 拷贝前端构建结果
COPY --from=frontend-builder /app/web/dist ./web/dist [cite: 2]

# 2. 拷贝 manifests 缓存依赖
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml [cite: 2]

# 创建哑目标以加速缓存
RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs [cite: 2, 3]

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked [cite: 3]

RUN rm -rf src benches crates/robot-kit/src [cite: 3]

# 3. 拷贝源码并正式编译
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY firmware/ firmware/ [cite: 3]

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw [cite: 3]

# 准备运行时配置：注入 Google API Key 和 Telegram 配置 
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace && \
    cat > /zeroclaw-data/.zeroclaw/config.toml <<EOF 
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = "AIzaSyCJcSfhSmgNl5VHZ_bdBPqvFF79fkWaPTQ"
default_provider = "google"
default_model = "google/gemini-2.0-flash"

[reliability]
api_keys = ["AIzaSyCJcSfhSmgNl5VHZ_bdBPqvFF79fkWaPTQ"]

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
paired_tokens = ["5247d9d046f1e53ed49a0f5b4d509cd8432aae614fb8971c9f3d3821866fb8e7"]
EOF

# 修正权限
RUN chown -R 65534:65534 /zeroclaw-data 

# ── Stage 3: Production Runtime (Debian Trixie) ──────────────
FROM debian:trixie-slim AS release

# 安装运行时必要的库
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* 

# 从编译阶段拷贝二进制文件
COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /zeroclaw-data /zeroclaw-data 

# 环境配置
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_GATEWAY_PORT=8080

# 切换到非 root 用户以保证安全
USER 65534:65534
WORKDIR /zeroclaw-data
EXPOSE 8080
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
