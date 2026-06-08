# Stage 1: Fetch dependencies + build (fmt, clippy, release)
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS build

WORKDIR /src
COPY . .

# Fetch all workspace dependencies (network available)
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo fetch

# Offline build: fmt check, clippy, release binaries
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    ARCH="$(uname -m)"
    export RUSTFLAGS="-C target-feature=+crt-static -C linker=rust-lld -C link-arg=-L/usr/lib"

    # Format check
    cargo fmt --all -- --check

    # Clippy (debug build, cleaned before release)
    CARGO_TARGET_DIR=/tmp/target \
    cargo clippy --all-targets --target "${ARCH}-unknown-linux-musl" -- -D warnings
    rm -rf /tmp/target

    # Build combined libstdc++.a from libc++.a + libc++abi.a (stagex ships LLVM libc++, not GCC libstdc++)
    (mkdir -p /tmp/libwrap && cd /tmp/libwrap && ar x /lib/libc++.a && ar x /lib/libc++abi.a && ar rcs /usr/lib/libstdc++.a *.o && rm -rf /tmp/libwrap)

    # Release build — zeroclawlabs (daemon)
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "${ARCH}-unknown-linux-musl" \
        --no-default-features \
        --features "agent-runtime,default-channels,acp-bridge,gateway,schema-export,observability-prometheus" \
        -p zeroclawlabs

    # Release build — zerocode (TUI config manager)
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "${ARCH}-unknown-linux-musl" \
        -p zerocode

    mkdir -p /rootfs/usr/bin
    cp /target/${ARCH}-unknown-linux-musl/release/zeroclaw /rootfs/usr/bin/zeroclaw
    cp /target/${ARCH}-unknown-linux-musl/release/zerocode /rootfs/usr/bin/zerocode
EOF

# Stage 2: Minimal runtime image (target: package)
FROM docker.io/stagex/core-filesystem@sha256:cd3a66471ce1f630fa77d5c9bd9829f9f9fab6302a1aaa64d67b74f1f069b750 AS package
COPY --from=build /rootfs/ /
COPY --from=docker.io/stagex/core-ca-certificates@sha256:7773dae6630aa3bdcc82cfec6c9265c0c501aaf0af67cc73631b09e1cff1b094 / /
ENTRYPOINT ["/usr/bin/zeroclaw"]

# Stage 3: Build fat binary with all channels (target: build-fat)
FROM docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd AS build-fat
COPY --from=build /src /src
WORKDIR /src
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --network=none \
    <<-EOF
    set -e
    ARCH="$(uname -m)"
    export RUSTFLAGS="-C target-feature=+crt-static -C linker=rust-lld -C link-arg=-L/usr/lib"
    CARGO_TARGET_DIR=/target \
    cargo build \
        --frozen \
        --release \
        --target "${ARCH}-unknown-linux-musl" \
        --no-default-features \
        --features "agent-runtime,channels-full,acp-bridge,gateway,schema-export,observability-prometheus" \
        -p zeroclawlabs
    mkdir -p /rootfs/usr/bin
    cp /target/${ARCH}-unknown-linux-musl/release/zeroclaw /rootfs/usr/bin/zeroclaw
EOF

# Stage 4: Full-channel runtime image (target: package-fat)
FROM docker.io/stagex/core-filesystem@sha256:cd3a66471ce1f630fa77d5c9bd9829f9f9fab6302a1aaa64d67b74f1f069b750 AS package-fat
COPY --from=build-fat /rootfs/ /
COPY --from=build /rootfs/usr/bin/zerocode /usr/bin/zerocode
COPY --from=docker.io/stagex/core-ca-certificates@sha256:7773dae6630aa3bdcc82cfec6c9265c0c501aaf0af67cc73631b09e1cff1b094 / /
ENTRYPOINT ["/usr/bin/zeroclaw"]
