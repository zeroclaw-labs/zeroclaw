//! zerocode TUI widgets reusable outside the main binary. Limited to
//! drawing/input primitives; consumers of the binary itself should
//! depend on `apps/zerocode/src/main.rs` directly.

// Bare `tokio::spawn` is the right primitive in this standalone TUI
// app. See `main.rs`'s `disallowed_methods` allow for the full
// reasoning.
#![allow(clippy::disallowed_methods)]

mod color_depth;
mod theme;
mod todo_tracker;
mod widgets;

pub mod client;
pub mod config;
pub mod jsonrpc;
pub mod keymap;
pub mod wire;
