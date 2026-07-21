//! Shared wasmtime component-model plumbing for all plugin worlds.

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
use crate::instance::PluginInstanceScope;

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

#[cfg(test)]
pub(crate) fn test_limits(call_fuel: u64) -> PluginLimits {
    PluginLimits {
        call_fuel,
        max_memory_bytes: 1024 * 1024,
        max_table_elements: 100,
        max_instances: 10,
    }
}

/// Complete host-side inputs for constructing one scoped plugin store.
///
/// The immutable instance scope supplies identity and effective grants. Limits
/// remain a per-store materialized view of canonical host configuration, while
/// the inbound queue is a process-local resource owned by this store.
pub(crate) struct PluginStoreSpec {
    scope: PluginInstanceScope,
    limits: PluginLimits,
    inbound: InboundQueue,
    http: bool,
}

impl PluginStoreSpec {
    /// Create a store specification with no host-fed inbound messages.
    #[must_use]
    pub(crate) fn new(scope: PluginInstanceScope, limits: PluginLimits) -> Self {
        Self {
            scope,
            limits,
            inbound: InboundQueue::default(),
            http: false,
        }
    }

    /// Attach HTTP only when this scope was granted `HttpClient`.
    ///
    /// Adapters opt into the surface explicitly. This prevents adding a grant
    /// to a scope from silently widening an adapter that has not implemented
    /// and tested the corresponding component boundary.
    #[must_use]
    pub(crate) fn with_granted_http(mut self) -> Self {
        self.http = self.scope.grants().allows(PluginPermission::HttpClient);
        self
    }

    /// Attach the queue shared with a host-owned inbound listener.
    #[must_use]
    pub(crate) fn with_inbound(mut self, inbound: InboundQueue) -> Self {
        self.inbound = inbound;
        self
    }
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

pub struct PluginState {
    scope: PluginInstanceScope,
    wasi: WasiCtx,
    table: ResourceTable,
    http: Option<WasiHttpCtx>,
    inbound: InboundQueue,
    limits: StoreLimits,
    fuel_per_call: u64,
}

impl PluginState {
    /// Build store state from one typed, host-issued specification.
    /// `HttpClient` is the only grant that can widen the host surface here, and
    /// only after the adapter calls [`PluginStoreSpec::with_granted_http`]. That
    /// opt-in attaches a `WasiHttpCtx` so the gated `wasi:http` import can be
    /// linked. Other grants are consumed by adapters or host services where
    /// implemented and do not widen ambient WASI.
    pub(crate) fn new(spec: PluginStoreSpec) -> Self {
        let http = spec.http.then(WasiHttpCtx::new);
        Self {
            scope: spec.scope,
            wasi: WasiCtx::builder().build(),
            table: ResourceTable::new(),
            http,
            inbound: spec.inbound,
            limits: StoreLimitsBuilder::new()
                .memory_size(spec.limits.max_memory_bytes)
                .table_elements(spec.limits.max_table_elements)
                .instances(spec.limits.max_instances)
                .build(),
            fuel_per_call: spec.limits.call_fuel,
        }
    }

    /// Immutable host-owned identity and authority for this store.
    #[must_use]
    pub(crate) fn scope(&self) -> &PluginInstanceScope {
        &self.scope
    }

    /// Whether this state was built with outbound HTTP attached.
    pub(crate) fn http_enabled(&self) -> bool {
        self.http.is_some()
    }

