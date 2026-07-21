use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::{Config, MqttConfig};

use crate::rpc::context::RpcContext;
use crate::rpc::tui_identity::TuiRegistry;

pub type StarterFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

#[derive(Clone)]
pub struct GatewayReloadControls {
    pub shutdown_tx: watch::Sender<bool>,
    pub reload_tx: watch::Sender<bool>,
}

/// Starts the gateway HTTP server for one daemon run/reload iteration.
///
/// The optional broadcast sender carries daemon events, the optional reload
/// controls let gateway/RPC surfaces coordinate in-process reloads, the
/// optional TUI registry powers the gateway's TUI identity endpoints, and
/// the optional readiness sender reports the actual bound address to the
/// daemon's foreground startup echo the moment the listener binds.
pub type GatewayStarter = Box<
    dyn Fn(
            String,
            u16,
            Config,
            Option<broadcast::Sender<Value>>,
            Option<GatewayReloadControls>,
            Option<Arc<TuiRegistry>>,
            Option<watch::Sender<Option<std::net::SocketAddr>>>,
        ) -> StarterFuture
        + Send
        + Sync,
>;

/// Starts the supervised channel orchestrator for one daemon run/reload iteration.
pub type ChannelsStarter = Box<dyn Fn(Config, CancellationToken) -> StarterFuture + Send + Sync>;

/// Starts an RPC transport using the shared daemon RPC context.
///
/// The optional readiness sender reports endpoint readiness to the daemon's
/// foreground startup echo the moment the transport's listener binds
/// Transports without an echo consumer receive `None`.
pub type RpcStarter = Box<
    dyn Fn(
            Arc<RpcContext>,
            CancellationToken,
            Arc<AtomicUsize>,
            Option<watch::Sender<bool>>,
        ) -> StarterFuture
        + Send
        + Sync,
>;

/// Starts the MQTT SOP listener for one configured MQTT channel alias.
pub type MqttStarter = Box<dyn Fn(MqttConfig) -> StarterFuture + Send + Sync>;

#[derive(Default)]
pub struct DaemonRegistry {
    gateway_start: Option<GatewayStarter>,
    channels_start: Option<ChannelsStarter>,
    socket_start: Option<RpcStarter>,
    wss_start: Option<RpcStarter>,
    mqtt_start: Option<MqttStarter>,
    /// Shared SOP engine built by the daemon reload loop. Passed through to
    /// RpcContext so RPC/TUI agent sessions share the same engine.
    sop_engine: Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
    sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,
}

impl DaemonRegistry {
    /// Create an empty registry. Missing starters are treated as unwired
    /// optional subsystems by `daemon::run`.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_gateway(&mut self, starter: GatewayStarter) -> &mut Self {
        self.gateway_start = Some(starter);
        self
    }

    #[cfg(test)]
    fn has_gateway_start(&self) -> bool {
        self.gateway_start.is_some()
    }

    pub fn register_channels(&mut self, starter: ChannelsStarter) -> &mut Self {
        self.channels_start = Some(starter);
        self
    }

    #[cfg(test)]
    fn has_channels_start(&self) -> bool {
        self.channels_start.is_some()
    }

    pub fn register_socket(&mut self, starter: RpcStarter) -> &mut Self {
        self.socket_start = Some(starter);
        self
    }

    pub(crate) fn has_socket_start(&self) -> bool {
        self.socket_start.is_some()
    }

    pub fn register_wss(&mut self, starter: RpcStarter) -> &mut Self {
        self.wss_start = Some(starter);
        self
    }

    pub(crate) fn has_wss_start(&self) -> bool {
        self.wss_start.is_some()
    }

    pub fn register_mqtt(&mut self, starter: MqttStarter) -> &mut Self {
        self.mqtt_start = Some(starter);
        self
    }

    #[cfg(test)]
    fn has_mqtt_start(&self) -> bool {
        self.mqtt_start.is_some()
    }

    pub(crate) fn take_gateway_start(&mut self) -> Option<GatewayStarter> {
        self.gateway_start.take()
    }

    pub(crate) fn take_channels_start(&mut self) -> Option<ChannelsStarter> {
        self.channels_start.take()
    }

    pub(crate) fn take_socket_start(&mut self) -> Option<RpcStarter> {
        self.socket_start.take()
    }

    pub(crate) fn take_wss_start(&mut self) -> Option<RpcStarter> {
        self.wss_start.take()
    }

    pub(crate) fn take_mqtt_start(&mut self) -> Option<MqttStarter> {
        self.mqtt_start.take()
    }

    /// Set the shared SOP engine for this daemon iteration.
    pub fn set_sop_engine(
        &mut self,
        sop_engine: Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
        sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,
    ) -> &mut Self {
        self.sop_engine = sop_engine;
        self.sop_audit = sop_audit;
        self
    }

    pub(crate) fn take_sop_engine(
        &mut self,
    ) -> (
        Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
        Option<Arc<crate::sop::SopAuditLogger>>,
    ) {
        (self.sop_engine.take(), self.sop_audit.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_starter() -> GatewayStarter {
        Box::new(|_, _, _, _, _, _, _| Box::pin(async { Ok(()) }))
    }

    fn channels_starter() -> ChannelsStarter {
        Box::new(|_, _| Box::pin(async { Ok(()) }))
    }

    fn rpc_starter() -> RpcStarter {
        Box::new(|_, _, _, _| Box::pin(async { Ok(()) }))
    }

    fn mqtt_starter() -> MqttStarter {
        Box::new(|_| Box::pin(async { Ok(()) }))
    }

    #[test]
    fn new_registry_has_no_start_hooks() {
        let registry = DaemonRegistry::new();

        assert!(!registry.has_gateway_start());
        assert!(!registry.has_channels_start());
        assert!(!registry.has_socket_start());
        assert!(!registry.has_wss_start());
        assert!(!registry.has_mqtt_start());
    }

    #[test]
    fn builder_records_typed_start_hooks() {
        let mut registry = DaemonRegistry::new();
        registry
            .register_gateway(gateway_starter())
            .register_channels(channels_starter())
            .register_socket(rpc_starter())
            .register_wss(rpc_starter())
            .register_mqtt(mqtt_starter());

        assert!(registry.has_gateway_start());
        assert!(registry.has_channels_start());
        assert!(registry.has_socket_start());
        assert!(registry.has_wss_start());
        assert!(registry.has_mqtt_start());
    }

    #[test]
    fn taking_start_hooks_consumes_slots() {
        let mut registry = DaemonRegistry::new();
        registry
            .register_gateway(gateway_starter())
            .register_channels(channels_starter())
            .register_socket(rpc_starter())
            .register_wss(rpc_starter())
            .register_mqtt(mqtt_starter());

        assert!(registry.take_gateway_start().is_some());
        assert!(registry.take_channels_start().is_some());
        assert!(registry.take_socket_start().is_some());
        assert!(registry.take_wss_start().is_some());
        assert!(registry.take_mqtt_start().is_some());

        assert!(!registry.has_gateway_start());
        assert!(!registry.has_channels_start());
        assert!(!registry.has_socket_start());
        assert!(!registry.has_wss_start());
        assert!(!registry.has_mqtt_start());
    }
}
