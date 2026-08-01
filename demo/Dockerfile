# ZeroClaw ESP32 Smart Room demo container.
#
# Full stack:
# - esp32_sim (axum + socat pty + WebSocket SVG visualizer)
# - ZeroClaw agent with hardware + dev-sim features
# - Uses smartroom tools (set_device / read_device) when board = "esp32-sim"
#
# The simulator and agent share /tmp so the pty at /tmp/zc-sim-esp32 is visible
# to both. Simulator runs by default on :8080; use run-zeroclaw.sh for the agent.

# ─── Stage 1: build ────────────────────────────────────────────────────────
# Matches the workspace `rust-version = "1.96.1"` MSRV declared in Cargo.toml.
FROM rust:1.96-slim-bookworm AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the workspace and build. We accept the slow first-build cost in
# exchange for a cleanly isolated runtime image.
COPY . .

# `.dockerignore` strips apps/tauri/src and apps/tauri/build.rs; stub them so
# Cargo's workspace resolution succeeds. (Same trick the production Dockerfile uses.)
RUN mkdir -p apps/tauri/src \
    && printf 'fn main() {}\n' > apps/tauri/src/main.rs \
    && printf 'fn main() {}\n' > apps/tauri/build.rs

# Debug builds (no --release) — faster compile, fine for a demo.
# Build the simulator example first, then the main binary.
# Requires the dev-sim feature (from the split) so /tmp/zc-sim-* paths are allowed.
# BuildKit cache mounts keep rebuilds fast.
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target-debug,target=/build/target,sharing=locked \
    cargo build \
        -p zeroclaw-hardware \
        --example esp32_sim \
        --features "hardware dev-sim" \
    && cargo build \
        --bin zeroclaw \
        --no-default-features \
        --features "agent-runtime hardware dev-sim" \
    && cp /build/target/debug/zeroclaw /tmp/zeroclaw \
    && cp /build/target/debug/examples/esp32_sim /tmp/esp32_sim

# ─── Stage 2: runtime ──────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    socat \
    tini \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /tmp/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /tmp/esp32_sim /usr/local/bin/esp32_sim

RUN mkdir -p /app/data/config /app/data/workspace
COPY demo/zeroclaw.toml.example /app/data/config/config.toml

ENV ZEROCLAW_CONFIG_DIR=/app/data/config \
    ZEROCLAW_WORKSPACE=/app/data/workspace \
    RUST_LOG=zeroclaw=info,zeroclaw_hardware=info,esp32_sim=info

EXPOSE 8080

# tini reaps zombies and forwards signals correctly.
ENTRYPOINT ["tini", "--"]
CMD ["/usr/local/bin/esp32_sim"]
