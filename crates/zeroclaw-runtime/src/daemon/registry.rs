use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use zeroclaw_config::schema::{Config, MqttConfig};

use super::{GatewayReadinessReporter, SocketReadinessReporter};
use crate::LiveConfigAuthority;
use crate::rpc::context::RpcContext;
use crate::rpc::tui_identity::TuiRegistry;

pub type ChannelRegistryClearer = Arc<dyn Fn() + Send + Sync>;

pub type StarterFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;

#[derive(Clone)]
pub struct GatewayReloadControls {
    pub shutdown_tx: watch::Sender<bool>,
    pub reload_tx: watch::Sender<bool>,
    /// The generation's in-process RPC connector, so the supervised gateway
    /// can dial the dispatcher over an in-memory duplex instead of the
    /// socket. `None` for a standalone gateway, which has no daemon to dial.
    pub inproc: Option<crate::rpc::inproc::InprocConnector>,
    pub(crate) channel_generation_control: Option<Arc<super::ChannelGenerationControl>>,
}

impl GatewayReloadControls {
    pub fn standalone(shutdown_tx: watch::Sender<bool>, reload_tx: watch::Sender<bool>) -> Self {
        Self {
            shutdown_tx,
            reload_tx,
            inproc: None,
            channel_generation_control: None,
        }
    }

    #[doc(hidden)]
    pub fn with_channel_generation(
        shutdown_tx: watch::Sender<bool>,
        reload_tx: watch::Sender<bool>,
        registry_clearer: ChannelRegistryClearer,
    ) -> Self {
        Self {
            shutdown_tx,
            reload_tx,
            inproc: None,
            channel_generation_control: Some(Arc::new(super::ChannelGenerationControl::new(Some(
                registry_clearer,
            )))),
        }
    }

    pub fn send(&self, value: bool) -> Result<(), watch::error::SendError<bool>> {
        self.reload_tx.send(value)
    }

    pub fn prepare_channel_generation(&self) -> Option<super::PreparedChannelGenerationDrain> {
        self.channel_generation_control.as_ref()?.prepare()
    }
}

/// The daemon generation's one inbound-authentication state, handed to the
/// supervised gateway so HTTP and RPC authenticate against the same accepted
/// policy over the same live configuration.
///
/// Both surfaces persist under the process-wide config write lock and then
/// publish the policy compiled from the configuration they just wrote into
/// `inbound_auth`, as the next accepted revision. Because they share the one
/// authority and the one live configuration, a revocation persisted through
/// either surface binds the other before the writer returns, and neither can
/// republish policy compiled from a stale copy of the configuration.
#[derive(Clone)]
pub struct DaemonInboundAuthority {
    /// Canonical live pairing authority for native bearer tokens.
    pub pairing: zeroclaw_config::pairing::PairingGuard,
    /// The accepted provider, profile and roster policy, shared with the RPC
    /// context.
    pub inbound_auth: Arc<crate::rpc::auth::RpcInboundAuth>,
    /// The live configuration the RPC context reads and writes.
    pub config: Arc<parking_lot::RwLock<Config>>,
}

pub type GatewayStarter = Box<
    dyn Fn(
            String,
            u16,
            Config,
            LiveConfigAuthority,
            // The daemon's event bus; its observer hook is already installed.
            Option<crate::observability::EventBus>,
            Option<GatewayReloadControls>,
            Option<Arc<TuiRegistry>>,
            // The daemon's one inbound-auth state: pairing guard, accepted
            // policy and live configuration, shared with the RPC context so
            // pairing, revocation and policy changes act on both surfaces at
            // once. `None` only for standalone gateways.
            Option<DaemonInboundAuthority>,
            Option<GatewayReadinessReporter>,
        ) -> StarterFuture
        + Send
        + Sync,
>;

/// Starts the supervised channel orchestrator for one daemon run/reload iteration.
pub type ChannelsStarter =
    Box<dyn Fn(LiveConfigAuthority, CancellationToken) -> StarterFuture + Send + Sync>;

/// Starts the local IPC transport and optionally reports its secured bind.
pub type SocketStarter = Box<
    dyn Fn(
            Arc<RpcContext>,
            CancellationToken,
            Arc<AtomicUsize>,
            Option<SocketReadinessReporter>,
        ) -> StarterFuture
        + Send
        + Sync,
>;

/// Starts an RPC transport using the shared daemon RPC context.
pub type RpcStarter = Box<
    dyn Fn(Arc<RpcContext>, CancellationToken, Arc<AtomicUsize>) -> StarterFuture + Send + Sync,
>;

/// Starts the MQTT SOP listener for one configured MQTT channel alias.
pub type MqttStarter = Box<dyn Fn(MqttConfig) -> StarterFuture + Send + Sync>;

#[derive(Default)]
pub struct DaemonRegistry {
    gateway_start: Option<GatewayStarter>,
    channels_start: Option<ChannelsStarter>,
    channel_registry_clearer: Option<ChannelRegistryClearer>,
    socket_start: Option<SocketStarter>,
    wss_start: Option<RpcStarter>,
    relay_start: Option<RpcStarter>,
    enroll_start: Option<RpcStarter>,
    mqtt_start: Option<MqttStarter>,
    /// Shared SOP engine built by the daemon reload loop. Passed through to
    /// RpcContext so RPC/TUI agent sessions share the same engine.
    sop_engine: Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
    sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,
    sop_driver_handles: Option<crate::sop::SopDriverHandles>,
    plugin_webhooks: Option<Arc<zeroclaw_infra::plugin_webhook::PluginWebhookIngress>>,
}

