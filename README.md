<p align="center">
  <img src="assets/hrafn-logo.png" alt="Hrafn" width="200" />
</p>

<h1 align="center">hrafn</h1>

<p align="center">
  <em>Lightweight, modular AI agent runtime. Hrafn thinks. <a href="https://github.com/5queezer/muninndb">MuninnDB</a> remembers.</em>
</p>

<p align="center">
  <a href="#quickstart">Quickstart</a> · <a href="#architecture">Architecture</a> · <a href="CONTRIBUTING.md">Contributing</a> · <a href="https://github.com/5queezer/hrafn/discussions">Discussions</a>
</p>

---

## What is Hrafn?

Hrafn is an autonomous AI agent runtime written in Rust. It connects to the messaging platforms you already use (Telegram, Discord, WhatsApp, Signal, Matrix, and more), runs on hardware as small as a Raspberry Pi, and keeps your data local.

Unlike monolithic agent frameworks, Hrafn is **modular by design**. You compile only what you need. Runtime extensibility comes through MCP -- every MCP server is a plugin.

## Why Hrafn?

**Modular, not monolithic.** Channels, tools, providers, and memory backends are Cargo features. The default build is small. You opt in to what you need.

**MCP as the plugin protocol.** No custom plugin API. Any MCP server works as a Hrafn plugin, in any language. The OpenClaw Bridge lets you test OC plugins via MCP before porting them to native Rust.

**MuninnDB.** Cognitive memory with Ebbinghaus-curve decay and Hebbian association learning. The Dream Engine consolidates memories via local LLM inference (Ollama), so your data never leaves your machine.

**A2A protocol.** Native Agent-to-Agent communication. Discover, delegate, and receive tasks from other agents over HTTP using the open A2A standard.

**Community-first governance.** Every PR gets a response within 48 hours. No silent closes. Public roadmap. Weekly community calls. See [CONTRIBUTING.md](CONTRIBUTING.md) for our promises.

## Quickstart

```bash
# Install from source
git clone https://github.com/5queezer/hrafn.git
cd hrafn
cargo build --release --locked
cargo install --path . --force --locked

# Guided setup
hrafn onboard

# Or quick start
hrafn onboard --api-key "sk-..." --provider openrouter

# Start the gateway (web dashboard + webhook server)
hrafn gateway

# Chat directly
hrafn agent -m "Hello, Hrafn!"

# Interactive mode
hrafn agent

# Full autonomous runtime
hrafn daemon

# Diagnostics
hrafn status
hrafn doctor
```

### Minimal feature build

```bash
# Only Telegram + shell tool + default memory
cargo build --release --no-default-features \
  --features "channel-telegram,tool-shell"
```

## Architecture

Hrafn's architecture is **trait-based**. Every subsystem is a Rust trait. Swap implementations through configuration, not code changes.

```
src/
├── providers/    # LLM backends       → Provider trait
├── channels/     # Messaging           → Channel trait
├── tools/        # Agent capabilities  → Tool trait
├── memory/       # Persistence         → Memory trait
├── gateway/      # HTTP/WS control plane
├── agent/        # Orchestration loop
└── config/       # TOML configuration
```

### Compile-time modularity

Every channel, tool, provider, and memory backend is gated behind a Cargo feature:

```toml
[features]
default = ["channel-telegram", "tool-shell", "gateway"]

# Channels
channel-telegram = []
channel-discord = []
channel-whatsapp = []
channel-signal = []
channel-matrix = []

# Tools
tool-shell = []
tool-a2a = []
tool-browser = []

# Memory
memory-muninndb = ["dep:muninndb"]

# Infrastructure
gateway = []
```

### Runtime extensibility

Any MCP server is a plugin. Configure in `config.toml`:

```toml
[mcp]
servers = [
  { name = "my-tool", command = "npx", args = ["-y", "my-mcp-server"] },
]
```

No recompilation needed. MCP plugins can be written in any language.

## OpenClaw Bridge

The OC Bridge lets Hrafn users run OpenClaw plugins via MCP without a native Rust port. It serves as a **validation funnel**: plugins that see sustained community usage get queued for native porting.

```
OC Plugin → MCP Adapter (Node.js) → Hrafn tests it → Community validates
  → Port Queue → Native Rust implementation → Review & merge
```

See `docs/oc-bridge.md` for setup.

## Key Integrations

### MuninnDB

Cognitive memory backend with Ebbinghaus-curve decay (memories fade naturally) and Hebbian learning (co-activated memories strengthen each other). The Dream Engine runs periodic consolidation via local LLM inference.

```toml
[memory]
backend = "muninndb"

[memory.muninndb]
db_path = "~/.hrafn/memory"
consolidation = true
ollama_model = "llama3.2"
```

### A2A Protocol

Native Agent-to-Agent communication per the open [A2A standard](https://github.com/a2aproject/A2A).

```toml
[a2a]
enabled = true
bind = "127.0.0.1:18800"    # localhost-only by default
bearer_token = "your-secret"
```

Inbound tasks route through the existing agent pipeline. The agent card is auto-generated from your configuration.

## Roadmap

See the [GitHub Projects board](https://github.com/5queezer/hrafn/projects) for current status.

## Contributing

We believe open-source communities deserve transparent governance and respect for contributors' work. Read [CONTRIBUTING.md](CONTRIBUTING.md) for our promises and workflow.

The short version:
- Every PR gets a response within 48 hours.
- No silent closes. Rejections come with explanations.
- Your code stays your code. Maintainers never re-submit contributor work under their own name.

## Community

- [GitHub Discussions](https://github.com/5queezer/hrafn/discussions) -- questions, RFCs, show & tell
- Weekly community calls (schedule in Discussions)

## Origin

Hrafn originated as a fork of [ZeroClaw](https://github.com/zeroclaw-labs/zeroclaw) (Apache-2.0). We thank the ZeroClaw contributors for the foundation.

## License

Apache-2.0. See [LICENSE](LICENSE). You retain copyright of your contributions.
