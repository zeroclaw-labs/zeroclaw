# AGENTS.md — QuantClaw

Cross-tool agent instructions for any AI coding assistant working on this repository.

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Full pre-PR validation (recommended):

```bash
./dev/ci.sh all
```

Docs-only changes: run markdown lint and link-integrity checks. If touching bootstrap scripts: `bash -n install.sh`.

## Project Snapshot

QuantClaw is a Rust-first autonomous agent runtime optimized for performance, efficiency, stability, extensibility, sustainability, and security.

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points:

- `crates/quantclaw-api/src/provider.rs` (`Provider`)
- `crates/quantclaw-api/src/channel.rs` (`Channel`)
- `crates/quantclaw-api/src/tool.rs` (`Tool`)
- `crates/quantclaw-api/src/memory_traits.rs` (`Memory`)
- `crates/quantclaw-api/src/observability_traits.rs` (`Observer`)
- `crates/quantclaw-api/src/runtime_traits.rs` (`RuntimeAdapter`)
- `crates/quantclaw-api/src/peripherals_traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## Stability Tiers

Every workspace crate carries a stability tier per the Microkernel Architecture RFC.

| Crate | Tier | Notes |
|-------|------|-------|
| `quantclaw-api` | Experimental | Stable at v1.0.0 (formal milestone) |
| `quantclaw-config` | Beta | Stable at v0.8.0 |
| `quantclaw-providers` | Beta | — |
| `quantclaw-memory` | Beta | — |
| `quantclaw-infra` | Beta | — |
| `quantclaw-tool-call-parser` | Beta | Stable at v0.8.0 |
| `quantclaw-channels` | Experimental | Plugin migration at v1.0.0 |
| `quantclaw-tools` | Experimental | Plugin migration at v1.0.0 |
| `quantclaw-runtime` | Experimental | Agent runtime (agent loop, security, cron, SOP, skills, observability) |
| `quantclaw-gateway` | Experimental | Separate binary at v0.9.0 |
| `quantclaw-tui` | Experimental | TUI onboarding wizard |
| `quantclaw-plugins` | Experimental | WASM plugin system — foundation for v1.0.0 plugin ecosystem |
| `quantclaw-hardware` | Experimental | USB discovery, peripherals, serial |
| `quantclaw-macros` | Beta | Tightly coupled to config schema |

**Tiers**: Stable = covered by breaking-change policy. Beta = breaking changes permitted in MINOR with changelog notes. Experimental = no stability guarantee.

Tiers are promoted, never demoted, through deliberate team decision.

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module re-exports and CLI command enum definitions
- `crates/quantclaw-api/` — public trait definitions (Provider, Channel, Tool, Memory, Observer, Peripheral)
- `crates/quantclaw-config/` — schema, config loading/merging
- `crates/quantclaw-macros/` — Configurable derive macro
- `crates/quantclaw-providers/` — model providers and resilient wrapper
- `crates/quantclaw-channels/` — messaging platform integrations (30+ channels)
- `crates/quantclaw-channels/src/orchestrator/` — channel lifecycle, routing, media pipeline
- `crates/quantclaw-tools/` — tool execution surface (shell, file, memory, browser)
- `crates/quantclaw-runtime/` — agent loop, security, cron, SOP, skills, onboarding wizard, observability
- `crates/quantclaw-memory/` — memory backends (markdown, sqlite, embeddings, vector merge)
- `crates/quantclaw-infra/` — shared infrastructure (debounce, session, stall watchdog)
- `crates/quantclaw-gateway/` — webhook/gateway server (separate binary)
- `crates/quantclaw-hardware/` — USB discovery, peripherals, serial, GPIO
- `crates/quantclaw-tui/` — TUI onboarding wizard
- `crates/quantclaw-plugins/` — WASM plugin system
- `crates/quantclaw-tool-call-parser/` — tool call parsing
- `docs/` — topic-based documentation (setup-guides, reference, ops, security, hardware, contributing, maintainers)
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `crates/*/src/**` behavior changes without boundary/security impact
- **High risk**: `crates/quantclaw-runtime/src/**` (especially `src/security/`), `crates/quantclaw-gateway/src/**`, `crates/quantclaw-tools/src/**`, `.github/workflows/**`, access-control boundaries

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

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
