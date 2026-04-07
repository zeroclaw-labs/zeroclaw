# Deploying ZeroClaw on Raspberry Pi

This guide covers installing and running ZeroClaw on Raspberry Pi models (Pi 3, Pi 4, Pi 5, Pi Zero 2 W).

## Hardware Requirements

| Model | RAM | Build from source? | Pre-built binary? |
| ----- | --- | ------------------ | ----------------- |
| Pi 5 (8 GB) | 8 GB | Yes (with swap or `release-fast` profile) | Yes |
| Pi 5 (4 GB) | 4 GB | Yes (with swap + `release-fast` profile) | Yes |
| Pi 4 (4 GB) | 4 GB | Yes (with swap + `release-fast` profile) | Yes |
| Pi 4 (2 GB) | 2 GB | Marginal (swap required, slow) | Yes |
| Pi 3 (1 GB) | 1 GB | Not recommended | Yes |
| Pi Zero 2 W | 512 MB | No | Yes |

**Runtime** memory is minimal (<5 MB RSS for the core agent). The challenge is **build-time** memory — Rust's compiler and linker need significantly more RAM than the resulting binary.

## Option 1: Pre-built Binary (Recommended)

The fastest path. No compiler needed, no memory pressure.

### Using the install script

```bash
curl -LsSf https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | bash -s -- --prebuilt-only
```

Or if you've cloned the repo:

```bash
./install.sh --prefer-prebuilt
```

The installer auto-detects `aarch64` (Pi 3/4/5 on 64-bit OS) and `armv7l` (32-bit Raspberry Pi OS) and downloads the matching release asset.

### Manual download

1. Go to <https://github.com/zeroclaw-labs/zeroclaw/releases/latest>
2. Download the archive matching your architecture:
   - **64-bit Raspberry Pi OS:** `zeroclaw-aarch64-unknown-linux-gnu.tar.gz`
   - **32-bit Raspberry Pi OS:** `zeroclaw-armv7-unknown-linux-gnueabihf.tar.gz`
3. Extract and install:

```bash
tar -xzf zeroclaw-*.tar.gz
sudo mv zeroclaw /usr/local/bin/
zeroclaw --version
```

### Check your architecture

```bash
uname -m
# aarch64 → 64-bit
# armv7l  → 32-bit
```

## Option 2: Cross-Compile from Another Machine

Build on your Mac/Linux desktop and copy the binary to the Pi. This avoids all memory constraints on the Pi.

### From macOS (Apple Silicon or Intel)

```bash
# Install the cross-compilation target
rustup target add aarch64-unknown-linux-gnu

# Install a cross-linker (via Homebrew)
brew install filosottile/musl-cross/musl-cross

# Build
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to your Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@<pi-ip>:/usr/local/bin/
```

