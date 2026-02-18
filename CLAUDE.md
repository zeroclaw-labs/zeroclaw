# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ZeroClaw is a lightweight autonomous AI assistant infrastructure written entirely in Rust. It is optimized for high performance, efficiency, and security with a trait-based pluggable architecture.

Key stats: ~3.4MB binary, <5MB RAM, <10ms startup, 1,017 tests.

## Common Commands

### Build
```bash
cargo build --release          # Optimized release build (~3.4MB)
CARGO_BUILD_JOBS=1 cargo build --release    # Low-memory fallback (Raspberry Pi 3, 1GB RAM)
cargo build --release --locked # Build with locked dependencies (fixes OpenSSL errors)
```

### Test
```bash
cargo test                     # Run all 1,017 tests
cargo test telegram --lib      # Test specific module
cargo test security            # Test security module
```

### Format & Lint
```bash
cargo fmt --all -- --check     # Check formatting
cargo fmt                       # Apply formatting
cargo clippy --all-targets -- -D clippy::correctness  # Baseline (required for CI)
cargo clippy --all-targets -- -D warnings             # Strict (optional)
```

### CI / Pre-push
```bash
./dev/ci.sh all                # Full CI in Docker
git config core.hooksPath .githooks  # Enable pre-push hook
git push --no-verify           # Skip hook if needed
```

## Architecture

ZeroClaw uses a **trait-based pluggable architecture** where every subsystem is swappable via traits and factory functions.

### Core Extension Points

| Trait | Purpose | Location |
|-------|---------|----------|
| `Provider` | LLM backends (22+ providers) | `src/providers/traits.rs` |
| `Channel` | Messaging platforms | `src/channels/traits.rs` |
| `Tool` | Agent capabilities | `src/tools/traits.rs` |
| `Memory` | Persistence/backends | `src/memory/traits.rs` |
| `Observer` | Metrics/logging | `src/observability/traits.rs` |
| `RuntimeAdapter` | Platform abstraction | `src/runtime/traits.rs` |
| `Peripheral` | Hardware boards | `src/peripherals/traits.rs` |

### Key Directory Structure

```
src/
├── main.rs          # CLI entrypoint, command routing
├── lib.rs           # Module exports, command enums
├── agent/           # Orchestration loop
├── channels/        # Telegram, Discord, Slack, WhatsApp, etc.
├── providers/       # OpenRouter, Anthropic, OpenAI, Ollama, etc.
├── tools/           # shell, file_read, file_write, memory, browser
├── memory/          # SQLite, Markdown, Lucid, None backends
├── gateway/         # Webhook/gateway server (Axum HTTP)
├── security/        # Policy, pairing, secret store
├── runtime/         # Native, Docker runtime adapters
├── peripherals/     # STM32, RPi GPIO hardware support
├── observability/   # Noop, Log, Multi, OTel observers
├── tunnel/          # Cloudflare, Tailscale, ngrok, custom
├── config/          # Schema + config loading/merging
└── identity/        # AIEOS/OpenClaw identity formats
```

## Memory System

ZeroClaw includes a full-stack search engine with zero external dependencies (no Pinecone, Elasticsearch, or LangChain):

- **Vector DB**: Embeddings stored as BLOB in SQLite, cosine similarity search
- **Keyword Search**: FTS5 virtual tables with BM25 scoring
- **Hybrid Merge**: Custom weighted merge function
- **Embeddings**: `EmbeddingProvider` trait — OpenAI, custom URL, or noop
- **Chunking**: Line-based markdown chunker with heading preservation

## Security Principles

ZeroClaw enforces security at every layer. Key patterns:

- **Gateway pairing**: 6-digit one-time code required for webhook access
- **Workspace-only execution**: Default sandbox scopes file operations
- **Path traversal blocking**: 14 system dirs + 4 sensitive dotfiles blocked
- **Command allowlisting**: No blocklists — only explicit allowlists
- **Secret encryption**: ChaCha20-Poly1305 AEAD for encrypted secrets
- **No logging of secrets**: Never log tokens, keys, or sensitive payloads

Critical security paths: `src/security/`, `src/runtime/`, `src/gateway/`, `src/tools/`, `.github/workflows/`

## Code Naming Conventions

- **Rust standard**: modules/files `snake_case`, types/traits `PascalCase`, functions/variables `snake_case`, constants `SCREAMING_SNAKE_CASE`
- **Domain-first naming**: `DiscordChannel`, `SecurityPolicy`, `SqliteMemory`
- **Trait implementers**: `*Provider`, `*Channel`, `*Tool`, `*Memory`, `*Observer`, `*RuntimeAdapter`
- **Factory keys**: lowercase, stable (`"openai"`, `"discord"`, `"shell"`)
- **Tests**: behavior-oriented (`allowlist_denies_unknown_user`)
- **Identity-like labels**: Use ZeroClaw-native only (`ZeroClawAgent`, `zeroclaw_user`) — never real names/personal data

## Architecture Boundary Rules

- Extend via trait implementations + factory registration first
- Keep dependency direction inward: concrete implementations depend on traits/config/util
- Avoid cross-subsystem coupling (e.g., provider importing channel internals)
- Keep modules single-purpose
- Treat config keys as public contract — document migrations

## Engineering Principles

From `AGENTS.md` — these are mandatory implementation constraints:

1. **KISS** — Prefer straightforward control flow over clever meta-programming
2. **YAGNI** — Don't add features/config/flags without a concrete use case
3. **DRY + Rule of Three** — Extract shared utilities only after repeated stable patterns
4. **SRP + ISP** — Keep modules focused; extend via narrow traits
5. **Fail Fast + Explicit Errors** — Prefer explicit `bail!`/errors; never silently broaden permissions
6. **Secure by Default + Least Privilege** — Deny-by-default for access boundaries
7. **Determinism + Reproducibility** — Prefer reproducible commands and locked dependencies
8. **Reversibility + Rollback-First** — Keep changes easy to revert

## Adding New Components

### New Provider
1. Create `src/providers/your_provider.rs`
2. Implement `Provider` trait
3. Register factory in `src/providers/mod.rs`

### New Channel
1. Create `src/channels/your_channel.rs`
2. Implement `Channel` trait
3. Register in `src/channels/mod.rs`

### New Tool
1. Create `src/tools/your_tool.rs`
2. Implement `Tool` trait with strict parameter schema
3. Register in `src/tools/mod.rs`

### New Peripheral
1. Create in `src/peripherals/`
2. Implement `Peripheral` trait (exposes `tools()` method)
3. See `docs/hardware-peripherals-design.md` for protocol

## Validation Matrix

Default local checks for code changes:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D clippy::correctness
cargo test
```

For Docker CI parity (recommended when available):

```bash
./dev/ci.sh all
```

## Risk Tiers by Path

- **Low risk**: docs/chore/tests-only
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, access-control boundaries

## Important Documentation

- `AGENTS.md` — Agent engineering protocol (primary guide for AI contributors)
- `CONTRIBUTING.md` — Contribution guide and architecture rules
- `docs/pr-workflow.md` — PR workflow and governance
- `docs/reviewer-playbook.md` — Reviewer operating checklist
- `docs/ci-map.md` — CI ownership and triage
- `docs/hardware-peripherals-design.md` — Hardware peripherals protocol

## Pre-push Hook

The repo includes a pre-push hook that runs `fmt`, `clippy`, and `tests` before every push. Enable once with:

```bash
git config core.hooksPath .githooks
```

Skip with `git push --no-verify` during rapid iteration (CI will catch issues).
