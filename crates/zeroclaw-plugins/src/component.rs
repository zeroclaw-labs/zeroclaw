//! Shared wasmtime component-model plumbing for all plugin worlds.
//!
//! One async-enabled engine, one store state carrying the host imports, and the
//! per-world linker wiring. Tool plugins use a fresh store per call; channel and
//! memory plugins hold a warm store guarded by an async mutex.

use anyhow::Result;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use wasmtime::component::{Component, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};

use crate::PluginPermission;

/// A host-owned queue of inbound messages destined for a channel plugin.
///
/// The host runs the listener (webhook server, vendor tunnel, polling client)
/// and pushes each received message in; the plugin drains it from the imported
/// `inbound` interface. Cloning shares the same underlying queue, so a listener
/// task can hold one handle while the plugin's store holds another.
#[derive(Clone, Default)]
pub struct InboundQueue {
    inner: Arc<Mutex<VecDeque<HostInboundMessage>>>,
}

/// A host-side inbound message, decoupled from any one WIT world's generated
/// type. The channel world's `Host` impl converts this into its bindings type
/// when the plugin polls.
#[derive(Clone, Debug, Default)]
pub struct HostInboundMessage {
    pub id: String,
    pub sender: String,
    pub reply_target: String,
    pub content: String,
    pub channel: String,
    pub channel_alias: Option<String>,
    pub timestamp: u64,
    pub thread_ts: Option<String>,
    pub interruption_scope_id: Option<String>,
    pub subject: Option<String>,
}

impl InboundQueue {
    /// Push a received message onto the queue for the plugin to drain. A
    /// poisoned lock is recovered rather than swallowed, so a panic in one
    /// producer cannot silently stop every later inbound message from landing.
    pub fn enqueue(&self, msg: HostInboundMessage) {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(msg);
    }

    /// Pop the next queued message, or `None` when empty. Recovers a poisoned
    /// lock so a producer panic does not strand the queued backlog.
    pub fn poll(&self) -> Option<HostInboundMessage> {
        let mut q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.pop_front()
    }

    /// Count of messages currently waiting. Recovers a poisoned lock so the
    /// drain side keeps reporting real depth after a producer panic.
    pub fn pending(&self) -> u32 {
        let q = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        q.len() as u32
    }
}

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
/// table WASI requires. Outbound HTTP is present only when the plugin's manifest
/// grants `HttpClient`; otherwise `http` is `None` and `wasi:http` is never
/// linked, so the component cannot reach the network at all.
pub struct PluginState {
    wasi: WasiCtx,
    table: ResourceTable,
    http: Option<WasiHttpCtx>,
    inbound: InboundQueue,
    limits: StoreLimits,
    fuel_per_call: u64,
}

impl PluginState {
    /// Build store state for a plugin holding `permissions` under `limits`.
    /// `HttpClient` is the only permission that widens the host surface here: it
    /// attaches a `WasiHttpCtx` so the gated `wasi:http` import can be linked.
    /// Every other permission resolves elsewhere (config jail, memory bridge)
    /// and leaves the WASI sandbox closed. `limits` sets the per-call fuel and
    /// the memory/table/instance ceilings the store limiter enforces.
    pub fn new(permissions: &[PluginPermission], limits: PluginLimits) -> Self {
        Self::with_inbound(permissions, InboundQueue::default(), limits)
    }

