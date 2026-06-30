# syntax=docker/dockerfile:1.7-labs

# >>> generated:base-arg-node from dev/ci/container-base-images.toml by `cargo generate installers` - do not edit <<<
ARG ZEROCLAW_BASE_NODE=node:24-bookworm-slim@sha256:c2d5ade763cacfb03fe9cb8e8af5d1be5041ff331921fa26a9b231ca3a4f780a
# >>> end generated:base-arg-node <<<
# >>> generated:base-arg-rust-slim from dev/ci/container-base-images.toml by `cargo generate installers` - do not edit <<<
ARG ZEROCLAW_BASE_RUST_SLIM=rust:1.94-slim@sha256:cf09adf8c3ebaba10779e5c23ff7fe4df4cccdab8a91f199b0c142c53fef3e1a
# >>> end generated:base-arg-rust-slim <<<
# >>> generated:base-arg-debian from dev/ci/container-base-images.toml by `cargo generate installers` - do not edit <<<
ARG ZEROCLAW_BASE_DEBIAN=debian:trixie-slim@sha256:4e401d95de7083948053197a9c3913343cd06b706bf15eb6a0c3ccd26f436a0e
# >>> end generated:base-arg-debian <<<
# >>> generated:base-arg-distroless from dev/ci/container-base-images.toml by `cargo generate installers` - do not edit <<<
ARG ZEROCLAW_BASE_DISTROLESS=gcr.io/distroless/cc-debian13:nonroot@sha256:d3cda6e91129130d7229a1806b6a73d292ef245ab032da7851907798024cefba
# >>> end generated:base-arg-distroless <<<

# ── Stage 0: Frontend build ─────────────────────────────────────
# The web dashboard bundle is architecture-independent (JS/WASM), so the
# frontend tooling stages are pinned to the native build platform. On a
# multi-platform (`--platform linux/amd64,linux/arm64`) build this keeps node
# and `cargo web build` running natively instead of under QEMU emulation.
FROM --platform=$BUILDPLATFORM ${ZEROCLAW_BASE_NODE} AS web-node

