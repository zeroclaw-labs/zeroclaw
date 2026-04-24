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

Branch/PR rules:
- Work from a non-`master` branch. Open PR to `master`; do not push directly.
- Use conventional commit titles (`size: XS/S/M` prefix helpful).
- Follow `.github/pull_request_template.md`.
- Never commit secrets, personal data, or real identity info (see `docs/contributing/pr-discipline.md`).

## Anti-Patterns

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags "just in case".
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.
- Do not hide behavior-changing side effects in refactor commits.
- Do not include personal identity or sensitive info in test data, examples, docs, or commits.

## Skills

AI coding assistant skills live in `.claude/skills/`. Use the right one:

- `.claude/skills/github-pr-review-session/SKILL.md` — PR review co-pilot. Trigger: `review 1234`, `re-review 1234`, `go through the queue`. Posts as WareWolf-MoonWall.
- `.claude/skills/changelog-generation/SKILL.md` — generates `CHANGELOG-next.md` between stable tags. Trigger: `generate changelog`, `release notes for v0.7.x`.
- `.claude/skills/zeroclaw/SKILL.md` — ZeroClaw CLI and gateway API operations. Trigger: anything involving `zeroclaw` binary, gateway API, memory, cron jobs, channels.
- `.claude/skills/systematic-debugging/SKILL.md` — bug/test failure investigation. Trigger: any bug or unexpected behavior.
- `.claude/skills/test-driven-development/SKILL.md` — TDD workflow. Trigger: implementing features or bugfixes.
- `.claude/skills/writing-plans/SKILL.md` — write implementation plans. Trigger: multi-step tasks with a spec or requirements.
- `.claude/skills/brainstorming/SKILL.md` — design exploration before implementation. Trigger: creating features, components, or modifying behavior.

## Linked References

- `docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes
- `docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates
- `docs/contributing/docs-contract.md` — docs system contract, i18n rules