    /// Build store state with a caller-supplied inbound queue. A channel plugin
    /// whose host listener feeds it inbound traffic shares the listener's queue
    /// handle here, so `inbound-poll` drains what the listener enqueued.
    pub fn with_inbound(
        permissions: &[PluginPermission],
        inbound: InboundQueue,
        limits: PluginLimits,
    ) -> Self {
        let http = permissions
            .contains(&PluginPermission::HttpClient)
            .then(WasiHttpCtx::new);
        Self {
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            http,
            inbound,
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.max_memory_bytes)
                .table_elements(limits.max_table_elements)
                .instances(limits.max_instances)
                .build(),
            fuel_per_call: limits.call_fuel,
        }
    }

    /// Whether this state was built with outbound HTTP attached.
    pub fn http_enabled(&self) -> bool {
        self.http.is_some()
    }

    /// The inbound queue this plugin drains. Host code holds a clone to enqueue.
    pub fn inbound(&self) -> &InboundQueue {
        &self.inbound
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

impl WasiHttpView for PluginState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        let ctx = self
            .http
            .as_mut()
            .expect("wasi:http called on a plugin without the HttpClient permission");
        WasiHttpCtxView {
            ctx,
            table: &mut self.table,
            hooks: wasmtime_wasi_http::p2::default_hooks(),
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

/// Wire the outbound `wasi:http` surface into a plugin linker. Only call this
/// for a linker that backs stores built with the `HttpClient` permission; the
/// store's `WasiHttpView::http` panics otherwise, which keeps a permission
/// mismatch from silently granting network access.
pub fn add_wasi_http(linker: &mut wasmtime::component::Linker<PluginState>) -> Result<()> {
    wt(
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(linker),
        "failed to add wasi:http imports to plugin linker",
    )
}

/// Assert that a store and the linker chosen for it agree on the `wasi:http`
/// surface before instantiation. The store carries a `WasiHttpCtx` only when
/// its manifest granted `HttpClient`; `linker_has_http` is whether the linker
/// picked for it wired `wasi:http`. A registration path that pairs an
/// http-linked linker with a store lacking the context (or the reverse) gets a
/// named startup error here instead of a `WasiHttpView::http` panic at the
/// first outbound call, so a misconfigured wiring cannot crash a live task and
/// cannot silently link a surface the store cannot back.
pub fn ensure_http_coherent(store: &Store<PluginState>, linker_has_http: bool) -> Result<()> {
    let store_has_http = store.data().http_enabled();
    if store_has_http != linker_has_http {
        anyhow::bail!(
            "plugin store/linker http mismatch: store HttpClient={store_has_http}, \
             linker wasi:http={linker_has_http}; refusing to instantiate"
        );
    }
    Ok(())
}

pub fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.consume_fuel(true);
        Engine::new(&config).expect("async-capable wasmtime engine")
    })
}

pub fn new_store(permissions: &[PluginPermission], limits: PluginLimits) -> Store<PluginState> {
    new_store_with_inbound(permissions, InboundQueue::default(), limits)
}

