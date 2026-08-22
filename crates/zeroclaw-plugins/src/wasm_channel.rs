//! Channel adapter: `WasmChannel` implements `zeroclaw_api::channel::Channel`
//! backed by the `channel-plugin` component world.

use crate::component::InboundQueue;
use crate::component::bindings::channel::ChannelPlugin;
use crate::component::bindings::channel::exports::zeroclaw::plugin::channel::{
    ApprovalRequest as WitApprovalRequest, ApprovalResponse as WitApprovalResponse,
    ChannelCapabilities, InboundMessage as WitInboundMessage,
    MediaAttachment as WitMediaAttachment, SendMessage as WitSendMessage,
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use wasmtime::Store;
use wasmtime::component::Component;
use wasmtime::component::Linker;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;

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
}

struct ChannelInstanceFactory {
    component: Component,
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
            component: load_component(wasm_path)?,
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
}

impl ChannelInstanceFactory {
    async fn instantiate(
        &self,
        endpoint: &PluginChannelEndpoint,
        inbound: InboundQueue,
    ) -> Result<ChannelInstance> {
        let mut store = crate::component::new_store(
            PluginStoreSpec::new(endpoint.scope().clone(), self.services.clone(), self.limits)
                .with_granted_http()
                .with_inbound(inbound),
        );
        let http = store.data().http_enabled();
        let linker = build_linker(http)?;
        crate::component::ensure_http_coherent(&store, http)?;
        let bindings = call_store!(store, async |store: &mut Store<PluginState>| {
            wt_instantiate(
                ChannelPlugin::instantiate_async(store, &self.component, &linker).await,
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
        const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
        const MAX_BACKOFF: Duration = Duration::from_millis(500);
        let mut backoff = INITIAL_BACKOFF;
        // Keep the poll loop inside the Channel::listen future. The
        // orchestrator owns cancellation and restart supervision; detaching a
        // second task here would make every apparent exit leak another loop.
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
                    if tx
                        .send(from_wit_inbound(wit_msg, &self.endpoint))
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                    continue;
                }
                Ok(None) => {
                    mark_poll_healthy(&self.poll_healthy, true);
                }
                Err(e) => {
                    mark_poll_healthy(&self.poll_healthy, false);
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Inbound)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "channel": self.endpoint.channel_type(),
                                "channel_alias": self.endpoint.alias(),
                                "error": format!("{e:#}"),
                            })),
                        "channel plugin poll-message trapped; backing off"
                    );
                }
            }

            tokio::select! {
                () = tx.closed() => return Ok(()),
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
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
