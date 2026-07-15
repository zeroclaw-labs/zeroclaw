//! zerocode TUI widgets reusable outside the main binary. Limited to
//! drawing/input primitives; consumers of the binary itself should
//! depend on `apps/zerocode/src/main.rs` directly.
//!
//! Also exposes the JSON-RPC transport + wire-shape mirrors the TUI
//! uses internally — exposed publicly only so the wire-drift
//! integration test can reach them.

// Bare `tokio::spawn` is the right primitive in this standalone TUI
// app. See `main.rs`'s `disallowed_methods` allow for the full
// reasoning.
#![allow(clippy::disallowed_methods)]

mod client_crypto;
mod color_depth;
mod relay_proto;
mod theme;
mod todo_tracker;
mod widgets;

pub mod client;
pub mod config;
pub mod enroll;
pub mod jsonrpc;
pub mod keymap;
pub mod wire;
