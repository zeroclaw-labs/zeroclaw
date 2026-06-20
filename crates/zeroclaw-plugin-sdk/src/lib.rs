//! Guest-side SDK for authoring ZeroClaw WASI Preview 2 / Component Model
//! plugins. Enable the `tool`, `memory`, or `channel` feature for the world
//! your plugin implements; bindings are generated from `wit/v0` (gated by
//! the `plugins-wit-v0` feature, matching the host's `zeroclaw-plugins`
//! crate so the two never drift on WIT version).

pub mod bindings;
