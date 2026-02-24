# ZeroClaw Setup Guide

Complete installation, configuration, and deployment guide for ZeroClaw v0.2.0.

## Table of Contents

- [System Requirements](#system-requirements)
- [Installation](#installation)
- [Initial Configuration](#initial-configuration)
- [Provider Setup](#provider-setup)
- [Channel Setup](#channel-setup)
- [Memory Configuration](#memory-configuration)
- [Security Configuration](#security-configuration)
- [Gateway & Tunnels](#gateway--tunnels)
- [Running ZeroClaw](#running-zeroclaw)
- [Service Management](#service-management)
- [Verification & Diagnostics](#verification--diagnostics)

---

## System Requirements

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| **RAM** | 5 MB | 64 MB |
| **Disk** | 20 MB | 100 MB |
| **CPU** | Any (ARM/x86/RISC-V) | 0.8 GHz+ |
| **OS** | Linux, macOS, Windows | Linux arm64 / macOS arm64 |
| **Rust** | 1.75+ stable | Latest stable |

### Build Dependencies

**Linux (Debian/Ubuntu):**
```bash
sudo apt install build-essential pkg-config
```

**Linux (Fedora/RHEL):**
```bash
sudo dnf group install development-tools && sudo dnf install pkg-config
```

**macOS:**
```bash
xcode-select --install
```

**Windows:**
```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
# Select "Desktop development with C++" workload
winget install Rustlang.Rustup
```

---

## Installation

### Option 1: One-Click Bootstrap (Recommended)

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

With system dependencies and Rust:
```bash
./bootstrap.sh --install-system-deps --install-rust
```

With onboarding in one step:
```bash
./bootstrap.sh --onboard --api-key "sk-..." --provider openrouter
```

### Option 2: Remote One-Liner

```bash
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main/scripts/install.sh | bash
```

### Option 3: Manual Build

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
cargo build --release --locked
cargo install --path . --force --locked
export PATH="$HOME/.cargo/bin:$PATH"
```

### Build Profiles

| Profile | Command | Use Case |
|---------|---------|----------|
| **Release** | `cargo build --release` | Production, low-memory devices (Raspberry Pi) |
| **Release-Fast** | `cargo build --profile release-fast` | Fast builds on 16GB+ RAM machines |
| **Dev** | `cargo build` | Development with debug symbols |

### Verify Installation

```bash
zeroclaw --version    # Should print version
zeroclaw status       # System status overview
zeroclaw doctor       # Full diagnostics
```

---

## Initial Configuration

Run the interactive onboarding wizard:

```bash
zeroclaw onboard --interactive
```

Or non-interactive quick setup:

```bash
zeroclaw onboard --api-key "sk-..." --provider openrouter
```

This creates `~/.zeroclaw/config.toml` with your settings.

### Configuration File Location

| Platform | Path |
|----------|------|
| Linux/macOS | `~/.zeroclaw/config.toml` |
| Windows | `%USERPROFILE%\.zeroclaw\config.toml` |

### Minimal Config

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-6"
default_temperature = 0.7
```

---

## Provider Setup

ZeroClaw supports 28+ providers. Set your default:

```toml
default_provider = "openrouter"  # or "anthropic", "openai", "ollama", etc.
api_key = "your-api-key"
```

### Provider-Specific Environment Variables

| Provider | Env Var | Notes |
|----------|---------|-------|
| OpenRouter | `OPENROUTER_API_KEY` | Access all models via single key |
| Anthropic | `ANTHROPIC_API_KEY` | Direct Claude access |
| OpenAI | `OPENAI_API_KEY` | GPT models |
| Gemini | `GEMINI_API_KEY` | Google AI models |
| Ollama | `OLLAMA_API_KEY` | Optional for remote; local needs no key |
| Groq | `GROQ_API_KEY` | Fast inference |
| DeepSeek | `DEEPSEEK_API_KEY` | DeepSeek models |
| xAI | `XAI_API_KEY` | Grok models |

### Ollama (Local)

```bash
ollama serve                      # Start local server
zeroclaw onboard --provider ollama --api-key "" --model llama3.2
```

### Ollama (Remote / Cloud)

```toml
default_provider = "ollama"
default_model = "qwen3:cloud"
api_url = "https://ollama.com"
api_key = "your-ollama-api-key"
```

### Custom OpenAI-Compatible Endpoint

```toml
default_provider = "custom:https://your-api.example.com"
api_key = "your-key"
```

### Custom Anthropic-Compatible Endpoint

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
api_key = "your-key"
```

### Model Routing

Route model calls by hint for multi-provider setups:

```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

Full provider list: [`docs/providers-reference.md`](providers-reference.md)

---

## Channel Setup

### Quick Start: Add One Channel

1. Add channel config to `~/.zeroclaw/config.toml`
2. Run `zeroclaw onboard --channels-only`
3. Start `zeroclaw daemon`
4. Send a test message
5. Tighten allowlist from `"*"` to specific IDs

### Telegram

1. Create a bot via [@BotFather](https://t.me/BotFather)
2. Copy the bot token

```toml
[channels_config.telegram]
bot_token = "123456:ABC-DEF..."
allowed_users = ["your-telegram-user-id"]
mention_only = false
```

Optional voice support:
```toml
[channels_config.telegram.voice]
enabled = true
api_key = "sk-..."               # OpenAI API key for Whisper/TTS
stt_model = "whisper-1"
tts_model = "tts-1"
tts_voice = "alloy"
respond_with_voice = true
```

### Discord

1. Create a bot at [Discord Developer Portal](https://discord.com/developers/applications)
2. Enable intents: GUILDS, GUILD_MESSAGES, MESSAGE_CONTENT, DIRECT_MESSAGES
3. Invite bot to your server

```toml
[channels_config.discord]
bot_token = "your-discord-bot-token"
guild_id = "123456789012345678"
allowed_users = ["your-discord-user-id"]
mention_only = false
```

### Slack

1. Create a Slack App at [api.slack.com](https://api.slack.com/apps)
2. Add bot token scopes: `chat:write`, `channels:history`, `channels:read`

```toml
[channels_config.slack]
bot_token = "xoxb-..."
channel_id = "C1234567890"
allowed_users = ["U1234567890"]
```

### WhatsApp

1. Create Meta Business App at [developers.facebook.com](https://developers.facebook.com)
2. Add WhatsApp product, get access token and phone number ID
3. Configure webhook pointing to `https://your-domain/whatsapp`

```toml
[channels_config.whatsapp]
access_token = "EAABx..."
phone_number_id = "123456789012345"
verify_token = "your-random-verify-token"
app_secret = "your-app-secret"
allowed_numbers = ["+1234567890"]
```

### All 15 Channels

Full configuration reference: [`docs/channels-reference.md`](channels-reference.md)

---

## Memory Configuration

```toml
[memory]
backend = "sqlite"             # "sqlite", "postgres", "lucid", "markdown", "none"
auto_save = true
embedding_provider = "none"    # "none", "openai", "custom:https://..."
vector_weight = 0.7
keyword_weight = 0.3
```

### PostgreSQL Backend

```toml
[storage.provider.config]
provider = "postgres"
db_url = "postgres://user:password@host:5432/zeroclaw"
schema = "public"
table = "memories"
connect_timeout_secs = 15
```

### Embedding Search

Enable semantic search with OpenAI embeddings:

```toml
[memory]
backend = "sqlite"
embedding_provider = "openai"  # Uses OPENAI_API_KEY
vector_weight = 0.7
keyword_weight = 0.3
```

---

## Security Configuration

### Autonomy Levels

```toml
[autonomy]
level = "supervised"           # "readonly", "supervised", "full"
workspace_only = true
allowed_commands = ["git", "npm", "cargo", "ls", "cat", "grep"]
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
```

| Level | Behavior |
|-------|----------|
| `readonly` | Can only read files; no writes, no shell commands |
| `supervised` | Can read/write within workspace; shell limited to allowlist |
| `full` | Unrestricted within workspace; use with caution |

### Encrypted Secrets

```toml
[secrets]
encrypt = true
```

Secrets are encrypted with ChaCha20-Poly1305 using a local key at `~/.zeroclaw/.secret_key`.

### Subscription Auth

```bash
# OpenAI Codex (ChatGPT subscription)
zeroclaw auth login --provider openai-codex --device-code

# Anthropic setup token
zeroclaw auth paste-token --provider anthropic --profile default --auth-kind authorization

# Check status
zeroclaw auth status
```

---

## Gateway & Tunnels

### Start Gateway

```bash
zeroclaw gateway                  # Default: 127.0.0.1:3000
zeroclaw gateway --port 8080      # Custom port
zeroclaw gateway --port 0         # Random port (security hardened)
```

### Tunnel Configuration

Required for webhook-based channels (WhatsApp) and remote access:

```toml
[tunnel]
provider = "cloudflare"    # "none", "cloudflare", "tailscale", "ngrok", "custom"
```

### Gateway Security

```toml
[gateway]
port = 3000
host = "127.0.0.1"
require_pairing = true
allow_public_bind = false
```

The gateway refuses to bind `0.0.0.0` unless a tunnel is active or `allow_public_bind = true`.

---

## Running ZeroClaw

### Interactive Chat

```bash
zeroclaw agent                    # Interactive REPL
zeroclaw agent -m "Hello!"        # Single message
```

### Daemon Mode (Channels + Gateway)

```bash
zeroclaw daemon                   # Start all configured channels + gateway
```

### Channel Operations

```bash
zeroclaw channel list             # List configured channels
zeroclaw channel doctor           # Check channel health
zeroclaw channel bind-telegram ID # Add Telegram user to allowlist
```

---

## Service Management

Install ZeroClaw as a background service:

```bash
zeroclaw service install          # Install user-level service
zeroclaw service status           # Check service status
```

---

## Verification & Diagnostics

```bash
zeroclaw status                   # Full system status
zeroclaw doctor                   # System diagnostics
zeroclaw channel doctor           # Channel health check
zeroclaw providers                # List available providers
zeroclaw models refresh           # Refresh model catalogs
```

### Pre-Push Validation

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Enable the pre-push hook:
```bash
git config core.hooksPath .githooks
```

---

## Next Steps

- [Channels Reference](channels-reference.md) — Full config for all 15 channels
- [Providers Reference](providers-reference.md) — All 28+ providers with env vars
- [Config Reference](config-reference.md) — Every config key documented
- [Operations Runbook](operations-runbook.md) — Day-to-day operations
- [Troubleshooting](troubleshooting.md) — Common issues and solutions
- [Commands Reference](commands-reference.md) — All CLI commands
