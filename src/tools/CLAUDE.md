# CLAUDE.md — tools/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

## Key Reminders

- This is a **high-risk** subsystem (see root CLAUDE.md risk tiers). Changes here require full `./dev/ci.sh all`.
- Always read the target tool file, `mod.rs` factory wiring, and `traits.rs` before editing.
- Security enforcement is mandatory: every new tool must call `is_rate_limited()`, policy checks, and `record_action()`.

## Linked References

- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — full tool addition playbook
- `@../security/` — `SecurityPolicy`, `ToolOperation`, `Sandbox` definitions
