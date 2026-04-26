# ZeroClaw ESP32 demo container.
#
# Both the simulator and the zeroclaw daemon live inside one container so they
# share /tmp and /dev/pts (necessary for the socat-managed pty pair to be
# usable by both processes). The simulator is the default ENTRYPOINT; chat
# with zeroclaw via `docker compose exec zeroclaw zeroclaw agent ...`.

# ─── Stage 1: build ────────────────────────────────────────────────────────
# Rust 1.91 (matches the workspace's actual feature usage — let-chains, etc.;
# the workspace's rust-version=1.87 in Cargo.toml is an outdated pin).
FROM rust:1.91-slim-bookworm AS builder

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
# Build the example first (smaller dependency surface = better failure mode),
# then the main binary; both share the cargo build cache.
# BuildKit cache mounts keep cargo's registry + target dir warm across rebuilds.
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
        --features "agent-runtime hardware dev-sim channel-telegram" \
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
COPY demo/zeroclaw.toml /app/data/config/config.toml

ENV ZEROCLAW_CONFIG_DIR=/app/data/config \
    ZEROCLAW_WORKSPACE=/app/data/workspace \
    RUST_LOG=zeroclaw=info,zeroclaw_hardware=info,esp32_sim=info

EXPOSE 8080

# tini reaps zombies and forwards signals correctly.
ENTRYPOINT ["tini", "--"]
CMD ["/usr/local/bin/esp32_sim"]