    /// The inbound queue this plugin drains. Host code holds a clone to enqueue.
    pub(crate) fn inbound(&self) -> &InboundQueue {
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

/// Build a Wasmtime store whose imports are derived from one admitted scope.
pub(crate) fn new_store(spec: PluginStoreSpec) -> Store<PluginState> {
    let call_fuel = spec.limits.call_fuel;
    let state = PluginState::new(spec);
    let mut store = Store::new(engine(), state);
    store.limiter(|state| &mut state.limits);
    set_call_fuel(&mut store, call_fuel);
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
    use crate::PluginCapability;

    fn scope(
        binding: &str,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> PluginInstanceScope {
        crate::instance::test_scope(PluginCapability::Tool, binding, grants)
    }

    fn spec(grants: impl IntoIterator<Item = PluginPermission>, call_fuel: u64) -> PluginStoreSpec {
        PluginStoreSpec::new(scope("main", grants), test_limits(call_fuel)).with_granted_http()
    }

    #[test]
    fn http_absent_without_permission() {
        let state = PluginState::new(spec([], 0));
        assert!(
            !state.http_enabled(),
            "no HttpClient permission means no outbound HTTP context"
        );
    }

    #[test]
    fn http_absent_for_unrelated_permissions() {
        let state = PluginState::new(spec(
            [
                PluginPermission::ConfigRead,
                PluginPermission::MemoryRead,
                PluginPermission::FileRead,
            ],
            0,
        ));
        assert!(
            !state.http_enabled(),
            "only HttpClient attaches the HTTP context"
        );
    }

    #[test]
    fn http_present_with_permission() {
        let state = PluginState::new(spec([PluginPermission::HttpClient], 0));
        assert!(
            state.http_enabled(),
            "HttpClient attaches the outbound HTTP context"
        );
    }

    #[test]
    fn grant_does_not_enable_http_without_adapter_opt_in() {
        let granted_scope = scope("main", [PluginPermission::HttpClient]);
        let state = PluginState::new(PluginStoreSpec::new(granted_scope, test_limits(0)));

        assert!(
            !state.http_enabled(),
            "an adapter must explicitly opt into its tested HTTP boundary"
        );
    }

    #[test]
    fn http_coherence_accepts_matching_store_and_linker() {
        let granted = new_store(spec([PluginPermission::HttpClient], 0));
        assert!(
            ensure_http_coherent(&granted, true).is_ok(),
            "granted store paired with an http linker is coherent"
        );
        let plain = new_store(spec([], 0));
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
        let granted = new_store(spec([PluginPermission::HttpClient], 0));
        assert!(
            ensure_http_coherent(&granted, false).is_err(),
            "granted store with a plain linker cannot back its own permission"
        );
        let plain = new_store(spec([], 0));
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
        let state = PluginState::new(spec([], 0).with_inbound(q.clone()));
        q.enqueue(sample_inbound("y"));
        assert_eq!(
            state.inbound().pending(),
            1,
            "state shares the supplied queue"
        );
    }

    #[test]
    fn engine_enables_fuel_metering() {
        let mut store = Store::new(engine(), PluginState::new(spec([], 0)));
        store
            .set_fuel(123)
            .expect("fuel must be enabled on the shared plugin engine");
        assert_eq!(store.get_fuel().expect("get_fuel"), 123);
    }

    #[test]
    fn new_store_seeds_configured_budget() {
        let store = new_store(spec([], 777));
        assert_eq!(store.get_fuel().expect("get_fuel"), 777);
    }

    #[test]
    fn zero_budget_traps_before_any_work() {
        let store = new_store(spec([], 0));
        assert_eq!(
            store.get_fuel().expect("get_fuel"),
            0,
            "a zero budget leaves no fuel, so the first consuming instruction traps"
        );
    }

    #[test]
    fn refuel_restores_per_call_budget_on_a_warm_store() {
        let mut store = new_store(spec([], 500));
        store.set_fuel(3).expect("set_fuel");
        assert_eq!(store.get_fuel().expect("get_fuel"), 3);
        refuel(&mut store);
        assert_eq!(
            store.get_fuel().expect("get_fuel"),
            500,
            "refuel must reset a drained warm store to the configured per-call budget"
        );
    }

    #[test]
    fn stores_share_only_their_issued_instance_scope() {
        let primary = scope("primary", []);
        let primary_store = new_store(PluginStoreSpec::new(primary.clone(), test_limits(0)));
        let second_primary_store = new_store(PluginStoreSpec::new(primary, test_limits(0)));
        let backup_store = new_store(PluginStoreSpec::new(scope("backup", []), test_limits(0)));

        assert!(std::ptr::eq(
            primary_store.data().scope().id(),
            second_primary_store.data().scope().id()
        ));
        assert_ne!(
            primary_store.data().scope().id(),
            backup_store.data().scope().id(),
            "separate bindings must not share a host-service namespace"
        );
    }
}
