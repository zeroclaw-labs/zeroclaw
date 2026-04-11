//! Long-running and on-demand background services.
//!
//! Unlike `tools/` (LLM-callable single-shot operations) and `gateway/`
//! (HTTP request handlers), modules here are infrastructure that runs
//! either in the background (file watchers, schedulers) or as orchestration
//! over multiple lower-level building blocks (document conversion + cache,
//! batch indexing, etc.).
//!
//! ## Current services
//!
//! - [`document_cache`] — Idempotent on-disk cache of every uploaded /
//!   linked / web-fetched document, converted to Markdown + HTML so the
//!   LLM can read and search the user's files without re-running the
//!   expensive conversion pipeline on every chat turn.

pub mod document_cache;