FROM --platform=$BUILDPLATFORM ${ZEROCLAW_BASE_RUST_SLIM} AS web-builder
WORKDIR /app
COPY --from=web-node /usr/local/bin/node /usr/local/bin/node
COPY --from=web-node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        g++ \
        pkg-config \
    && ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
    && rm -rf /var/lib/apt/lists/*
COPY web/package.json web/package-lock.json web/
RUN cd web && npm ci --ignore-scripts
COPY . .
RUN mkdir -p apps/tauri/src \
    && echo "fn main() {}" > apps/tauri/src/main.rs \
    && echo "fn main() {}" > apps/tauri/build.rs
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-web-target,target=/app/target,sharing=locked \
    cargo web build

# ── Stage 1: Build ────────────────────────────────────────────
# Pinned to the native build platform; the Rust toolchain runs on the host arch
# (amd64 on the GitHub-hosted runners) and cross-compiles to $TARGETARCH so
# rustc never runs under QEMU. TARGETARCH is injected by BuildKit per target
# platform (`amd64`/`arm64`).
FROM --platform=$BUILDPLATFORM ${ZEROCLAW_BASE_RUST_SLIM} AS builder

WORKDIR /app
ARG TARGETARCH
# >>> generated:docker-features-arg by `cargo generate installers` - do not edit <<<
ARG ZEROCLAW_CARGO_FLAGS="--no-default-features --features acp-bridge,agent-runtime,channel-acp-server,channel-amqp,channel-bluesky,channel-clawdtalk,channel-dingtalk,channel-discord,channel-email,channel-filesystem,channel-imessage,channel-irc,channel-lark,channel-linq,channel-mattermost,channel-mochat,channel-mqtt,channel-nextcloud,channel-notion,channel-qq,channel-reddit,channel-signal,channel-slack,channel-telegram,channel-twitch,channel-twitter,channel-voice-call,channel-wati,channel-webhook,channel-wecom,channel-wecom-ws,channel-whatsapp-cloud,gateway,observability-prometheus,schema-export"
# >>> end generated:docker-features-arg <<<

# Install build dependencies. g++ is required by inkjet (zerocode's syntax
# highlighter) to compile its tree-sitter grammars; the slim base ships cc but
# not a C++ compiler. For arm64 cross-builds, also install the aarch64 GNU
# cross toolchain (C and C++, for cc-crate-built C/C++ deps such as inkjet's
# grammars), the arm64 libc dev files, and the Rust target.
RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
        g++ \
    && if [ "$TARGETARCH" = "arm64" ]; then \
        dpkg --add-architecture arm64 && apt-get update && apt-get install -y \
            gcc-aarch64-linux-gnu \
            g++-aarch64-linux-gnu \
            libc6-dev-arm64-cross \
        && rustup target add aarch64-unknown-linux-gnu; \
    fi \
    && rm -rf /var/lib/apt/lists/*

# 1. Copy manifests to cache dependencies
COPY Cargo.toml Cargo.lock ./
# Copy every workspace-member manifest in one glob — adding or removing a crate
# no longer requires editing this file.  --parents preserves the
# crates/<name>/Cargo.toml directory structure.
COPY --parents crates/*/Cargo.toml ./
# zeroclaw-macros is a proc-macro crate, compiled for the host even on a cross
# build. If only a stub lib.rs is present during the pre-fetch, its host-cached
# artifact is reused in the real build under the target-triple dir, leaving
# `zeroclaw_macros::Configurable` unresolved. Copy its real source now so the
# proc-macro is built from the genuine implementation during the pre-fetch.
COPY --parents crates/zeroclaw-macros/src/ ./
# apps/tauri: .dockerignore whitelists only Cargo.toml; src and build.rs are stubbed below.
COPY apps/tauri/Cargo.toml apps/tauri/Cargo.toml
# apps/zerocode: TUI app not shipped in the server image; copy only its manifest
# so Cargo can resolve the workspace, then stub its src/main.rs and build.rs
# below. Its real build.rs reads web/src/contexts/themes.json and would panic in
# this pre-fetch stage, so it is stubbed exactly like apps/tauri.
COPY apps/zerocode/Cargo.toml apps/zerocode/Cargo.toml
# tools/fill-translations and xtask are dev/build tools; copy manifests only so
# Cargo can resolve the workspace, then stub their entry points so the
# dependency pre-fetch step succeeds without building them into the image.
COPY tools/fill-translations/Cargo.toml tools/fill-translations/Cargo.toml
COPY xtask/Cargo.toml xtask/Cargo.toml
# Create dummy targets for all workspace members so manifest parsing succeeds.
# `src/bin/zeroclaw-acp-bridge.rs` is required because the `acp-bridge` feature
# is in the root crate's default set; cargo selects the bin target during the
# pre-fetch build even with only the workspace lib stubbed.
RUN mkdir -p src src/bin benches apps/tauri/src apps/zerocode/src tools/fill-translations/src xtask/src/bin \
    && echo "fn main() {}" > src/main.rs \
    && echo "" > src/lib.rs \
    && echo "fn main() {}" > src/bin/zeroclaw-acp-bridge.rs \
    && echo "fn main() {}" > benches/agent_benchmarks.rs \
    && echo "fn main() {}" > apps/tauri/src/main.rs \
    && echo "fn main() {}" > apps/tauri/build.rs \
    && echo "fn main() {}" > apps/zerocode/src/main.rs \
    && echo "fn main() {}" > apps/zerocode/build.rs \
    && echo "fn main() {}" > tools/fill-translations/src/main.rs \
    && echo "" > xtask/src/lib.rs \
    && echo "fn main() {}" > xtask/src/bin/mdbook.rs \
    && echo "fn main() {}" > xtask/src/bin/fluent.rs \
    && echo "fn main() {}" > xtask/src/bin/web.rs \
    && mkdir -p crates/zeroclaw-hardware/examples \
    && echo "fn main() {}" > crates/zeroclaw-hardware/examples/esp32_sim.rs \
    && for d in crates/*/; do [ "$d" = "crates/zeroclaw-macros/" ] && continue; mkdir -p "${d}src" && printf '' > "${d}src/lib.rs"; done
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    if [ "$TARGETARCH" = "arm64" ]; then \
      export RUST_TARGET=aarch64-unknown-linux-gnu \
             CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
             CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
             CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
             PKG_CONFIG_ALLOW_CROSS=1 \
             PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig; \
    else \
      export RUST_TARGET=x86_64-unknown-linux-gnu; \
    fi && \
    if [ -n "$ZEROCLAW_CARGO_FLAGS" ]; then \
      cargo build --release --locked --target "$RUST_TARGET" -p zeroclawlabs -p zerocode $ZEROCLAW_CARGO_FLAGS; \
    else \
      cargo build --release --locked --target "$RUST_TARGET" -p zeroclawlabs -p zerocode; \
    fi
RUN rm -rf src benches crates xtask tools/fill-translations

# 2. Copy only build-relevant source paths (avoid cache-busting on docs/tests/scripts)
COPY src/ src/
COPY benches/ benches/
COPY crates/ crates/
COPY xtask/ xtask/
COPY tools/fill-translations/ tools/fill-translations/
# apps/zerocode ships in the image; copy its real source. Its build.rs reads the
# dashboard theme registry under web/src/contexts, so that path must be present.
COPY apps/zerocode/ apps/zerocode/
COPY web/src/ web/src/
# locales.toml lives at repo root and is embedded by zeroclaw-runtime via
# include_str!("../../../locales.toml"); the real build needs it present.
COPY locales.toml .
COPY *.rs .
RUN touch src/main.rs apps/zerocode/src/main.rs
# Bust the stubbed workspace crates so the real sources rebuild. zerocode is
# purged from BOTH the target tree and the host tree (target/release/): with
# --target, build scripts compile in the host tree, so its real build.rs (which
# generates theme_presets.rs into OUT_DIR) must replace the no-op stub compiled
# during the dependency pre-fetch, otherwise the include! finds no file.
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    if [ "$TARGETARCH" = "arm64" ]; then \
      export RUST_TARGET=aarch64-unknown-linux-gnu STRIP=aarch64-linux-gnu-strip \
             CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
             CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
             CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
             PKG_CONFIG_ALLOW_CROSS=1 \
             PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig; \
    else \
      export RUST_TARGET=x86_64-unknown-linux-gnu STRIP=strip; \
    fi && \
    rm -rf target/"$RUST_TARGET"/release/.fingerprint/zeroclawlabs-* \
           target/"$RUST_TARGET"/release/deps/zeroclawlabs-* \
           target/"$RUST_TARGET"/release/incremental/zeroclawlabs-* \
           target/"$RUST_TARGET"/release/.fingerprint/zeroclaw-* \
           target/"$RUST_TARGET"/release/deps/zeroclaw_* \
           target/"$RUST_TARGET"/release/incremental/zeroclaw_* \
           target/"$RUST_TARGET"/release/.fingerprint/xtask-* \
           target/"$RUST_TARGET"/release/deps/xtask-* \
           target/"$RUST_TARGET"/release/.fingerprint/fill-translations-* \
           target/"$RUST_TARGET"/release/deps/fill_translations-* \
           target/"$RUST_TARGET"/release/.fingerprint/zerocode-* \
           target/"$RUST_TARGET"/release/deps/zerocode-* \
           target/"$RUST_TARGET"/release/incremental/zerocode-* \
           target/"$RUST_TARGET"/release/build/zerocode-* \
           target/release/.fingerprint/zerocode-* \
           target/release/build/zerocode-* && \
    if [ -n "$ZEROCLAW_CARGO_FLAGS" ]; then \
      cargo build --release --locked --target "$RUST_TARGET" -p zeroclawlabs -p zerocode $ZEROCLAW_CARGO_FLAGS; \
    else \
      cargo build --release --locked --target "$RUST_TARGET" -p zeroclawlabs -p zerocode; \
    fi && \
    cp target/"$RUST_TARGET"/release/zeroclaw /app/zeroclaw && \
    cp target/"$RUST_TARGET"/release/zerocode /app/zerocode && \
    "$STRIP" /app/zeroclaw /app/zerocode
RUN for b in zeroclaw zerocode; do \
      size=$(stat -c%s "/app/$b") && \
      if [ "$size" -lt 1000000 ]; then echo "ERROR: $b too small (${size} bytes), likely dummy build artifact" && exit 1; fi; \
    done

# Prepare runtime directory structure and default config inline (no extra stage).
# Dashboard assets live at /usr/share/zeroclawlabs/web/dist (outside the documented
# /zeroclaw-data mount point) so a bind mount on /zeroclaw-data cannot shadow them.
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/data && \
    printf '%s\n' \
        'api_key = ""' \
        'default_provider = "openrouter"' \
        'default_model = "anthropic/claude-sonnet-4-20250514"' \
        'default_temperature = 0.7' \
        '' \
        '[gateway]' \
        'port = 42617' \
        'host = "[::]"' \
        'allow_public_bind = true' \
        'require_pairing = false' \
        'web_dist_dir = "/usr/share/zeroclawlabs/web/dist"' \
        '' \
        '[risk_profiles.default]' \
        'level = "supervised"' \
        'auto_approve = ["file_read", "file_write", "file_edit", "memory_recall", "memory_store", "web_search_tool", "web_fetch", "calculator", "glob_search", "content_search", "image_info", "weather", "git_operations"]' \
        > /zeroclaw-data/.zeroclaw/config.toml && \
    chown -R 65534:65534 /zeroclaw-data

# ── Stage 2: Development Runtime (Debian) ────────────────────
FROM ${ZEROCLAW_BASE_DEBIAN} AS dev

# Install essential runtime dependencies only (use docker-compose.override.yml for dev tools)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    vim-tiny \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /zeroclaw-data /zeroclaw-data
COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /app/zerocode /usr/local/bin/zerocode
# Install the dashboard at /usr/share/zeroclawlabs/web/dist (outside the
# documented /zeroclaw-data mount) so user volumes do not shadow it (#6400).
COPY --from=web-builder /app/web/dist /usr/share/zeroclawlabs/web/dist

# Overwrite minimal config with DEV template (Ollama defaults)
COPY dev/config.template.toml /zeroclaw-data/.zeroclaw/config.toml
RUN chown 65534:65534 /zeroclaw-data/.zeroclaw/config.toml

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
# Bootstrap (uppercase tail) — pre-load: decides where the config file lives.
ENV ZEROCLAW_DATA_DIR=/zeroclaw-data/data
ENV HOME=/zeroclaw-data
# V0.8.0 env-var grammar: `ZEROCLAW_<dotted_path_with_double_underscores>=<value>`
# mirrors the TOML config 1:1; `__` is the path separator. Operators inject
# credentials and runtime knobs at `docker run -e ...` (or via docker-compose
# `environment:`). Legacy `PROVIDER`, `ZEROCLAW_MODEL`, `ANTHROPIC_API_KEY`,
# `API_KEY`, etc. fallbacks were eradicated. Example:
#   docker run -e ZEROCLAW_providers__models__anthropic__default__api_key=sk-ant-... ...
ENV ZEROCLAW_gateway__port=42617

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617
HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["zeroclaw", "status", "--format=exit-code"]
ENTRYPOINT ["zeroclaw"]
CMD ["daemon"]

# ── Stage 3: Production Runtime (Distroless) ─────────────────
FROM ${ZEROCLAW_BASE_DISTROLESS} AS release

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=builder /app/zerocode /usr/local/bin/zerocode
COPY --from=builder /zeroclaw-data /zeroclaw-data
# Install the dashboard at /usr/share/zeroclawlabs/web/dist (outside the
# documented /zeroclaw-data mount) so user volumes do not shadow it (#6400).
COPY --from=web-builder /app/web/dist /usr/share/zeroclawlabs/web/dist

# Environment setup
# Ensure UTF-8 locale so CJK / multibyte input is handled correctly
ENV LANG=C.UTF-8
ENV ZEROCLAW_DATA_DIR=/zeroclaw-data/data
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
CMD ["daemon"]
