# AGENTS.md — ZeroClaw

Cross-tool agent instructions for any AI coding assistant working on this repository.

## ABSOLUTE RULE — SINGLE SOURCE OF TRUTH (NO DRY VIOLATIONS)

**No piece of state lives in two places. Ever. Anywhere in this codebase.**

This is not a guideline. It is not a preference. It is not deferrable to a
follow-up PR. If a fact already lives somewhere in this codebase, you do NOT
copy it into a new field, struct, config block, schema entry, runtime cache,
or anywhere else. You reference it. You resolve it from its source on demand.

**Why this matters more than anything else you're tempted to ship:** every
duplicate state breeds a drift bug whose symptoms surface months later in
production — operator edits the canonical location, the cached copy serves
stale data, the agent silently misbehaves. The previous incarnation of this
codebase had channel `allowed_users` Vec fields cached inside channel handles
while the truth lived in config TOML; reloading config didn't refresh the
channels; an authorized user couldn't talk to the bot until daemon restart.
Every such field is now banned by this rule.

### Forcing mechanism — what happens when you violate

Adding a duplicate state field is an automatic-revert-on-detect change. The
pre-push gate runs `dev/ci.sh dry-check`. If it fires, the maintainer will
`git reset --hard` your branch back to the prior good state, and the time you
spent is wasted. Save yourself the burn: do not write the duplicate in the
first place.

### Pre-edit ritual — before any new struct field, channel/handle field, schema field, config entry

State, in your response text, the source of truth for the new data BEFORE you
write the field. Two valid answers:

  1. **"This is the source of truth — created here."** OK to write the
     field. State what it represents.
  2. **"Source of truth is `<path/to/canonical>` — this would be a
     duplicate."** Do NOT write the field. Resolve from the canonical
     location at use-time (closure, helper, `&Config` parameter, getter
     trait, whatever fits — never a cache).

Any third answer ("we'll only refresh on restart", "snapshot is fine",
"orchestrator passes a Vec in") is a duplicate. Refuse the edit. Find the
canonical source and resolve from there.

### Examples of patterns that ARE duplicate state (forbidden):

- A channel handle struct holding `Vec<String>` of "authorized users" alongside
  `peer_groups` in `Config`.
- A schema enum variant list duplicated across an enum and a `const &[Variant]`
  table that aren't generated from the same macro.
- A `ConfigSnapshot` struct that clones live `Config` fields the runtime can
  already reach through its `Arc<RwLock<Config>>` handle.
- Re-emitting a model-provider's API key into a runtime struct field when the
  runtime already has the typed alias config.

### Patterns that are NOT duplicate state (allowed):

- Resolver closures (`Arc<dyn Fn() -> T + Send + Sync>`) that close over
  `Arc<RwLock<Config>>` and resolve on call.
- `&Config` / `&AgentConfig` parameters threaded through call sites.
- Materialized views built ON-DEMAND from canonical state (cached per-call,
  not stored).
- Derive macros that emit multiple surfaces from one input table (e.g.
  enum + const list from one macro invocation — both come from the same
  source of truth at expansion time).

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

ZeroClaw is a Rust-first autonomous agent runtime optimized for performance, efficiency, stability, extensibility, sustainability, and security.

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points:

- `crates/zeroclaw-api/src/provider.rs` (`Provider`)
- `crates/zeroclaw-api/src/channel.rs` (`Channel`)
- `crates/zeroclaw-api/src/tool.rs` (`Tool`)
- `crates/zeroclaw-api/src/memory_traits.rs` (`Memory`)
- `crates/zeroclaw-api/src/observability_traits.rs` (`Observer`)
- `crates/zeroclaw-api/src/runtime_traits.rs` (`RuntimeAdapter`)
- `crates/zeroclaw-api/src/peripherals_traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## Stability Tiers

Every workspace crate carries a stability tier per the Microkernel Architecture RFC.

| Crate | Tier | Notes |
|-------|------|-------|
| `zeroclaw-api` | Experimental | Stable at v1.0.0 (formal milestone) |
| `zeroclaw-config` | Beta | Stable at v0.8.0 |
| `zeroclaw-providers` | Beta | — |
| `zeroclaw-memory` | Beta | — |
| `zeroclaw-infra` | Beta | — |
| `zeroclaw-tool-call-parser` | Beta | Stable at v0.8.0 |
| `zeroclaw-channels` | Experimental | Plugin migration at v1.0.0 |
| `zeroclaw-tools` | Experimental | Plugin migration at v1.0.0 |
| `zeroclaw-runtime` | Experimental | Agent runtime (agent loop, security, cron, SOP, skills, observability) |
| `zeroclaw-gateway` | Experimental | Separate binary at v0.9.0 |
| `zeroclaw-tui` | Experimental | TUI onboarding wizard |
| `zeroclaw-plugins` | Experimental | WASM plugin system — foundation for v1.0.0 plugin ecosystem |
| `zeroclaw-hardware` | Experimental | USB discovery, peripherals, serial |
| `zeroclaw-macros` | Beta | Tightly coupled to config schema |

**Tiers**: Stable = covered by breaking-change policy. Beta = breaking changes permitted in MINOR with changelog notes. Experimental = no stability guarantee.

Tiers are promoted, never demoted, through deliberate team decision.

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module re-exports and CLI command enum definitions
- `crates/zeroclaw-api/` — public trait definitions (Provider, Channel, Tool, Memory, Observer, Peripheral)
- `crates/zeroclaw-config/` — schema, config loading/merging
- `crates/zeroclaw-macros/` — Configurable derive macro
- `crates/zeroclaw-providers/` — model providers and resilient wrapper
- `crates/zeroclaw-channels/` — messaging platform integrations (30+ channels)
- `crates/zeroclaw-channels/src/orchestrator/` — channel lifecycle, routing, media pipeline
- `crates/zeroclaw-tools/` — tool execution surface (shell, file, memory, browser)
- `crates/zeroclaw-runtime/` — agent loop, security, cron, SOP, skills, onboarding wizard, observability
- `crates/zeroclaw-memory/` — memory backends (markdown, sqlite, embeddings, vector merge)
- `crates/zeroclaw-infra/` — shared infrastructure (debounce, session, stall watchdog)
- `crates/zeroclaw-gateway/` — webhook/gateway server (separate binary)
- `crates/zeroclaw-hardware/` — USB discovery, peripherals, serial, GPIO
- `crates/zeroclaw-tui/` — TUI onboarding wizard
- `crates/zeroclaw-plugins/` — WASM plugin system
- `crates/zeroclaw-tool-call-parser/` — tool call parsing
- `docs/` — topic-based documentation (setup-guides, reference, ops, security, hardware, contributing, maintainers)
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `crates/*/src/**` behavior changes without boundary/security impact
- **High risk**: `crates/zeroclaw-runtime/src/**` (especially `src/security/`), `crates/zeroclaw-gateway/src/**`, `crates/zeroclaw-tools/src/**`, `.github/workflows/**`, access-control boundaries

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
- Never commit secrets, personal data, or real identity information (see `@docs/book/src/contributing/privacy.md`).

## Anti-Patterns

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags "just in case".
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.
- Do not hide behavior-changing side effects in refactor commits.
- Do not suppress unused production code with underscore prefixes or `#[allow(dead_code)]`; delete it, wire it into behavior, or track a follow-up issue. Reserve underscore names for required but intentionally unused API, trait, or callback parameters.
- Do not leave `unwrap()` / `expect()` in production paths; propagate errors or document the invariant that makes panic impossible.
- Do not include personal identity or sensitive information in test data, examples, docs, or commits.

## Skills

AI coding assistant skills live in `.claude/skills/`. Use the right one for the job:

- `.claude/skills/github-pr-review-session/SKILL.md` — PR review co-pilot; assists **you** as the human reviewer. Resolves the active reviewer from session state or `gh`, uses the RFC feedback taxonomy (🔴/🟡/✅/🔵/🟢), and formats formal review findings as H3 headings that start with the taxonomy emoji. Trigger: `review 1234`, `re-review 1234`, `go through the queue`.
- `.claude/skills/changelog-generation/SKILL.md` — generates `CHANGELOG-next.md` between stable tags, resolves contributors via GraphQL, feeds the release workflow. Trigger: `generate changelog`, `release notes for v0.7.x`.
- `.claude/skills/github-issue-triage/SKILL.md` — Issue triage and lifecycle management; manages the backlog, labels, and stale policies. Trigger: `triage issues`, `sweep issues`, `handle issue #N`.
- `.claude/skills/github-issue/SKILL.md` — Interactively files structured GitHub issues (bug reports or feature requests) using repo templates. Trigger: `file issue`, `report bug`, `feature request`.
- `.claude/skills/github-pr/SKILL.md` — Opens or updates GitHub PRs, handles validation evidence, and manages PR descriptions. Trigger: `open PR`, `update PR`, `submit for review`.
- `.claude/skills/skill-creator/SKILL.md` — Framework for creating, testing, evaluating, and optimizing new AI skills. Trigger: `create skill`, `improve skill`, `run skill evals`.
- `.claude/skills/squash-merge/SKILL.md` — Performs conventional squash-merges into master with preserved commit history. Trigger: `squash-merge #123`, `land #789`.
- `.claude/skills/zeroclaw/SKILL.md` — Operational guide for interacting with a ZeroClaw agent instance via CLI or API. Trigger: `check agent status`, `manage memory`, `zeroclaw config`.

## Localization

- All user-facing output (CLI messages, tool descriptions, onboarding prompts) must use `fl!()` / Fluent strings — never bare string literals.
- Log messages, `tracing::` spans/events, and panic messages stay in English with stable `error_key` fields (RFC #5653 §4.6).
- Panics and `tracing::` lines are never translated.
- The Wiki and internal developer docs are English only.

Dev-operational contracts — files consumed by AI coding skills and development tooling. Do not move or delete without updating all consuming skills and AGENTS.md:

| Protected file | Consuming skill / tool |
|---|---|
| `docs/book/src/contributing/pr-review-protocol.md` | `github-pr-review-session` — review protocol |
| `docs/book/src/maintainers/changelog-generation.md` | `changelog-generation` — release procedure |
| `docs/book/src/maintainers/reviewer-playbook.md` | `github-issue-triage` — triage governance |
| `docs/book/src/maintainers/pr-workflow.md` | `github-issue-triage` — triage discipline |
| `docs/book/src/contributing/privacy.md` | `github-issue-triage`, PR template — privacy rules |
| `docs/book/src/foundations/fnd-00*.md` | `github-pr-review-session` — RFC reference data; public transparency documents |

## Linked References

- `@docs/book/src/developing/extension-examples.md` — adding providers, channels, tools, peripherals; tool shared-state contract; architecture boundary rules
- `@docs/book/src/contributing/privacy.md` — privacy rules and neutral-placeholder palette
- `@docs/book/src/maintainers/superseding.md` — superseded-PR attribution, PR/commit templates, handoff template
