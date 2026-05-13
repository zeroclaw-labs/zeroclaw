# DaemonClaw on Arduino Uno Q — Step-by-Step Guide

Run DaemonClaw on the Arduino Uno Q's Linux side. Telegram works over WiFi; GPIO control uses the Bridge (requires a minimal App Lab app).

---

## What's Included (No Code Changes Needed)

DaemonClaw includes everything needed for Arduino Uno Q. **Clone the repo and follow this guide — no patches or custom code required.**

| Component | Location | Purpose |
|-----------|----------|---------|
| Bridge app | `firmware/uno-q-bridge/` | MCU sketch + Python socket server (port 9999) for GPIO |
| Bridge tools | `src/peripherals/uno_q_bridge.rs` | `gpio_read` / `gpio_write` tools that talk to the Bridge over TCP |
| Setup command | `src/peripherals/uno_q_setup.rs` | `daemonclaw peripheral setup-uno-q` deploys the Bridge via scp + arduino-app-cli |
| Config schema | `board = "arduino-uno-q"`, `transport = "bridge"` | Supported in `config.toml` |

Build with `--features hardware` to include Uno Q support.

---

## Prerequisites

- Arduino Uno Q with WiFi configured
- Arduino App Lab installed on your Mac (for initial setup and deployment)
- API key for LLM (OpenRouter, etc.)

---

## Phase 1: Initial Uno Q Setup (One-Time)

### 1.1 Configure Uno Q via App Lab

1. Download [Arduino App Lab](https://docs.arduino.cc/software/app-lab/) (tar.gz on Linux).
2. Connect Uno Q via USB, power it on.
3. Open App Lab, connect to the board.
4. Follow the setup wizard:
   - Set username and password (for SSH)
   - Configure WiFi (SSID, password)
   - Apply any firmware updates
5. Note the IP address shown (e.g. `arduino@192.168.1.42`) or find it later via `ip addr show` in App Lab's terminal.

### 1.2 Verify SSH Access

```bash
ssh arduino@<UNO_Q_IP>
# Enter the password you set
```

---

## Phase 2: Install DaemonClaw on Uno Q

### Option A: Build on the Device (Simpler, ~20–40 min)

```bash
# SSH into Uno Q
ssh arduino@<UNO_Q_IP>

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Install build deps (Debian)
sudo apt-get update
sudo apt-get install -y pkg-config libssl-dev

# Clone daemonclaw (or scp your project)
git clone https://github.com/DeliveryBoyTech/daemonclaw.git
cd daemonclaw

# Build (takes ~15–30 min on Uno Q)
cargo build --release --features hardware

# Install
sudo cp target/release/daemonclaw /usr/local/bin/
```

### Option B: Cross-Compile on Mac (Faster)

```bash
# On your Mac — add aarch64 target
rustup target add aarch64-unknown-linux-gnu

# Install cross-compiler (macOS; required for linking)
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu

# Build
CC_aarch64_unknown_linux_gnu=aarch64-unknown-linux-gnu-gcc cargo build --release --target aarch64-unknown-linux-gnu --features hardware

# Copy to Uno Q
scp target/aarch64-unknown-linux-gnu/release/daemonclaw arduino@<UNO_Q_IP>:~/
ssh arduino@<UNO_Q_IP> "sudo mv ~/daemonclaw /usr/local/bin/"
```

If cross-compile fails, use Option A and build on the device.

---

## Phase 3: Configure DaemonClaw

### 3.1 Run Onboard (or Create Config Manually)

```bash
ssh arduino@<UNO_Q_IP>

# Quick config
daemonclaw onboard --api-key YOUR_OPENROUTER_KEY --provider openrouter

# Or create config manually
mkdir -p ~/.daemonclaw/workspace
nano ~/.daemonclaw/config.toml
```

### 3.2 Minimal config.toml

```toml
api_key = "YOUR_OPENROUTER_API_KEY"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"

[peripherals]
enabled = false
# GPIO via Bridge requires Phase 4

[channels_config.telegram]
bot_token = "YOUR_TELEGRAM_BOT_TOKEN"
allowed_users = ["*"]

[gateway]
host = "127.0.0.1"
port = 42617
allow_public_bind = false

[agent]
compact_context = true
```

---

## Phase 4: Run DaemonClaw Daemon

```bash
ssh arduino@<UNO_Q_IP>

# Run daemon (Telegram polling works over WiFi)
daemonclaw daemon --host 127.0.0.1 --port 42617
```

**At this point:** Telegram chat works. Send messages to your bot — DaemonClaw responds. No GPIO yet.

---

## Phase 5: GPIO via Bridge (DaemonClaw Handles It)

DaemonClaw includes the Bridge app and setup command.

### 5.1 Deploy Bridge App

**From your Mac** (with daemonclaw repo):
```bash
daemonclaw peripheral setup-uno-q --host 192.168.0.48
```

**From the Uno Q** (SSH'd in):
```bash
daemonclaw peripheral setup-uno-q
```

This copies the Bridge app to `~/ArduinoApps/uno-q-bridge` and starts it.

### 5.2 Add to config.toml

```toml
[peripherals]
enabled = true

[[peripherals.boards]]
board = "arduino-uno-q"
transport = "bridge"
```

### 5.3 Run DaemonClaw

```bash
daemonclaw daemon --host 127.0.0.1 --port 42617
```

Now when you message your Telegram bot *"Turn on the LED"* or *"Set pin 13 high"*, DaemonClaw uses `gpio_write` via the Bridge.

---

## Summary: Commands Start to End

| Step | Command |
|------|---------|
| 1 | Configure Uno Q in App Lab (WiFi, SSH) |
| 2 | `ssh arduino@<IP>` |
| 3 | `curl -sSf https://sh.rustup.rs \| sh -s -- -y && source ~/.cargo/env` |
| 4 | `sudo apt-get install -y pkg-config libssl-dev` |
| 5 | `git clone https://github.com/DeliveryBoyTech/daemonclaw.git && cd daemonclaw` |
| 6 | `cargo build --release --features hardware` |
| 7 | `daemonclaw onboard --api-key KEY --provider openrouter` |
| 8 | Edit `~/.daemonclaw/config.toml` (add Telegram bot_token) |
| 9 | `daemonclaw daemon --host 127.0.0.1 --port 42617` |
| 10 | Message your Telegram bot — it responds |

---

## Troubleshooting

- **"command not found: daemonclaw"** — Use full path: `/usr/local/bin/daemonclaw` or ensure `~/.cargo/bin` is in PATH.
- **Telegram not responding** — Check bot_token, allowed_users, and that the Uno Q has internet (WiFi).
- **Out of memory** — Keep features minimal (`--features hardware` for Uno Q); consider `compact_context = true`.
- **GPIO commands ignored** — Ensure Bridge app is running (`daemonclaw peripheral setup-uno-q` deploys and starts it). Config must have `board = "arduino-uno-q"` and `transport = "bridge"`.
- **LLM provider (GLM/Zhipu)** — Use `default_provider = "glm"` or `"zhipu"` with `GLM_API_KEY` in env or config. DaemonClaw uses the correct v4 endpoint.
