//! Channel adapter: `WasmChannel` implements `zeroclaw_api::channel::Channel`
//! backed by the `channel-plugin` component world.

use crate::component::InboundQueue;
use crate::component::bindings::channel::ChannelPlugin;
use crate::component::bindings::channel::exports::zeroclaw::plugin::channel::{
    ApprovalRequest as WitApprovalRequest, ApprovalResponse as WitApprovalResponse,
    ChannelCapabilities, InboundMessage as WitInboundMessage,
    MediaAttachment as WitMediaAttachment, SendMessage as WitSendMessage,
    WebhookRejection as WitWebhookRejection,
};
use crate::component::{
    PluginState, PluginStoreSpec, call_channel, call_channel_store, call_store, engine,
    load_component, wt,
};
use crate::endpoint::PluginChannelEndpoint;
use crate::host::AdmittedComponent;
use crate::services::PluginHostServices;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use wasmtime::Store;
use wasmtime::component::{Component, Linker};
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_api::webhook::{
    MAX_WEBHOOK_RESPONSE_BODY_BYTES, RawWebhook, WEBHOOK_REPLY_CHANNEL, WebhookOutcome,
    WebhookReject,
};

/// Host-supplied authorization against the live canonical peer policy.
pub type SenderAuthorizer = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Clone)]
struct ChannelInstanceFactory {
    endpoint: PluginChannelEndpoint,
    component: Arc<Component>,
    services: PluginHostServices,
    limits: crate::component::PluginLimits,
}

impl ChannelInstanceFactory {
    async fn instantiate(
        &self,
        inbound: InboundQueue,
    ) -> Result<(Store<PluginState>, ChannelPlugin)> {
        self.services.resolve_config(self.endpoint.scope())?;
        let mut store = crate::component::new_store(
            PluginStoreSpec::new(
                self.endpoint.scope().clone(),
                self.services.clone(),
                self.limits,
            )
            .with_granted_http()
            .with_inbound(inbound),
        );
        let http = store.data().http_enabled();
        let linker = build_linker(http)?;
        crate::component::ensure_http_coherent(&store, http)?;
        let bindings: Result<_> = call_store!(store, async |store: &mut Store<PluginState>| {
            wt(
                ChannelPlugin::instantiate_async(store, self.component.as_ref(), &linker).await,
                "failed to instantiate channel plugin",
            )
        });
        let bindings = bindings?;
        let channel = bindings.zeroclaw_plugin_channel();
        let configure_result: Result<()> =
            call_channel_store!(store, async |store: &mut Store<PluginState>| {
                wt(
                    channel.call_configure(store).await,
                    "channel.configure trapped",
                )?
                .map_err(anyhow::Error::msg)
            });
        configure_result?;
        Ok((store, bindings))
    }

