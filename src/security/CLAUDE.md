# CLAUDE.md — security/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

## Key Rules

- Read `@AGENTS.md` fully before any edit in this directory.
- This is a HIGH RISK subsystem. Never weaken defaults (`Supervised`, `workspace_only: true`, `block_high_risk_commands: true`).
- Run `cargo test -p zeroclaw --lib security` after every change; adversarial tests exist and must pass.
- Changes to `policy.rs` defaults, `forbidden_paths`, or `Sandbox` implementations require a second reviewer.
- Never commit plaintext secrets or key material in test fixtures — use `SecretStore::new(tempdir, true)`.

## Linked References

- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md` — security change playbook
- `@docs/contributing/pr-discipline.md` — privacy and commit hygiene rules
