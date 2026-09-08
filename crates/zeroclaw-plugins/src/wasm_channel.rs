//! Channel adapter: `WasmChannel` implements `zeroclaw_api::channel::Channel`
//! backed by the `channel-plugin` component world.

use crate::component::InboundQueue;
use crate::component::bindings::channel::ChannelPlugin;
use crate::component::bindings::channel::exports::zeroclaw::plugin::channel::{
    ApprovalPosition as WitApprovalPosition, ApprovalRequest as WitApprovalRequest,
    ApprovalResponse as WitApprovalResponse, ChannelCapabilities,
    InboundMessage as WitInboundMessage, MediaAttachment as WitMediaAttachment,
    SendMessage as WitSendMessage, WebhookRejection as WitWebhookRejection,
};
use crate::component::{
    PluginState, PluginStoreSpec, WarmPluginState, call_channel, call_channel_store, call_store,
    engine, load_component, wt, wt_instantiate,
};
use crate::endpoint::PluginChannelEndpoint;
use crate::services::PluginHostServices;
use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use wasmtime::Store;
use wasmtime::component::Component;
use wasmtime::component::Linker;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_api::webhook::{
    RawWebhook, WebhookIdempotency, WebhookReject, WebhookReservation, WebhookReservationStatus,
    WebhookReservationToken,
};

/// Live host policy for a normalized channel-plugin sender.
pub type SenderAuthorizer = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// A channel backed by a WIT component-model plugin.
pub struct WasmChannel {
    endpoint: PluginChannelEndpoint,
    capabilities: ChannelCapabilities,
    state: Mutex<WarmPluginState<ChannelPlugin>>,
    factory: ChannelInstanceFactory,
    inbound: InboundQueue,
    // Static component metadata, fixed for one admitted logical binding.
    // Changing the external account or these capabilities requires rebuilding
    // the channel; point-of-use config refresh is only for that same binding.
    cached_self_handle: Option<String>,
    cached_self_addressed_mention: Option<String>,
    cached_multi_message_delay_ms: u64,
    poll_healthy: AtomicBool,
    /// Applied at the final host boundary for polling and webhook delivery.
    sender_authorizer: SenderAuthorizer,
    /// Drain end of the bounded gateway-to-plugin queue. Set once by runtime
    /// route publication and taken by the supervised listener.
    webhook_rx: std::sync::Mutex<Option<mpsc::Receiver<RawWebhook>>>,
}

#[derive(Clone)]
struct ChannelInstanceFactory {
    component: Arc<Component>,
    /// Required host-service bundle carrying the live config resolver. A
    /// rebuilt instance re-runs the no-arg `configure()` and re-resolves
    /// config and secrets through these services at each point of use, so an
    /// interrupted instance is reconstructed against the same canonical config
    /// source rather than a captured plaintext snapshot. The reinstantiation
    /// metadata check still guards against the external account or capabilities
    /// drifting under a rebuilt instance.
    services: PluginHostServices,
    limits: crate::component::PluginLimits,
}

struct ChannelInstance {
    state: (Store<PluginState>, ChannelPlugin),
    capabilities: ChannelCapabilities,
    self_handle: Option<String>,
    self_addressed_mention: Option<String>,
    multi_message_delay_ms: u64,
}

/// Whether the listen loop's last `poll-message` did not trap. A channel whose
/// poll bridge is trapping is reported unhealthy even when the plugin exposes no
/// `health-check` export, so a broken plugin cannot masquerade as idle forever.
fn poll_health_ok(flag: &AtomicBool) -> bool {
    flag.load(Ordering::Relaxed)
}

fn mark_poll_healthy(flag: &AtomicBool, healthy: bool) {
    flag.store(healthy, Ordering::Relaxed);
}

fn deny_all_senders() -> SenderAuthorizer {
    Arc::new(|_| false)
}

impl Attributable for WasmChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::Plugin)
    }
    fn alias(&self) -> &str {
        self.endpoint.alias()
    }
}

fn build_linker(http: bool) -> Result<Linker<PluginState>> {
    let mut linker = Linker::new(engine());
    crate::component::add_wasi(&mut linker)?;
    if http {
        crate::component::add_wasi_http(&mut linker)?;
    }
    let mut options = crate::component::bindings::channel::LinkOptions::default();
    options.plugins_wit_v0(true);
    wt(
        ChannelPlugin::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
            &mut linker,
            &options,
            |s| s,
        ),
        "failed to add channel plugin imports to linker",
    )?;
    Ok(linker)
}