    async fn parse_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Result<Vec<WitInboundMessage>, WitWebhookRejection>> {
        let (mut store, bindings) = self.instantiate(InboundQueue::default()).await?;
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

/// A channel backed by a WIT component-model plugin.
pub struct WasmChannel {
    endpoint: PluginChannelEndpoint,
    capabilities: ChannelCapabilities,
    state: Mutex<(Store<PluginState>, ChannelPlugin)>,
    webhook_factory: ChannelInstanceFactory,
    inbound: InboundQueue,
    // Static component metadata, fixed for one admitted logical binding.
    // Changing the external account or these capabilities requires rebuilding
    // the channel; point-of-use config refresh is only for that same binding.
    cached_self_handle: Option<String>,
    cached_self_addressed_mention: Option<String>,
    cached_multi_message_delay_ms: u64,
    poll_healthy: AtomicBool,
    authorizer: SenderAuthorizer,
    webhook_rx: std::sync::Mutex<Option<mpsc::Receiver<RawWebhook>>>,
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

impl WasmChannel {
    pub async fn from_wasm(
        endpoint: PluginChannelEndpoint,
        component: &AdmittedComponent,
        services: &PluginHostServices,
        limits: crate::component::PluginLimits,
    ) -> Result<Self> {
        Self::from_wasm_with_authorizer(endpoint, component, services, limits, Arc::new(|_| false))
            .await
    }

    pub async fn from_wasm_with_authorizer(
        endpoint: PluginChannelEndpoint,
        component: &AdmittedComponent,
        services: &PluginHostServices,
        limits: crate::component::PluginLimits,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        services.resolve_config(endpoint.scope())?;
        let factory = ChannelInstanceFactory {
            endpoint: endpoint.clone(),
            component: Arc::new(load_component(component)?),
            services: services.clone(),
            limits,
        };
        let inbound = InboundQueue::default();
        let (mut store, bindings) = factory.instantiate(inbound.clone()).await?;

        let channel = bindings.zeroclaw_plugin_channel();

        let static_exports: Result<_> = call_store!(store, async |store: &mut Store<
            PluginState,
        >| {
            let capabilities = wt(
                channel.call_get_channel_capabilities(&mut *store).await,
                "channel.get-channel-capabilities failed",
            )?;
            let cached_self_handle = if capabilities.contains(ChannelCapabilities::SELF_HANDLE) {
                wt(
                    channel.call_self_handle(&mut *store).await,
                    "channel.self-handle failed",
                )?
            } else {
                None
            };
            let cached_self_addressed_mention =
                if capabilities.contains(ChannelCapabilities::SELF_ADDRESSED_MENTION) {
                    wt(
                        channel.call_self_addressed_mention(&mut *store).await,
                        "channel.self-addressed-mention failed",
                    )?
                } else {
                    None
                };
            let cached_multi_message_delay_ms =
                if capabilities.contains(ChannelCapabilities::MULTI_MESSAGE_DELAY_MS) {
                    wt(
                        channel.call_multi_message_delay_ms(store).await,
                        "channel.multi-message-delay-ms failed",
                    )?
                } else {
                    800
                };
            Ok((
                capabilities,
                cached_self_handle,
                cached_self_addressed_mention,
                cached_multi_message_delay_ms,
            ))
        });
        let (
            capabilities,
            cached_self_handle,
            cached_self_addressed_mention,
            cached_multi_message_delay_ms,
        ) = static_exports?;

        Ok(Self {
            endpoint,
            capabilities,
            state: Mutex::new((store, bindings)),
            webhook_factory: factory,
            inbound,
            cached_self_handle,
            cached_self_addressed_mention,
            cached_multi_message_delay_ms,
            poll_healthy: AtomicBool::new(true),
            authorizer,
            webhook_rx: std::sync::Mutex::new(None),
        })
    }

    pub fn has_webhook_ingress(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::WEBHOOK_INGRESS)
    }

    pub async fn webhook_path(&self) -> Option<String> {
        if !self.has_webhook_ingress() {
            return None;
        }
        let result: Result<Option<String>> = call_channel!(
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
        );
        result.ok().flatten()
    }

    pub fn set_webhook_rx(&self, rx: mpsc::Receiver<RawWebhook>) {
        let mut webhook_rx = self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *webhook_rx = Some(rx);
    }

    /// Handle to this channel's inbound queue. A host-run listener clones it and
    /// calls [`InboundQueue::enqueue`] for each received message; the plugin
    /// drains them through its imported `inbound` interface.
    pub fn inbound(&self) -> InboundQueue {
        self.inbound.clone()
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
) -> bool {
    if authorizer(&message.sender) {
        return true;
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "channel": endpoint.channel_type(),
                "channel_alias": endpoint.alias(),
                "error_key": "plugin_channel_sender_unauthorized",
            })),
        "Channel plugin message rejected by live sender policy"
    );
    false
}

async fn forward_if_authorized(
    tx: &mpsc::Sender<ChannelMessage>,
    authorizer: &SenderAuthorizer,
    endpoint: &PluginChannelEndpoint,
    message: ChannelMessage,
) -> std::result::Result<(), mpsc::error::SendError<ChannelMessage>> {
    if sender_is_authorized(authorizer, endpoint, &message) {
        tx.send(message).await
    } else {
        Ok(())
    }
}

