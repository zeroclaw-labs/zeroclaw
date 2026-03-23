# CLAUDE.md — memory/ (Claude Code-specific)

> Domain-specific instructions live in `@AGENTS.md`.
> This file is reserved for Claude Code-specific directives.

- Before modifying any backend, read its struct + `Memory` impl fully. SQLite is 800+ lines.
- Run `cargo test -p zeroclaw --lib memory` to validate changes scoped to this subsystem.
- Embedding API key resolution is subtle (env var > caller key). Trace through `resolve_embedding_config` before touching key plumbing.
- `battle_tests` module runs cross-backend integration tests; do not skip these for backend changes.

## Linked References
- `@AGENTS.md` — primary agent instructions for this subsystem
- `@docs/contributing/change-playbooks.md`
