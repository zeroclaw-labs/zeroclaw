//! osAgent MCP server registry and tool wrappers — ENGINEER-ONLY.
//!
//! This crate is **structurally excluded** from the wizard binary. The wizard
//! binary's `bins/wizard/Cargo.toml` contains no dependency edge to this crate,
//! and the CI gate `wizard-no-mcp-gate` (4 layers: source-grep + nm
//! `--defined-only` + cargo-bloat + strings) fails any build whose
//! `target/release/wizard` contains MCP symbols.
//!
//! ## Phase 1.3 status (M1)
//!
//! This is a STUB. The physical MCP source files
//! (`crates/zeroclaw-tools/src/mcp_*.rs` and `src/tools/mcp_*.rs`) migrate
//! into this crate in Phase 1.4/1.5 alongside the broader source-strip work.
//! At Phase 1.3 we ship only the workspace slot + the CI gate.
//!
//! ## Why a separate crate (not a feature flag)?
//!
//! Cargo's resolver=2 unifies *features* across workspace members during
//! `cargo build --workspace`. A `feature = "mcp"` gate on a shared crate
//! would silently relink MCP code into the wizard binary on workspace
//! builds. The only sound exclusion mechanism is the absence of a
//! dependency edge. See `.planning/research/ARCHITECTURE.md`.

// Placeholder. Phase 1.4 / 1.5 moves the real MCP source here.
