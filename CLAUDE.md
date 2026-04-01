# CLAUDE.md — Hrafn

Hrafn is a Rust autonomous agent runtime with trait-driven modules for providers, channels, tools, memory, and hardware peripherals. Core architecture is trait-driven and modular — extend by implementing traits and registering in factory modules.

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features ci-all          # full feature coverage
./dev/ci.sh all                       # full pre-PR gate (Docker Compose)
```

Docs-only changes: run markdown lint and link-integrity checks. Bootstrap scripts: `bash -n install.sh`.

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
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO)
- `src/runtime/` — runtime adapters (currently native)
- `docs/` — topic-based documentation
- `.github/` — CI, templates, automation workflows

Key extension traits: `Provider`, `Channel`, `Tool`, `Memory`, `Observer`, `RuntimeAdapter`, `Peripheral` — each in `src/<module>/traits.rs`.

## Risk Tiers

- **Low**: docs/chore/tests-only changes
- **Medium**: most `src/**` behavior changes without boundary/security impact
- **High**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

## Workflow

1. Read before write — inspect existing module, factory wiring, and adjacent tests before editing.
2. One concern per PR — avoid mixed feature+refactor+infra patches.
3. Implement minimal patch — no speculative abstractions, no config keys without a concrete use case.
4. Validate by risk tier — docs-only: lightweight checks. Code changes: full relevant checks.
5. Work from a non-`master` branch. Open a PR to `master`; do not push directly.
6. Use conventional commit titles. Prefer small PRs (`size: XS/S/M`).
7. Never commit secrets, personal data, or real identity information.

## Anti-Patterns

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags "just in case".
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.

## Tech Stack Notes

- Min supported Rust: 1.87. Edition 2024. Do not use unstable features.
- Feature flags are additive. Never enable a flag in `default` without discussion.
- Hardware features (`hardware`, `peripheral-rpi`) require physical devices — skip those tests locally.

## Gotchas

- `./dev/ci.sh` runs in Docker Compose — it is not a plain shell script. Don't inline its logic.
- Matrix SDK E2EE state is persistent. Avoid tests that create real Matrix sessions.
- The `browser-native` feature pulls in Fantoccini (WebDriver) — only enable when needed, it adds significant compile time.

## CI / Automation

- Never push directly to `master`. Always use a feature branch and PR.
- In CI contexts, run `./dev/ci.sh all` for the full gate, not individual cargo commands.
- PR validation uses `checks-on-pr.yml` — check its required status checks before adding new CI jobs.

## Compaction Survival

When context is compacted, preserve:
- Current risk tier classification of files being modified
- Which feature flags are relevant to the current task
- Any hardware constraints (target board, peripheral requirements)

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity
