//! Shared wasmtime component-model plumbing for all plugin worlds.
//!
//! One async-enabled engine, one store state carrying the host imports, and the
//! per-world linker wiring. Tool plugins use a fresh store per call; channel and
//! memory plugins hold a warm store guarded by an async mutex.

use anyhow::Result;
use std::path::Path;
use std::sync::OnceLock;
use wasmtime::component::{Component, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// Resolved per-call execution limits applied to a plugin store. The host
/// builds this from `[plugins.limits]` config and hands it to `new_store`.
/// There is deliberately no `Default`: limits always come from the config
/// registry so no code path can construct an unsandboxed store by accident.
#[derive(Debug, Clone, Copy)]
pub struct PluginLimits {
    pub call_fuel: u64,
    pub max_memory_bytes: usize,
    pub max_table_elements: usize,
    pub max_instances: usize,
}

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

/// Per-store host state. Carries a sandboxed WASI context (no preopens, no
/// network) so Rust-compiled wasip2 components instantiate, plus the resource
/// table WASI requires. Host imports beyond `logging` are deliberately absent.
pub struct PluginState {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    fuel_per_call: u64,
}

impl PluginState {
    fn with_limits(limits: PluginLimits) -> Self {
        Self {
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.max_memory_bytes)
                .table_elements(limits.max_table_elements)
                .instances(limits.max_instances)
                .build(),
            fuel_per_call: limits.call_fuel,
        }
    }
}

impl WasiView for PluginState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// Wire the sandboxed WASI p2 surface into a plugin linker.
pub fn add_wasi(linker: &mut wasmtime::component::Linker<PluginState>) -> Result<()> {
    wt(
        wasmtime_wasi::p2::add_to_linker_async(linker),
        "failed to add WASI imports to plugin linker",
    )
}

pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.consume_fuel(true);
        Engine::new(&config).expect("async-capable wasmtime engine")
    })
}

pub fn new_store(limits: PluginLimits) -> Store<PluginState> {
    let mut store = Store::new(engine(), PluginState::with_limits(limits));
    store.limiter(|state| &mut state.limits);
    set_call_fuel(&mut store, limits.call_fuel);
    store
}

fn set_call_fuel(store: &mut Store<PluginState>, call_fuel: u64) {
    store
        .set_fuel(call_fuel)
        .expect("fuel is enabled on the plugin engine");
}

/// Reset a warm store's fuel before a call so reused channel/memory stores get
/// a fresh per-call budget instead of draining across their lifetime.
pub fn refuel(store: &mut Store<PluginState>) {
    let call_fuel = store.data().fuel_per_call;
    set_call_fuel(store, call_fuel);
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
        crate::component::refuel(store);
        let f = $body;
        f(store, bindings).await
    }};
}
pub(crate) use call_plugin;

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(call_fuel: u64) -> PluginLimits {
        PluginLimits {
            call_fuel,
            max_memory_bytes: 256 * 1024 * 1024,
            max_table_elements: 100_000,
            max_instances: 64,
        }
    }

    #[test]
    fn engine_enables_fuel_metering() {
        let mut store = Store::new(engine(), PluginState::with_limits(limits(0)));
        store
            .set_fuel(123)
            .expect("fuel must be enabled on the shared plugin engine");
        assert_eq!(store.get_fuel().expect("get_fuel"), 123);
    }

    #[test]
    fn new_store_seeds_configured_budget() {
        let store = new_store(limits(777));
        assert_eq!(store.get_fuel().expect("get_fuel"), 777);
    }

    #[test]
    fn zero_budget_traps_before_any_work() {
        let store = new_store(limits(0));
        assert_eq!(
            store.get_fuel().expect("get_fuel"),
            0,
            "a zero budget leaves no fuel, so the first consuming instruction traps"
        );
    }

    #[test]
    fn refuel_restores_per_call_budget_on_a_warm_store() {
        let mut store = new_store(limits(500));
        store.set_fuel(3).expect("set_fuel");
        assert_eq!(store.get_fuel().expect("get_fuel"), 3);
        refuel(&mut store);
        assert_eq!(
            store.get_fuel().expect("get_fuel"),
            500,
            "refuel must reset a drained warm store to the configured per-call budget"
        );
    }
}
