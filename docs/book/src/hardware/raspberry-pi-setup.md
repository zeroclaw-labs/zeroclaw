# Raspberry Pi Setup

This guide covers installing and running ZeroClaw on Raspberry Pi (Pi 3, Pi 4, Pi 5, Pi Zero 2 W).

The README's "runs on <$10 hardware with <5 MB RAM" claim is true for the **runtime**. Build-time is a different story — Rust's compiler and linker need significantly more RAM than the resulting binary, so the on-device build path needs swap and a tuned profile to avoid OOM-kills during link.

For most Pi users, the **pre-built binary is the path of least resistance**.

## Hardware Compatibility

| Model | RAM | Pre-built binary | Build from source |
|---|---|---|---|
| Pi 5 (16 GB) | 16 GB | ✅ | ✅ comfortable |
| Pi 5 (8 GB) | 8 GB | ✅ | ✅ with swap or `release-fast` profile |
| Pi 5 (4 GB) | 4 GB | ✅ | ✅ with swap + `release-fast` profile |
| Pi 4 (8 GB) | 8 GB | ✅ | ✅ with `release-fast` profile |
| Pi 4 (4 GB) | 4 GB | ✅ | ✅ with swap + `release-fast` profile |
| Pi 4 (2 GB) | 2 GB | ✅ | Marginal — swap required, slow |
| Pi 3 (1 GB) | 1 GB | ✅ | ❌ Not recommended |
| Pi Zero 2 W | 512 MB | ✅ | ❌ |

**Runtime memory is minimal.** Even on a Pi Zero 2 W, the core agent runs in well under 5 MB RSS once it's started. The hardware ladder above is about whether you can compile on the device, not whether ZeroClaw can run on it.

## Option 1: Pre-built Binary (Recommended)

Fastest path. No compiler, no swap, no OOM risk.

### Using the install script

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

The script auto-detects your architecture (`aarch64` or `armv7`) and installs the matching release binary into `$CARGO_HOME/bin/zeroclaw` (defaulting to `~/.cargo/bin/zeroclaw`). Make sure that directory is on your `PATH`.

### Manual download

