<p align="center">
  <img src="zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
</p>

The fastest, smallest, fully autonomous AI assistant — deploy anywhere, swap anything.

```
~3.4MB binary · <10ms startup · 1,017 tests · 22+ providers · 8 traits · Pluggable everything
```

## Benchmark Snapshot (ZeroClaw vs OpenClaw)

Local machine quick benchmark (macOS arm64, Feb 2026), same host, 3 runs each.

| Metric | ZeroClaw (Rust release binary) | OpenClaw (Node + built `dist`) |
|---|---:|---:|
| Build output size | `target/release/zeroclaw`: **3.4 MB** | `dist/`: **28 MB** |
| `--help` startup (cold/warm) | **0.38s / ~0.00s** | **3.31s / ~1.11s** |
| `status` command runtime (best of 3) | **~0.00s** | **5.98s** |
| `--help` max RSS observed | **~7.3 MB** | **~394 MB** |
| `status` max RSS observed | **~7.8 MB** | **~1.52 GB** |

> Notes: measured with `/usr/bin/time -l`; first run includes cold-start effects. OpenClaw results include `pnpm install` + `pnpm build` before execution.

## Quick Start

```bash
git clone https://github.com/theonlyhennygod/zeroclaw.git
cd zeroclaw
cargo build --release

# Quick setup (no prompts)
cargo run --release -- onboard --api-key sk-... --provider openrouter

# Or interactive wizard
cargo run --release -- onboard --interactive

# Chat
cargo run --release -- agent -m "Hello, ZeroClaw!"

# Interactive mode
cargo run --release -- agent

# Start the gateway (webhook server)
cargo run --release -- gateway                # default: 127.0.0.1:8080
cargo run --release -- gateway --port 0       # random port (security hardened)

# Check status
cargo run --release -- status

# Check channel health
cargo run --release -- channel doctor

# Get integration setup details
cargo run --release -- integrations info Telegram
```

> **Tip:** Run `cargo install --path .` to install `zeroclaw` globally, then use `zeroclaw` instead of `cargo run --release --`.

## Architecture

Every subsystem is a **trait** — swap implementations with a config change, zero code changes.

<p align="center">
  <img src="docs/architecture.svg" alt="ZeroClaw Architecture" width="900" />
</p>

| Subsystem | Trait | Ships with | Extend |
|-----------|-------|------------|--------|
| **AI Models** | `Provider` | 22+ providers (OpenRouter, Anthropic, OpenAI, Ollama, Venice, Groq, Mistral, xAI, DeepSeek, Together, Fireworks, Perplexity, Cohere, Bedrock, etc.) | `custom:https://your-api.com` — any OpenAI-compatible API |
| **Channels** | `Channel` | CLI, Telegram, Discord, Slack, iMessage, Matrix, Webhook | Any messaging API |
| **Memory** | `Memory` | SQLite with hybrid search (FTS5 + vector cosine similarity), Markdown | Any persistence backend |
| **Tools** | `Tool` | shell, file_read, file_write, memory_store, memory_recall, memory_forget, browser_open (Brave + allowlist), composio (optional) | Any capability |
| **Observability** | `Observer` | Noop, Log, Multi | Prometheus, OTel |
| **Runtime** | `RuntimeAdapter` | Native (Mac/Linux/Pi) | Docker, WASM |
| **Security** | `SecurityPolicy` | Gateway pairing, sandbox, allowlists, rate limits, filesystem scoping, encrypted secrets | — |
| **Tunnel** | `Tunnel` | None, Cloudflare, Tailscale, ngrok, Custom | Any tunnel binary |
| **Heartbeat** | Engine | HEARTBEAT.md periodic tasks | — |
| **Skills** | Loader | TOML manifests + SKILL.md instructions | Community skill packs |
| **Integrations** | Registry | 50+ integrations across 9 categories | Plugin system |

### Memory System (Full-Stack Search Engine)

All custom, zero external dependencies — no Pinecone, no Elasticsearch, no LangChain:

| Layer | Implementation |
|-------|---------------|
| **Vector DB** | Embeddings stored as BLOB in SQLite, cosine similarity search |
| **Keyword Search** | FTS5 virtual tables with BM25 scoring |
| **Hybrid Merge** | Custom weighted merge function (`vector.rs`) |
| **Embeddings** | `EmbeddingProvider` trait — OpenAI, custom URL, or noop |
| **Chunking** | Line-based markdown chunker with heading preservation |
| **Caching** | SQLite `embedding_cache` table with LRU eviction |
| **Safe Reindex** | Rebuild FTS5 + re-embed missing vectors atomically |

The agent automatically recalls, saves, and manages memory via tools.

```toml
[memory]
backend = "sqlite"          # "sqlite", "markdown", "none"
auto_save = true
embedding_provider = "openai"
vector_weight = 0.7
keyword_weight = 0.3
```

## Security

ZeroClaw enforces security at **every layer** — not just the sandbox. It passes all items from the community security checklist.

### Security Checklist

| # | Item | Status | How |
|---|------|--------|-----|
| 1 | **Gateway not publicly exposed** | ✅ | Binds `127.0.0.1` by default. Refuses `0.0.0.0` without tunnel or explicit `allow_public_bind = true`. |
| 2 | **Pairing required** | ✅ | 6-digit one-time code on startup. Exchange via `POST /pair` for bearer token. All `/webhook` requests require `Authorization: Bearer <token>`. |
| 3 | **Filesystem scoped (no /)** | ✅ | `workspace_only = true` by default. 14 system dirs + 4 sensitive dotfiles blocked. Null byte injection blocked. Symlink escape detection via canonicalization. |
| 4 | **Access via tunnel only** | ✅ | Gateway refuses public bind without active tunnel. Supports Tailscale, Cloudflare, ngrok, or any custom tunnel. |

> **Run your own nmap:** `nmap -p 1-65535 <your-host>` — ZeroClaw binds to localhost only, so nothing is exposed unless you explicitly configure a tunnel.

### Channel allowlists (Telegram / Discord / Slack)

Inbound sender policy is now consistent:

- Empty allowlist = **deny all inbound messages**
- `"*"` = **allow all** (explicit opt-in)
- Otherwise = exact-match allowlist

This keeps accidental exposure low by default.

## Configuration

Config: `~/.zeroclaw/config.toml` (created by `onboard`)

```toml
api_key = "sk-..."
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7

[memory]
backend = "sqlite"              # "sqlite", "markdown", "none"
auto_save = true
embedding_provider = "openai"   # "openai", "noop"
vector_weight = 0.7
keyword_weight = 0.3

[gateway]
require_pairing = true          # require pairing code on first connect
allow_public_bind = false       # refuse 0.0.0.0 without tunnel

[autonomy]
level = "supervised"            # "readonly", "supervised", "full" (default: supervised)
workspace_only = true           # default: true — scoped to workspace
allowed_commands = ["git", "npm", "cargo", "ls", "cat", "grep"]
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]

[heartbeat]
enabled = false
interval_minutes = 30

[tunnel]
provider = "none"               # "none", "cloudflare", "tailscale", "ngrok", "custom"

[secrets]
encrypt = true                  # API keys encrypted with local key file

[browser]
enabled = false                 # opt-in browser_open tool
allowed_domains = ["docs.rs"]  # required when browser is enabled

[composio]
enabled = false                 # opt-in: 1000+ OAuth apps via composio.dev
```

## Gateway API

| Endpoint | Method | Auth | Description |
|----------|--------|------|-------------|
| `/health` | GET | None | Health check (always public, no secrets leaked) |
| `/pair` | POST | `X-Pairing-Code` header | Exchange one-time code for bearer token |
| `/webhook` | POST | `Authorization: Bearer <token>` | Send message: `{"message": "your prompt"}` |

## Commands

| Command | Description |
|---------|-------------|
| `onboard` | Quick setup (default) |
| `onboard --interactive` | Full interactive 7-step wizard |
| `agent -m "..."` | Single message mode |
| `agent` | Interactive chat mode |
| `gateway` | Start webhook server (default: `127.0.0.1:8080`) |
| `gateway --port 0` | Random port mode |
| `status` | Show full system status |
| `channel doctor` | Run health checks for configured channels |
| `integrations info <name>` | Show setup/status details for one integration |

## Development

```bash
cargo build              # Dev build
cargo build --release    # Release build (~3.4MB)
cargo test               # 1,017 tests
cargo clippy             # Lint (0 warnings)
cargo fmt                # Format

# Run the SQLite vs Markdown benchmark
cargo test --test memory_comparison -- --nocapture
```

### Pre-push hook

A git hook runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` before every push. Enable it once:

```bash
git config core.hooksPath .githooks
```

To skip the hook when you need a quick push during development:

```bash
git push --no-verify
```

## License

MIT — see [LICENSE](LICENSE)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Implement a trait, submit a PR:
- New `Provider` → `src/providers/`
- New `Channel` → `src/channels/`
- New `Observer` → `src/observability/`
- New `Tool` → `src/tools/`
- New `Memory` → `src/memory/`
- New `Tunnel` → `src/tunnel/`
- New `Skill` → `~/.zeroclaw/workspace/skills/<name>/`

---

**ZeroClaw** — Zero overhead. Zero compromise. Deploy anywhere. Swap anything. 🦀
