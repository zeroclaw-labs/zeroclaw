//! Shared wasmtime component-model plumbing for all plugin worlds.

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use wasmtime::component::{Component, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::WasiHttpCtx;
use wasmtime_wasi_http::p2::{WasiHttpCtxView, WasiHttpView};

use crate::config::ResolvedPluginConfig;
use crate::error::PluginError;
use crate::host::AdmittedComponent;
use crate::instance::PluginInstanceScope;
use crate::services::{
    ConfigLookupError, PluginHostServices, PluginStateError, PluginStateKey, PluginStateValue,
    SecretLookupError,
};
use crate::{PluginCapability, PluginPermission};

/// Hard safety ceiling for ZeroClaw-owned WIT imports in one service frame.
///
/// A fixed limit avoids retaining live operator configuration in warm stores.
const MAX_HOST_CALLS_PER_FRAME: u64 = 1_000;

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
    services: PluginHostServices,
    limits: PluginLimits,
    inbound: InboundQueue,
    http: bool,
}

impl PluginStoreSpec {
    /// Create a store specification with no host-fed inbound messages.
    #[must_use]
    pub(crate) fn new(
        scope: PluginInstanceScope,
        services: PluginHostServices,
        limits: PluginLimits,
    ) -> Self {
        Self {
            scope,
            services,
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
    services: PluginHostServices,
    call_config: CallConfig,
    host_calls_remaining: u64,
    wasi: WasiCtx,
    table: ResourceTable,
    http: Option<WasiHttpCtx>,
    inbound: InboundQueue,
    limits: StoreLimits,
    fuel_per_call: u64,
}

/// The host-dispatched service frame that is currently active.
///
/// This is the authority for phase-specific imports. Keeping it in the same
/// state machine as the resolved config prevents a separate boolean grant from
/// drifting away from the config revision it is meant to protect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PluginCallPhase {
    Standard,
    ToolExecute,
    ChannelService,
}

/// Lazily materialized config for exactly one host-dispatched service frame.
///
/// This is a transient view, not another canonical config store. It is cleared
/// after every frame so live resolvers observe changes on their next invocation
/// while all reads within one frame share one revision.
enum CallConfig {
    Inactive,
    Unresolved(PluginCallPhase),
    Resolved(PluginCallPhase, ResolvedPluginConfig),
    Failed(PluginCallPhase),
}

impl CallConfig {
    fn phase(&self) -> Option<PluginCallPhase> {
        match self {
            Self::Inactive => None,
            Self::Unresolved(phase) | Self::Resolved(phase, _) | Self::Failed(phase) => {
                Some(*phase)
            }
        }
    }
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
            services: spec.services,
            call_config: CallConfig::Inactive,
            host_calls_remaining: 0,
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

    fn start_call(&mut self, phase: PluginCallPhase) {
        self.call_config = CallConfig::Unresolved(phase);
        self.host_calls_remaining = MAX_HOST_CALLS_PER_FRAME;
    }

    fn finish_call(&mut self) {
        self.call_config = CallConfig::Inactive;
        self.host_calls_remaining = 0;
    }

    fn with_call_config<T>(
        &mut self,
        use_config: impl FnOnce(&ResolvedPluginConfig) -> T,
    ) -> Result<T, PluginError> {
        let phase = self.call_config.phase().ok_or_else(|| {
            PluginError::InvalidConfig(
                "plugin host service called outside an active invocation frame".to_string(),
            )
        })?;
        if matches!(self.call_config, CallConfig::Unresolved(_)) {
            match self.services.resolve_config(&self.scope) {
                Ok(config) => self.call_config = CallConfig::Resolved(phase, config),
                Err(error) => {
                    self.call_config = CallConfig::Failed(phase);
                    return Err(error);
                }
            }
        }
        match &self.call_config {
            CallConfig::Resolved(_, config) => Ok(use_config(config)),
            CallConfig::Inactive | CallConfig::Unresolved(_) | CallConfig::Failed(_) => Err(
                PluginError::InvalidConfig("plugin call config could not be resolved".to_string()),
            ),
        }
    }

