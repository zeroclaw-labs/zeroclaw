# Raspberry Pi Setup

This guide covers installing and running ZeroClaw on Raspberry Pi.

The runtime is small enough to run comfortably on a Pi. Build-time is a
different story: Rust's compiler and linker need substantially more RAM than the
resulting binary, so the on-device build path needs swap and a tuned profile to
avoid OOM-kills during link.

For most Pi users, the **pre-built binary is the path of least resistance**.

## Hardware Compatibility

Any Pi that can run a 64-bit (`aarch64`) or 32-bit (`armv7`) Raspberry Pi OS can
run the **pre-built binary** — there is no meaningful memory floor for the
runtime. The constraint is **building from source on the device**: the linker is
memory-hungry, so lower-RAM boards need swap and a lighter build profile (see
[Option 3](#option-3-build-on-the-pi)). Cross-compiling from a larger machine
([Option 2](#option-2-cross-compile-from-another-machine)) sidesteps the
on-device memory pressure entirely.

The prebuilt Pi binaries come from these release targets:

{{#include ../_snippets/hardware-release-targets.md}}

Use `aarch64-unknown-linux-gnu` for 64-bit Raspberry Pi OS and
`armv7-unknown-linux-gnueabihf` for 32-bit.

## Option 1: Pre-built Binary (Recommended)

Fastest path. No compiler, no swap, no OOM risk.

### Using the install script

<div class="os-tabs-src">

#### sh

```sh
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

</div>

The script auto-detects your architecture (`aarch64` or `armv7`) and installs the matching release binary into `$CARGO_HOME/bin/zeroclaw` (defaulting to `~/.cargo/bin/zeroclaw`). Make sure that directory is on your `PATH`.

### Manual download

Pick the matching tarball from the [latest release](https://github.com/zeroclaw-labs/zeroclaw/releases/latest):

<div class="os-tabs-src">

#### sh

```sh
# 64-bit (Pi 4/5 with 64-bit Raspberry Pi OS)
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-gnu.tar.gz
tar xzf zeroclaw-aarch64-unknown-linux-gnu.tar.gz
sudo install -m 0755 zeroclaw /usr/local/bin/

# 32-bit (Pi Zero 2 W, older Pi 3 with 32-bit OS)
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz
tar xzf zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz
sudo install -m 0755 zeroclaw /usr/local/bin/
```

</div>

### Check your architecture

<div class="os-tabs-src">

#### sh

```sh
uname -m
# aarch64 → 64-bit (use the aarch64 binary)
# armv7l  → 32-bit (use the armv7 binary)
# armv6l  → Pi 1 / Zero (not currently supported, see #4623)
```

</div>

## Option 2: Cross-Compile From Another Machine

If you already have a beefier machine, cross-compiling is faster than building on the Pi.

### From macOS (Apple Silicon or Intel)

<div class="os-tabs-src">

#### sh

```sh
# Install the cross-compilation target
rustup target add aarch64-unknown-linux-gnu

# Install a Linux GNU cross-toolchain — same pattern used by the Arduino Uno Q guide
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# Build
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-unknown-linux-gnu-gcc \
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to your Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi:~/
```

</div>

> **Note:** earlier drafts of this guide suggested `aarch64-elf-gcc` from Homebrew. That toolchain produces bare-metal ELF binaries and links against newlib, not glibc. It will not produce a working Raspberry Pi OS binary. Use the `messense/macos-cross-toolchains` tap above (a real Linux GNU/glibc toolchain), or fall back to Option 3 (build on the Pi).

### From Linux x86_64

<div class="os-tabs-src">

#### sh

```sh
# Install cross-compilation toolchain
sudo apt-get install -y gcc-aarch64-linux-gnu

# Add target
rustup target add aarch64-unknown-linux-gnu

# Configure linker
# [target.aarch64-unknown-linux-gnu]
# linker = "aarch64-linux-gnu-gcc"

# Build
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi:~/
```

</div>

## Option 3: Build on the Pi

Possible on a Pi if you set up swap and pick the right profile. Build time scales with the board; on lower-RAM boards it can be slow.

### Step 1: Install Rust toolchain

<div class="os-tabs-src">

#### sh

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

</div>

### Step 2: Add swap (recommended for lower-RAM boards)

The default `release` profile uses fat LTO, which is memory-hungry at link time.
On a board without enough RAM, that triggers the OOM-killer mid-link. Swap (plus
a lighter profile, Step 3) avoids it.

<div class="os-tabs-src">

#### sh

```sh
# Create a 4 GB swap file
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# Verify
free -h

# Make persistent across reboots
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

</div>

### Step 3: Choose a build profile

The default `release` profile uses `lto = "fat"` and `codegen-units = 1`: best runtime performance, worst build memory. The `release-fast` profile raises `codegen-units` to 8 for more parallelism (lighter on the linker), at a minor runtime cost. The `ci` profile goes further with `lto = "thin"` and `codegen-units = 16` for the fastest, lowest-memory link.

#### Automatic low-memory LTO (install.sh)

If you install via `install.sh` rather than building by hand, the script already
applies the low-memory build heuristic for you:

{{#include ../_snippets/hardware-lowmem-lto.md}}

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Higher-RAM board (with swap): default release works
cargo build --release

# Mid-RAM board (with swap): use release-fast for a lighter link
cargo build --profile release-fast

# Low-RAM or constrained board: use ci profile (debug-info-stripped, fast link)
cargo build --profile ci
```

</div>

### Step 4: Install the binary

<div class="os-tabs-src">

#### sh

```sh
# From the build profile you used:
sudo install -m 0755 target/release/zeroclaw /usr/local/bin/
# or target/release-fast/zeroclaw, or target/ci/zeroclaw
```

</div>

### Building with GPIO support

If you want to use Pi GPIO peripherals from skills, enable the relevant feature flag (see the `peripherals` crate). Most users don't need this for typical agent workloads. It's only relevant if you're writing skills that talk to attached hardware.

## Containerized deployment (Podman recommended over Docker)

Pis are memory-constrained, and on a small board everything you stack alongside
ZeroClaw (channel transports, browser-control, MCP servers, an adjacent agent or
two, plus the OS) competes for the same fixed pool. Memory you don't spend on
container infrastructure is memory ZeroClaw and its tools get to use, which is
why container runtime choice matters more here than on a developer laptop.

**Three reasons Podman is the better fit on Pi than Docker:**

1. **Rootless by default → security headroom.** Podman doesn't need a root daemon; containers run as your user. On an exposed edge device that matters more than on a developer laptop.
2. **systemd-native via Quadlets → operational simplicity.** Podman ships `.container` unit files that systemd manages directly, same lifecycle, logging, and dependency model as any other unit. No separate `docker.service` to babysit, no separate logging layer.
3. **No persistent daemon → memory headroom.** Docker keeps a long-running `dockerd` resident; Podman does not. Dropping that daemon is the single biggest memory knob you can turn without sacrificing isolation.

The trade-off: Podman's rootless network model uses slirp4netns (or pasta on newer versions), which is slower than the bridge that Docker's daemon sets up. For workloads that move a lot of HTTP traffic between containers on the same Pi, that's worth measuring. For ZeroClaw's typical "one or two long-running agent containers" pattern, the difference is negligible, and on memory-constrained hardware the daemon savings dominate the calculation anyway.

### Quick install (Raspberry Pi OS Bookworm/Trixie)

<div class="os-tabs-src">

#### sh

```sh
sudo apt-get install -y podman
# Optional: shorter aliases — many docker-compose flows just work with podman-compose
sudo apt-get install -y podman-compose
```

</div>

### Running ZeroClaw under Podman

The published OCI image works under Podman without modification:

<div class="os-tabs-src">

#### sh

```sh
podman pull ghcr.io/zeroclaw-labs/zeroclaw:latest

podman run --rm -d \
  --name zeroclaw \
  -p 42617:42617 \
  -v ~/.zeroclaw:/root/.zeroclaw \
  ghcr.io/zeroclaw-labs/zeroclaw:latest \
  daemon --host 0.0.0.0 --port 42617
```

</div>

> **Bind gotcha:** ZeroClaw defaults to `127.0.0.1` for the gateway. Inside a container that means the gateway is unreachable from the host. Always pass `--host 0.0.0.0` (or set `ZEROCLAW_BIND=0.0.0.0`) when running in a container.

### Running as a systemd unit via Quadlet

Drop a `.container` file in `/etc/containers/systemd/` (system) or `~/.config/containers/systemd/` (rootless user):

```ini
# ~/.config/containers/systemd/zeroclaw.container
[Unit]
Description=ZeroClaw gateway
After=network-online.target
Wants=network-online.target

[Container]
Image=ghcr.io/zeroclaw-labs/zeroclaw:latest
ContainerName=zeroclaw
PublishPort=42617:42617
Environment=ZEROCLAW_BIND=0.0.0.0
Exec=daemon --host 0.0.0.0 --port 42617
Volume=zeroclaw-data:/root/.zeroclaw

[Service]
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target default.target
```

<div class="os-tabs-src">

#### sh

```sh
systemctl --user daemon-reload
systemctl --user start zeroclaw.service
```

</div>

For rootless setups, also run `loginctl enable-linger $USER` so the service starts before you log in.

## Post-Install: Native (non-container) setup

### 1. Initialize ZeroClaw

<div class="os-tabs-src">

#### sh

```sh
zeroclaw quickstart
```

</div>

This walks you through provider auth, gateway config, and creates your ZeroClaw config.

### 2. Verify it works

<div class="os-tabs-src">

#### sh

```sh
zeroclaw doctor
zeroclaw agent -a assistant -m "what's 2+2?"
```

</div>

### 3. Run as a persistent service

<div class="os-tabs-src">

#### sh

```sh
# Install and start the systemd user service
zeroclaw service install
systemctl --user enable --now zeroclaw

# So it survives logout / reboot:
loginctl enable-linger $USER
```

</div>

### 4. Run as a foreground daemon

For dev / debugging:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon --host 0.0.0.0 --port 42617
```

</div>

### 5. Enable channels

ZeroClaw can connect to chat platforms (Matrix, Mattermost, Discord, Telegram, etc.). See [Channels → Overview](../channels/overview.md). Most channel transports work fine on a Pi; the heaviest is the WebRTC stack used by some voice channels, which can spike CPU during call setup.

## GPIO and Hardware Peripherals

If you want skills to drive GPIO pins (LEDs, buttons, sensors, etc.):

1. Add your user to the `gpio` group:
   <div class="os-tabs-src">

   #### sh

   ```sh
   sudo usermod -aG gpio $USER
   # Log out and back in for the group change to take effect
   ```

   </div>
2. Use the `peripherals` crate's GPIO bindings from your skills. See [Hardware → Peripherals design](./hardware-peripherals-design.md) for the abstraction model.

## Troubleshooting

### OOM-killed during build

Fat LTO linking in the `release` profile is the memory peak. Either:

- Switch to `cargo build --profile release-fast` (more codegen parallelism, lighter link).
- Add swap (Step 2 above).
- Cross-compile from a larger machine (Option 2).

If you're using `release-fast` and still OOMing on a low-RAM board, drop to `--profile ci` or use the pre-built binary.

### Build extremely slow

Expected on lower-RAM boards; build time scales with the board. Use cross-compilation (Option 2) if build time matters.

### Pre-built binary: "Exec format error"

Architecture mismatch. Check `uname -m` and download the matching binary. `aarch64` is 64-bit (most Pi 4/5 with 64-bit Raspberry Pi OS); `armv7l` is 32-bit.

### GPIO permission denied

<div class="os-tabs-src">

#### sh

```sh
sudo usermod -aG gpio $USER
# Log out and back in
```

</div>

### Service won't start after reboot

Make sure user-level systemd persists across logout:

<div class="os-tabs-src">

#### sh

```sh
loginctl enable-linger $USER
```

</div>

### Container can't reach gateway from host

ZeroClaw binds `127.0.0.1` by default. Inside a container that means localhost-of-the-container. Pass `--host 0.0.0.0` (or `ZEROCLAW_BIND=0.0.0.0`) when running in Podman/Docker.

## Performance tips

- **Use an SSD or fast SD card.** Compilation is heavily I/O-bound; a USB 3.0 SSD on a Pi 4/5 cuts build time significantly.
- **Run headless.** Stop the desktop environment if not needed: `sudo systemctl set-default multi-user.target`.
- **tmpfs for build artifacts** (if you have RAM + swap headroom): `export CARGO_TARGET_DIR=/tmp/zeroclaw-target`.
- **Check that `clk_ignore_unused` isn't set** on the kernel cmdline if you're using a custom image: that flag (occasionally seen on vendor BSPs) inhibits clock gating and increases idle power. Stock Raspberry Pi OS doesn't ship with it.

## Related

- [Linux setup](../setup/linux.md): non-Pi-specific Linux setup, applicable here too once the binary's installed
- [Service management](../setup/service.md): systemd patterns, deeper than what's above
- [Hardware → Peripherals design](./hardware-peripherals-design.md): GPIO and the peripherals crate
- [Hardware → Adding boards & tools](./adding-boards-and-tools.md): extending hardware support
