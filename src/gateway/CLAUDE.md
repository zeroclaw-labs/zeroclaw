# CLAUDE.md — gateway/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

- This subsystem is **high risk** — changes here are public-facing. Run `./dev/ci.sh all` before opening a PR.
- Auth logic is duplicated in four files (see AGENTS.md "Common Gotchas"). Grep for `require_auth` and `is_authenticated` before modifying.
- Do not add CORS middleware without explicit security review and scoping.
- Feature-gated code (`plugins-wasm`) must stay behind `#[cfg(feature = ...)]` — test with `cargo check` (no features) to verify.

## Linked References
- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — gateway change checklist
- `@src/security/` — `PairingGuard`, `SecurityPolicy`, secret store
