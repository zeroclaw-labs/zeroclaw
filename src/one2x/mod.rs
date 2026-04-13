//! One2X custom extensions for ZeroClaw.
//!
//! All custom functionality lives here, minimizing upstream file changes.
//! New files in this module have **zero merge conflict risk**.
//!
//! ## Upstream Integration Points (tiny patches, one per file)
//!
//! | File | Change | Lines |
//! |------|--------|-------|
//! | `lib.rs` | module declaration | 2 |
//! | `Cargo.toml` | `one2x = []` feature | 2 |
//! | `config/schema.rs` | `web` field on `ChannelsConfig` | ~8 |
//! | `channels/orchestrator` | session hygiene hooks + tool pairing | ~15 |
//! | `gateway/lib.rs` | `extend_router()` call | ~3 |
//! | `memory/traits.rs` | `list_by_prefix` default method | ~13 |
//! | `memory/sqlite.rs` | `list_by_prefix` SQLite impl | ~43 |
//! | `daemon/mod.rs` | heartbeat validation arms | ~22 |
//! | `tools/shell.rs` | `ZEROCLAW_SESSION_ID` env | ~9 |
//! | `agent/loop_.rs` | planning-without-execution + tool pairing repair | ~20 |
//! | `agent/context_compressor.rs` | MIN_CONTEXT_WINDOW_FLOOR | ~2 |
//! | `agent/tool_execution.rs` | case-insensitive tool lookup fallback | ~8 |
//! | `providers/reliable.rs` | stream idle timeout + retry jitter | ~30 |
//!
//! ## Architecture Note (v6)
//!
//! In v6, code was extracted to workspace crates. Hook implementations that
//! were previously in this module are now duplicated in the sub-crate `one2x`
//! modules (`zeroclaw-channels/src/one2x.rs`, `zeroclaw-runtime/src/one2x.rs`).
//! This root-crate module provides types and functions used by the root crate
//! itself (e.g. gateway route registration, web channel type).
//!
//! ## Merge Workflow
//!
//! Run `dev/merge-upstream.sh` to automate upstream syncing.

pub mod agent_hooks;
// TODO(v6): agent_sse needs deep refactoring for crate paths
// pub mod agent_sse;
// TODO(v6): compaction needs deep refactoring for crate paths
// pub mod compaction;
pub mod config;
pub mod gateway_ext;
pub mod session_hygiene;
pub mod tool_pairing;
pub mod web_channel;