    /// Resolve public configuration lazily for the active service frame.
    pub(crate) fn public_config(&mut self) -> Result<serde_json::Value, PluginError> {
        self.with_call_config(|config| config.public_json().clone())
    }

    /// Charge one ZeroClaw-owned host import against the active call budget.
    pub(crate) fn charge_host_call(&mut self) -> bool {
        if matches!(self.call_config, CallConfig::Inactive) || self.host_calls_remaining == 0 {
            return false;
        }
        self.host_calls_remaining -= 1;
        true
    }

    /// Whether the active frame may use this instance's scoped host services.
    fn instance_services_enabled(&self) -> bool {
        matches!(
            (self.call_config.phase(), self.scope.id().capability()),
            (Some(PluginCallPhase::ToolExecute), PluginCapability::Tool)
                | (
                    Some(PluginCallPhase::ChannelService),
                    PluginCapability::Channel
                )
        )
    }

    /// Resolve the typed public object for a guest host-service call.
    pub(crate) fn guest_public_config(&mut self) -> Result<serde_json::Value, ConfigLookupError> {
        if !self.charge_host_call()
            || !matches!(
                (self.call_config.phase(), self.scope.id().capability()),
                (
                    Some(PluginCallPhase::ChannelService),
                    PluginCapability::Channel
                )
            )
        {
            return Err(ConfigLookupError::Unavailable);
        }
        if !self.scope.grants().allows(PluginPermission::ConfigRead) {
            return Err(ConfigLookupError::AccessDenied);
        }
        self.public_config()
            .map_err(|_| ConfigLookupError::Unavailable)
    }

    /// Resolve one secret from the same config revision used by this call.
    pub(crate) fn secret(&mut self, name: &str) -> Result<String, SecretLookupError> {
        if !self.charge_host_call() {
            return Err(SecretLookupError::Unavailable);
        }
        if !self.instance_services_enabled() {
            return Err(SecretLookupError::Unavailable);
        }
        if !self.scope.grants().allows(PluginPermission::ConfigRead) {
            return Err(SecretLookupError::AccessDenied);
        }
        self.with_call_config(|config| config.secret(name).map(ToOwned::to_owned))
            .map_err(|_| SecretLookupError::Unavailable)?
            .ok_or(SecretLookupError::NotFound)
    }

    /// Read durable state under the immutable store-owned instance scope.
    pub(crate) async fn state_get(
        &mut self,
        key: String,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        if !self.charge_host_call() || !self.instance_services_enabled() {
            return Err(PluginStateError::Unavailable);
        }
        if !self.scope.grants().allows(PluginPermission::StateRead) {
            return Err(PluginStateError::AccessDenied);
        }
        let key = PluginStateKey::parse(key)?;
        let state = self.services.state().clone();
        let scope = self.scope.clone();
        state.get(&scope, &key).await
    }

    /// Commit durable state with compare-and-swap semantics.
    pub(crate) async fn state_put(
        &mut self,
        key: String,
        value: Vec<u8>,
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        if !self.charge_host_call() || !self.instance_services_enabled() {
            return Err(PluginStateError::Unavailable);
        }
        if !self.scope.grants().allows(PluginPermission::StateWrite) {
            return Err(PluginStateError::AccessDenied);
        }
        let key = PluginStateKey::parse(key)?;
        let state = self.services.state().clone();
        let scope = self.scope.clone();
        state.put(&scope, &key, &value, expected_revision).await
    }

    /// Delete durable state with compare-and-swap semantics.
    pub(crate) async fn state_delete(
        &mut self,
        key: String,
        expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        if !self.charge_host_call() || !self.instance_services_enabled() {
            return Err(PluginStateError::Unavailable);
        }
        if !self.scope.grants().allows(PluginPermission::StateWrite) {
            return Err(PluginStateError::AccessDenied);
        }
        let key = PluginStateKey::parse(key)?;
        let state = self.services.state().clone();
        let scope = self.scope.clone();
        state.delete(&scope, &key, expected_revision).await
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

/// Cancellation-safe owner of one active host-service frame.
pub(crate) struct ActivePluginCall<'a> {
    store: &'a mut Store<PluginState>,
}

impl<'a> ActivePluginCall<'a> {
    /// Start a standard frame where phase-specific services are unavailable.
    pub(crate) fn new(store: &'a mut Store<PluginState>) -> Self {
        Self::start(store, PluginCallPhase::Standard)
    }

