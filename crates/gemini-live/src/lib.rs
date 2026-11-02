//! `gemini-live` — a focused Rust client for the Google Gemini Live API.
//!
//! Owns everything about talking to Gemini Live correctly: wire types, setup
//! serialization, server-message parsing (including affective tokens), the
//! WebSocket transport (proxy + TLS + close diagnostics), and the
//! reconnect/resumption session driver. Consumers drive it through the async
//! event API in [`session`].
//!
//! The layers are populated task-by-task per the kutsu implementation plan.

// Vendored MIT crate (see PROVENANCE.md): it uses standard `tracing` macros and
// `tokio::spawn` rather than ZeroClaw's `zeroclaw_log::record!` /
// `zeroclaw_spawn::spawn!` attribution wrappers, to preserve parity with its
// upstream (dual-published) source. The workspace `clippy.toml` disallows those
// in first-party code and explicitly sanctions a local `#![allow(...)]` for
// exempt files; this crate is the exempt case.
#![allow(clippy::disallowed_macros, clippy::disallowed_methods)]

pub mod session;
pub mod transport;
pub mod types;
pub mod wire;
