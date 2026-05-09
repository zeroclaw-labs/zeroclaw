# AGENTS.md — ZeroClaw

Cross-tool agent instructions for any AI coding assistant working on this repository.

## Commands

```bash
# Format, lint, and test (fast, no Docker)
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test

# Full validation (runs in Docker — from repo root or dev/)
./dev/ci.sh all

# Sub-commands for partial validation:
./dev/ci.sh lint-strict    # clippy with warnings-as-errors (strict gate)
./dev/ci.sh lint-delta     # lint only changed lines
./dev/ci.sh test-component # unit tests
./dev/ci.sh test-integration
./dev/ci.sh test-system
./dev/ci.sh test-live      # requires real credentials
```

Docs-only changes: run markdown lint and link-integrity checks. If touching bootstrap scripts: `bash -n install.sh`.

## Project Snapshot

ZeroClaw is a Rust-first autonomous agent runtime. Core architecture is trait-driven and modular.

Rust edition **2024** (workspace-level, `Cargo.toml:7`). Requires Rust 1.87+.

Key extension traits (`crates/zeroclaw-api/src/`):

- `provider.rs` — `Provider`
- `channel.rs` — `Channel`, `ChannelApprovalRequest`, `ChannelApprovalResponse`
- `tool.rs` — `Tool`
- `memory_traits.rs` — `Memory`
- `observability_traits.rs` — `Observer`
- `runtime_traits.rs` — `RuntimeAdapter`
- `peripherals_traits.rs` — `Peripheral`

## Stability Tiers

| Crate | Tier |
|-------|------|
| `zeroclaw-api` | Experimental |
| `zeroclaw-config` | Beta |
| `zeroclaw-providers` | Beta |
| `zeroclaw-memory` | Beta |
| `zeroclaw-infra` | Beta |
| `zeroclaw-tool-call-parser` | Beta |
| `zeroclaw-channels` | Experimental |
| `zeroclaw-tools` | Experimental |
| `zeroclaw-runtime` | Experimental |
| `zeroclaw-gateway` | Experimental |
| `zeroclaw-tui` | Experimental |
| `zeroclaw-plugins` | Experimental |
| `zeroclaw-hardware` | Experimental |
| `zeroclaw-macros` | Beta |

## Repository Map

- `src/main.rs` — CLI entrypoint
- `src/lib.rs` — module re-exports, command enum
- `crates/zeroclaw-api/` — trait definitions
- `crates/zeroclaw-runtime/src/agent/loop_.rs` — **agent loop**: tool call execution, approval flow
- `crates/zeroclaw-runtime/src/approval/mod.rs` — `ApprovalManager`, `ApprovalRequest`, `summarize_args()`
- `crates/zeroclaw-channels/` — channel implementations; each implements `Channel` trait
  - `src/wukongim.rs` — WuKongIM channel with structured approval card (type=20)
  - `src/lark.rs` — Lark channel with approval support
  - `src/telegram.rs` — Telegram channel (HTML-formatted approval)
  - `src/orchestrator/` — channel lifecycle and routing
- `crates/zeroclaw-runtime/src/sop/` — SOP engine, dispatch, audit
- `crates/zeroclaw-runtime/src/skills/` — skill loading, skill-to-tool conversion
- `crates/zeroclaw-config/` — schema, config loading/merging (published separately on crates.io)
- `crates/zeroclaw-tools/` — built-in tool implementations (shell, file, memory, browser, etc.)
- `crates/zeroclaw-providers/` — model providers
- `crates/zeroclaw-memory/` — memory backends
- `crates/zeroclaw-infra/` — shared infra (debounce, session, stall watchdog)
- `crates/zeroclaw-gateway/` — webhook/gateway server (separate binary)
- `crates/zeroclaw-hardware/` — USB discovery, peripherals, serial, GPIO
- `crates/zeroclaw-tui/` — TUI onboarding wizard
- `crates/zeroclaw-plugins/` — WASM plugin system
- `crates/zeroclaw-tool-call-parser/` — tool call parsing (XML, markdown, GLM formats)
- `.claude/skills/` — AI assistant skills (zeroclaw, github-pr-review-session, changelog-generation, etc.)
- `docs/` — documentation (setup-guides, reference, ops, security, hardware, contributing)
- `dev/` — CI scripts (`ci.sh`) and Docker configuration

