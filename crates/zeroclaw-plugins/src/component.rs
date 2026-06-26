//! Shared wasmtime component-model plumbing for all plugin worlds.
//!
//! One async-enabled engine, one store state carrying the host imports, and the
//! per-world linker wiring. Tool plugins use a fresh store per call; channel and
//! memory plugins hold a warm store guarded by an async mutex.

use anyhow::Result;
use std::path::Path;
use std::sync::OnceLock;
use wasmtime::component::Component;
use wasmtime::{Config, Engine};

pub mod bindings {
    pub mod tool {
        wasmtime::component::bindgen!({
            world: "tool-plugin",
            path: "../../wit/v0",
            imports: { default: async },
            exports: { default: async },
        });
    }
    pub mod channel {
        wasmtime::component::bindgen!({
            world: "channel-plugin",
            path: "../../wit/v0",
            imports: { default: async },
            exports: { default: async },
        });
    }
    pub mod memory {
        wasmtime::component::bindgen!({
            world: "memory-plugin",
            path: "../../wit/v0",
            imports: { default: async },
            exports: { default: async },
        });
    }
}

/// Per-store host state. Holds nothing today; the logging and types host imports
/// are stateless. A field lands here when a host import needs per-instance state.
#[derive(Default)]
pub struct PluginState;

pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let config = Config::new();
        Engine::new(&config).expect("async-capable wasmtime engine")
    })
}

pub fn wt<T>(r: wasmtime::Result<T>, ctx: &'static str) -> Result<T> {
    r.map_err(|e| anyhow::Error::msg(format!("{ctx}: {e}")))
}

/// Compile a component from a WASM file. With a JIT backend present a `.wasm`
/// component is compiled on load; in runtime-only builds the file is a
/// precompiled `.cwasm` deserialized directly.
pub fn load_component(wasm_path: &Path) -> Result<Component> {
    wt(load_inner(wasm_path), "failed to load WASM component")
}

#[cfg(feature = "plugins-wasm-cranelift")]
fn load_inner(wasm_path: &Path) -> wasmtime::Result<Component> {
    Component::from_file(engine(), wasm_path)
}

#[cfg(not(feature = "plugins-wasm-cranelift"))]
fn load_inner(wasm_path: &Path) -> wasmtime::Result<Component> {
    // SAFETY: the file is a wasmtime-produced `.cwasm` for this engine; a
    // mismatched artifact is rejected by deserialize's version check.
    unsafe { Component::deserialize_file(engine(), wasm_path) }
}

/// Run an async call against a warm `Arc<Mutex<(Store, bindings)>>` plugin,
/// holding the store lock for the duration of the single component call.
macro_rules! call_plugin {
    ($self:expr, $body:expr) => {{
        let mut guard = $self.state.lock().await;
        let (ref mut store, ref mut bindings) = *guard;
        let f = $body;
        f(store, bindings).await
    }};
}
pub(crate) use call_plugin;
