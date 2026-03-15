# CLAUDE.md — ZeroClaw

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

- `src/providers/traits.rs` (`Provider`)
- `src/channels/traits.rs` (`Channel`)
- `src/tools/traits.rs` (`Tool`)
- `src/memory/traits.rs` (`Memory`)
- `src/observability/traits.rs` (`Observer`)
- `src/runtime/traits.rs` (`RuntimeAdapter`)
- `src/peripherals/traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)
- `src/security/taint.rs` (`TaintLabel`, `TaintSource`) — information flow tracking
- `src/sop/workflow.rs` (`WorkflowStep`, `StepHandler`) — workflow DAG engine
- `src/hands/types.rs` (`Hand`, `HandContext`) — autonomous agent packages

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
- `src/tools/` — tool execution surface (shell, file, memory, browser)
- `src/hands/` — autonomous knowledge-accumulating agent packages (Hands system)
- `src/sop/` — standard operating procedures + workflow DAG engine
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO)
- `src/runtime/` — runtime adapters (currently native)
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

## Contributing Guidelines (from CONTRIBUTING.md)

### Branch & PR Rules
- `master` is the ONLY default branch. There is no `main` branch.
- Fork the repo, create `feat/*` or `fix/*` branch from `master`, open PR targeting `master`.
- IMPORTANT: Use `feat/` NOT `feature/` — the prefix must be `feat/` or `fix/` exactly.
- Use conventional commit titles (e.g., `feat(channels):`, `fix(security):`, `chore(ci):`).
- Complete **every section** in `.github/pull_request_template.md` — all 15 sections are mandatory.
- One concern per PR. Prefer size `XS/S/M`. Split large work into stacked PRs.
- If replacing an older PR, add `Supersedes #...` and request maintainers close the old one.

### PR Template (All Sections Required)
1. Summary (base branch, problem, what changed, what didn't change)
2. Label Snapshot (risk, size, scope, module labels)
3. Change Metadata (change type, primary scope)
4. Linked Issue
5. Validation Evidence (`cargo fmt`, `cargo clippy`, `cargo test` results)
6. Security Impact (permissions, network calls, secrets, file access)
7. Privacy and Data Hygiene (pass/needs-follow-up, neutral wording)
8. Compatibility / Migration (backward compatible, config changes, migration)
9. i18n Follow-Through (triggered? locale parity updated?)
10. Human Verification (verified scenarios, edge cases, gaps)
11. Side Effects / Blast Radius (affected subsystems, unintended effects, guardrails)
12. Rollback Plan (fast rollback, feature flags, failure symptoms)
13. Risks and Mitigations

### Collaboration Tracks (Risk-Based)
- **Track A (Low risk)**: docs/tests/chore — 1 maintainer review + green CI
- **Track B (Medium risk)**: providers/channels/memory/tools behavior — 1 subsystem-aware review + validation evidence
- **Track C (High risk)**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `.github/workflows/**` — 2-pass review, rollback plan required

### Code Naming Conventions
- Rust casing: modules/files `snake_case`, types/traits/enums `PascalCase`, functions/variables `snake_case`, constants `SCREAMING_SNAKE_CASE`
- Domain-first naming: `DiscordChannel`, `SecurityPolicy`, `SqliteMemory` (not `Manager`, `Helper`, `Util`)
- Trait implementers: `*Provider`, `*Channel`, `*Tool`, `*Memory`, `*Observer`
- Factory keys: lowercase, stable (`openai`, `discord`, `shell`)
- Tests: behavior-oriented names (`subject_expected_behavior`), neutral project-scoped fixtures
- Identity labels: use ZeroClaw-native identifiers only (`ZeroClawAgent`, `zeroclaw_user`, `zeroclaw_node`)

### Architecture Boundary Rules
- Extend via trait implementations + factory registration before considering broad refactors.
- Dependency direction: concrete integrations depend on shared traits/config/util, not on each other.
- No cross-subsystem coupling (provider <-> channel internals, tools mutating security directly).
- Single-purpose modules: `agent` = orchestration, `channels` = transport, `providers` = model I/O, `security` = policy, `tools` = execution, `memory` = persistence.
- Shared abstractions only after rule-of-three (3+ stable callers).
- `src/config/schema.rs` keys are public contract — document compatibility, migration, rollback.

### Validation Commands
```bash
# Required before every PR
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked

# Full quality gate
./scripts/ci/rust_quality_gate.sh

# Strict delta lint (changed lines only)
./scripts/ci/rust_strict_delta_gate.sh
```

### What Must Never Be Committed
- `.env` files (use `.env.example` only)
- API keys, tokens, passwords, credentials (plain or encrypted)
- OAuth tokens, session identifiers, webhook signing secrets
- `~/.zeroclaw/.secret_key` or similar key files
- Personal identifiers or real user data in tests/fixtures

### CI Details
- Quality Gate: Lint (fmt+clippy), Build (x86_64-linux + aarch64-darwin), Test, Security Audit
- CI uses Rust 1.92.0 (may differ from local toolchain)
- `clippy::large_futures` triggers at 16KB on Linux — fix with `Box::pin()`
- Windows-only test failures (Unix path assumptions) don't affect CI

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
- `CONTRIBUTING.md` — full contributor contract, collaboration tracks, PR template, naming conventions
