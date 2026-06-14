# No libgcc stage needed — pallet-rust ships libunwind with all _Unwind_*
# symbols. lightningcss's native .node addon links against libgcc_s.so.1
# by SONAME, so we create a symlink in web-build. Pure StageX — no
# Alpine, no core-llvm-libgcc, no external deps for the GCC runtime.

# ── Stage: config-gen (generate default config template) ────
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS config-gen

# Default config template consumed by build/build-fat. Single source of truth
# so operators get a working config on first run without migration overhead.
RUN <<-EOF
    set -e
    mkdir -p /rootfs/zeroclaw-data/.zeroclaw /rootfs/zeroclaw-data/data
    # allow_public_bind: bind to [::] (all interfaces). Inside a container this
    # is safe — the runtime sandboxes network access. The port is only reachable
    # when the operator explicitly publishes it via -p/--publish.
    printf '%s\n' \
        'schema_version = 3' \
        'default_provider = "custom"' \
        'default_model = "opencode/big-pickle"' \
        'default_temperature = 0.7' \
        '' \
        '[gateway]' \
        'port = 42617' \
        'host = "[::]"' \
        'allow_public_bind = true' \
        'web_dist_dir = "/usr/share/zeroclawlabs/web/dist"' \
        '' \
        '[providers.models.custom.opencode]' \
        'uri = "https://api.opencode.ai/v1"' \
        'api_key = ""' \
        'model = "opencode/big-pickle"' \
        '' \
        '[risk_profiles.default]' \
        'level = "supervised"' \
        'auto_approve = ["file_read", "file_write", "file_edit", "memory_recall", "memory_store", "web_search_tool", "web_fetch", "calculator", "glob_search", "content_search", "image_info", "weather", "git_operations"]' \
        > /rootfs/zeroclaw-data/.zeroclaw/config.toml
EOF

# ── Stage: nodejs (reference for Node.js toolchain) ──────────
FROM docker.io/stagex/pallet-nodejs@sha256:81bc04b9490a4f4401a8b6fd277736d75f1f0ad4bd98e8f6b4b3616e18b75f7b AS nodejs

# ── Stage: web-build (web dashboard via xtask + npm build) ──
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS web-build

WORKDIR /src
COPY . .

# Copy Node.js toolchain from pallet-nodejs
# Only copy libs that pallet-rust doesn't already have (brotli, cares, nghttp2, icu, openssl shared)
COPY --from=nodejs /usr/bin/node /usr/bin/node
COPY --from=nodejs /usr/bin/env /usr/bin/env
COPY --from=nodejs /usr/lib/node_modules /usr/lib/node_modules
COPY --from=nodejs /lib/libbrotli* /lib/
COPY --from=nodejs /lib/libcares* /lib/
COPY --from=nodejs /lib/libnghttp2* /lib/
COPY --from=nodejs /lib/libicudata* /lib/
COPY --from=nodejs /lib/libicui18n* /lib/
COPY --from=nodejs /lib/libicuuc* /lib/
COPY --from=nodejs /lib/libcrypto.so* /lib/
COPY --from=nodejs /lib/libssl.so* /lib/

# Create npm/npx symlinks (COPY --from resolves symlinks, so we create them here)
RUN ln -s /usr/lib/node_modules/npm/bin/npm-cli.js /usr/bin/npm && \
    ln -s /usr/lib/node_modules/npm/bin/npx-cli.js /usr/bin/npx

# Provide libgcc_s.so.1 — lightningcss's .node addon links against this
# SONAME for exception unwinding. pallet-rust has libunwind with all the
# needed _Unwind_* symbols, so a symlink is sufficient (no GCC runtime
# or core-llvm-libgcc COPY needed).
RUN test -f /usr/lib/libunwind.so && ln -s libunwind.so /usr/lib/libgcc_s.so.1 || ln -s libunwind.so.1 /usr/lib/libgcc_s.so.1

