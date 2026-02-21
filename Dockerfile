# syntax=docker/dockerfile:1.7

# ── Stage 1: Build Frontend (Node.js) ─────────────────────────
# 新增阶段：构建前端静态文件，解决 RustEmbed 报错问题
FROM node:20-slim AS frontend-builder
WORKDIR /app/web
COPY web/package*.json ./
# 即使没有 lock 文件也能尝试安装
RUN npm install
COPY web/ .
RUN npm run build

# ── Stage 2: Build Backend (Rust) ─────────────────────────────
FROM rust:1.93-slim@sha256:9663b80a1621253d30b146454f903de48f0af925c967be48c84745537cd35d8b AS builder

WORKDIR /app

# 安装构建所需的系统依赖
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 1. 拷贝前端构建结果 (解决 web/dist/ does not exist 错误)
COPY --from=frontend-builder /app/web/dist ./web/dist

# 2. 拷贝 manifests 缓存依赖
COPY Cargo.toml Cargo.lock ./
COPY crates/robot-kit/Cargo.toml crates/robot-kit/Cargo.toml

# 创建哑目标以加速缓存
RUN mkdir -p src benches crates/robot-kit/src \
    && echo "fn main() {}" > src/main.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "pub fn placeholder() {}" > crates/robot-kit/src/lib.rs

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked

RUN rm -rf src benches crates/robot-kit/src

# 3. 拷贝源码并正式编译
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

# 准备运行时配置
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace && \
    cat > /zeroclaw-data/.zeroclaw/config.toml <<EOF && \
    chown -R 65534:65534 /zeroclaw-data
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = ""
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7

[gateway]
port = 8080
host = "0.0.0.0"
allow_public_bind = true
EOF

# ── Stage 3: Production Runtime (Distroless) ─────────────────
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:84fcd3c223b144b0cb6edc5ecc75641819842a9679a3a58fd6294bec47532bf AS release

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /zeroclaw-data /zeroclaw-data

ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_GATEWAY_PORT=8080

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 8080
ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
