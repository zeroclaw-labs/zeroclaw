# Deploying ZeroClaw on Raspberry Pi

Use this guide when the target host is a Raspberry Pi running Linux.
It covers only the Raspberry-Pi-specific deltas:

- which ARM binary to use
- when to prefer a published binary vs. a source build
- which build profiles and feature flags matter on Pi hardware
- when native Raspberry Pi GPIO requires a custom build

For generic install/bootstrap, service lifecycle, and network exposure, use the
existing docs linked throughout this page instead of duplicating them here.

## Choose the Smallest Path

| Goal | Recommended path |
|---|---|
| Run the core agent/runtime on a Pi quickly | Install a published ARM release binary |
| Build on a stronger machine and copy to the Pi | Cross-compile for the Pi target triple |
| Use native Raspberry Pi GPIO or USB/serial peripheral tooling | Build from source with the required feature flags |

Important:

- Published Linux release assets exist for `aarch64`, `armv7`, and `armv6`
  (`arm-unknown-linux-gnueabihf` for `armv6l` boards).
- The release workflows build ARM binaries with the standard release feature set,
  not `hardware` or `peripheral-rpi`; on 32-bit ARM they drop
  `observability-prometheus` via `--no-default-features` because Prometheus
  requires 64-bit atomics.
- If you need native Pi GPIO or hardware peripheral tooling, plan on a source build.

## 1. Confirm the Pi Architecture

```bash
uname -m
# aarch64 -> 64-bit Raspberry Pi OS
# armv7l  -> 32-bit Raspberry Pi OS on Pi 2/3/4-class boards
# armv6l  -> 32-bit Raspberry Pi OS on Pi Zero / Pi 1-class boards
```

Use the matching Rust target triple when downloading or building:

- `aarch64` -> `aarch64-unknown-linux-gnu`
- `armv7l` -> `armv7-unknown-linux-gnueabihf`
- `armv6l` -> `arm-unknown-linux-gnueabihf`

## 2. Recommended: Install a Published ARM Binary

If the Pi only needs the core runtime, avoid compiling on the device.

From a repository checkout:

```bash
./install.sh --prefer-prebuilt
```

To require a published binary and fail instead of falling back to source:

```bash
./install.sh --prebuilt-only
```

You can also download the matching release asset directly from
<https://github.com/zeroclaw-labs/zeroclaw/releases/latest>:

- `zeroclaw-aarch64-unknown-linux-gnu.tar.gz`
- `zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz`
- `zeroclaw-arm-unknown-linux-gnueabihf.tar.gz`

Use this path for normal Raspberry Pi agent/daemon deployments that do not need
`peripheral-rpi` or `hardware`.

## 3. Cross-Compile on a Stronger Host

Cross-compilation is the best source-build path when the Pi itself is memory-constrained.

### Linux x86_64 Example

```bash
rustup target add aarch64-unknown-linux-gnu
sudo apt install gcc-aarch64-linux-gnu

CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
cargo build --release --locked --target aarch64-unknown-linux-gnu

scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@<pi-ip>:~/
ssh pi@<pi-ip> "sudo install -m 0755 ~/zeroclaw /usr/local/bin/zeroclaw"
```

If the deployed binary also needs native Pi GPIO, rebuild with
`--features peripheral-rpi`. If the same binary also needs USB/serial board
tooling, use `--features hardware,peripheral-rpi`.

For a 32-bit Raspberry Pi OS target, choose the target by userland:

- `armv7l` -> `armv7-unknown-linux-gnueabihf`
- `armv6l` -> `arm-unknown-linux-gnueabihf`

For local source builds on either 32-bit target, mirror the installer's 32-bit
baseline feature set with `--no-default-features --features channel-nostr,skill-creation`
so `observability-prometheus` stays off.

### macOS / scripted deploy

The repository already ships a Raspberry Pi deploy flow in
`scripts/deploy-rpi.sh` with setup notes in `scripts/README.md`. Use that path
when you want a scripted cross-compile + SSH deploy flow for a 64-bit Pi.

## 4. Build on the Pi

