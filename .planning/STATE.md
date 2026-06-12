# Project State

## Project Reference

See: .planning/PROJECT.md (updated 2026-06-12)

**Core value:** Wizard cannot exfiltrate Vault secrets via an MCP server because the wizard binary has compile-time zero MCP, verified by a 4-layer CI gate (source-grep + `nm --defined-only` + `cargo-bloat --crates` + `strings`).
**Current focus:** Phase 1.1 — Fork & Attribution & Sync Runbook

## Current Position

Phase: 1.1 of 1.6 (Fork & Attribution & Sync Runbook)
Plan: 0 of TBD in current phase
Status: Ready to plan
Last activity: 2026-06-12 — Roadmap created for M1 (6 phases, 20 requirements mapped, 0 orphans)

Progress: [░░░░░░░░░░] 0%

## Performance Metrics

**Velocity:**
- Total plans completed: 0
- Average duration: —
- Total execution time: 0 hours

**By Phase:**

| Phase | Plans | Total | Avg/Plan |
|-------|-------|-------|----------|
| — | — | — | — |

**Recent Trend:**
- Last 5 plans: none yet
- Trend: —

*Updated after each plan completion*

## Accumulated Context

### Decisions

Decisions are logged in PROJECT.md Key Decisions table (42 ratified pre-init decisions).
Cross-cutting refinements applied at roadmap creation:

- **Refinement #1 (Decision #1):** MCP boundary is structural crate exclusion (`osagent-tools-mcp` separate crate, wizard has no dependency edge), NOT Cargo features. Defeats resolver=2 feature unification.
- **Refinement #2 (Decision #25):** CI gate is 4-layer (source-grep + `nm --defined-only` + `cargo bloat --crates` + `strings`), NOT single `nm` grep. LTO/strip/DCE defeats single-layer.
- **Refinement #3:** Signal channel is out-of-process `signal-cli` subprocess (M4); cargo-deny ban entries for AGPL Rust Signal SDKs (presage, libsignal*) land in M1 Phase 1.1/1.4.

### Pending Todos

None yet.

### Blockers/Concerns

None yet.

## Session Continuity

Last session: 2026-06-12
Stopped at: Roadmap + STATE initialized; ready to plan Phase 1.1
Resume file: None
