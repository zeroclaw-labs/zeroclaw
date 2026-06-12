//! osAgent MCP server registry and tool wrappers — ENGINEER-ONLY.
//!
//! This crate is **structurally excluded** from the wizard binary. The wizard
//! binary's `bins/wizard/Cargo.toml` contains no dependency edge to this crate,
//! and the CI gate `wizard-no-mcp-gate` (4 layers: source-grep + nm
//! `--defined-only` + cargo-bloat + strings) fails any build whose
//! `target/release/wizard` contains MCP symbols.
//!
//! ## Modules
//!
//! - [`mcp_client`] — Multi-server MCP client registry with routing
//! - [`mcp_protocol`] — MCP wire-format types
//! - [`mcp_transport`] — stdio + HTTP transport abstraction
//! - [`mcp_tool`] — Tool wrapper that exposes MCP server tools as agent-callable tools
//! - [`mcp_deferred`] — Deferred-loading variant (only fetch tool schemas on demand)
//!
//! ## Why a separate crate (not a feature flag)?
//!
//! Cargo's resolver=2 unifies *features* across workspace members during
//! `cargo build --workspace`. A `feature = "mcp"` gate on a shared crate
//! would silently relink MCP code into the wizard binary on workspace
//! builds. The only sound exclusion mechanism is the absence of a
//! dependency edge. See `.planning/research/ARCHITECTURE.md`.

pub mod mcp_client;
pub mod mcp_deferred;
pub mod mcp_protocol;
pub mod mcp_tool;
pub mod mcp_transport;