/// Like [`new_store`], but the resulting state shares `inbound` so a host
/// listener can enqueue traffic the plugin drains. The limiter and per-call
/// fuel are wired identically to [`new_store`].
pub fn new_store_with_inbound(
    permissions: &[PluginPermission],
    inbound: InboundQueue,
    limits: PluginLimits,
) -> Store<PluginState> {
    let state = PluginState::with_inbound(permissions, inbound, limits);
    let mut store = Store::new(engine(), state);
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
    fn http_absent_without_permission() {
        let state = PluginState::new(&[], limits(0));
        assert!(
            !state.http_enabled(),
            "no HttpClient permission means no outbound HTTP context"
        );
    }

    #[test]
    fn http_absent_for_unrelated_permissions() {
        let state = PluginState::new(
            &[
                PluginPermission::ConfigRead,
                PluginPermission::MemoryRead,
                PluginPermission::FileRead,
            ],
            limits(0),
        );
        assert!(
            !state.http_enabled(),
            "only HttpClient attaches the HTTP context"
        );
    }

    #[test]
    fn http_present_with_permission() {
        let state = PluginState::new(&[PluginPermission::HttpClient], limits(0));
        assert!(
            state.http_enabled(),
            "HttpClient attaches the outbound HTTP context"
        );
    }

    #[test]
    fn http_coherence_accepts_matching_store_and_linker() {
        let granted = new_store(&[PluginPermission::HttpClient], limits(0));
        assert!(
            ensure_http_coherent(&granted, true).is_ok(),
            "granted store paired with an http linker is coherent"
        );
        let plain = new_store(&[], limits(0));
        assert!(
            ensure_http_coherent(&plain, false).is_ok(),
            "ungranted store paired with a plain linker is coherent"
        );
    }

    #[test]
    fn http_coherence_rejects_a_store_linker_mismatch() {
        // A registration path that links wasi:http against a store with no
        // HttpClient context (or the reverse) is refused at instantiate time
        // with a named error, not a WasiHttpView::http panic on first call.
        let granted = new_store(&[PluginPermission::HttpClient], limits(0));
        assert!(
            ensure_http_coherent(&granted, false).is_err(),
            "granted store with a plain linker cannot back its own permission"
        );
        let plain = new_store(&[], limits(0));
        assert!(
            ensure_http_coherent(&plain, true).is_err(),
            "plain store with an http linker would panic on first outbound call"
        );
    }

    #[cfg(feature = "plugins-wasm-cranelift")]
    #[test]
    fn http_linker_builds_only_when_granted() {
        // The base linker (no wasi:http) always builds.
        let mut base = wasmtime::component::Linker::<PluginState>::new(engine());
        add_wasi(&mut base).expect("base WASI links");

        // Adding wasi:http on top must also succeed; this is the surface an
        // HttpClient-granted plugin gets. A store built without the permission
        // never reaches this linker, so its WasiHttpView is never invoked.
        add_wasi_http(&mut base).expect("wasi:http links onto a granted linker");
    }

    fn sample_inbound(id: &str) -> HostInboundMessage {
        HostInboundMessage {
            id: id.to_string(),
            sender: "caller".to_string(),
            content: format!("body-{id}"),
            channel: "inkbox".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn inbound_queue_drains_fifo() {
        let q = InboundQueue::default();
        assert_eq!(q.pending(), 0);
        assert!(q.poll().is_none(), "empty queue polls none");

        q.enqueue(sample_inbound("1"));
        q.enqueue(sample_inbound("2"));
        q.enqueue(sample_inbound("3"));
        assert_eq!(q.pending(), 3);

        assert_eq!(q.poll().unwrap().id, "1");
        assert_eq!(q.poll().unwrap().id, "2");
        assert_eq!(q.pending(), 1);
        assert_eq!(q.poll().unwrap().id, "3");
        assert!(q.poll().is_none(), "drained queue polls none");
        assert_eq!(q.pending(), 0);
    }

    #[test]
    fn inbound_queue_handle_is_shared() {
        // A listener clone and the store's clone must see the same queue, so a
        // message enqueued by the host listener is visible to the plugin drain.
        let listener = InboundQueue::default();
        let store_side = listener.clone();
        listener.enqueue(sample_inbound("x"));
        assert_eq!(store_side.pending(), 1, "clone shares the backing queue");
        assert_eq!(store_side.poll().unwrap().id, "x");
        assert_eq!(
            listener.pending(),
            0,
            "drain on one clone empties the other"
        );
    }

    #[test]
    fn inbound_queue_survives_a_poisoned_lock() {
        // A producer that panics while holding the lock must not permanently
        // silence the queue: later enqueue/poll/pending recover the poison and
        // keep delivering, since silent inbound loss is worse than a noisy trap.
        let q = InboundQueue::default();
        q.enqueue(sample_inbound("before"));

        let poisoned = q.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.inner.lock().unwrap();
            panic!("poison the queue lock");
        })
        .join();

        assert!(q.inner.is_poisoned(), "lock is poisoned after the panic");
        assert_eq!(q.pending(), 1, "pending recovers the poisoned lock");
        q.enqueue(sample_inbound("after"));
        assert_eq!(q.pending(), 2, "enqueue recovers and appends");
        assert_eq!(q.poll().unwrap().id, "before", "drain recovers, FIFO holds");
        assert_eq!(q.poll().unwrap().id, "after");
        assert_eq!(q.pending(), 0);
    }

    #[test]
    fn plugin_state_exposes_its_inbound_queue() {
        let q = InboundQueue::default();
        let state = PluginState::with_inbound(&[], q.clone(), limits(0));
        q.enqueue(sample_inbound("y"));
        assert_eq!(
            state.inbound().pending(),
            1,
            "state shares the supplied queue"
        );
    }

    #[test]
    fn engine_enables_fuel_metering() {
        let mut store = Store::new(engine(), PluginState::new(&[], limits(0)));
        store
            .set_fuel(123)
            .expect("fuel must be enabled on the shared plugin engine");
        assert_eq!(store.get_fuel().expect("get_fuel"), 123);
    }

    #[test]
    fn new_store_seeds_configured_budget() {
        let store = new_store(&[], limits(777));
        assert_eq!(store.get_fuel().expect("get_fuel"), 777);
    }

    #[test]
    fn zero_budget_traps_before_any_work() {
        let store = new_store(&[], limits(0));
        assert_eq!(
            store.get_fuel().expect("get_fuel"),
            0,
            "a zero budget leaves no fuel, so the first consuming instruction traps"
        );
    }

    #[test]
    fn refuel_restores_per_call_budget_on_a_warm_store() {
        let mut store = new_store(&[], limits(500));
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
