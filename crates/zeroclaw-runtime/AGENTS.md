# zeroclaw-runtime — Transitional Holding Crate

This crate is a **temporary holding area**, not a permanent home. It contains 126K LOC of subsystems extracted from the original monolith that have not yet been decomposed into their final crate structure.

Do not add new functionality here. The RFC's Phase 2-4 roadmap defines the decomposition plan: agent loop, gateway, channels orchestrator, daemon, cron, security, observability, hardware, TUI, skills, and doctor will each be extracted into dedicated crates or converted to WASM plugins.

**Stability tier:** Experimental — no stability guarantee. Decomposition begins at v0.8.0.
