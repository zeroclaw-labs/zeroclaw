# Raspberry Pi Setup

This guide covers installing and running ZeroClaw on Raspberry Pi.

The runtime is small enough to run comfortably on any Pi. The only constraint is **building from source on the device**: Rust's linker is memory-hungry (fat LTO can OOM a low-RAM board), so the on-device build path needs swap and a lighter profile. Most users should take the **pre-built binary** and skip all of that.

## Hardware Compatibility

Any Pi that can run a 64-bit (`aarch64`) or 32-bit (`armv7`) Raspberry Pi OS runs the pre-built binary; there is no meaningful memory floor for the runtime. The prebuilt Pi binaries come from these release targets (64-bit `aarch64` for 64-bit Raspberry Pi OS, 32-bit `armv7`/`arm` for 32-bit OS):

{{#include ../_snippets/hardware-pi-targets.md}}

## Option 1: Pre-built Binary (Recommended)

Fastest path. No compiler, no swap, no OOM risk.

### Using the install script

{{#include ../_snippets/install.md:linux}}

The script auto-detects your architecture (`aarch64`, `armv7`, or `armv6`) and installs the matching release binary into `$CARGO_HOME/bin/zeroclaw` (defaulting to `~/.cargo/bin/zeroclaw`). Make sure that directory is on your `PATH`.

When the script builds from source instead of taking a prebuilt binary, it also adapts the build to the board's available memory:

{{#include ../_snippets/hardware-lowmem-lto.md}}

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
# aarch64 → 64-bit (use the aarch64-unknown-linux-gnu binary)
# armv7l  → 32-bit (use the armv7-unknown-linux-gnueabihf binary)
# armv6l  → Pi 1 / Zero / Zero W (use the arm-unknown-linux-gnueabihf binary)
```

</div>

## Option 2: Cross-Compile From Another Machine

If you already have a beefier machine, cross-compiling is faster than building on the Pi.

<div class="os-tabs-src">

#### macOS (Apple Silicon or Intel)

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

> **Note:** earlier drafts of this guide suggested `aarch64-elf-gcc` from Homebrew. That toolchain produces bare-metal ELF binaries and links against newlib, not glibc. It will not produce a working Raspberry Pi OS binary. Use the `messense/macos-cross-toolchains` tap above (a real Linux GNU/glibc toolchain), or fall back to Option 3 (build on the Pi).

#### Linux x86_64

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

The agent compiling itself on the device. Works on any Pi with swap and the right build profile; slower on lower-RAM boards.

### Step 1: Install Rust toolchain

<div class="os-tabs-src">

#### sh

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

</div>

### Step 2: Add swap

Fat LTO peaks during the final link; without swap, a low-RAM board OOM-kills mid-link.

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

### Step 3: Build

Pick a profile by available RAM. `release` is fat LTO (best binary, heaviest link); `release-fast` raises codegen-units for a lighter link; `ci` uses thin LTO for the lowest-memory link. (`install.sh` picks this automatically; see [Using the install script](#using-the-install-script).)

<div class="os-tabs-src">

#### sh

```sh
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw

cargo build --release           # higher-RAM board
cargo build --profile release-fast   # mid-RAM board
cargo build --profile ci        # low-RAM / constrained board

# Install the binary you built:
sudo install -m 0755 target/release/zeroclaw /usr/local/bin/
# (or target/release-fast/zeroclaw, or target/ci/zeroclaw)
```

</div>

### GPIO support

To drive Pi GPIO from skills, build with the relevant `peripherals` feature flag. Most agent workloads don't need it; see [Peripherals design](./hardware-peripherals-design.md).

## Containerized deployment (Podman recommended over Docker)

On a memory-constrained Pi, container runtime choice matters: everything you stack alongside ZeroClaw competes for the same fixed pool, so memory not spent on container infrastructure is memory the agent gets.

**Why Podman over Docker on a Pi:**

1. **Rootless by default.** No root daemon; containers run as your user, which matters on an exposed edge device.
2. **systemd-native via Quadlets.** `.container` unit files systemd manages directly, with no separate `docker.service` or logging layer.
3. **No persistent daemon.** Docker keeps `dockerd` resident; Podman does not, freeing the largest single chunk of memory without losing isolation.

The trade-off: Podman's rootless network (slirp4netns/pasta) is slower than Docker's bridge. For ZeroClaw's "one or two long-running agent containers" pattern that's negligible, and the daemon savings dominate on constrained hardware.

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

- **OOM-killed during build:** add swap (Option 3 Step 2), drop to a lighter profile (`release-fast` or `ci`), or use the pre-built binary / cross-compile.
- **Build extremely slow:** expected on lower-RAM boards; cross-compile (Option 2) if it matters.
- **Pre-built binary "Exec format error":** architecture mismatch. `uname -m` and grab the matching binary (`aarch64` = 64-bit, `armv7l` = 32-bit).
- **GPIO permission denied:** you are not in the `gpio` group; `sudo usermod -aG gpio $USER`, then re-login.
- **Service won't start after reboot:** `loginctl enable-linger $USER` so the user service survives logout.
- **Container can't reach gateway from host:** the gateway binds `127.0.0.1`; pass `--host 0.0.0.0` (or `ZEROCLAW_BIND=0.0.0.0`).

## Performance tips

- **Use an SSD or fast SD card.** Compilation is I/O-bound; a USB 3.0 SSD on a Pi 4/5 cuts build time significantly.
- **Run headless:** `sudo systemctl set-default multi-user.target`.
- **tmpfs for build artifacts** (with RAM + swap headroom): `export CARGO_TARGET_DIR=/tmp/zeroclaw-target`.
- **Check `clk_ignore_unused`** isn't on the kernel cmdline if you use a custom image; it inhibits clock gating and raises idle power. Stock Raspberry Pi OS doesn't set it.

## Related

- [Linux setup](../setup/linux.md): non-Pi-specific Linux setup, applicable here too once the binary's installed
- [Service management](../setup/service.md): systemd patterns, deeper than what's above
- [Hardware → Peripherals design](./hardware-peripherals-design.md): GPIO and the peripherals crate
- [Hardware → Adding boards & tools](./adding-boards-and-tools.md): extending hardware support
