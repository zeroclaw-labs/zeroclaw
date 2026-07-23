# Coding Agent Guidelines

The repository-root `AGENTS.md` is the compact, always-loaded contract for AI coding assistants. This page provides details that are useful for some tasks but should not consume every session's prompt budget.

These rules apply regardless of model size or where the model runs. Compact prompt profiles may change how much context is loaded eagerly; they do not weaken safety, privacy, authorization, or contribution requirements.

## How To Use This Page

Start with the [architecture and contribution map](./architecture-map.md). Its change-path table routes each task to the current architecture, foundation, testing, security, and maintainer documentation. Return here only for the agent-specific subjects below.

## Single Source Of Truth Examples

No piece of state should live in two independently maintained places. If a fact already exists in config, schema, runtime state, or a generated definition, resolve or derive it from that source instead of copying it into another field.

Before adding a struct field, channel or handle field, schema field, or config entry, state one of these answers:

1. "This is the source of truth, created here." State what it represents.
2. "The source of truth is `<path>`; this would duplicate it." Resolve it from that location at use time.

Do not defer duplicate-state cleanup to a follow-up. A restart-only snapshot is still duplicate state.

Forbidden examples:

- a channel handle caching authorized users while live config owns them;
- an enum and a separate hand-maintained variant list;
- a config snapshot cloning fields the runtime can read from live config;
- copying a provider credential into another runtime field.

Allowed examples:

- resolver closures over `Arc<RwLock<Config>>`;
- borrowed `Config` or typed config parameters;
- on-demand views that are not stored beyond the operation;
- macros or generators that emit several surfaces from one input.

## Architecture And Ownership

ZeroClaw is a Rust-first, trait-driven agent runtime. Primary extension traits live in `crates/zeroclaw-api/src/`:

- `model_provider.rs` (`ModelProvider`)
- `channel.rs` (`Channel`)
- `tool.rs` (`Tool`)
- `memory_traits.rs` (`Memory`)
- `observability_traits.rs` (`Observer`)
- `runtime_traits.rs` (`RuntimeAdapter`)
- `peripherals_traits.rs` (`Peripheral`)

Do not maintain another crate or repository inventory here. Use [Crates](../architecture/crates.md) for ownership and dependency direction, the workspace members in the root `Cargo.toml` for current membership, and the architecture map for provider, channel, tool, plugin, runtime, and config change paths.

## Stability And Risk

The stability-tier definitions and versioning policy live in [FND-001](../foundations/fnd-001-intentional-architecture.md#stability-tiers). Component-local `AGENTS.md` files and plugin registry manifests are the target ownership model. Until every component has one, the table below is the canonical source for current assignments; do not copy it into another hand-maintained aggregate.

### Current Stability Assignments

| Component | Tier | Notes |
| --- | --- | --- |
| `zeroclaw-api` | Experimental | Stable at v1.0.0 (formal milestone) |
| `zeroclaw-config` | Beta | Stable at v0.8.0 |
| `zeroclaw-log` | Beta | Unified log emission, JSONL persistence, and broadcast hook |
| `zeroclaw-providers` | Beta | |
| `zeroclaw-memory` | Beta | |
| `zeroclaw-infra` | Beta | |
| `zeroclaw-commands` | Experimental | Built-in command catalog and metadata |
| `zeroclaw-tool-call-parser` | Beta | Stable at v0.8.0 |
| `zeroclaw-channels` | Experimental | Plugin migration at v1.0.0 |
| `zeroclaw-tools` | Experimental | Plugin migration at v1.0.0 |
| `zeroclaw-runtime` | Experimental | Agent runtime: agent loop, security, cron, SOP, skills, and observability |
| `zeroclaw-gateway` | Experimental | Separate binary at v0.9.0 |
| `zerocode` | Experimental | TUI onboarding wizard |
| `zeroclaw-plugins` | Experimental | WASM plugin system and foundation for the v1.0.0 plugin ecosystem |
| `zeroclaw-hardware` | Experimental | USB discovery, peripherals, and serial support |
| `zeroclaw-macros` | Beta | Tightly coupled to the config schema |
| `zeroclaw-eval` | Experimental | Agent evaluation harness with deterministic replay of LLM trace fixtures |
| `zeroclaw-spawn` | Beta | Attribution-propagating `tokio::spawn` wrapper layered on `zeroclaw-log` |
| `robot-kit` | Experimental | Robot control toolkit: drive, vision, speech, sensors, and safety |
| `aardvark-sys` | Experimental | Low-level FFI bindings for the Total Phase Aardvark adapter; the only crate where `unsafe` is permitted |

Stable components follow the breaking-change policy. Beta components may make breaking changes in a MINOR release with changelog notes. Experimental components carry no stability guarantee. Tiers are promoted, never demoted, through deliberate team decision.

Change-risk routing is:

- **Low:** docs, chores, and tests without behavior changes.
- **Medium:** most implementation changes without boundary or security impact.
- **High:** `crates/zeroclaw-runtime/src/`, especially `src/security/`; `crates/zeroclaw-gateway/src/`; `crates/zeroclaw-tools/src/`; `.github/workflows/`; access control; and other trust-boundary changes.

Classify uncertainty upward. Validation and rollback evidence should match the actual blast radius, not only the number of changed lines. Use [How to contribute](./how-to.md) for PR mechanics and [Testing](./testing.md) for the validation taxonomy.

## Skill Discovery

Repository-owned coding-assistant skills live in `.claude/skills/`. Inspect the available `*/SKILL.md` files and load only the skill matching the requested operation. Do not maintain a second skill catalog in this page; the directory is the current inventory and each skill file owns its workflow.

## Protected Operational Documents

These files are consumed by skills or development tooling. Do not move or delete them without updating their consumers and repository guidance.

| File | Consumer |
| --- | --- |
| `docs/book/src/contributing/pr-review-protocol.md` | PR review skill |
| `.claude/skills/changelog-generation/SKILL.md` | Changelog skill loader and release runbook |
| `docs/book/src/maintainers/reviewer-playbook.md` | Issue triage skill |
| `docs/book/src/maintainers/pr-workflow.md` | Issue triage and maintainer workflow |
| `docs/book/src/contributing/privacy.md` | Issue and PR privacy gates |
| `docs/book/src/foundations/fnd-00*.md` | Review architecture references |

## Localization And Privacy

User-facing text and English-only logging rules remain in root `AGENTS.md`. The Wiki and internal developer documentation are also English-only. For the full contracts, use [Privacy and PII discipline](./privacy.md) and [Docs and translations](../maintainers/docs-and-translations.md).

## Further Reading

- [Architecture and contribution map](./architecture-map.md)
- [How to contribute](./how-to.md)
- [Privacy and PII discipline](./privacy.md)
- [Testing](./testing.md)
- [Architecture overview](../architecture/overview.md)
- [Superseding pull requests](../maintainers/superseding.md)
- [Audit policy](https://github.com/zeroclaw-labs/zeroclaw/blob/master/docs/maintainers/audit-policy.md)
