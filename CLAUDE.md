# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Feature-gated builds (WhatsApp Web requires its feature flag):

```bash
cargo clippy --features whatsapp-web --all-targets -- -D warnings
cargo test --features whatsapp-web
cargo build --release --features whatsapp-web
```

Cross-compile for Linux (Synology/NAS deployment):

```bash
CC_x86_64_unknown_linux_musl=x86_64-linux-musl-gcc \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
cargo build --release --target x86_64-unknown-linux-musl --features whatsapp-web
```

Full pre-PR validation (recommended):

```bash
./dev/ci.sh all
```

Docs-only changes: run markdown lint and link-integrity checks. If touching bootstrap scripts: `bash -n install.sh`.

## Project Snapshot

ZeroClaw is a Rust-first autonomous agent runtime optimized for performance, efficiency, stability, extensibility, sustainability, and security.

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points:

- `src/providers/traits.rs` (`Provider`)
- `src/channels/traits.rs` (`Channel`)
- `src/tools/traits.rs` (`Tool`)
- `src/memory/traits.rs` (`Memory`)
- `src/observability/traits.rs` (`Observer`)
- `src/runtime/traits.rs` (`RuntimeAdapter`)
- `src/peripherals/traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module exports and shared command enums
- `src/config/` — schema + config loading/merging
- `src/agent/` — orchestration loop
- `src/gateway/` — webhook/gateway server
- `src/security/` — policy, pairing, secret store
- `src/memory/` — markdown/sqlite memory backends + embeddings/vector merge
- `src/providers/` — model providers and resilient wrapper
- `src/channels/` — Telegram/Discord/Slack/etc channels
- `src/tools/` — tool execution surface (shell, file, memory, browser, MCP client)
- `src/tools/mcp_client.rs` — MCP server lifecycle, stdio/HTTP/SSE transports
- `src/tools/mcp_tool.rs` — MCP tool wrapper implementing `Tool` trait
- `src/tools/mcp_deferred.rs` — deferred (lazy) MCP tool loading via `tool_search`
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO)
- `src/runtime/` — runtime adapters (currently native)
- `src/multimodal.rs` — image marker parsing, base64 encoding, provider vision checks
- `docs/` — topic-based documentation (setup-guides, reference, ops, security, hardware, contributing, maintainers)
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

## Workflow

1. **Read before write** — inspect existing module, factory wiring, and adjacent tests before editing.
2. **One concern per PR** — avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch** — no speculative abstractions, no config keys without a concrete use case.
4. **Validate by risk tier** — docs-only: lightweight checks. Code changes: full relevant checks.
5. **Document impact** — update PR notes for behavior, risk, side effects, and rollback.
6. **Queue hygiene** — stacked PR: declare `Depends on #...`. Replacing old PR: declare `Supersedes #...`.

Branch/commit/PR rules:
- Work from a non-`master` branch. Open a PR to `master`; do not push directly.
- Use conventional commit titles. Prefer small PRs (`size: XS/S/M`).
- Follow `.github/pull_request_template.md` fully.
- Never commit secrets, personal data, or real identity information (see `@docs/contributing/pr-discipline.md`).

## Anti-Patterns

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags "just in case".
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.
- Do not hide behavior-changing side effects in refactor commits.
- Do not include personal identity or sensitive information in test data, examples, docs, or commits.

## Architecture Notes

### Provider Resolution

Providers are created in `src/providers/mod.rs` via `create_provider_with_url_and_options()`. The `ReliableProvider` wrapper (in `reliable.rs`) adds retry, fallback chain, and API key rotation. Provider names map to constructors — `custom:` prefix creates a generic OpenAI-compatible provider with `supports_vision: true`. Named providers (e.g. `zai`) use specific constructors that set correct capabilities.

### MCP Tool Pipeline

MCP servers are configured in `[mcp]` config section. With `deferred_loading = true` (default), only tool names are loaded; full schemas are fetched on-demand via `tool_search`. With `deferred_loading = false`, all tools are eagerly loaded at startup. The vision fallback in `src/agent/loop_.rs` accesses MCP tools from the registry directly, bypassing LLM tool specs.

### WhatsApp Web Message Flow

`src/channels/whatsapp_web.rs` uses `wa_rs` crate for WhatsApp Web protocol. Messages are wrapped in ephemeral/device-sent layers — use `msg.get_base_message()` to unwrap. Bot identity for mention detection uses `client.get_pn()` (Phone Number JID) and `client.get_lid()` (LID JID), not config values. JID formats: `user@s.whatsapp.net` (DM), `id@g.us` (group).

### i18n Docs

Config and channel reference docs are maintained in 3 locales: EN (`docs/reference/`), zh-CN (`docs/i18n/zh-CN/reference/`), VI (`docs/vi/`). All three must be updated together when adding config fields or channel features.

### Pre-existing Clippy Warnings

`src/channels/whatsapp_storage.rs` has pre-existing clippy errors (wildcard import, `as_ref.map`). `src/channels/whatsapp_web.rs` has a pre-existing `large_futures` warning. These are not from current work.

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