/// Build the sandboxed store backing a channel plugin.
///
/// Channel outbound HTTP is intentionally UNAVAILABLE on this host build: the
/// store is constructed WITHOUT [`PluginStoreSpec::with_granted_http`], so even
/// a channel whose manifest grants `HttpClient` receives no `wasi:http` surface.
/// [`PluginState::http_enabled`] is therefore false, [`build_linker`] never
/// wires `wasi:http`, and the guest cannot reach the network at all — genuinely
/// unavailable, not "deny-all".
///
/// The reason this is fail-closed rather than governed: enabling `wasi:http`
/// here would attach `wasmtime_wasi_http::p2::default_hooks()` — UNRESTRICTED
/// egress with no destination allowlist, no metadata/loopback denial, no DNS
/// pinning, and no connection budget. That is a fresh instance of the egress
/// SSRF hole (issue 9395), which making channel construction a production
/// caller must not introduce. Egress-governed channel HTTP is a follow-up that
/// threads the `EgressHostService` through channel construction on top of the
/// `PluginEgressHooks` work (issue 9582); until that lands, the channel surface
/// stays off.
fn new_channel_store(
    scope: crate::instance::PluginInstanceScope,
    services: PluginHostServices,
    limits: crate::component::PluginLimits,
    inbound: InboundQueue,
) -> Store<PluginState> {
    crate::component::new_store(PluginStoreSpec::new(scope, services, limits).with_inbound(inbound))
}

impl WasmChannel {
    /// Build one admitted channel component.
    ///
    /// Inbound sender policy starts deny-all. A production owner must install
    /// its live host policy with [`Self::with_sender_authorizer`] before calling
    /// [`Channel::listen`].
    pub async fn from_wasm(
        endpoint: PluginChannelEndpoint,
        wasm_path: &Path,
        services: &PluginHostServices,
        limits: crate::component::PluginLimits,
    ) -> Result<Self> {
        // Resolve and validate the operator config before any guest code is
        // loaded, so an invalid section rejects registration rather than
        // reaching a running instance. Config then stays host-owned and is
        // served live through point-of-use imports; the factory replays the
        // no-arg `configure()` against these same services when it rebuilds an
        // interrupted instance, so a rebuilt instance re-resolves config
        // rather than replaying a captured plaintext snapshot.
        services.resolve_config(endpoint.scope())?;
        let inbound = InboundQueue::default();
        let factory = ChannelInstanceFactory {
            component: Arc::new(load_component(wasm_path)?),
            services: services.clone(),
            limits,
        };
        let instance = factory.instantiate(&endpoint, inbound.clone()).await?;

        Ok(Self {
            endpoint,
            capabilities: instance.capabilities,
            state: Mutex::new(Some(instance.state)),
            factory,
            inbound,
            cached_self_handle: instance.self_handle,
            cached_self_addressed_mention: instance.self_addressed_mention,
            cached_multi_message_delay_ms: instance.multi_message_delay_ms,
            poll_healthy: AtomicBool::new(true),
            sender_authorizer: deny_all_senders(),
            webhook_rx: std::sync::Mutex::new(None),
        })
    }

    /// Rebuild an interrupted warm instance from the host-owned component,
    /// scope, generation-scoped config snapshot, and limits, reattaching the
    /// queued inbound backlog. A message the interrupted call had already
    /// dequeued through `inbound-poll` is not requeued: inbound delivery to
    /// the guest is at-most-once across an interruption, and only the
    /// still-queued backlog survives reconstruction.
    /// This does not lock `state`, so the shared call boundary may invoke it
    /// while holding the slot lock.
    async fn reinstantiate(&self) -> Result<(Store<PluginState>, ChannelPlugin)> {
        let instance = self
            .factory
            .instantiate(&self.endpoint, self.inbound.clone())
            .await?;
        if instance.capabilities != self.capabilities
            || instance.self_handle != self.cached_self_handle
            || instance.self_addressed_mention != self.cached_self_addressed_mention
            || instance.multi_message_delay_ms != self.cached_multi_message_delay_ms
        {
            anyhow::bail!(
                "channel plugin metadata changed while recreating an interrupted instance"
            );
        }
        Ok(instance.state)
    }

    /// Handle to this channel's inbound queue. A host-run listener clones it and
    /// calls [`InboundQueue::enqueue`] for each received message; the plugin
    /// drains them through its imported `inbound` interface.
    pub fn inbound(&self) -> InboundQueue {
        self.inbound.clone()
    }

    /// Install the live host sender policy used by every inbound bridge.
    /// Empty or omitted policy is never interpreted as allow-all.
    #[must_use]
    pub fn with_sender_authorizer(mut self, authorizer: SenderAuthorizer) -> Self {
        self.sender_authorizer = authorizer;
        self
    }