Build on-device only when you need local source changes or source-only features
such as `peripheral-rpi`.

### Build-memory and profile guidance

The repo already documents the baseline resource floor in
[../setup-guides/one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md):
start from at least 2 GB RAM plus swap and 6 GB free disk.

Profile guidance from `Cargo.toml`:

- `release`: the default shipping profile; uses `codegen-units = 1` to reduce
  peak compile pressure and should be the first source-build attempt on a Pi.
- `release-fast`: inherits `release` but raises `codegen-units` for faster
  builds on stronger machines.
- `ci`: uses `thin` LTO plus higher codegen parallelism; useful for CI-style
  iteration, but not the default deployment artifact.

On a constrained Pi, start with `--release`. If that still OOMs, prefer a
published binary or cross-compilation instead of forcing longer local rebuild
loops.

### Add swap before a local source build

```bash
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
free -h
```

To keep the swap file across reboots:

```bash
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

### Local source-build commands

Start with the matching release profile. On 32-bit Pi OS, use the installer's
32-bit baseline feature set:

```bash
# 64-bit Raspberry Pi OS
cargo build --release --locked

# 32-bit Raspberry Pi OS (armv7l / armv6l)
cargo build --release --locked --no-default-features --features channel-nostr,skill-creation
```

Optional feature builds:

```bash
# Native Raspberry Pi GPIO only (64-bit)
cargo build --release --locked --features peripheral-rpi

# Native Raspberry Pi GPIO only (32-bit)
cargo build --release --locked --no-default-features --features channel-nostr,skill-creation,peripheral-rpi

# Native Raspberry Pi GPIO plus USB/serial board tooling (64-bit)
cargo build --release --locked --features hardware,peripheral-rpi

# Native Raspberry Pi GPIO plus USB/serial board tooling (32-bit)
cargo build --release --locked --no-default-features --features channel-nostr,skill-creation,hardware,peripheral-rpi
```

If you need to reduce local parallelism further:

```bash
CARGO_BUILD_JOBS=1 cargo build --release --locked
```

The installed binary path for a local release build is:

```bash
sudo install -m 0755 target/release/zeroclaw /usr/local/bin/zeroclaw
```

## 5. Optional Native GPIO on the Pi

Native Raspberry Pi GPIO requires a source build with `--features peripheral-rpi`.

After installing that build, persist the board in config with:

```bash
zeroclaw peripheral add rpi-gpio native
```

If the same Pi also needs USB or serial peripherals, build with
`--features hardware,peripheral-rpi`.

For the underlying peripheral model and config shape, see:

- [hardware-peripherals-design.md](hardware-peripherals-design.md)
- [../contributing/adding-boards-and-tools.md](../contributing/adding-boards-and-tools.md)
- [../reference/api/config-reference.md](../reference/api/config-reference.md)

## 6. What to Read Next

Once the binary is on the Pi, continue with the existing docs instead of
duplicating those flows here:

- Generic install/bootstrap/onboarding:
  [../setup-guides/one-click-bootstrap.md](../setup-guides/one-click-bootstrap.md)
- Service lifecycle, logs, and day-2 operations:
  [../ops/operations-runbook.md](../ops/operations-runbook.md)
- LAN binding, webhook channels, and tunnel setup:
  [../ops/network-deployment.md](../ops/network-deployment.md)

## 7. Troubleshooting

### Build dies with `signal: 9` or another OOM symptom

- Prefer `./install.sh --prefer-prebuilt` on constrained Pi hosts.
- Add swap before local source builds.
- If local `cargo build --release --locked` is still not viable, move the build
  to a stronger host and cross-compile instead.

### The binary fails with `Exec format error`

- Re-run `uname -m`.
- Verify that the installed binary target matches the Pi userland
  (`aarch64-unknown-linux-gnu` vs `armv7-unknown-linux-gnueabihf` vs
  `arm-unknown-linux-gnueabihf`).

### Native GPIO tools are missing

- Official release binaries do not ship with `peripheral-rpi`.
- Rebuild from source with `--features peripheral-rpi`.
