//! Channel adapter: `WasmChannel` implements `zeroclaw_api::channel::Channel`
//! backed by the `channel-plugin` component world.
//!
//! The polling/outbound store is warm and held behind an async mutex. Webhook
//! parsing uses a disposable configured store per request so host cancellation
//! cannot poison the warm instance. `listen` runs the inbound bridges.

use crate::PluginPermission;
use crate::component::InboundQueue;
use crate::component::bindings::channel::ChannelPlugin;
use crate::component::bindings::channel::exports::zeroclaw::plugin::channel::{
    ApprovalRequest as WitApprovalRequest, ApprovalResponse as WitApprovalResponse,
    ChannelCapabilities, InboundMessage as WitInboundMessage,
    MediaAttachment as WitMediaAttachment, SendMessage as WitSendMessage,
    WebhookRejection as WitWebhookRejection,
};
use crate::component::{PluginState, call_plugin, engine, load_component_with_digest, wt};
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
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
use zeroclaw_api::webhook::{RawWebhook, WebhookReject};

/// Host-supplied sender authorization for normalized inbound messages.
///
/// The runtime builds this resolver over the live canonical configuration. It
/// is intentionally a closure rather than an allowlist snapshot, so a channel
/// never owns a second copy of authorization state.
pub type SenderAuthorizer = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// A channel backed by a WIT component-model plugin.
pub struct WasmChannel {
    /// Canonical host-owned routing identity: `plugin.<manifest-name>` for a
    /// novel plugin or `<provided-channel>.<config-alias>` for a mirror.
    channel_ref: String,
    capabilities: ChannelCapabilities,
    state: Arc<Mutex<(Store<PluginState>, ChannelPlugin)>>,
    /// Verified executable and permission recipe plus an on-demand resolver for
    /// canonical config/limits. Webhook parsing materializes a disposable store
    /// from this factory so cancelling one request never poisons the warm channel
    /// instance used by polling and outbound calls.
    webhook_factory: ChannelInstanceFactory,
    inbound: InboundQueue,
    cached_self_handle: Option<String>,
    cached_self_addressed_mention: Option<String>,
    cached_multi_message_delay_ms: u64,
    poll_healthy: Arc<AtomicBool>,
    /// Applied at the final host boundary before any inbound transport can
    /// forward a normalized message to the agent.
    authorizer: SenderAuthorizer,
    /// Sink-drain end for host-fed webhooks (set by the orchestrator when this
    /// channel declares a `webhook-path`). Taken once by `listen`.
    webhook_rx: std::sync::Mutex<Option<mpsc::Receiver<RawWebhook>>>,
}

/// Resolves the current configure payload and execution limits for a channel
/// component. Production resolvers read the canonical shared `Config` handle on
/// every call; the tuple is a per-call materialized view and is never cached.
pub type ChannelRuntimeResolver =
    Arc<dyn Fn() -> Result<(String, crate::component::PluginLimits)> + Send + Sync>;

#[derive(Clone)]
struct ChannelInstanceFactory {
    component: Arc<Component>,
    permissions: Arc<[PluginPermission]>,
    runtime: ChannelRuntimeResolver,
}

impl ChannelInstanceFactory {
    async fn instantiate(
        &self,
        inbound: InboundQueue,
    ) -> Result<(Store<PluginState>, ChannelPlugin)> {
        let (config_json, limits) = (self.runtime)()?;
        let mut store =
            crate::component::new_store_with_inbound(self.permissions.as_ref(), inbound, limits);
        let http = store.data().http_enabled();
        let linker = build_linker(http)?;
        crate::component::ensure_http_coherent(&store, http)?;
        let bindings = wt(
            ChannelPlugin::instantiate_async(&mut store, self.component.as_ref(), &linker).await,
            "failed to instantiate channel plugin",
        )?;
        wt(
            bindings
                .zeroclaw_plugin_channel()
                .call_configure(&mut store, &config_json)
                .await,
            "channel.configure trapped",
        )?
        .map_err(anyhow::Error::msg)?;
        Ok((store, bindings))
    }

