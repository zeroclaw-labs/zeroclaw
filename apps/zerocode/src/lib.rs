//! Reusable ZeroCode keymap contract.
//!
//! The TUI implementation belongs to the binary target. The library exists so
//! workspace tooling can derive documentation from the same keymap catalogue.

// Bare `tokio::spawn` is the right primitive in this standalone TUI
// app. See `main.rs`'s `disallowed_methods` allow for the full
// reasoning.
#![allow(clippy::disallowed_methods)]

pub mod keymap;
