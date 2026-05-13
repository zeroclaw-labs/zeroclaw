# AGENTS.md — DaemonClaw

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

DaemonClaw is a Rust-first autonomous agent runtime optimized for performance, efficiency, stability, extensibility, sustainability, and security.

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points:

- `crates/daemonclaw-api/src/provider.rs` (`Provider`)
- `crates/daemonclaw-api/src/channel.rs` (`Channel`)
- `crates/daemonclaw-api/src/tool.rs` (`Tool`)
- `crates/daemonclaw-api/src/memory_traits.rs` (`Memory`)
- `crates/daemonclaw-api/src/observability_traits.rs` (`Observer`)
- `crates/daemonclaw-api/src/runtime_traits.rs` (`RuntimeAdapter`)
- `crates/daemonclaw-api/src/peripherals_traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## Stability Tiers

Every workspace crate carries a stability tier per the Microkernel Architecture RFC.

| Crate | Tier | Notes |
|-------|------|-------|
| `daemonclaw-api` | Experimental | Stable at v1.0.0 (formal milestone) |
| `daemonclaw-config` | Beta | Stable at v0.8.0 |
| `daemonclaw-providers` | Beta | — |
| `daemonclaw-memory` | Beta | — |
| `daemonclaw-infra` | Beta | — |
| `daemonclaw-tool-call-parser` | Beta | Stable at v0.8.0 |
| `daemonclaw-channels` | Experimental | Plugin migration at v1.0.0 |
| `daemonclaw-tools` | Experimental | Plugin migration at v1.0.0 |
| `daemonclaw-runtime` | Experimental | Agent runtime (agent loop, security, cron, SOP, skills, observability) |
| `daemonclaw-gateway` | Experimental | Separate binary at v0.9.0 |
| `daemonclaw-tui` | Experimental | TUI onboarding wizard |
| `daemonclaw-plugins` | Experimental | WASM plugin system — foundation for v1.0.0 plugin ecosystem |
| `daemonclaw-hardware` | Experimental | USB discovery, peripherals, serial |
| `daemonclaw-macros` | Beta | Tightly coupled to config schema |

**Tiers**: Stable = covered by breaking-change policy. Beta = breaking changes permitted in MINOR with changelog notes. Experimental = no stability guarantee.

Tiers are promoted, never demoted, through deliberate team decision.

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module re-exports and CLI command enum definitions
- `crates/daemonclaw-api/` — public trait definitions (Provider, Channel, Tool, Memory, Observer, Peripheral)
- `crates/daemonclaw-config/` — schema, config loading/merging
- `crates/daemonclaw-macros/` — Configurable derive macro
- `crates/daemonclaw-providers/` — model providers and resilient wrapper
- `crates/daemonclaw-channels/` — messaging platform integrations (30+ channels)
- `crates/daemonclaw-channels/src/orchestrator/` — channel lifecycle, routing, media pipeline
- `crates/daemonclaw-tools/` — tool execution surface (shell, file, memory, browser)
- `crates/daemonclaw-runtime/` — agent loop, security, cron, SOP, skills, onboarding wizard, observability
- `crates/daemonclaw-memory/` — memory backends (markdown, sqlite, embeddings, vector merge)
- `crates/daemonclaw-infra/` — shared infrastructure (debounce, session, stall watchdog)
- `crates/daemonclaw-gateway/` — webhook/gateway server (separate binary)
- `crates/daemonclaw-hardware/` — USB discovery, peripherals, serial, GPIO
- `crates/daemonclaw-tui/` — TUI onboarding wizard
- `crates/daemonclaw-plugins/` — WASM plugin system
- `crates/daemonclaw-tool-call-parser/` — tool call parsing
- `docs/` — topic-based documentation (setup-guides, reference, ops, security, hardware, contributing, maintainers)
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `crates/*/src/**` behavior changes without boundary/security impact
- **High risk**: `crates/daemonclaw-runtime/src/**` (especially `src/security/`), `crates/daemonclaw-gateway/src/**`, `crates/daemonclaw-tools/src/**`, `.github/workflows/**`, access-control boundaries

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

## Skills

AI coding assistant skills live in `.claude/skills/`. Use the right one for the job:

- `.claude/skills/github-pr-review-session/SKILL.md` — PR review co-pilot; assists **you** as the human reviewer. Posts reviews as WareWolf-MoonWall using the RFC feedback taxonomy (🔴/🟡/✅/🔵/🟢). Trigger: `review 1234`, `re-review 1234`, `go through the queue`.
- `.claude/skills/changelog-generation/SKILL.md` — generates `CHANGELOG-next.md` between stable tags, resolves contributors via GraphQL, feeds the release workflow. Trigger: `generate changelog`, `release notes for v0.7.x`.

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
