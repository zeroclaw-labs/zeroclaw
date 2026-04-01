# CLAUDE.md — Hrafn

Hrafn is a Rust autonomous agent runtime with trait-driven modules for providers, channels, tools, memory, and hardware peripherals.

Follow all instructions in [`AGENTS.md`](./AGENTS.md) for commands, architecture, risk tiers, workflow, and anti-patterns.

## Claude Code-Specific

- Use `--features ci-all` when running full test suite (`cargo test --features ci-all`).
- Min supported Rust: 1.87. Edition 2024. Do not use unstable features.
- Feature flags are additive. Never enable a flag in `default` without discussion.
- Hardware features (`stm32`, `rpi-gpio`, `usb-periph`) require physical devices — skip those tests locally.

## Gotchas

- `./dev/ci.sh` runs in Docker Compose — it is not a plain shell script. Don't inline its logic.
- Matrix SDK E2EE state is persistent. Avoid tests that create real Matrix sessions.
- The `browser` feature pulls in Fantoccini (WebDriver) — only enable when needed, it adds significant compile time.

## CI / Automation

- Never push directly to `master`. Always use a feature branch and PR.
- In CI contexts, run `./dev/ci.sh all` for the full gate, not individual cargo commands.
- PR validation uses `checks-on-pr.yml` — check its required status checks before adding new CI jobs.

## Compaction Survival

When context is compacted, preserve:
- Current risk tier classification of files being modified
- Which feature flags are relevant to the current task
- Any hardware constraints (target board, peripheral requirements)