    /// Whether this component advertises host-fed webhook ingress.
    #[must_use]
    pub fn has_webhook_ingress(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::WEBHOOK_INGRESS)
    }

    /// Resolve the guest-declared route before the runtime publishes any sink.
    pub async fn webhook_path(&self) -> Result<Option<String>> {
        if !self.has_webhook_ingress() {
            return Ok(None);
        }
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_webhook_path(store)
                        .await,
                    "channel.webhook-path failed",
                )
            }
        )
    }

    /// Attach the bounded gateway sink before this channel enters `listen`.
    pub fn set_webhook_receiver(&self, receiver: mpsc::Receiver<RawWebhook>) {
        *self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(receiver);
    }
}

impl ChannelInstanceFactory {
    async fn instantiate(
        &self,
        endpoint: &PluginChannelEndpoint,
        inbound: InboundQueue,
    ) -> Result<ChannelInstance> {
        // Channel outbound HTTP is intentionally withheld: routing through
        // `new_channel_store` builds the store WITHOUT `with_granted_http`, so
        // even a channel whose manifest grants `HttpClient` receives no
        // `wasi:http` surface. Granting it would attach the ungoverned
        // `default_hooks()` egress (a fresh instance of the SSRF hole, issue
        // 9395); `new_channel_store` carries the full rationale and the issue
        // 9582 egress-governed follow-up. Config and secrets still reach the
        // guest through the live host services threaded into the store here.
        let mut store = new_channel_store(
            endpoint.scope().clone(),
            self.services.clone(),
            self.limits,
            inbound,
        );
        let http = store.data().http_enabled();
        let linker = build_linker(http)?;
        crate::component::ensure_http_coherent(&store, http)?;
        let bindings = call_store!(store, async |store: &mut Store<PluginState>| {
            wt_instantiate(
                ChannelPlugin::instantiate_async(store, self.component.as_ref(), &linker).await,
                "failed to instantiate channel plugin",
            )
        })?;

        // Let the plugin initialize before static discovery. Config stays
        // host-owned and is served live through the point-of-use imports in
        // this channel-service frame, so the plugin resolves config and secrets
        // itself rather than receiving them as a `configure` argument.
        call_channel_store!(store, async |store: &mut Store<PluginState>| {
            wt(
                bindings
                    .zeroclaw_plugin_channel()
                    .call_configure(store)
                    .await,
                "channel.configure trapped",
            )?
            .map_err(anyhow::Error::msg)
        })?;

        let capabilities = call_store!(store, async |store: &mut Store<PluginState>| {
            wt(
                bindings
                    .zeroclaw_plugin_channel()
                    .call_get_channel_capabilities(store)
                    .await,
                "channel.get-channel-capabilities failed",
            )
        })?;

        let cached_self_handle = if capabilities.contains(ChannelCapabilities::SELF_HANDLE) {
            call_store!(store, async |store: &mut Store<PluginState>| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_self_handle(store)
                        .await,
                    "channel.self-handle failed",
                )
            })?
        } else {
            None
        };
        let cached_self_addressed_mention =
            if capabilities.contains(ChannelCapabilities::SELF_ADDRESSED_MENTION) {
                call_store!(store, async |store: &mut Store<PluginState>| {
                    wt(
                        bindings
                            .zeroclaw_plugin_channel()
                            .call_self_addressed_mention(store)
                            .await,
                        "channel.self-addressed-mention failed",
                    )
                })?
            } else {
                None
            };
        let cached_multi_message_delay_ms =
            if capabilities.contains(ChannelCapabilities::MULTI_MESSAGE_DELAY_MS) {
                call_store!(store, async |store: &mut Store<PluginState>| {
                    wt(
                        bindings
                            .zeroclaw_plugin_channel()
                            .call_multi_message_delay_ms(store)
                            .await,
                        "channel.multi-message-delay-ms failed",
                    )
                })?
            } else {
                800
            };

        Ok(ChannelInstance {
            state: (store, bindings),
            capabilities,
            self_handle: cached_self_handle,
            self_addressed_mention: cached_self_addressed_mention,
            multi_message_delay_ms: cached_multi_message_delay_ms,
        })
    }

    /// Run webhook authentication and parsing in a disposable configured
    /// instance. Cancelling this future drops only this store; it cannot poison
    /// the warm poll/send instance.
    async fn parse_webhook(
        &self,
        endpoint: &PluginChannelEndpoint,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Result<Vec<WitInboundMessage>, WitWebhookRejection>> {
        let instance = self.instantiate(endpoint, InboundQueue::default()).await?;
        let (mut store, bindings) = instance.state;
        call_channel_store!(store, async |store: &mut Store<PluginState>| {
            wt(
                bindings
                    .zeroclaw_plugin_channel()
                    .call_parse_webhook(store, headers, body)
                    .await,
                "channel.parse-webhook trapped",
            )
        })
    }
}

fn to_wit_media(a: &MediaAttachment) -> WitMediaAttachment {
    WitMediaAttachment {
        file_name: a.file_name.clone(),
        data: a.data.clone(),
        mime_type: a.mime_type.clone(),
    }
}