    async fn parse_webhook(
        &self,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<Result<Vec<WitInboundMessage>, WitWebhookRejection>> {
        let (mut store, bindings) = self.instantiate(InboundQueue::default()).await?;
        wt(
            bindings
                .zeroclaw_plugin_channel()
                .call_parse_webhook(&mut store, headers, body)
                .await,
            "channel.parse-webhook trapped",
        )
    }
}

fn fixed_runtime_resolver(
    config_json: String,
    limits: crate::component::PluginLimits,
) -> ChannelRuntimeResolver {
    Arc::new(move || Ok((config_json.clone(), limits)))
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
        split_channel_ref(&self.channel_ref)
            .1
            .unwrap_or(&self.channel_ref)
    }
}

fn split_channel_ref(channel_ref: &str) -> (&str, Option<&str>) {
    channel_ref
        .split_once('.')
        .map_or((channel_ref, None), |(channel_type, alias)| {
            (channel_type, Some(alias))
        })
}

/// Resolve the JSON config section handed to a channel plugin's `configure`.
/// Withheld (an empty object) unless the manifest grants `ConfigRead`, so a
/// plugin without the permission can never be configured with another channel's
/// secrets. Mirrors the tool-plugin `__config` rule.
fn resolve_configure_json(
    config: &HashMap<String, String>,
    permissions: &[PluginPermission],
) -> String {
    if permissions.contains(&PluginPermission::ConfigRead) {
        serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string())
    } else {
        "{}".to_string()
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
    /// Compile and instantiate one channel plugin from digest-bound bytes.
    /// `channel_ref` is the host-owned routing identity. `runtime` resolves the
    /// current permission-filtered config and execution limits whenever a warm
    /// or disposable instance is configured.
    async fn instantiate(
        channel_ref: String,
        wasm_path: &Path,
        expected_sha256: Option<&str>,
        permissions: &[PluginPermission],
        runtime: ChannelRuntimeResolver,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        let factory = ChannelInstanceFactory {
            component: Arc::new(load_component_with_digest(wasm_path, expected_sha256)?),
            permissions: Arc::from(permissions),
            runtime,
        };
        let inbound = InboundQueue::default();
        let (mut store, bindings) = factory.instantiate(inbound.clone()).await?;

        let channel = bindings.zeroclaw_plugin_channel();

        let capabilities = wt(
            channel.call_get_channel_capabilities(&mut store).await,
            "channel.get-channel-capabilities failed",
        )?;

        let cached_self_handle = if capabilities.contains(ChannelCapabilities::SELF_HANDLE) {
            wt(
                channel.call_self_handle(&mut store).await,
                "channel.self-handle failed",
            )?
        } else {
            None
        };
        let cached_self_addressed_mention =
            if capabilities.contains(ChannelCapabilities::SELF_ADDRESSED_MENTION) {
                wt(
                    channel.call_self_addressed_mention(&mut store).await,
                    "channel.self-addressed-mention failed",
                )?
            } else {
                None
            };
        let cached_multi_message_delay_ms =
            if capabilities.contains(ChannelCapabilities::MULTI_MESSAGE_DELAY_MS) {
                wt(
                    channel.call_multi_message_delay_ms(&mut store).await,
                    "channel.multi-message-delay-ms failed",
                )?
            } else {
                800
            };

        Ok(Self {
            channel_ref,
            capabilities,
            state: Arc::new(Mutex::new((store, bindings))),
            webhook_factory: factory,
            inbound,
            cached_self_handle,
            cached_self_addressed_mention,
            cached_multi_message_delay_ms,
            poll_healthy: Arc::new(AtomicBool::new(true)),
            authorizer,
            webhook_rx: std::sync::Mutex::new(None),
        })
    }

    /// Whether this plugin advertises `webhook-ingress` (serves inbound via a
    /// host webhook route rather than self-polling).
    pub fn has_webhook_ingress(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::WEBHOOK_INGRESS)
    }

    /// The URL path segment this channel serves webhooks on, or `None` for a
    /// poll-only channel. Calls the plugin's `webhook-path` export (only when it
    /// advertised the capability).
    pub async fn webhook_path(&self) -> Option<String> {
        if !self.has_webhook_ingress() {
            return None;
        }
        let result: Result<Option<String>> = call_plugin!(
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

    /// Hand this channel the drain end of its webhook sink, before it is boxed
    /// into an `Arc<dyn Channel>`; `listen` takes it once.
    pub fn set_webhook_rx(&self, rx: mpsc::Receiver<RawWebhook>) {
        let mut webhook_rx = self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *webhook_rx = Some(rx);
    }

    /// Instantiate a novel channel plugin from its `[[plugins.entries]]` config.
    pub async fn from_wasm(
        plugin_name: impl Into<String>,
        wasm_path: &Path,
        permissions: &[PluginPermission],
        config: &HashMap<String, String>,
        limits: crate::component::PluginLimits,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        Self::from_wasm_with_digest(
            plugin_name,
            wasm_path,
            None,
            permissions,
            config,
            limits,
            authorizer,
        )
        .await
    }

    /// Instantiate a novel channel plugin while binding its component to the
    /// digest carried by the verified manifest.
    pub async fn from_wasm_with_digest(
        plugin_name: impl Into<String>,
        wasm_path: &Path,
        expected_sha256: Option<&str>,
        permissions: &[PluginPermission],
        config: &HashMap<String, String>,
        limits: crate::component::PluginLimits,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        let plugin_name = plugin_name.into();
        let config_json = resolve_configure_json(config, permissions);
        Self::from_wasm_with_runtime_resolver_and_digest(
            plugin_name,
            wasm_path,
            expected_sha256,
            permissions,
            fixed_runtime_resolver(config_json, limits),
            authorizer,
        )
        .await
    }

    /// Instantiate a novel channel while resolving config and limits on demand.
    /// The resolver is called at startup and for each disposable webhook parser.
    pub async fn from_wasm_with_runtime_resolver(
        plugin_name: impl Into<String>,
        wasm_path: &Path,
        permissions: &[PluginPermission],
        runtime: ChannelRuntimeResolver,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        Self::from_wasm_with_runtime_resolver_and_digest(
            plugin_name,
            wasm_path,
            None,
            permissions,
            runtime,
            authorizer,
        )
        .await
    }

    /// Instantiate a novel channel from a live runtime resolver while binding
    /// the executable to the digest from its verified manifest.
    pub async fn from_wasm_with_runtime_resolver_and_digest(
        plugin_name: impl Into<String>,
        wasm_path: &Path,
        expected_sha256: Option<&str>,
        permissions: &[PluginPermission],
        runtime: ChannelRuntimeResolver,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        let plugin_name = plugin_name.into();
        let channel_ref = zeroclaw_api::channel::plugin_channel_ref(&plugin_name);
        Self::instantiate(
            channel_ref,
            wasm_path,
            expected_sha256,
            permissions,
            runtime,
            authorizer,
        )
        .await
    }

    /// Instantiate a mirror from canonical `[channels.<type>.<alias>]` config.
    pub async fn from_wasm_mirror(
        channel_type: impl Into<String>,
        alias: impl Into<String>,
        wasm_path: &Path,
        permissions: &[PluginPermission],
        config_json: &str,
        limits: crate::component::PluginLimits,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        Self::from_wasm_mirror_with_digest(
            channel_type,
            alias,
            wasm_path,
            None,
            permissions,
            config_json,
            limits,
            authorizer,
        )
        .await
    }

    /// Instantiate a mirror while binding its component to the digest carried
    /// by the verified manifest.
    pub async fn from_wasm_mirror_with_digest(
        channel_type: impl Into<String>,
        alias: impl Into<String>,
        wasm_path: &Path,
        expected_sha256: Option<&str>,
        permissions: &[PluginPermission],
        config_json: &str,
        limits: crate::component::PluginLimits,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        let channel_type = channel_type.into();
        let alias = alias.into();
        let config_json = if permissions.contains(&PluginPermission::ConfigRead) {
            config_json.to_string()
        } else {
            "{}".to_string()
        };
        Self::from_wasm_mirror_with_runtime_resolver_and_digest(
            channel_type,
            alias,
            wasm_path,
            expected_sha256,
            permissions,
            fixed_runtime_resolver(config_json, limits),
            authorizer,
        )
        .await
    }

    /// Instantiate a mirrored channel while resolving config and limits on
    /// demand. The resolver must already enforce `ConfigRead` withholding.
    pub async fn from_wasm_mirror_with_runtime_resolver(
        channel_type: impl Into<String>,
        alias: impl Into<String>,
        wasm_path: &Path,
        permissions: &[PluginPermission],
        runtime: ChannelRuntimeResolver,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        Self::from_wasm_mirror_with_runtime_resolver_and_digest(
            channel_type,
            alias,
            wasm_path,
            None,
            permissions,
            runtime,
            authorizer,
        )
        .await
    }

    /// Instantiate a mirrored channel from a live runtime resolver while
    /// binding the executable to the digest from its verified manifest.
    pub async fn from_wasm_mirror_with_runtime_resolver_and_digest(
        channel_type: impl Into<String>,
        alias: impl Into<String>,
        wasm_path: &Path,
        expected_sha256: Option<&str>,
        permissions: &[PluginPermission],
        runtime: ChannelRuntimeResolver,
        authorizer: SenderAuthorizer,
    ) -> Result<Self> {
        let channel_type = channel_type.into();
        let alias = alias.into();
        Self::instantiate(
            format!("{channel_type}.{alias}"),
            wasm_path,
            expected_sha256,
            permissions,
            runtime,
            authorizer,
        )
        .await
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

fn from_wit_inbound(msg: WitInboundMessage, channel_ref: &str) -> ChannelMessage {
    let (channel_type, alias) = split_channel_ref(channel_ref);
    ChannelMessage {
        id: msg.id,
        sender: msg.sender,
        reply_target: msg.reply_target,
        content: msg.content,
        channel: channel_type.to_string(),
        // Always stamp the host-owned route. Guest-provided identity cannot
        // select a different owner or session namespace.
        channel_alias: alias.map(str::to_string),
        timestamp: msg.timestamp,
        thread_ts: msg.thread_ts,
        interruption_scope_id: msg.interruption_scope_id,
        attachments: msg.attachments.into_iter().map(from_wit_media).collect(),
        subject: msg.subject,
        ..Default::default()
    }
}

/// Forward a normalized inbound message only when its sender is currently
/// authorized by the host.
///
/// Keeping this check at the final host-to-agent boundary makes the same gate
/// reusable by polling today and by later host-fed transports without trusting
/// a guest to enforce operator policy.
async fn forward_if_authorized(
    tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    authorizer: &SenderAuthorizer,
    channel_alias: &str,
    msg: ChannelMessage,
) -> Result<(), tokio::sync::mpsc::error::SendError<ChannelMessage>> {
    if !authorizer(&msg.sender) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Inbound)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "channel_alias": channel_alias,
                    "sender": msg.sender.as_str(),
                })),
            "ignoring channel-plugin inbound from unauthorized sender"
        );
        return Ok(());
    }

    tx.send(msg).await
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
        split_channel_ref(&self.channel_ref).0
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let wit_msg = to_wit_send(message);
        call_plugin!(
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
        let poll_channel_ref = self.channel_ref.clone();
        let webhook_channel_ref = poll_channel_ref.clone();
        let webhook_tx = tx.clone();
        let poll_authorizer = Arc::clone(&self.authorizer);
        let webhook_authorizer = Arc::clone(&self.authorizer);
        let webhook_rx = self
            .webhook_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();

        let poll_loop = async {
            const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
            const MAX_BACKOFF: Duration = Duration::from_millis(500);
            let mut backoff = INITIAL_BACKOFF;

            loop {
                if tx.is_closed() {
                    return Ok(());
                }
                let polled = {
                    let mut guard = self.state.lock().await;
                    let (ref mut store, ref mut bindings) = *guard;
                    crate::component::refuel(store);
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_poll_message(store)
                        .await
                };
                match polled {
                    Ok(Some(wit_msg)) => {
                        mark_poll_healthy(&self.poll_healthy, true);
                        backoff = INITIAL_BACKOFF;
                        if forward_if_authorized(
                            &tx,
                            &poll_authorizer,
                            &poll_channel_ref,
                            from_wit_inbound(wit_msg, &poll_channel_ref),
                        )
                        .await
                        .is_err()
                        {
                            return Ok(());
                        }
                    }
                    Ok(None) => {
                        mark_poll_healthy(&self.poll_healthy, true);
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
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
                                "channel_ref": poll_channel_ref,
                                "error": format!("{error:#}"),
                            })),
                            "channel plugin poll-message trapped; backing off"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
        };
        tokio::pin!(poll_loop);

        let Some(mut webhook_rx) = webhook_rx else {
            return poll_loop.await;
        };
        let webhook_factory = self.webhook_factory.clone();
        let webhook_loop = async move {
            while let Some(RawWebhook {
                headers,
                body,
                cancellation,
                reply,
            }) = webhook_rx.recv().await
            {
                let decoded = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => None,
                    result = webhook_factory.parse_webhook(&headers, &body) => Some(result),
                };
                match decoded {
                    Some(Ok(Ok(messages))) => {
                        let mut delivery_failed = false;
                        for message in messages {
                            let message = from_wit_inbound(message, &webhook_channel_ref);
                            let sent = tokio::select! {
                                biased;
                                () = cancellation.cancelled() => None,
                                result = forward_if_authorized(
                                    &webhook_tx,
                                    &webhook_authorizer,
                                    &webhook_channel_ref,
                                    message,
                                ) => Some(result),
                            };
                            match sent {
                                Some(Ok(())) => {}
                                Some(Err(_)) => {
                                    delivery_failed = true;
                                    break;
                                }
                                None => break,
                            }
                        }
                        let response = if cancellation.is_cancelled() {
                            Err(WebhookReject::Timeout)
                        } else if delivery_failed {
                            Err(WebhookReject::BadRequest(
                                "channel inbound receiver closed".to_string(),
                            ))
                        } else {
                            Ok(())
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
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Inbound
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "channel_ref": webhook_channel_ref,
                                "error_key": "plugin_webhook_parse_cancelled",
                            })),
                            "channel plugin webhook parse cancelled at request deadline"
                        );
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
        let result: Result<bool> = call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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
        call_plugin!(
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

    #[test]
    fn configure_withholds_section_without_config_read() {
        let mut config = HashMap::new();
        config.insert("api_key".to_string(), "secret".to_string());
        let json = resolve_configure_json(&config, &[PluginPermission::HttpClient]);
        assert_eq!(json, "{}", "no ConfigRead means an empty config object");
    }

    #[test]
    fn configure_passes_section_with_config_read() {
        let mut config = HashMap::new();
        config.insert("identity".to_string(), "on-call".to_string());
        let json = resolve_configure_json(&config, &[PluginPermission::ConfigRead]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["identity"], "on-call", "granted section round-trips");
    }

    #[test]
    fn inbound_route_is_stamped_from_verified_manifest_name() {
        let message = from_wit_inbound(
            WitInboundMessage {
                id: "evt-1".to_string(),
                sender: "sender".to_string(),
                reply_target: "room".to_string(),
                content: "hello".to_string(),
                channel: "guest-controlled".to_string(),
                channel_alias: Some("other-owner".to_string()),
                timestamp: 0,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: Vec::new(),
                subject: None,
            },
            "plugin.weather-alerts",
        );

        assert_eq!(message.channel, zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
        assert_eq!(message.channel_alias.as_deref(), Some("weather-alerts"));
    }

    #[test]
    fn host_enqueued_inbound_reaches_the_drain_handle() {
        // The inbound contract is host-fed: a listener the orchestrator owns
        // (vendor tunnel, webhook) enqueues through the handle from
        // `WasmChannel::inbound()`, and the plugin drains the same queue. Prove
        // the producer side here so the transport is not just asserted at the
        // queue type but at the handle a host listener actually holds.
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

    #[test]
    fn from_wit_inbound_host_stamps_canonical_route() {
        let make = || WitInboundMessage {
            id: "1".into(),
            sender: "u".into(),
            reply_target: "u".into(),
            content: "hi".into(),
            channel: "plugin-ignored".into(),
            channel_alias: Some("plugin-supplied".into()),
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
            subject: None,
        };

        // A mirror stamps the host-owned alias, overriding whatever the plugin
        // put in `channel_alias`, so routing/session keys never trust the plugin.
        let stamped = from_wit_inbound(make(), "telegram.main");
        assert_eq!(stamped.channel, "telegram");
        assert_eq!(stamped.channel_alias.as_deref(), Some("main"));

        // Novel plugins use the same host-owned route grammar and never trust
        // the guest's channel or alias fields.
        let novel = from_wit_inbound(make(), "plugin.echo.channel");
        assert_eq!(novel.channel, zeroclaw_api::channel::PLUGIN_CHANNEL_TYPE);
        assert_eq!(novel.channel_alias.as_deref(), Some("echo.channel"));
    }
}