    /// Start a tool-execute frame where scoped secrets are available.
    pub(crate) fn tool_execute(store: &'a mut Store<PluginState>) -> Self {
        Self::start(store, PluginCallPhase::ToolExecute)
    }

    /// Start a channel service frame where scoped secrets are available.
    pub(crate) fn channel_service(store: &'a mut Store<PluginState>) -> Self {
        Self::start(store, PluginCallPhase::ChannelService)
    }

    fn start(store: &'a mut Store<PluginState>, phase: PluginCallPhase) -> Self {
        refuel(store);
        store.data_mut().start_call(phase);
        Self { store }
    }

    pub(crate) fn store_mut(&mut self) -> &mut Store<PluginState> {
        self.store
    }
}

impl Drop for ActivePluginCall<'_> {
    fn drop(&mut self) {
        self.store.data_mut().finish_call();
    }
}

pub fn wt<T>(r: wasmtime::Result<T>, ctx: &'static str) -> Result<T> {
    r.map_err(|e| anyhow::Error::msg(format!("{ctx}: {e}")))
}

/// Compile or deserialize the exact component bytes admitted by the host.
pub fn load_component(component: &AdmittedComponent) -> Result<Component> {
    wt(load_inner(component), "failed to load WASM component")
}

#[cfg(feature = "plugins-wasm-cranelift")]
fn load_inner(component: &AdmittedComponent) -> wasmtime::Result<Component> {
    Component::new(engine(), component.bytes())
}

#[cfg(not(feature = "plugins-wasm-cranelift"))]
fn load_inner(component: &AdmittedComponent) -> wasmtime::Result<Component> {
    // SAFETY: the bytes are a wasmtime-produced `.cwasm` for this engine; a
    // mismatched artifact is rejected by deserialize's version check.
    unsafe { Component::deserialize(engine(), component.bytes()) }
}

/// Run an async call against a warm mutex-protected `(Store, bindings)` pair,
/// holding the store lock for the duration of the single component call.
macro_rules! call_plugin_frame {
    ($self:expr, $constructor:ident, $body:expr) => {{
        let mut guard = $self.state.lock().await;
        let (ref mut store, ref mut bindings) = *guard;
        let mut active_call = crate::component::ActivePluginCall::$constructor(store);
        let f = $body;
        let result = f(active_call.store_mut(), bindings).await;
        drop(active_call);
        result
    }};
}

macro_rules! call_plugin {
    ($self:expr, $body:expr) => {{ crate::component::call_plugin_frame!($self, new, $body) }};
}
pub(crate) use call_plugin;

macro_rules! call_tool_execute {
    ($self:expr, $body:expr) => {{ crate::component::call_plugin_frame!($self, tool_execute, $body) }};
}
pub(crate) use call_plugin_frame;
pub(crate) use call_tool_execute;

macro_rules! call_channel {
    ($self:expr, $body:expr) => {{ crate::component::call_plugin_frame!($self, channel_service, $body) }};
}
pub(crate) use call_channel;

/// Run one direct store call inside the same transient service frame used by
/// warm adapter calls. The RAII guard drops the transient resolved-config view
/// on success, error, trap, panic unwinding, or future cancellation.
macro_rules! call_store_frame {
    ($store:ident, $constructor:ident, $body:expr) => {{
        let mut active_call = crate::component::ActivePluginCall::$constructor(&mut $store);
        let f = $body;
        let result = f(active_call.store_mut()).await;
        drop(active_call);
        result
    }};
}
pub(crate) use call_store_frame;

macro_rules! call_store {
    ($store:ident, $body:expr) => {{ crate::component::call_store_frame!($store, new, $body) }};
}
pub(crate) use call_store;

macro_rules! call_channel_store {
    ($store:ident, $body:expr) => {{ crate::component::call_store_frame!($store, channel_service, $body) }};
}
pub(crate) use call_channel_store;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PluginConfigResolver, resolve_plugin_config};
    use crate::{PluginCapability, PluginManifest};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scope(
        binding: &str,
        grants: impl IntoIterator<Item = PluginPermission>,
    ) -> PluginInstanceScope {
        crate::instance::test_scope(PluginCapability::Tool, binding, grants)
    }

    fn spec(grants: impl IntoIterator<Item = PluginPermission>, call_fuel: u64) -> PluginStoreSpec {
        PluginStoreSpec::new(
            scope("main", grants),
            crate::services::test_host_services(),
            test_limits(call_fuel),
        )
        .with_granted_http()
    }

    fn secret_manifest(capability: PluginCapability) -> PluginManifest {
        PluginManifest {
            name: "fixture".to_string(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![capability],
            permissions: vec![PluginPermission::ConfigRead],
            config_schema: Some(serde_json::json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "revision": {"type": "string"},
                    "api_key": {"type": "string", "x-secret": true}
                },
                "required": ["revision", "api_key"],
                "additionalProperties": false
            })),
            signature: None,
            publisher_key: None,
        }
    }

    fn secret_scope(
        manifest: &PluginManifest,
        capability: PluginCapability,
        binding: &str,
        grant_config: bool,
    ) -> PluginInstanceScope {
        PluginInstanceScope::from_manifest(
            manifest,
            capability,
            binding,
            grant_config.then_some(PluginPermission::ConfigRead),
        )
        .expect("valid secret test scope")
    }

    fn configured(revision: &str, secret: &str) -> HashMap<String, String> {
        HashMap::from([
            ("revision".to_string(), revision.to_string()),
            ("api_key".to_string(), secret.to_string()),
        ])
    }

    fn static_services(
        manifest: PluginManifest,
        values: HashMap<String, String>,
    ) -> PluginHostServices {
        crate::services::test_services(PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&manifest, scope, Some(&values))
        }))
    }

    #[test]
    fn denied_secret_lookup_never_invokes_the_resolver() {
        for (capability, phase) in [
            (PluginCapability::Tool, PluginCallPhase::ToolExecute),
            (PluginCapability::Channel, PluginCallPhase::ChannelService),
        ] {
            let manifest = secret_manifest(capability);
            let denied = secret_scope(&manifest, capability, "main", false);
            let calls = Arc::new(AtomicUsize::new(0));
            let resolver_calls = Arc::clone(&calls);
            let services = crate::services::test_services(PluginConfigResolver::new(move |_| {
                resolver_calls.fetch_add(1, Ordering::SeqCst);
                panic!("denied lookup must not invoke config resolution")
            }));
            let mut state =
                PluginState::new(PluginStoreSpec::new(denied, services, test_limits(1_000)));

            state.start_call(phase);
            if capability == PluginCapability::Channel {
                assert_eq!(
                    state.guest_public_config(),
                    Err(ConfigLookupError::AccessDenied)
                );
            }
            assert_eq!(
                state.secret("api_key"),
                Err(SecretLookupError::AccessDenied)
            );
            state.finish_call();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn disabled_secret_frame_never_invokes_the_resolver() {
        let manifest = secret_manifest(PluginCapability::Channel);
        let scope = secret_scope(&manifest, PluginCapability::Channel, "main", true);
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver_calls = Arc::clone(&calls);
        let services = crate::services::test_services(PluginConfigResolver::new(move |_| {
            resolver_calls.fetch_add(1, Ordering::SeqCst);
            panic!("disabled secret frame must not invoke config resolution")
        }));
        let mut state = PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        state.start_call(PluginCallPhase::Standard);
        assert_eq!(
            state.guest_public_config(),
            Err(ConfigLookupError::Unavailable)
        );
        assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
        state.finish_call();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn mismatched_secret_phase_never_invokes_the_resolver() {
        for (capability, phase) in [
            (PluginCapability::Channel, PluginCallPhase::ToolExecute),
            (PluginCapability::Tool, PluginCallPhase::ChannelService),
        ] {
            let manifest = secret_manifest(capability);
            let scope = secret_scope(&manifest, capability, "main", true);
            let calls = Arc::new(AtomicUsize::new(0));
            let resolver_calls = Arc::clone(&calls);
            let services = crate::services::test_services(PluginConfigResolver::new(move |_| {
                resolver_calls.fetch_add(1, Ordering::SeqCst);
                panic!("a mismatched call phase must not resolve secrets")
            }));
            let mut state =
                PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

            state.start_call(phase);
            assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
            state.finish_call();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn channel_service_reads_secret_from_its_call_config_revision() {
        let manifest = secret_manifest(PluginCapability::Channel);
        let scope = secret_scope(&manifest, PluginCapability::Channel, "main", true);
        let services = static_services(manifest, configured("one", "channel-token"));
        let mut state = PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        state.start_call(PluginCallPhase::ChannelService);
        state.host_calls_remaining = 2;
        assert_eq!(
            state.guest_public_config().expect("public config"),
            serde_json::json!({"revision": "one"})
        );
        assert_eq!(state.secret("api_key"), Ok("channel-token".to_string()));
        assert_eq!(
            state.guest_public_config(),
            Err(ConfigLookupError::Unavailable),
            "public config and secrets must share one host-call budget"
        );
        state.finish_call();
    }

    #[test]
    fn secret_lookup_rejects_a_view_from_another_scope_issuance() {
        let manifest = Arc::new(secret_manifest(PluginCapability::Tool));
        let requested = secret_scope(&manifest, PluginCapability::Tool, "main", true);
        let issued = secret_scope(&manifest, PluginCapability::Tool, "backup", true);
        let resolver_manifest = Arc::clone(&manifest);
        let services = crate::services::test_services(PluginConfigResolver::new(move |_| {
            let values = configured("one", "backup-token");
            resolve_plugin_config(&resolver_manifest, &issued, Some(&values))
        }));
        let mut state = PluginState::new(PluginStoreSpec::new(
            requested,
            services,
            test_limits(1_000),
        ));

        state.start_call(PluginCallPhase::ToolExecute);
        assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
        state.finish_call();
    }

    #[test]
    fn one_frame_shares_one_live_revision_and_next_frame_refreshes() {
        let manifest = Arc::new(secret_manifest(PluginCapability::Tool));
        let scope = secret_scope(&manifest, PluginCapability::Tool, "main", true);
        let values = Arc::new(std::sync::RwLock::new(configured("one", "token-one")));
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver_manifest = Arc::clone(&manifest);
        let resolver_values = Arc::clone(&values);
        let resolver_calls = Arc::clone(&calls);
        let services = crate::services::test_services(PluginConfigResolver::new(move |scope| {
            resolver_calls.fetch_add(1, Ordering::SeqCst);
            let values = resolver_values
                .read()
                .unwrap_or_else(|error| error.into_inner());
            resolve_plugin_config(&resolver_manifest, scope, Some(&values))
        }));
        let mut state = PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        state.start_call(PluginCallPhase::ToolExecute);
        assert_eq!(
            state.public_config().expect("public config")["revision"],
            "one"
        );
        *values.write().unwrap_or_else(|error| error.into_inner()) = configured("two", "token-two");
        assert_eq!(state.secret("api_key"), Ok("token-one".to_string()));
        assert_eq!(state.secret("api_key"), Ok("token-one".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.finish_call();

        state.start_call(PluginCallPhase::ToolExecute);
        assert_eq!(
            state.public_config().expect("public config")["revision"],
            "two"
        );
        assert_eq!(state.secret("api_key"), Ok("token-two".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        state.finish_call();
    }

    #[test]
    fn failed_resolution_is_cached_for_the_frame() {
        let manifest = secret_manifest(PluginCapability::Tool);
        let scope = secret_scope(&manifest, PluginCapability::Tool, "main", true);
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver_calls = Arc::clone(&calls);
        let services = crate::services::test_services(PluginConfigResolver::new(move |_| {
            resolver_calls.fetch_add(1, Ordering::SeqCst);
            Err(PluginError::InvalidConfig("resolver detail".to_string()))
        }));
        let mut state = PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        state.start_call(PluginCallPhase::ToolExecute);
        assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
        assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.finish_call();
    }

    #[test]
    fn host_call_budget_exhausts_and_resets_per_frame() {
        let manifest = secret_manifest(PluginCapability::Tool);
        let scope = secret_scope(&manifest, PluginCapability::Tool, "main", true);
        let services = static_services(manifest, configured("one", "token"));
        let mut state = PluginState::new(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        state.start_call(PluginCallPhase::ToolExecute);
        for _ in 0..MAX_HOST_CALLS_PER_FRAME {
            assert_eq!(state.secret("api_key"), Ok("token".to_string()));
        }
        assert_eq!(state.secret("api_key"), Err(SecretLookupError::Unavailable));
        state.finish_call();

        state.start_call(PluginCallPhase::ToolExecute);
        assert_eq!(state.secret("api_key"), Ok("token".to_string()));
        state.finish_call();
    }

    #[tokio::test]
    async fn cancellation_drops_the_active_config_and_budget() {
        let manifest = secret_manifest(PluginCapability::Tool);
        let scope = secret_scope(&manifest, PluginCapability::Tool, "main", true);
        let services = static_services(manifest, configured("one", "token"));
        let mut store = new_store(PluginStoreSpec::new(scope, services, test_limits(1_000)));

        let cancelled = tokio::time::timeout(std::time::Duration::from_millis(1), async {
            let mut active_call = ActivePluginCall::new(&mut store);
            active_call
                .store_mut()
                .data_mut()
                .public_config()
                .expect("config resolves inside an active frame");
            std::future::pending::<()>().await;
            drop(active_call);
        })
        .await;

        assert!(cancelled.is_err(), "pending invocation must be cancelled");
        assert!(store.data_mut().public_config().is_err());
        assert_eq!(store.data().call_config.phase(), None);
        assert_eq!(store.data().host_calls_remaining, 0);
    }

    #[tokio::test]
    async fn logging_and_inbound_share_the_frame_budget() {
        use bindings::tool::zeroclaw::plugin::logging::{
            Host as LoggingHost, LogLevel, PluginAction, PluginEvent,
        };

        let mut state = PluginState::new(spec([], 1_000));
        state.inbound().enqueue(sample_inbound("budgeted"));
        state.start_call(PluginCallPhase::Standard);
        state.host_calls_remaining = 1;

        <PluginState as LoggingHost>::log_record(
            &mut state,
            LogLevel::Info,
            PluginEvent {
                function_name: "fixture::execute".to_string(),
                action: PluginAction::Note,
                outcome: None,
                duration_ms: None,
                attrs: None,
                message: "budget test".to_string(),
            },
        )
        .await;
        assert_eq!(state.host_calls_remaining, 0);
        assert_eq!(
            <PluginState as bindings::channel::zeroclaw::plugin::inbound::Host>::inbound_pending(
                &mut state
            )
            .await,
            0
        );
        assert_eq!(
            state.inbound().pending(),
            1,
            "exhaustion must not drain data"
        );
        state.finish_call();

        state.start_call(PluginCallPhase::Standard);
        let message =
            <PluginState as bindings::channel::zeroclaw::plugin::inbound::Host>::inbound_poll(
                &mut state,
            )
            .await
            .expect("a fresh frame can reach the queued message");
        assert_eq!(message.id, "budgeted");
        state.finish_call();
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
        let state = PluginState::new(PluginStoreSpec::new(
            granted_scope,
            crate::services::test_host_services(),
            test_limits(0),
        ));

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
        let primary_store = new_store(PluginStoreSpec::new(
            primary.clone(),
            crate::services::test_host_services(),
            test_limits(0),
        ));
        let second_primary_store = new_store(PluginStoreSpec::new(
            primary,
            crate::services::test_host_services(),
            test_limits(0),
        ));
        let backup_store = new_store(PluginStoreSpec::new(
            scope("backup", []),
            crate::services::test_host_services(),
            test_limits(0),
        ));

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
