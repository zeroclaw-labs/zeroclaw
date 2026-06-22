//! Generated wasmtime component bindings for the `tool-plugin` world.
//!
//! `bindgen!` reads the WIT package in `wit/v0/` and generates the host-side
//! traits for the imported interfaces (`host`, `logging`) and typed accessors
//! for the exported interfaces (`plugin-info`, `tool`). bindgen forces
//! `all_features = true`, so the `@unstable`-gated WIT items are always generated.

#![allow(clippy::all)]

wasmtime::component::bindgen!({
    path: "../../wit/v0",
    world: "tool-plugin",
    with: {},
});