fn from_wit_media(a: WitMediaAttachment) -> MediaAttachment {
    MediaAttachment {
        file_name: a.file_name,
        data: a.data,
        mime_type: a.mime_type,
        // The plugin ABI carries bytes, not the host's text rendering, so a
        // plugin-supplied attachment is unreferenced by definition. Widening
        // the WIT record would be a breaking ABI change for no gain: a plugin
        // channel has no marker for the pipeline to join against.
        marker: None,
    }
}

fn to_wit_send(msg: &SendMessage) -> WitSendMessage {
    WitSendMessage {
        content: msg.content.clone(),
        recipient: msg.recipient.clone(),
        subject: msg.subject.clone(),
        thread_ts: msg.thread_ts.clone(),
        attachments: msg.attachments.iter().map(to_wit_media).collect(),
        in_reply_to: msg.in_reply_to.clone(),
    }
}

fn from_wit_inbound(msg: WitInboundMessage, endpoint: &PluginChannelEndpoint) -> ChannelMessage {
    ChannelMessage {
        id: msg.id,
        sender: msg.sender,
        reply_target: msg.reply_target,
        content: msg.content,
        // Routing identity is issued by the host. Guest-supplied channel and
        // alias fields cannot select a different owner or session namespace.
        channel: endpoint.channel_type().to_string(),
        channel_alias: Some(endpoint.alias().to_string()),
        timestamp: msg.timestamp,
        thread_ts: msg.thread_ts,
        interruption_scope_id: msg.interruption_scope_id,
        attachments: msg.attachments.into_iter().map(from_wit_media).collect(),
        subject: msg.subject,
        ..Default::default()
    }
}

fn sender_is_authorized(
    authorizer: &SenderAuthorizer,
    endpoint: &PluginChannelEndpoint,
    message: &ChannelMessage,
    ingress: &str,
) -> bool {
    if authorizer(&message.sender) {
        return true;
    }

    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "plugin": endpoint.instance_id().package(),
                "channel_alias": endpoint.alias(),
                "ingress": ingress,
                "error_key": "plugin_channel_sender_denied",
            })),
        "Ignoring channel-plugin inbound from an unauthorized sender"
    );
    false
}

struct OwnedWebhookReservation {
    idempotency: WebhookIdempotency,
    token: Option<WebhookReservationToken>,
}

impl OwnedWebhookReservation {
    fn new(idempotency: WebhookIdempotency, token: WebhookReservationToken) -> Self {
        Self {
            idempotency,
            token: Some(token),
        }
    }

    fn commit(mut self) -> bool {
        let Some(token) = self.token.take() else {
            return false;
        };
        self.idempotency.commit(&token)
    }
}

impl Drop for OwnedWebhookReservation {
    fn drop(&mut self) {
        if let Some(token) = self.token.take() {
            let _ = self.idempotency.rollback(&token);
        }
    }
}

enum DeliveryReservation {
    Untracked,
    Owner(OwnedWebhookReservation),
    Duplicate,
}

const WEBHOOK_DIAGNOSTIC_MAX_CHARS: usize = 2_048;

fn bounded_webhook_detail(detail: impl AsRef<str>) -> String {
    let detail = detail.as_ref();
    if detail.chars().count() <= WEBHOOK_DIAGNOSTIC_MAX_CHARS {
        return detail.to_string();
    }
    let mut bounded: String = detail
        .chars()
        .take(WEBHOOK_DIAGNOSTIC_MAX_CHARS.saturating_sub(1))
        .collect();
    bounded.push('…');
    bounded
}

fn log_webhook_rejection(endpoint: &PluginChannelEndpoint, rejection: &WebhookReject) {
    let (error_key, detail) = match rejection {
        WebhookReject::Unauthorized(detail) => {
            ("plugin_webhook_unauthorized", Some(detail.as_str()))
        }
        WebhookReject::BadRequest(detail) => ("plugin_webhook_invalid", Some(detail.as_str())),
        WebhookReject::Unavailable(detail) => ("plugin_webhook_unavailable", Some(detail.as_str())),
        WebhookReject::Timeout => ("plugin_webhook_timeout", None),
    };
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "plugin": endpoint.instance_id().package(),
                "channel_alias": endpoint.alias(),
                "error": detail,
                "error_key": error_key,
            })),
        "Channel-plugin webhook request rejected"
    );
}