/// The SOP wiring one daemon generation hands from `main` into the RPC
/// context: the shared engine, the audit logger, and the generation's
/// driver supervisor set.
type SopWiring = (
    Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
    Option<Arc<crate::sop::SopAuditLogger>>,
    Option<crate::sop::SopDriverHandles>,
);

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

    pub fn register_channel_registry_clearer(
        &mut self,
        clearer: ChannelRegistryClearer,
    ) -> &mut Self {
        self.channel_registry_clearer = Some(clearer);
        self
    }

    #[cfg(test)]
    fn has_channels_start(&self) -> bool {
        self.channels_start.is_some()
    }

    pub fn register_socket(&mut self, starter: SocketStarter) -> &mut Self {
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

    pub fn register_relay(&mut self, starter: RpcStarter) -> &mut Self {
        self.relay_start = Some(starter);
        self
    }

    pub(crate) fn has_relay_start(&self) -> bool {
        self.relay_start.is_some()
    }

    pub(crate) fn take_relay_start(&mut self) -> Option<RpcStarter> {
        self.relay_start.take()
    }

    /// Register the certificate enrollment endpoint (the bootstrap surface a
    /// certless client reaches for its first cert). Supervised like the WSS
    /// listener; the starter parks when `[enroll]` is disabled.
    pub fn register_enroll(&mut self, starter: RpcStarter) -> &mut Self {
        self.enroll_start = Some(starter);
        self
    }

    pub(crate) fn has_enroll_start(&self) -> bool {
        self.enroll_start.is_some()
    }

    pub(crate) fn take_enroll_start(&mut self) -> Option<RpcStarter> {
        self.enroll_start.take()
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

    pub(crate) fn take_channel_registry_clearer(&mut self) -> Option<ChannelRegistryClearer> {
        self.channel_registry_clearer.take()
    }

    pub(crate) fn take_socket_start(&mut self) -> Option<SocketStarter> {
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
        sop_driver_handles: Option<crate::sop::SopDriverHandles>,
    ) -> &mut Self {
        self.sop_engine = sop_engine;
        self.sop_audit = sop_audit;
        self.sop_driver_handles = sop_driver_handles;
        self
    }

    pub(crate) fn take_sop_engine(&mut self) -> SopWiring {
        (
            self.sop_engine.take(),
            self.sop_audit.take(),
            self.sop_driver_handles.take(),
        )
    }

    /// Set this daemon iteration's plugin webhook ingress: the instance the
    /// gateway and channel starters were given, placed in the RPC context too.
    pub fn set_plugin_webhooks(
        &mut self,
        ingress: Arc<zeroclaw_infra::plugin_webhook::PluginWebhookIngress>,
    ) -> &mut Self {
        self.plugin_webhooks = Some(ingress);
        self
    }

    pub(crate) fn take_plugin_webhooks(
        &mut self,
    ) -> Option<Arc<zeroclaw_infra::plugin_webhook::PluginWebhookIngress>> {
        self.plugin_webhooks.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_starter() -> GatewayStarter {
        Box::new(|_, _, _, _, _, _, _, _, _| Box::pin(async { Ok(()) }))
    }

    fn channels_starter() -> ChannelsStarter {
        Box::new(|_, _| Box::pin(async { Ok(()) }))
    }

    fn socket_starter() -> SocketStarter {
        Box::new(|_, _, _, _| Box::pin(async { Ok(()) }))
    }

    fn rpc_starter() -> RpcStarter {
        Box::new(|_, _, _| Box::pin(async { Ok(()) }))
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
            .register_socket(socket_starter())
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
            .register_socket(socket_starter())
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

    #[test]
    fn supervised_starters_receive_one_live_config_authority() {
        let authority = LiveConfigAuthority::new(Config::default());
        let expected_config = authority.config();
        let expected_write_lock = authority.config_write_lock();

        let gateway: GatewayStarter = Box::new({
            let expected_config = expected_config.clone();
            let expected_write_lock = expected_write_lock.clone();
            move |_, _, _, received_authority, _, _, _, _, _| {
                assert!(Arc::ptr_eq(&expected_config, &received_authority.config()));
                assert!(Arc::ptr_eq(
                    &expected_write_lock,
                    &received_authority.config_write_lock()
                ));
                Box::pin(async { Ok(()) })
            }
        });
        let channels: ChannelsStarter = Box::new({
            let expected_config = expected_config.clone();
            let expected_write_lock = expected_write_lock.clone();
            move |received_authority, _| {
                assert!(Arc::ptr_eq(&expected_config, &received_authority.config()));
                assert!(Arc::ptr_eq(
                    &expected_write_lock,
                    &received_authority.config_write_lock()
                ));
                Box::pin(async { Ok(()) })
            }
        });

        std::mem::drop(gateway(
            String::new(),
            0,
            Config::default(),
            authority.clone(),
            None,
            None,
            None,
            None,
            None,
        ));
        std::mem::drop(channels(authority, CancellationToken::new()));
    }

    #[test]
    fn plugin_webhook_ingress_is_taken_once() {
        let mut registry = DaemonRegistry::new();
        assert!(registry.take_plugin_webhooks().is_none());

        let ingress = Arc::new(zeroclaw_infra::plugin_webhook::PluginWebhookIngress::new(
            300, 16,
        ));
        registry.set_plugin_webhooks(Arc::clone(&ingress));
        let taken = registry
            .take_plugin_webhooks()
            .expect("the registered ingress is handed over");
        assert!(Arc::ptr_eq(&taken, &ingress));
        assert!(registry.take_plugin_webhooks().is_none());
    }
}
