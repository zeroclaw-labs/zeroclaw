# CLAUDE.md — providers/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

## Directives

- Read `@AGENTS.md` before modifying any provider file.
- Run `cargo test -p zeroclaw --lib providers` to validate provider changes in isolation.
- When adding a new provider, follow `@AGENTS.md` §Extension Playbook steps 1-8 exactly.
- The `compatible.rs` tool-schema fallback is a critical recovery path — do not remove or weaken `is_native_tool_schema_unsupported` without understanding downstream impact.

## Linked References

- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — full provider addition playbook with review checklist