Pick the matching tarball from the [latest release](https://github.com/zeroclaw-labs/zeroclaw/releases/latest):

```bash
# 64-bit (Pi 4/5 with 64-bit Raspberry Pi OS)
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-aarch64-unknown-linux-gnu.tar.gz
tar xzf zeroclaw-aarch64-unknown-linux-gnu.tar.gz
sudo install -m 0755 zeroclaw /usr/local/bin/

# 32-bit (Pi Zero 2 W, older Pi 3 with 32-bit OS)
curl -LO https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz
tar xzf zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz
sudo install -m 0755 zeroclaw /usr/local/bin/
```

### Check your architecture

```bash
uname -m
# aarch64 → 64-bit (use the aarch64 binary)
# armv7l  → 32-bit (use the armv7 binary)
# armv6l  → Pi 1 / Zero (not currently supported, see #4623)
```

## Option 2: Cross-Compile From Another Machine

If you already have a beefier machine, cross-compiling is faster than building on the Pi.

### From macOS (Apple Silicon or Intel)

```bash
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

> **Note:** earlier drafts of this guide suggested `aarch64-elf-gcc` from Homebrew. That toolchain produces bare-metal ELF binaries and links against newlib, not glibc — it will not produce a working Raspberry Pi OS binary. Use the `messense/macos-cross-toolchains` tap above (a real Linux GNU/glibc toolchain), or fall back to Option 3 (build on the Pi).

### From Linux x86_64

```bash
# Install cross-compilation toolchain
sudo apt-get install -y gcc-aarch64-linux-gnu

# Add target
rustup target add aarch64-unknown-linux-gnu

# Configure linker (~/.cargo/config.toml)
# [target.aarch64-unknown-linux-gnu]
# linker = "aarch64-linux-gnu-gcc"

# Build
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@raspberrypi:~/
```

## Option 3: Build on the Pi

Possible on Pi 4/5 if you set up swap and pick the right profile. Expect 20-40 minutes on a Pi 5 (8 GB), longer on Pi 4.

### Step 1: Install Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Step 2: Add swap (critical for Pi 5 with ≤ 8 GB or any Pi 4)

The default `release` profile peaks around 8-10 GB RSS during fat LTO linking. Without swap, that triggers the OOM-killer mid-link.

```bash
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

### Step 3: Choose a build profile

The default `release` profile uses `lto = "fat"` and `codegen-units = 1` — best runtime performance, worst build memory. The `release-fast` profile (`codegen-units = 8`, `lto = "thin"`) drops peak RAM by ~half, with only minor runtime impact.

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

# Pi 5 (8 GB, with swap): default release works
cargo build --release

# Pi 4 (4 GB, with swap): use release-fast
cargo build --profile release-fast

# Pi 4 (2 GB) or constrained: use ci profile (debug-info-stripped, fast link)
cargo build --profile ci
```

### Step 4: Install the binary

```bash
# From the build profile you used:
sudo install -m 0755 target/release/zeroclaw /usr/local/bin/
# or target/release-fast/zeroclaw, or target/ci/zeroclaw
```

### Building with GPIO support

If you want to use Pi GPIO peripherals from skills, enable the relevant feature flag (see the `peripherals` crate). Most users don't need this for typical agent workloads — it's only relevant if you're writing skills that talk to attached hardware.

## Containerized deployment (Podman recommended over Docker)

**Pis are memory-constrained, and that's the operating reality this section is written against.** The 2 GB Pi 4 is the low-bar test unit for this guide — if a setup doesn't leave headroom on a 2 GB box, it's not a setup we recommend. ZeroClaw itself runs in well under 5 MB RSS at runtime, but everything you stack alongside it (channel transports, browser-control, MCP servers, an adjacent agent or two, plus the OS) competes for the same fixed pool. Memory you don't spend on container infrastructure is memory ZeroClaw and its tools get to use.

Concrete budget on a 2 GB Pi 4 running Raspberry Pi OS Bookworm/Trixie headless:

| Component | Approx RSS |
|---|---|
| Kernel + base userspace + sshd | ~150-250 MB |
| `dockerd` (idle, no containers) | ~150-200 MB |
| ZeroClaw runtime (gateway only) | ~5 MB |
| One agent container (e.g. ghcr.io/zeroclaw-labs/zeroclaw) | ~30-80 MB |
| **Available with Docker** | ~1.3-1.5 GB |
| **Available with Podman (no daemon)** | ~1.5-1.7 GB |

The Podman delta is on the order of ~150-200 MB freed up — small in absolute terms, large as a percentage of what's left over after the OS gets its share. On a 2 GB unit that's the difference between comfortably running ZeroClaw + a heavy channel transport (Matrix with media, browser-automation skills) and OOM-killing under load.

**Three reasons Podman is the better fit on Pi than Docker:**

1. **Rootless by default → security headroom.** Podman doesn't need a root daemon; containers run as your user. On an exposed edge device that matters more than on a developer laptop.
2. **systemd-native via Quadlets → operational simplicity.** Podman ships `.container` unit files that systemd manages directly — same lifecycle, logging, and dependency model as any other unit. No separate `docker.service` to babysit, no separate logging layer.
3. **No daemon RSS → memory headroom.** Skipping `dockerd`'s persistent ~150-200 MB is the single biggest knob you can turn on a 2 GB Pi without sacrificing isolation.

The trade-off: Podman's rootless network model uses slirp4netns (or pasta on newer versions), which is slower than the bridge that Docker's daemon sets up. For workloads that move a lot of HTTP traffic between containers on the same Pi, that's worth measuring. For ZeroClaw's typical "one or two long-running agent containers" pattern, the difference is negligible — and on memory-constrained hardware, the daemon-RSS savings dominate the calculation anyway.

### Quick install (Raspberry Pi OS Bookworm/Trixie)

```bash
sudo apt-get install -y podman
# Optional: shorter aliases — many docker-compose flows just work with podman-compose
sudo apt-get install -y podman-compose
```

### Running ZeroClaw under Podman

The published OCI image works under Podman without modification:

```bash
podman pull ghcr.io/zeroclaw-labs/zeroclaw:latest

podman run --rm -d \
  --name zeroclaw \
  -p 42617:42617 \
  -v ~/.zeroclaw:/root/.zeroclaw \
  ghcr.io/zeroclaw-labs/zeroclaw:latest \
  daemon --host 0.0.0.0 --port 42617
```

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

```bash
systemctl --user daemon-reload
systemctl --user start zeroclaw.service
```

For rootless setups, also run `loginctl enable-linger $USER` so the service starts before you log in.

## Post-Install: Native (non-container) setup

### 1. Initialize ZeroClaw

```bash
zeroclaw onboard
```

This walks you through provider auth, gateway config, and creates `~/.zeroclaw/config.toml`.

### 2. Verify it works

```bash
zeroclaw doctor
zeroclaw agent -m "what's 2+2?"
```

### 3. Run as a persistent service

```bash
# Install and start the systemd user service
zeroclaw service install
systemctl --user enable --now zeroclaw

# So it survives logout / reboot:
loginctl enable-linger $USER
```

### 4. Run as a foreground daemon

For dev / debugging:

```bash
zeroclaw daemon --host 0.0.0.0 --port 42617
```

### 5. Enable channels

ZeroClaw can connect to chat platforms (Matrix, Mattermost, Discord, Telegram, etc.). See [Channels → Overview](../channels/overview.md). Most channel transports work fine on a Pi; the heaviest is the WebRTC stack used by some voice channels, which can spike CPU during call setup.

## GPIO and Hardware Peripherals

If you want skills to drive GPIO pins (LEDs, buttons, sensors, etc.):

1. Add your user to the `gpio` group:
   ```bash
   sudo usermod -aG gpio $USER
   # Log out and back in for the group change to take effect
   ```
2. Use the `peripherals` crate's GPIO bindings from your skills. See [Hardware → Peripherals design](./hardware-peripherals-design.md) for the abstraction model.

## Troubleshooting

### OOM-killed during build

The `release` profile peaks at ~8-10 GB RSS during the final link. Either:

- Switch to `cargo build --profile release-fast` (drops peak to ~4-6 GB).
- Add a 4 GB swap file (Step 2 above).
- Cross-compile from a beefier machine (Option 2).

If you're using `release-fast` and still OOMing on a Pi 4 (2 GB), drop to `--profile ci` or use the pre-built binary.

### Build extremely slow

Expected on Pi 4. A clean release build takes 30-60 minutes; incremental builds are reasonable. Use cross-compilation (Option 2) if build time matters.

### Pre-built binary: "Exec format error"

Architecture mismatch. Check `uname -m` and download the matching binary. `aarch64` is 64-bit (most Pi 4/5 with 64-bit Raspberry Pi OS); `armv7l` is 32-bit.

### GPIO permission denied

```bash
sudo usermod -aG gpio $USER
# Log out and back in
```

### Service won't start after reboot

Make sure user-level systemd persists across logout:

```bash
loginctl enable-linger $USER
```

### Container can't reach gateway from host

ZeroClaw binds `127.0.0.1` by default — inside a container that means localhost-of-the-container. Pass `--host 0.0.0.0` (or `ZEROCLAW_BIND=0.0.0.0`) when running in Podman/Docker.

## Performance tips

- **Use an SSD or fast SD card.** Compilation is heavily I/O-bound; a USB 3.0 SSD on a Pi 4/5 cuts build time significantly.
- **Run headless.** Stop the desktop environment if not needed: `sudo systemctl set-default multi-user.target`.
- **tmpfs for build artifacts** (if you have RAM + swap headroom): `export CARGO_TARGET_DIR=/tmp/zeroclaw-target`.
- **Check that `clk_ignore_unused` isn't set** on the kernel cmdline if you're using a custom image — that flag (occasionally seen on vendor BSPs) inhibits clock gating and increases idle power. Stock Raspberry Pi OS doesn't ship with it.

## Related

- [Linux setup](../setup/linux.md) — non-Pi-specific Linux setup, applicable here too once the binary's installed
- [Service management](../setup/service.md) — systemd patterns, deeper than what's above
- [Hardware → Peripherals design](./hardware-peripherals-design.md) — GPIO and the peripherals crate
- [Hardware → Adding boards & tools](./adding-boards-and-tools.md) — extending hardware support