## Approval Architecture (Tool Call)

```
LLM tool_calls: [{ name: "tool_name", arguments: "{...}" }]
  ↓
loop_.rs:1562 → needs_approval(tool_name) — uses ApprovalManager
  ↓
loop_.rs:1564 → ApprovalRequest { tool_name, arguments: Value }
  ↓
loop_.rs:1575 → ChannelApprovalRequest { tool_name, arguments_summary, ... }
  ↓
channel.request_approval(recipient, request) → ChannelApprovalResponse
  ↓
ApprovalResponse: Approve / Deny / AlwaysApprove
```

**Skill tools** follow the same flow. Their names are prefixed with the skill name (e.g., `weather_skill.get_weather`). Skill shell tools always require explicit approval (`security.validate_command_execution(cmd, approved=true)`).

**`summarize_args()`** in `crates/zeroclaw-runtime/src/approval/mod.rs:224` converts raw JSON arguments into a flattened key-value string for display. **This is lossy** — truncates string values to 80 chars, collapses nested objects to `to_string()`. New approval code should pass structured data instead.

## Risk Tiers

- **Low**: docs/chore/tests-only
- **Medium**: most `crates/*/src/**` behavior changes without boundary/security impact
- **High**: `crates/zeroclaw-runtime/src/**` (especially `src/security/`), `crates/zeroclaw-gateway/src/**`, `crates/zeroclaw-tools/src/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify higher.

## Workflow

1. **Read before write** — inspect existing module, factory wiring, and adjacent tests.
2. **One concern per PR** — avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch** — no speculative abstractions, no config keys without a concrete use case.
4. **Validate by risk tier** — low: lightweight checks; code: full checks (`cargo clippy`, `cargo test`).
5. **Document impact** — update PR notes for behavior, risk, side effects, rollback.
6. **Stacked PRs**: declare `Depends on #...`. Replacing old PR: declare `Supersedes #...`.

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

AI coding assistant skills live in `.claude/skills/`. Use the right one:

- `.claude/skills/github-pr-review-session/SKILL.md` — PR review co-pilot; assists **you** as the human reviewer. Resolves the active reviewer from session state or `gh`, uses the RFC feedback taxonomy (🔴/🟡/✅/🔵/🟢), and formats formal review findings as H3 headings that start with the taxonomy emoji. Trigger: `review 1234`, `re-review 1234`, `go through the queue`.
- `.claude/skills/changelog-generation/SKILL.md` — generates `CHANGELOG-next.md` between stable tags, resolves contributors via GraphQL, feeds the release workflow. Trigger: `generate changelog`, `release notes for v0.7.x`.
- `.claude/skills/github-issue-triage/SKILL.md` — Issue triage and lifecycle management; manages the backlog, labels, and stale policies. Trigger: `triage issues`, `sweep issues`, `handle issue #N`.
- `.claude/skills/github-issue/SKILL.md` — Interactively files structured GitHub issues (bug reports or feature requests) using repo templates. Trigger: `file issue`, `report bug`, `feature request`.
- `.claude/skills/github-pr/SKILL.md` — Opens or updates GitHub PRs, handles validation evidence, and manages PR descriptions. Trigger: `open PR`, `update PR`, `submit for review`.
- `.claude/skills/skill-creator/SKILL.md` — Framework for creating, testing, evaluating, and optimizing new AI skills. Trigger: `create skill`, `improve skill`, `run skill evals`.
- `.claude/skills/squash-merge/SKILL.md` — Performs conventional squash-merges into master with preserved commit history. Trigger: `squash-merge #123`, `land #789`.
- `.claude/skills/zeroclaw/SKILL.md` — Operational guide for interacting with a ZeroClaw agent instance via CLI or API. Trigger: `check agent status`, `manage memory`, `zeroclaw config`.
- `.claude/skills/systematic-debugging/SKILL.md` — bug/test failure investigation. Trigger: any bug or unexpected behavior.
- `.claude/skills/test-driven-development/SKILL.md` — TDD workflow. Trigger: implementing features or bugfixes.
- `.claude/skills/writing-plans/SKILL.md` — write implementation plans. Trigger: multi-step tasks with a spec or requirements.
- `.claude/skills/brainstorming/SKILL.md` — design exploration before implementation. Trigger: creating features, components, or modifying behavior.

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
