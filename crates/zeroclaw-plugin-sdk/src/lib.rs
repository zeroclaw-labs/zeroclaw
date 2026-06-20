//! Guest-side SDK for authoring ZeroClaw WASI Preview 2 / Component Model
//! plugins. Enable the `tool`, `memory`, or `channel` feature for the world
//! your plugin implements; bindings are generated from `wit/v0` (gated by
//! the `plugins-wit-v0` feature, matching the host's `zeroclaw-plugins`
//! crate so the two never drift on WIT version).
//!
//! A plugin author should implement the relevant trait
//! ([`tool::ToolPlugin`], `memory::MemoryPlugin`, `channel::ChannelPlugin`)
//! and call the matching `export_*!` macro once. The raw generated
//! `bindings` module is public for advanced use, but most plugins should
//! not need to reach into it directly.

pub mod bindings;
#[cfg(feature = "channel")]
pub mod channel;
mod macros;
#[cfg(feature = "memory")]
pub mod memory;
#[cfg(feature = "tool")]
pub mod tool;