fn to_wit_approval_request(req: &ChannelApprovalRequest) -> WitApprovalRequest {
    WitApprovalRequest {
        tool_name: req.tool_name.clone(),
        arguments_summary: req.arguments_summary.clone(),
        raw_arguments: req.raw_arguments.as_ref().map(|v| v.to_string()),
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
        let poll_tx = tx.clone();
        let poll_endpoint = self.endpoint.clone();
        let poll_authorizer = Arc::clone(&self.authorizer);
        let poll_loop = async {
            const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
            const MAX_BACKOFF: Duration = Duration::from_millis(500);
            let mut backoff = INITIAL_BACKOFF;
            loop {
                let polled = call_channel!(
                    self,
                    async move |store: &mut Store<PluginState>, bindings: &mut ChannelPlugin| {
                        bindings
                            .zeroclaw_plugin_channel()
                            .call_poll_message(store)
                            .await
                    }
                );
                match polled {
                    Ok(Some(wit_msg)) => {
                        mark_poll_healthy(&self.poll_healthy, true);
                        backoff = INITIAL_BACKOFF;
                        if forward_if_authorized(
                            &poll_tx,
                            &poll_authorizer,
                            &poll_endpoint,
                            from_wit_inbound(wit_msg, &poll_endpoint),
                        )
                        .await
                        .is_err()
                        {
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
                                "channel": poll_endpoint.channel_type(),
                                "channel_alias": poll_endpoint.alias(),
                                "error": format!("{error:#}"),
                            })),
                            "channel plugin poll-message trapped; backing off"
                        );
                    }
                }
                tokio::select! {
                    () = poll_tx.closed() => return Ok(()),
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        };
        tokio::pin!(poll_loop);

        let webhook_rx = self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(mut webhook_rx) = webhook_rx else {
            return poll_loop.await;
        };
        let webhook_factory = self.webhook_factory.clone();
        let webhook_endpoint = self.endpoint.clone();
        let webhook_authorizer = Arc::clone(&self.authorizer);
        let webhook_tx = tx;
        let webhook_loop = async move {
            while let Some(RawWebhook {
                method,
                query,
                headers,
                body,
                cancellation,
                idempotency,
                reply,
            }) = webhook_rx.recv().await
            {
                // `parse-webhook` keeps its additive `(headers, body)` WIT
                // signature. Materialize the host-owned request line as
                // reserved headers for this call only.
                let webhook_headers = reserved_webhook_headers(method, query, headers);
                let decoded = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => None,
                    result = webhook_factory.parse_webhook(&webhook_headers, &body) => Some(result),
                };
                match decoded {
                    Some(Ok(Ok(messages))) => {
                        // A single reserved-channel message is a verification
                        // handshake response. It never enters sender
                        // authorization, idempotency, or the agent queue.
                        if let [message] = messages.as_slice()
                            && message.channel == WEBHOOK_REPLY_CHANNEL
                        {
                            let response = if cancellation.is_cancelled() {
                                Err(WebhookReject::Timeout)
                            } else {
                                let bounded = bounded_webhook_response(&message.content);
                                if bounded.is_err() {
                                    ::zeroclaw_log::record!(
                                            WARN,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Inbound
                                            )
                                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                            .with_attrs(::serde_json::json!({
                                                "channel": webhook_endpoint.channel_type(),
                                                "channel_alias": webhook_endpoint.alias(),
                                                "error_key": "plugin_webhook_response_too_large",
                                                "response_bytes": message.content.len(),
                                                "max_response_bytes": MAX_WEBHOOK_RESPONSE_BODY_BYTES,
                                            })),
                                            "channel plugin webhook response exceeded host limit"
                                        );
                                }
                                bounded
                            };
                            let _ = reply.send(response);
                            continue;
                        }
                        let mut delivery_failed = false;
                        for message in messages {
                            let message = from_wit_inbound(message, &webhook_endpoint);
                            if !sender_is_authorized(
                                &webhook_authorizer,
                                &webhook_endpoint,
                                &message,
                            ) {
                                continue;
                            }
                            let message_id = message.id.trim().to_string();
                            let reservation = if message_id.is_empty() {
                                None
                            } else if let Some(idempotency) = idempotency.as_ref() {
                                if !idempotency.reserve(&message_id) {
                                    continue;
                                }
                                Some((idempotency.clone(), message_id))
                            } else {
                                None
                            };
                            let sent = tokio::select! {
                                biased;
                                () = cancellation.cancelled() => None,
                                result = webhook_tx.send(message) => Some(result),
                            };
                            match sent {
                                Some(Ok(())) => {}
                                Some(Err(_)) => {
                                    if let Some((idempotency, message_id)) = reservation.as_ref() {
                                        idempotency.rollback(message_id);
                                    }
                                    delivery_failed = true;
                                    break;
                                }
                                None => {
                                    if let Some((idempotency, message_id)) = reservation.as_ref() {
                                        idempotency.rollback(message_id);
                                    }
                                    break;
                                }
                            }
                        }
                        let response = if cancellation.is_cancelled() {
                            Err(WebhookReject::Timeout)
                        } else if delivery_failed {
                            Err(WebhookReject::BadRequest(
                                "channel inbound receiver closed".to_string(),
                            ))
                        } else {
                            Ok(WebhookOutcome::Ack)
                        };
                        let _ = reply.send(response);
                    }
                    Some(Ok(Err(WitWebhookRejection::Unauthorized(reason)))) => {
                        let _ = reply.send(Err(WebhookReject::Unauthorized(reason)));
                    }
                    Some(Ok(Err(WitWebhookRejection::BadRequest(reason)))) => {
                        let _ = reply.send(Err(WebhookReject::BadRequest(reason)));
                    }
                    Some(Err(error)) => {
                        let _ = reply.send(Err(WebhookReject::BadRequest(format!("{error:#}"))));
                    }
                    None => {
                        let _ = reply.send(Err(WebhookReject::Timeout));
                    }
                }
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

/// Build the plugin-visible webhook header list: the host's authoritative HTTP
/// `method` / `query` as the reserved `x-webhook-method` / `x-webhook-query`
/// headers, followed by the inbound headers with any inbound copies of those
/// reserved names dropped. Verification handlers branch on the reserved names,
/// so an external caller must not be able to spoof them past the plugin
/// boundary — a plugin that folds headers into a last-write-wins map would
/// otherwise read the attacker value appended after the host's.
fn reserved_webhook_headers(
    method: String,
    query: String,
    inbound: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut headers = Vec::with_capacity(inbound.len() + 2);
    headers.push(("x-webhook-method".to_string(), method));
    headers.push(("x-webhook-query".to_string(), query));
    headers.extend(inbound.into_iter().filter(|(k, _)| {
        !k.eq_ignore_ascii_case("x-webhook-method") && !k.eq_ignore_ascii_case("x-webhook-query")
    }));
    headers
}

/// Materialize a guest response only after enforcing the public egress limit,
/// so oversized content is never cloned into a host-owned HTTP outcome.
fn bounded_webhook_response(body: &str) -> Result<WebhookOutcome, WebhookReject> {
    if body.len() > MAX_WEBHOOK_RESPONSE_BODY_BYTES {
        return Err(WebhookReject::InvalidResponse);
    }
    Ok(WebhookOutcome::Body(body.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginCapability;
    use crate::config::PluginConfigResolver;

    #[test]
    fn reserved_webhook_headers_drop_spoofed_inbound() {
        // An external caller supplies the reserved names on the HTTP request
        // (including a mixed-case copy); the host method/query must still win.
        let out = reserved_webhook_headers(
            "GET".to_string(),
            "hub.challenge=real".to_string(),
            vec![
                ("x-webhook-method".to_string(), "POST".to_string()),
                (
                    "x-webhook-query".to_string(),
                    "hub.challenge=attacker".to_string(),
                ),
                ("X-Webhook-Method".to_string(), "DELETE".to_string()),
                ("x-fixture-secret".to_string(), "s".to_string()),
            ],
        );
        let methods: Vec<&str> = out
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("x-webhook-method"))
            .map(|(_, v)| v.as_str())
            .collect();
        let queries: Vec<&str> = out
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("x-webhook-query"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            methods,
            ["GET"],
            "only the host method survives; spoofed copies dropped"
        );
        assert_eq!(
            queries,
            ["hub.challenge=real"],
            "only the host query survives"
        );
        assert!(
            out.iter().any(|(k, v)| k == "x-fixture-secret" && v == "s"),
            "legitimate inbound headers are preserved"
        );
    }

    #[test]
    fn webhook_response_is_bounded_before_host_materialization() {
        let at_limit = "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES);
        assert!(matches!(
            bounded_webhook_response(&at_limit),
            Ok(WebhookOutcome::Body(body)) if body.len() == MAX_WEBHOOK_RESPONSE_BODY_BYTES
        ));

        let oversized = "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES + 1);
        assert!(matches!(
            bounded_webhook_response(&oversized),
            Err(WebhookReject::InvalidResponse)
        ));
    }

    #[test]
    fn media_round_trip() {
        let ma = MediaAttachment {
            file_name: "photo.jpg".into(),
            data: vec![0xFF, 0xD8, 0xFF],
            mime_type: Some("image/jpeg".into()),
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

    #[tokio::test]
    async fn channel_validates_config_before_loading_guest_code() {
        let scope = crate::instance::test_scope(PluginCapability::Channel, "main", []);
        let endpoint = PluginChannelEndpoint::new(scope, "plugin").unwrap();
        let services = crate::services::test_services(PluginConfigResolver::new(|_| {
            Err(crate::error::PluginError::InvalidConfig(
                "invalid-before-load".to_string(),
            ))
        }));
        let component = AdmittedComponent::test_component(b"not-a-component");
        let result = WasmChannel::from_wasm(
            endpoint,
            &component,
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