# Install npm dependencies (cached layer: only invalidated when package files change)
# Also explicitly install the musl platform variant of lightningcss — npm's
# optional-dependency resolver misdetects musl as glibc in StageX and skips it.
RUN npm ci --prefix web && npm install --prefix web lightningcss-linux-$(uname -m | sed 's/x86_64/x64/;s/aarch64/arm64/')-musl

# Fetch cargo dependencies (network allowed)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch --locked

# Build the web dashboard (gen-api + typescript build), no network
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    # -crt-static so the ephemeral xtask and its proc-macro/build-script deps
    # link dynamically. The musl target defaults crt-static on, which the StageX
    # host (host == target == musl) cannot satisfy for host artifacts.
    export RUSTFLAGS="-C target-feature=-crt-static"
    cargo web build
EOF

# ── Stage: check (fmt, clippy, test validation) ────────────
# Single source of truth for "what passes" in the deterministic StageX
# musl environment. Used by CI and developers as a pre-push gate.
# Does NOT depend on web-build (creates a stub for compilation).
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS check

WORKDIR /src
COPY . .

# Fetch all workspace dependencies (network available)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch

# Format + clippy (fully isolated — no network, reproducible)
# -crt-static: the musl target defaults crt-static on, but host == target in
# StageX, so proc-macro/build-script host artifacts cannot link statically.
# This validates code correctness; the target static link is checked in build.
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    export RUSTFLAGS="-C target-feature=-crt-static"
    mkdir -p web/dist
    touch web/dist/.gitkeep
    cargo fmt --all -- --check
    # --features ci-all matches CI's Lint job — validates all feature-gated code.
    # --exclude zeroclaw-desktop: needs GTK/WebKit (not in StageX).
    # --exclude zerocode: inkjet/tree-sitter needs C++ compiler (not in StageX).
    cargo clippy --workspace --exclude zeroclaw-desktop --exclude zerocode --all-targets --features ci-all --locked -- -D warnings
EOF

# Test (needs loopback for wiremock — no --network=none)
# --offline prevents cargo from fetching even if network is available.
# --exclude zeroclaw-desktop: requires GTK/GLib (tauri + tray-icon), not in StageX.
# --exclude zerocode: tree-sitter/inkjet inject -lstdc++ and need real C++ runtime
#   symbols (operator new/delete, __cxa_throw, etc.) for YAML scanner code.
#   The build stage succeeds because it uses -static + libstdc++.a stub, but test
#   (dynamic) linking needs a real libstdc++.so that pallet-rust doesn't ship.
# --exclude xtask: its doc-gen gates read docs/ and .github/ paths that
#   .dockerignore keeps out of the build context; those gates run in the
#   standard CI Test job against the full tree.
# --exclude zeroclaw-tools: content_search/git_operations tests shell out to
#   GNU rg/grep/git, which the minimal StageX image does not ship (busybox grep
#   lacks the GNU flags); they run in the standard CI Test job.
# --lib --bins --tests selects unit and integration tests only: doctests are
#   compiled by rustdoc as host artifacts that hit the musl static-link wall,
#   and the criterion bench links a C dep (alloca) that the static link
#   rejects. Both run in the standard CI Test/Benchmarks jobs.
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    <<-EOF
    set -e
    export RUSTFLAGS="-C target-feature=-crt-static"
    cargo test --workspace --lib --bins --tests --exclude zeroclaw-desktop --exclude zerocode --exclude xtask --exclude zeroclaw-tools --offline --locked
EOF

# ── Stage: build (zeroclaw + zerocode, default channels) ────
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS build

WORKDIR /src
COPY . .

# Fetch all workspace dependencies (network available)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch

# Offline build: release binaries (validation moved to check stage)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    ARCH="$(uname -m)"

    # Host build-scripts/proc-macros and the target binary share the musl triple
    # in StageX. The per-target RUSTFLAGS env is target-scoped (it does not apply
    # to host artifacts), so the final binary links +crt-static -static while host
    # build-scripts keep the default dynamic link they require. A plain RUSTFLAGS
    # would hit both and break the host link.
    TARGET="${ARCH}-unknown-linux-musl"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"

    # Build combined libstdc++.a from libc++.a + libc++abi.a (stagex ships LLVM libc++, not GCC libstdc++)
    (mkdir -p /tmp/libwrap/cxx /tmp/libwrap/cxxabi && cd /tmp/libwrap/cxx && ar x /usr/lib/libc++.a && cd /tmp/libwrap/cxxabi && ar x /usr/lib/libc++abi.a && ar rcs /usr/lib/libstdc++.a /tmp/libwrap/cxx/*.o /tmp/libwrap/cxxabi/*.o && rm -rf /tmp/libwrap)

    # Release build — zeroclawlabs (daemon)
    # >>> generated:container-standard by `cargo generate installers` - do not edit <<<
    ZEROCLAW_FEATURES="acp-bridge,agent-runtime,channel-acp-server,channel-amqp,channel-bluesky,channel-clawdtalk,channel-dingtalk,channel-discord,channel-email,channel-imessage,channel-irc,channel-lark,channel-linq,channel-mattermost,channel-mochat,channel-mqtt,channel-nextcloud,channel-notion,channel-qq,channel-reddit,channel-signal,channel-slack,channel-telegram,channel-twitch,channel-twitter,channel-voice-call,channel-wati,channel-webhook,channel-wecom,channel-wecom-ws,channel-whatsapp-cloud,gateway,observability-prometheus,schema-export"
# >>> end generated:container-standard <<<
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "$TARGET" \
        --no-default-features \
        --features "${ZEROCLAW_FEATURES}" \
        -p zeroclawlabs

    # Release build — zerocode (TUI config manager)
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "$TARGET" \
        -p zerocode

    mkdir -p /rootfs/usr/bin /rootfs/usr/share/zeroclawlabs/web/dist
    cp /target/${TARGET}/release/zeroclaw /rootfs/usr/bin/zeroclaw
    cp /target/${TARGET}/release/zerocode /rootfs/usr/bin/zerocode
EOF

# Copy default config template into rootfs (consumed by package stage)
COPY --from=config-gen /rootfs/ /rootfs/

# Copy web dashboard dist
COPY --from=web-build /src/web/dist /rootfs/usr/share/zeroclawlabs/web/dist

# ── Stage: package (minimal runtime) ─────────────────────────
FROM docker.io/stagex/core-filesystem@sha256:cd3a66471ce1f630fa77d5c9bd9829f9f9fab6302a1aaa64d67b74f1f069b750 AS package

# Copy binaries, web dist, and default config; set data dir ownership to nobody(65534)
COPY --from=build /rootfs/ /
COPY --from=build --chown=65534:65534 /rootfs/zeroclaw-data /zeroclaw-data
COPY --from=docker.io/stagex/core-ca-certificates@sha256:7773dae6630aa3bdcc82cfec6c9265c0c501aaf0af67cc73631b09e1cff1b094 / /

ENV ZEROCLAW_DATA_DIR=/zeroclaw-data/data
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_gateway__port=42617

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617

HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["zeroclaw", "status", "--format=exit-code"]

ENTRYPOINT ["/usr/bin/zeroclaw"]
CMD ["daemon"]

# ── Stage: build-fat (zeroclaw + zerocode, all channels) ────
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS build-fat

WORKDIR /src
COPY . .

# Fetch all workspace dependencies (network available)
# Shares cache with the build stage via mount target
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch

# Offline build: release binaries with all channels
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    ARCH="$(uname -m)"

    # Host build-scripts/proc-macros and the target binary share the musl triple
    # in StageX. The per-target RUSTFLAGS env is target-scoped (it does not apply
    # to host artifacts), so the final binary links +crt-static -static while host
    # build-scripts keep the default dynamic link they require. A plain RUSTFLAGS
    # would hit both and break the host link.
    TARGET="${ARCH}-unknown-linux-musl"
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"

    # Build combined libstdc++.a from libc++.a + libc++abi.a (stagex ships LLVM libc++, not GCC libstdc++)
    (mkdir -p /tmp/libwrap/cxx /tmp/libwrap/cxxabi && cd /tmp/libwrap/cxx && ar x /usr/lib/libc++.a && cd /tmp/libwrap/cxxabi && ar x /usr/lib/libc++abi.a && ar rcs /usr/lib/libstdc++.a /tmp/libwrap/cxx/*.o /tmp/libwrap/cxxabi/*.o && rm -rf /tmp/libwrap)

    # Release build — zeroclawlabs (all channels)
    # >>> generated:container-fat by `cargo generate installers` - do not edit <<<
    ZEROCLAW_FEATURES="acp-bridge,agent-runtime,browser-native,channel-acp-server,channel-amqp,channel-bluesky,channel-clawdtalk,channel-dingtalk,channel-discord,channel-email,channel-feishu,channel-imessage,channel-irc,channel-lark,channel-line,channel-linq,channel-matrix,channel-mattermost,channel-mochat,channel-mqtt,channel-nextcloud,channel-nostr,channel-notion,channel-qq,channel-reddit,channel-signal,channel-slack,channel-telegram,channel-twitch,channel-twitter,channel-voice-call,channel-wati,channel-webhook,channel-wechat,channel-wecom,channel-wecom-ws,channel-whatsapp-cloud,dev-sim,gateway,hardware,memory-postgres,observability-otel,observability-prometheus,peripheral-rpi,plugins-wasm,probe,rag-pdf,sandbox-bubblewrap,sandbox-landlock,schema-export,voice-wake,webauthn,whatsapp-web"
# >>> end generated:container-fat <<<
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "$TARGET" \
        --no-default-features \
        --features "${ZEROCLAW_FEATURES}" \
        -p zeroclawlabs

    # Release build — zerocode (TUI config manager)
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "$TARGET" \
        -p zerocode

    mkdir -p /rootfs/usr/bin /rootfs/usr/share/zeroclawlabs/web/dist
    cp /target/${TARGET}/release/zeroclaw /rootfs/usr/bin/zeroclaw
    cp /target/${TARGET}/release/zerocode /rootfs/usr/bin/zerocode
EOF

# Copy default config template into rootfs (consumed by package-fat stage)
COPY --from=config-gen /rootfs/ /rootfs/

# Copy web dashboard dist
COPY --from=web-build /src/web/dist /rootfs/usr/share/zeroclawlabs/web/dist

# ── Stage: package-fat (full-channel runtime) ────────────────
FROM docker.io/stagex/core-filesystem@sha256:cd3a66471ce1f630fa77d5c9bd9829f9f9fab6302a1aaa64d67b74f1f069b750 AS package-fat

# Copy binaries, web dist, and default config; set data dir ownership to nobody(65534)
COPY --from=build-fat /rootfs/ /
COPY --from=build-fat --chown=65534:65534 /rootfs/zeroclaw-data /zeroclaw-data
COPY --from=docker.io/stagex/core-ca-certificates@sha256:7773dae6630aa3bdcc82cfec6c9265c0c501aaf0af67cc73631b09e1cff1b094 / /

ENV ZEROCLAW_DATA_DIR=/zeroclaw-data/data
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_gateway__port=42617

WORKDIR /zeroclaw-data
USER 65534:65534
EXPOSE 42617

HEALTHCHECK --interval=60s --timeout=10s --retries=3 --start-period=10s \
    CMD ["zeroclaw", "status", "--format=exit-code"]

ENTRYPOINT ["/usr/bin/zeroclaw"]
CMD ["daemon"]