> **Note:** Cross-compiling with `rustls` (ZeroClaw's default TLS) avoids needing OpenSSL cross-headers. If you hit linker errors, use [cross](https://github.com/cross-rs/cross) which handles toolchains via Docker:
>
> ```bash
> cargo install cross
> cross build --release --target aarch64-unknown-linux-gnu
> ```

### From Linux x86_64

```bash
# Install cross-compilation toolchain
sudo apt install gcc-aarch64-linux-gnu

# Add target
rustup target add aarch64-unknown-linux-gnu

# Configure linker in ~/.cargo/config.toml
# [target.aarch64-unknown-linux-gnu]
# linker = "aarch64-linux-gnu-gcc"

# Build
cargo build --release --target aarch64-unknown-linux-gnu

# Copy to Pi
scp target/aarch64-unknown-linux-gnu/release/zeroclaw pi@<pi-ip>:/usr/local/bin/
```

## Option 3: Build on the Pi

If you need to build from source directly on the Pi (e.g., for custom features like `peripheral-rpi`), you'll need to work around memory limits.

### Step 1: Install Rust toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### Step 2: Add swap (critical for Pi 5 with 8 GB or less)

The default `release` profile uses `lto = "fat"` and `codegen-units = 1`, which can peak above 8 GB RSS during linking. Add swap to prevent OOM kills:

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

| Profile | Command | Peak RAM | Binary size | Notes |
| ------- | ------- | -------- | ----------- | ----- |
| `release` | `cargo build --release` | ~8-10 GB | Smallest | Default; needs swap on 8 GB Pi |
| `release-fast` | `cargo build --profile release-fast` | ~4-6 GB | Slightly larger | `codegen-units = 8`; best for Pi |
| `ci` | `cargo build --profile ci` | ~3-4 GB | Larger | `thin` LTO + 16 codegen units |

**Recommended for Pi 5 (8 GB):**

```bash
cargo build --profile release-fast
```

**Recommended for Pi 4 (4 GB) with swap:**

```bash
cargo build --profile ci
```

### Step 4: Install the binary

```bash
# From the build profile you used:
sudo cp target/release-fast/zeroclaw /usr/local/bin/

# Or use cargo install with the profile:
cargo install --path . --profile release-fast
```

### Building with GPIO support

To enable Raspberry Pi GPIO tools (pin read/write via the agent):

```bash
cargo build --profile release-fast --features peripheral-rpi
```

This pulls in the `rppal` crate for direct GPIO/I2C/SPI access (Linux only).

## Post-Install Setup

### 1. Initialize ZeroClaw

```bash
zeroclaw init
```

This runs the onboarding wizard: set your preferred provider, API key, and default channel.

### 2. Verify it works

```bash
zeroclaw ask "Hello, are you running on a Raspberry Pi?"
```

### 3. Run as a persistent service

For headless operation (the common Pi use case):

```bash
# Install and start the systemd user service
zeroclaw service install
zeroclaw service start
zeroclaw service status
```

To view logs:

```bash
journalctl --user -u zeroclaw.service -f
```

### 4. Run as a foreground daemon

For testing or development:

```bash
zeroclaw daemon
```

### 5. Enable channels

Connect your Pi-hosted agent to Telegram, Discord, Slack, etc.:

```bash
zeroclaw channel add telegram
zeroclaw channel add discord
```

Then restart the service:

```bash
zeroclaw service stop
zeroclaw service start
```

## GPIO and Hardware Peripherals

If you built with `--features peripheral-rpi`, you can register the Pi as a peripheral:

```bash
zeroclaw peripheral add rpi-gpio native
```

This exposes GPIO tools (`gpio_read`, `gpio_write`) to the agent, allowing natural language hardware control:

> "Turn on the LED connected to GPIO pin 17"

See [hardware-peripherals-design.md](hardware-peripherals-design.md) for the full peripheral architecture.

## Troubleshooting

### OOM killed during build

**Symptom:** Build process is killed with `signal: 9 (SIGKILL)` or `out of memory`.

**Fix:** Add more swap (see Step 2 above) and use `release-fast` or `ci` profile instead of the default `release` profile.

### Build extremely slow

Expect 20-40 minutes on a Pi 5, longer on Pi 4. Use cross-compilation (Option 2) if build time is a concern.

### GPIO permission denied

GPIO access requires either root or membership in the `gpio` group:

```bash
sudo usermod -aG gpio $USER
# Log out and back in for group change to take effect
```

### Pre-built binary: "Exec format error"

Architecture mismatch. Check `uname -m` and download the matching binary (`aarch64` vs `armv7`).

### Service won't start after reboot

Ensure the systemd user service is enabled for linger:

```bash
loginctl enable-linger $USER
```

## Performance Tips

- **Use an SSD or fast SD card.** Compilation is heavily I/O-bound; a USB 3.0 SSD on Pi 4/5 cuts build time significantly.
- **Minimize running services.** Stop desktop environment if running headless: `sudo systemctl set-default multi-user.target`.
- **Use tmpfs for build artifacts** if you have enough RAM+swap: set `CARGO_TARGET_DIR=/tmp/zeroclaw-target`.
