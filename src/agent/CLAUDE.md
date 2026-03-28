# CLAUDE.md — agent/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

## Key Constraints

- Run `cargo test --lib agent` to validate changes to this subsystem.
- The tool-call loop is high-risk (`loop_.rs`, `dispatcher.rs`). Changes require full `./dev/ci.sh all`.
- `thinking.rs` and `classifier.rs` are medium-risk with self-contained test suites.
- Do not add new `PromptSection` impls without a concrete use case — prompt token budget is finite.
- Cost tracking uses `task_local!` — mock it in tests or accept silent no-ops.

## Linked References

- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, PR templates
