# ZeroClaw — Repository Summary

## Overview

**ZeroClaw** is a **Rust-first autonomous agent runtime framework** — a lean, secure, and fully-swappable infrastructure layer for building and running AI agentic workflows anywhere.

> *"Zero overhead. Zero compromise. 100% Rust. 100% Agnostic."*

Runs in **<5MB RAM** on release builds (~99% less memory than comparable TypeScript-based runtimes) and starts in **<10ms** even on low-power hardware.

Official repository: [zeroclaw-labs/zeroclaw](https://github.com/zeroclaw-labs/zeroclaw)
Official website: [zeroclawlabs.ai](https://zeroclawlabs.ai)

---

## Core Philosophy

- **Zero overhead** — single-binary Rust runtime with near-instant cold starts
- **Zero compromise** — secure-by-default, no silent permission broadening
- **100% Agnostic** — swap providers, channels, tools, memory, and runtimes freely
- **Deploy anywhere** — ARM, x86, RISC-V, microcontrollers, cloud, edge

---

## Key Features

| Feature | Description |
|---|---|
| **Lean Runtime** | <5MB RAM on release builds; common CLI workflows run in a few-megabyte memory envelope |
| **Fast Cold Starts** | <10ms startup on a single-binary Rust runtime |
| **Cost-Efficient** | Designed for low-cost boards and small cloud instances |
| **Portable** | Single binary-first workflow across ARM, x86, and RISC-V |
| **Research-Phase Agent Loop** | Proactively gathers facts via tools before generating responses, reducing hallucinations |
| **Secure by Default** | Pairing, strict sandboxing, explicit allowlists, deny-by-default access |
| **No Lock-in** | OpenAI-compatible provider support + pluggable custom endpoints |
| **Hardware Support** | Peripherals layer for STM32, Raspberry Pi GPIO, and other boards |
| **Migration Support** | One-command migration from OpenClaw |

---

## Architecture

ZeroClaw uses a **trait-driven, modular architecture** where all core systems are swappable via traits registered in factory modules.

### Source Layout (`src/`)

| Module | Purpose |
|---|---|
| `main.rs` | CLI entrypoint and command routing |
| `lib.rs` | Module exports and shared command enums |
| `agent/` | Orchestration loop |
| `providers/` | LLM model providers (OpenAI-compatible + custom) |
| `channels/` | Messaging channels: Telegram, Discord, Slack, etc. |
| `tools/` | Tool execution surface: shell, file, memory, browser |
| `memory/` | Markdown/SQLite memory backends + embeddings/vector merge |
| `gateway/` | Webhook/API gateway server |
| `security/` | Policy enforcement, pairing, secret store |
| `runtime/` | Runtime adapters (currently native) |
| `peripherals/` | Hardware peripheral integrations (STM32, RPi GPIO) |
| `config/` | Schema + config loading and merging |
| `rag/` | Retrieval-augmented generation support |
| `skills/` / `skillforge/` | Skill composition and marketplace compatibility |
| `observability/` | Tracing and metrics |
| `identity.rs` | Identity management |
| `migration.rs` | OpenClaw migration engine |
| `multimodal.rs` | Multimodal input/output support |

### Key Extension Points (Traits)

- `src/providers/traits.rs` — `Provider`
- `src/channels/traits.rs` — `Channel`
- `src/tools/traits.rs` — `Tool`
- `src/memory/traits.rs` — `Memory`
- `src/observability/traits.rs` — `Observer`
- `src/runtime/traits.rs` — `RuntimeAdapter`
- `src/peripherals/traits.rs` — `Peripheral`

---

## Performance Benchmark

Local benchmark (macOS arm64, Feb 2026) normalized for 0.8GHz edge hardware:

|  | OpenClaw | NanoBot | PicoClaw | **ZeroClaw** |
|---|---|---|---|---|
| **Language** | TypeScript | Python | Go | **Rust** |
| **RAM** | >1GB | >100MB | <10MB | **<5MB** |
| **Startup (0.8GHz)** | >500s | >30s | <1s | **<10ms** |
| **Binary Size** | ~28MB | N/A | ~8MB | **~8.8MB** |
| **Cost** | Mac Mini $599 | Linux SBC ~$50 | Linux Board $10 | **Any hardware** |

---

## Installation

```bash
# Option 0: One-line installer
curl -fsSL https://zeroclawlabs.ai/install.sh | bash

# Option 1: Homebrew (macOS/Linuxbrew)
brew install zeroclaw

# Option 2: Clone + Bootstrap
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh

# Option 3: Cargo
cargo install zeroclaw
```

### First Run

```bash
# Start the gateway (Web Dashboard API/UI)
zeroclaw gateway

# Or chat directly
zeroclaw chat "Hello!"
```

### Migrate from OpenClaw

```bash
zeroclaw migrate openclaw
zeroclaw migrate openclaw --dry-run
```

---

## Documentation

| Resource | Path |
|---|---|
| Docs Hub | [`docs/README.md`](docs/README.md) |
| Unified TOC | [`docs/SUMMARY.md`](docs/SUMMARY.md) |
| Commands Reference | [`docs/commands-reference.md`](docs/commands-reference.md) |
| Config Reference | [`docs/config-reference.md`](docs/config-reference.md) |
| Providers Reference | [`docs/providers-reference.md`](docs/providers-reference.md) |
| Channels Reference | [`docs/channels-reference.md`](docs/channels-reference.md) |
| Operations Runbook | [`docs/operations-runbook.md`](docs/operations-runbook.md) |
| Troubleshooting | [`docs/troubleshooting.md`](docs/troubleshooting.md) |
| Hardware Peripherals | [`docs/hardware/README.md`](docs/hardware/README.md) |
| Security | [`docs/security/README.md`](docs/security/README.md) |
| Contributing | [`CONTRIBUTING.md`](CONTRIBUTING.md) |

Localized docs available for: `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`, `es`, `pt`, `it`.

---

## License

ZeroClaw is dual-licensed for maximum openness and contributor protection:

| License | Use Case |
|---|---|
| [MIT](LICENSE-MIT) | Open-source, research, academic, personal use |
| [Apache 2.0](LICENSE-APACHE) | Patent protection, institutional, commercial deployment |

Contributors automatically grant rights under both licenses. See [CLA.md](CLA.md).

---

## Community

- Website: [zeroclawlabs.ai](https://zeroclawlabs.ai)
- X: [@zeroclawlabs](https://x.com/zeroclawlabs)
- Telegram: [@zeroclawlabs](https://t.me/zeroclawlabs)
- Reddit: [r/zeroclawlabs](https://www.reddit.com/r/zeroclawlabs/)
- Facebook: [Group](https://www.facebook.com/groups/zeroclaw)

> Built by students and members of the Harvard, MIT, and Sundai.Club communities.