async fn reserve_webhook_delivery(
    idempotency: Option<&WebhookIdempotency>,
    message_id: &str,
    cancellation: &zeroclaw_api::webhook::WebhookCancellation,
) -> Result<DeliveryReservation, WebhookReject> {
    let Some(idempotency) = idempotency else {
        return Ok(DeliveryReservation::Untracked);
    };
    if message_id.is_empty() {
        return Ok(DeliveryReservation::Untracked);
    }

    loop {
        match idempotency.begin(message_id) {
            WebhookReservation::Owner(token) => {
                return Ok(DeliveryReservation::Owner(OwnedWebhookReservation::new(
                    idempotency.clone(),
                    token,
                )));
            }
            WebhookReservation::Committed => return Ok(DeliveryReservation::Duplicate),
            WebhookReservation::Unavailable => {
                return Err(WebhookReject::Unavailable(
                    "plugin webhook idempotency store is saturated".to_string(),
                ));
            }
            WebhookReservation::InFlight(mut waiter) => {
                let status = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(WebhookReject::Timeout),
                    status = waiter.wait() => status,
                };
                match status {
                    WebhookReservationStatus::Committed => {
                        return Ok(DeliveryReservation::Duplicate);
                    }
                    WebhookReservationStatus::RolledBack => continue,
                    WebhookReservationStatus::InFlight => {
                        return Err(WebhookReject::Unavailable(
                            "plugin webhook idempotency waiter remained in flight".to_string(),
                        ));
                    }
                }
            }
        }
    }
}

async fn deliver_webhook_messages(
    messages: Vec<WitInboundMessage>,
    tx: &mpsc::Sender<ChannelMessage>,
    authorizer: &SenderAuthorizer,
    endpoint: &PluginChannelEndpoint,
    cancellation: &zeroclaw_api::webhook::WebhookCancellation,
    idempotency: Option<&WebhookIdempotency>,
) -> Result<(), WebhookReject> {
    for message in messages {
        let message = from_wit_inbound(message, endpoint);
        if !sender_is_authorized(authorizer, endpoint, &message, "webhook") {
            continue;
        }

        let reservation =
            reserve_webhook_delivery(idempotency, message.id.trim(), cancellation).await?;
        if matches!(reservation, DeliveryReservation::Duplicate) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "plugin": endpoint.instance_id().package(),
                        "channel_alias": endpoint.alias(),
                        "error_key": "plugin_webhook_duplicate",
                    })),
                "Duplicate plugin webhook message ignored"
            );
            continue;
        }

        let sent = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(WebhookReject::Timeout),
            result = tx.send(message) => result,
        };
        sent.map_err(|_| {
            WebhookReject::Unavailable("channel inbound receiver closed".to_string())
        })?;

        if let DeliveryReservation::Owner(owner) = reservation
            && !owner.commit()
        {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": endpoint.instance_id().package(),
                        "channel_alias": endpoint.alias(),
                        "error_key": "plugin_webhook_reservation_lost",
                    })),
                "Plugin webhook delivery completed after its idempotency ownership was lost"
            );
        }
    }

    Ok(())
}

fn to_wit_approval_request(req: &ChannelApprovalRequest) -> WitApprovalRequest {
    WitApprovalRequest {
        tool_name: req.tool_name.clone(),
        arguments_summary: req.arguments_summary.clone(),
        raw_arguments: req.raw_arguments.as_ref().map(|v| v.to_string()),
        position: req.position.map(|p| WitApprovalPosition {
            index: p.index,
            total: p.total,
        }),
    }
}

fn from_wit_approval_response(r: WitApprovalResponse) -> ChannelApprovalResponse {
    match r {
        WitApprovalResponse::Approve => ChannelApprovalResponse::Approve,
        WitApprovalResponse::Deny => ChannelApprovalResponse::Deny,
        WitApprovalResponse::AlwaysApprove => ChannelApprovalResponse::AlwaysApprove,
        WitApprovalResponse::DenyWithEdit(s) => {
            ChannelApprovalResponse::DenyWithEdit { replacement: s }
        }
    }
}

