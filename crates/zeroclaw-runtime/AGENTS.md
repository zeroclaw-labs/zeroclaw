# zeroclaw-runtime — Transitional Holding Crate

This crate is a **temporary holding area**, not a permanent home. It contains 126K LOC of subsystems extracted from the original monolith that have not yet been decomposed into their final crate structure.

Do not add new functionality here, unless the Core Team has granted a recorded exception (see below). The RFC's Phase 2-4 roadmap defines the decomposition plan: agent loop, gateway, channels orchestrator, daemon, cron, security, observability, hardware, TUI, skills, and doctor will each be extracted into dedicated crates or converted to WASM plugins.

## Exceptions

Extraction is the default. The Core Team may grant a bounded exception when immediate extraction would require a disproportionate refactor, or would establish a crate boundary the roadmap does not intend. See [ADR-016](../../docs/book/src/architecture/decisions/ADR-016-holding-crate-exceptions.md).

An exception must name its permitted scope, intended destination, approving authority, and expiry or review condition, and must be recorded before the feature it covers merges. A feature pull request cannot grant itself one: the contract it would be waiving is the one constraining it.

An exception requires a concrete supported use case. Code with no receiving caller does not qualify; retirement or an explicit ownership decision is the right answer there.

An exception permits continued work on a subsystem already held here. It never permits introducing a new subsystem, and it does not generalise from one subsystem to another.

### Active exceptions

| Scope | Destination | Approved by | Expires |
| --- | --- | --- | --- |
| _(none)_ | | | |

**Stability tier:** Experimental — no stability guarantee. Decomposition begins at v0.8.0.
