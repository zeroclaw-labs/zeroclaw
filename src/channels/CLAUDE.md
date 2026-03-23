# CLAUDE.md — channels/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

## Directives

- Read `@AGENTS.md` first for trait contract, extension playbook, and gotchas.
- Run `cargo clippy --all-targets -- -D warnings` after any change; channels use many platform-specific lints.
- Feature-gated channels (`channel-lark`, `channel-matrix`, `channel-nostr`) require `--features` flag to compile/test.
- When adding a channel, follow the 8-step playbook in `@AGENTS.md` exactly — missing factory wiring is the most common review failure.
- Chunking logic must live inside `send()`, not in the orchestrator. Test at-limit, one-over, and multibyte boundaries.

## Linked References

- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — cross-subsystem extension procedures
- `@docs/contributing/pr-discipline.md` — privacy, attribution, commit rules