#[async_trait]
impl Channel for WasmChannel {
    fn name(&self) -> &str {
        self.endpoint.channel_type()
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let wit_msg = to_wit_send(message);
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_send(store, &wit_msg)
                        .await,
                    "channel.send trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let webhook_receiver = self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();

        // Keep both bridges inside Channel::listen. The orchestrator owns
        // cancellation and restart supervision; detached tasks would outlive
        // the channel generation that published their route.
        let poll_loop = async {
            const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
            const MAX_BACKOFF: Duration = Duration::from_millis(500);
            let mut backoff = INITIAL_BACKOFF;
            loop {
                let polled: Result<Option<WitInboundMessage>> = call_channel!(
                    self,
                    async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                        wt(
                            bindings
                                .zeroclaw_plugin_channel()
                                .call_poll_message(store)
                                .await,
                            "channel.poll-message trapped",
                        )
                    }
                );
                match polled {
                    Ok(Some(wit_msg)) => {
                        mark_poll_healthy(&self.poll_healthy, true);
                        backoff = INITIAL_BACKOFF;
                        let message = from_wit_inbound(wit_msg, &self.endpoint);
                        if !sender_is_authorized(
                            &self.sender_authorizer,
                            &self.endpoint,
                            &message,
                            "poll",
                        ) {
                            continue;
                        }
                        if tx.send(message).await.is_err() {
                            return Ok(());
                        }
                        continue;
                    }
                    Ok(None) => mark_poll_healthy(&self.poll_healthy, true),
                    Err(error) => {
                        mark_poll_healthy(&self.poll_healthy, false);
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Inbound
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "channel": self.endpoint.channel_type(),
                                "channel_alias": self.endpoint.alias(),
                                "error": bounded_webhook_detail(format!("{error:#}")),
                            })),
                            "Channel plugin poll-message trapped; backing off"
                        );
                    }
                }

                tokio::select! {
                    () = tx.closed() => return Ok(()),
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        };
        tokio::pin!(poll_loop);

        let Some(mut webhook_receiver) = webhook_receiver else {
            return poll_loop.await;
        };
        let webhook_factory = self.factory.clone();
        let webhook_endpoint = self.endpoint.clone();
        let webhook_authorizer = Arc::clone(&self.sender_authorizer);
        let webhook_tx = tx.clone();
        let webhook_loop = async move {
            while let Some(RawWebhook {
                headers,
                body,
                cancellation,
                idempotency,
                reply,
            }) = webhook_receiver.recv().await
            {
                let parsed = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => None,
                    result = webhook_factory.parse_webhook(&webhook_endpoint, &headers, &body) => {
                        Some(result)
                    }
                };
                let outcome = match parsed {
                    Some(Ok(Ok(messages))) => {
                        deliver_webhook_messages(
                            messages,
                            &webhook_tx,
                            &webhook_authorizer,
                            &webhook_endpoint,
                            &cancellation,
                            idempotency.as_ref(),
                        )
                        .await
                    }
                    Some(Ok(Err(WitWebhookRejection::Unauthorized(detail)))) => {
                        Err(WebhookReject::Unauthorized(bounded_webhook_detail(detail)))
                    }
                    Some(Ok(Err(WitWebhookRejection::BadRequest(detail)))) => {
                        Err(WebhookReject::BadRequest(bounded_webhook_detail(detail)))
                    }
                    Some(Err(error)) => Err(WebhookReject::Unavailable(bounded_webhook_detail(
                        format!("{error:#}"),
                    ))),
                    None => Err(WebhookReject::Timeout),
                };
                if let Err(rejection) = &outcome {
                    log_webhook_rejection(&webhook_endpoint, rejection);
                }
                let _ = reply.send(outcome);
            }
        };
        tokio::pin!(webhook_loop);

        tokio::select! {
            result = &mut poll_loop => result,
            () = &mut webhook_loop => poll_loop.await,
        }
    }

    async fn health_check(&self) -> bool {
        if !poll_health_ok(&self.poll_healthy) {
            return false;
        }
        if !self
            .capabilities
            .contains(ChannelCapabilities::HEALTH_CHECK)
        {
            return true;
        }
        let result: Result<bool> = call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_health_check(store)
                        .await,
                    "channel.health-check failed",
                )
            }
        );
        result.unwrap_or(false)
    }

    fn self_handle(&self) -> Option<String> {
        self.cached_self_handle.clone()
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.cached_self_addressed_mention.clone()
    }

    fn drop_self_messages(&self, msg: &ChannelMessage) -> bool {
        let Some(handle) = self.self_handle() else {
            return false;
        };
        let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
        let sender_norm = msg.sender.trim_start_matches('@').to_ascii_lowercase();
        !handle_norm.is_empty() && handle_norm == sender_norm
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::START_TYPING)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_start_typing(store, &recipient)
                        .await,
                    "channel.start-typing trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn stop_typing(&self, recipient: &str) -> Result<()> {
        if !self.capabilities.contains(ChannelCapabilities::STOP_TYPING) {
            return Ok(());
        }
        let recipient = recipient.to_string();
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_stop_typing(store, &recipient)
                        .await,
                    "channel.stop-typing trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_draft_updates(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_DRAFT_UPDATES)
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        if !self.capabilities.contains(ChannelCapabilities::SEND_DRAFT) {
            return Ok(None);
        }
        let wit_msg = to_wit_send(message);
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_send_draft(store, &wit_msg)
                        .await,
                    "channel.send-draft trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UPDATE_DRAFT)
        {
            return Ok(());
        }
        let (recipient, message_id, text) = (
            recipient.to_string(),
            message_id.to_string(),
            text.to_string(),
        );
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_update_draft(store, &recipient, &message_id, &text)
                        .await,
                    "channel.update-draft trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UPDATE_DRAFT_PROGRESS)
        {
            return Ok(());
        }
        let (recipient, message_id, text) = (
            recipient.to_string(),
            message_id.to_string(),
            text.to_string(),
        );
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_update_draft_progress(store, &recipient, &message_id, &text)
                        .await,
                    "channel.update-draft-progress trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::FINALIZE_DRAFT)
        {
            return Ok(());
        }
        let (recipient, message_id, text) = (
            recipient.to_string(),
            message_id.to_string(),
            text.to_string(),
        );
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_finalize_draft(store, &recipient, &message_id, &text)
                        .await,
                    "channel.finalize-draft trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::CANCEL_DRAFT)
        {
            return Ok(());
        }
        let (recipient, message_id) = (recipient.to_string(), message_id.to_string());
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_cancel_draft(store, &recipient, &message_id)
                        .await,
                    "channel.cancel-draft trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_MULTI_MESSAGE_STREAMING)
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.cached_multi_message_delay_ms
    }

    async fn add_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::ADD_REACTION)
        {
            return Ok(());
        }
        let (channel_id, message_id, emoji) = (
            channel_id.to_string(),
            message_id.to_string(),
            emoji.to_string(),
        );
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_add_reaction(store, &channel_id, &message_id, &emoji)
                        .await,
                    "channel.add-reaction trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn remove_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REMOVE_REACTION)
        {
            return Ok(());
        }
        let (channel_id, message_id, emoji) = (
            channel_id.to_string(),
            message_id.to_string(),
            emoji.to_string(),
        );
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_remove_reaction(store, &channel_id, &message_id, &emoji)
                        .await,
                    "channel.remove-reaction trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn pin_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        if !self.capabilities.contains(ChannelCapabilities::PIN_MESSAGE) {
            return Ok(());
        }
        let (channel_id, message_id) = (channel_id.to_string(), message_id.to_string());
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_pin_message(store, &channel_id, &message_id)
                        .await,
                    "channel.pin-message trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn unpin_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UNPIN_MESSAGE)
        {
            return Ok(());
        }
        let (channel_id, message_id) = (channel_id.to_string(), message_id.to_string());
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_unpin_message(store, &channel_id, &message_id)
                        .await,
                    "channel.unpin-message trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn redact_message(
        &self,
        channel_id: &str,
        message_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REDACT_MESSAGE)
        {
            return Ok(());
        }
        let (channel_id, message_id) = (channel_id.to_string(), message_id.to_string());
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_redact_message(store, &channel_id, &message_id, reason.as_deref())
                        .await,
                    "channel.redact-message trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REQUEST_APPROVAL)
        {
            return Ok(None);
        }
        let recipient = recipient.to_string();
        let wit_req = to_wit_approval_request(request);
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                let out = wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_request_approval(store, &recipient, &wit_req)
                        .await,
                    "channel.request-approval trapped",
                )?
                .map_err(anyhow::Error::msg)?;
                Ok(out.map(from_wit_approval_response))
            }
        )
    }

    async fn request_choice(
        &self,
        question: &str,
        choices: &[String],
        timeout: Duration,
    ) -> Result<Option<String>> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REQUEST_CHOICE)
        {
            return Ok(None);
        }
        let question = question.to_string();
        let choices = choices.to_vec();
        let timeout_secs = timeout.as_secs();
        call_channel!(
            self,
            async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                wt(
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_request_choice(store, &question, &choices, timeout_secs)
                        .await,
                    "channel.request-choice trapped",
                )?
                .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_free_form_ask(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_FREE_FORM_ASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCapability;
    use crate::config::PluginConfigResolver;

    #[test]
    fn media_round_trip() {
        let ma = MediaAttachment {
            file_name: "photo.jpg".into(),
            data: vec![0xFF, 0xD8, 0xFF],
            mime_type: Some("image/jpeg".into()),
            marker: None,
        };
        let back = from_wit_media(to_wit_media(&ma));
        assert_eq!(back.file_name, "photo.jpg");
        assert_eq!(back.data, vec![0xFF_u8, 0xD8, 0xFF]);
        assert_eq!(back.mime_type.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn capabilities_bitfield() {
        let caps = ChannelCapabilities::HEALTH_CHECK | ChannelCapabilities::SEND_DRAFT;
        assert!(caps.contains(ChannelCapabilities::HEALTH_CHECK));
        assert!(!caps.contains(ChannelCapabilities::PIN_MESSAGE));
    }

    #[test]
    fn webhook_diagnostics_are_utf8_safe_and_bounded() {
        let detail = "λ".repeat(WEBHOOK_DIAGNOSTIC_MAX_CHARS + 50);
        let bounded = bounded_webhook_detail(detail);

        assert_eq!(bounded.chars().count(), WEBHOOK_DIAGNOSTIC_MAX_CHARS);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn direct_channel_construction_has_no_implicit_webhook_sender_grant() {
        assert!(!deny_all_senders()("any-sender"));
    }

    #[test]
    fn poll_trap_marks_channel_unhealthy() {
        let flag = AtomicBool::new(true);
        assert!(poll_health_ok(&flag), "starts healthy");

        // A trapping poll clears the flag; a broken plugin can no longer look
        // like a quiet, idle one.
        mark_poll_healthy(&flag, false);
        assert!(!poll_health_ok(&flag), "trap surfaces as unhealthy");

        // A subsequent successful poll clears the condition.
        mark_poll_healthy(&flag, true);
        assert!(poll_health_ok(&flag), "recovers after a clean poll");
    }

    #[test]
    fn channel_http_surface_is_unavailable_even_when_granted() {
        // Regression for the channel-activation egress hole. Making
        // `WasmChannel::from_wasm` a production caller must NOT grant channels a
        // usable outbound-HTTP surface on this host build: enabling `wasi:http`
        // would attach the ungoverned `default_hooks()` egress (a fresh instance
        // of the egress SSRF hole, issue 9395). The store is the security
        // boundary, so a channel whose manifest GRANTS `HttpClient` must still
        // carry no `wasi:http` context — the surface is unavailable, not
        // "deny-all".
        let granted_scope = crate::instance::test_scope(
            PluginCapability::Channel,
            "main",
            [crate::PluginPermission::HttpClient],
        );
        assert!(
            granted_scope
                .grants()
                .allows(crate::PluginPermission::HttpClient),
            "precondition: the channel scope must actually grant HttpClient"
        );

        let store = new_channel_store(
            granted_scope,
            crate::services::test_host_services(),
            crate::component::test_limits(0),
            InboundQueue::default(),
        );

        assert!(
            !store.data().http_enabled(),
            "a channel that grants HttpClient must NOT receive an outbound-HTTP \
             surface: ungoverned default_hooks() egress is the SSRF hole"
        );
    }

    #[tokio::test]
    async fn channel_validates_config_before_loading_guest_code() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
        let endpoint = PluginChannelEndpoint::new(scope, "plugin").unwrap();
        let services = PluginHostServices::new(PluginConfigResolver::new(|_| {
            Err(crate::error::PluginError::InvalidConfig(
                "invalid-before-load".to_string(),
            ))
        }));
        let result = WasmChannel::from_wasm(
            endpoint,
            Path::new("/path/that/must/not/exist.wasm"),
            &services,
            crate::component::test_limits(0),
        )
        .await;
        let error = match result {
            Ok(_) => panic!("invalid config must reject registration"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("invalid-before-load"));
    }

    #[test]
    fn host_endpoint_overrides_guest_routing_identity() {
        for (channel_type, alias, guest_alias) in [
            ("plugin", "acme.chat", Some("guest-selected-alias")),
            ("telegram", "work", None),
            ("gmail_push", "main", Some("")),
        ] {
            let scope = crate::instance::test_scope(PluginCapability::Channel, alias, []);
            let endpoint = PluginChannelEndpoint::new(scope, channel_type).unwrap();
            let message = from_wit_inbound(
                WitInboundMessage {
                    id: "evt-1".to_string(),
                    sender: "sender".to_string(),
                    reply_target: "room".to_string(),
                    content: "hello".to_string(),
                    channel: "guest-selected-type".to_string(),
                    channel_alias: guest_alias.map(str::to_string),
                    timestamp: 42,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                    subject: None,
                },
                &endpoint,
            );

            assert_eq!(message.channel, channel_type);
            assert_eq!(message.channel_alias.as_deref(), Some(alias));
            assert_ne!(message.channel, endpoint.instance_id().package());
            assert_eq!(message.content, "hello");
            assert!(message.internal_sop_event.is_none());
            assert!(!message.passive_context);
            assert!(!message.explicitly_addressed);
        }
    }

    #[test]
    fn host_enqueued_inbound_reaches_the_drain_handle() {
        let queue = crate::component::InboundQueue::default();
        let listener_handle = queue.clone();
        assert_eq!(queue.pending(), 0, "starts empty");

        listener_handle.enqueue(crate::component::HostInboundMessage {
            id: "evt-1".into(),
            sender: "+15550100".into(),
            reply_target: "+15550100".into(),
            content: "inbound sms".into(),
            channel: "inkbox".into(),
            channel_alias: Some("on-call".into()),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            subject: None,
        });

        assert_eq!(
            queue.pending(),
            1,
            "host enqueue is visible on the drain side"
        );
        let drained = queue
            .poll()
            .expect("the plugin-side drain sees the message");
        assert_eq!(drained.id, "evt-1");
        assert_eq!(drained.content, "inbound sms");
        assert_eq!(queue.pending(), 0, "draining empties the shared queue");
    }
}
