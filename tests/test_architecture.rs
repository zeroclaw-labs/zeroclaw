//! Workspace architecture-invariant test entry. Each submodule here is a
//! detector that fails the workspace test suite when the corresponding
//! invariant is violated. See AGENTS.md §1 ("ABSOLUTE RULE — SINGLE
//! SOURCE OF TRUTH") for context on why these gates exist.

#[path = "architecture/no_duplicate_state.rs"]
mod no_duplicate_state;

#[path = "architecture/config_save_isolation.rs"]
mod config_save_isolation;
