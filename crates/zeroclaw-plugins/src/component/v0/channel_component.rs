// Channel adapter: `ComponentChannel` implements `zeroclaw_api::channel::Channel`
// backed by a WIT component-model plugin (the `channel-plugin` world in
// `wit/channel.wit`).
//
// Instance lifecycle: warm — the `Store` and `ChannelPlugin` bindings are
// created once at construction and held in an `Arc<Mutex<...>>`.
//
// `listen()` runs a poll-to-push bridge in a tokio task with exponential
// backoff (50 ms initial, 500 ms ceiling, reset on any successful message).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;

use super::bindings::channel::{
    ChannelPlugin,
    exports::zeroclaw::plugin::channel::{
        ApprovalRequest as WitApprovalRequest, ApprovalResponse as WitApprovalResponse,
        ChannelCapabilities, InboundMessage as WitInboundMessage,
        MediaAttachment as WitMediaAttachment, SendMessage as WitSendMessage,
    },
};
use super::plugin_store::{self, PluginStore};
use crate::component::engine::ComponentEngine;
use crate::error::PluginError;
use crate::{call_plugin, call_plugin_sync};

// ── Attributable ──────────────────────────────────────────────────────────────

/// A channel backed by a WIT Component Model plugin (WASIP2 ABI).
pub struct ComponentChannel {
    alias: String,
    capabilities: ChannelCapabilities,
    state: Arc<Mutex<(wasmtime::Store<PluginStore>, ChannelPlugin)>>,
    /// Canonical plugin name as self-reported by `plugin-info`. Source of truth.
    plugin_name: String,
    /// Plugin version string as self-reported by `plugin-info`. Source of truth.
    plugin_version: String,
}

impl Attributable for ComponentChannel {
    fn role(&self) -> Role {
        Role::Channel(ChannelKind::Plugin)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

// ── Construction ──────────────────────────────────────────────────────────────

impl ComponentChannel {
    /// Compile and instantiate a channel plugin from raw WASM bytes.
    ///
    /// Calls `get-channel-capabilities` once and stores the result.
    ///
    /// `permissions` is applied to the long-lived store so that filesystem,
    /// TCP, UDP, and HTTP access are restricted to the declared
    /// `fine_grained_permissions` list.
    pub async fn from_bytes(
        alias: impl Into<String>,
        engine: Arc<ComponentEngine>,
        bytes: &[u8],
        permissions: Vec<crate::FineGrainedPermission>,
    ) -> anyhow::Result<Self> {
        let component = engine.compile(bytes)?;
        let mut linker = wasmtime::component::Linker::<PluginStore>::new(engine.engine());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker).map_err(PluginError::from)?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(PluginError::from)?;
        plugin_store::add_to_linker_channel(&mut linker)?;
        let host = PluginStore::with_permissions(&permissions).await?;
        let mut store = wasmtime::Store::new(engine.engine(), host);

        let instance = linker
            .instantiate(&mut store, &component)
            .map_err(PluginError::from)?;
        let bindings = ChannelPlugin::new(&mut store, &instance).map_err(PluginError::from)?;

        // Phase 2: read plugin-info exports — canonical source of truth.
        let plugin_info = bindings.zeroclaw_plugin_plugin_info();
        let plugin_name = plugin_info
            .call_plugin_name(&mut store)
            .map_err(PluginError::from)?;
        let plugin_version = plugin_info
            .call_plugin_version(&mut store)
            .map_err(PluginError::from)?;

        let capabilities = bindings
            .zeroclaw_plugin_channel()
            .call_get_channel_capabilities(&mut store)
            .map_err(PluginError::from)?;

        Ok(Self {
            alias: alias.into(),
            capabilities,
            state: Arc::new(Mutex::new((store, bindings))),
            plugin_name,
            plugin_version,
        })
    }
}

// ── Type conversions ──────────────────────────────────────────────────────────

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

fn from_wit_inbound(msg: WitInboundMessage, channel_name: &str) -> ChannelMessage {
    ChannelMessage {
        id: msg.id,
        sender: msg.sender,
        reply_target: msg.reply_target,
        content: msg.content,
        channel: channel_name.to_string(),
        channel_alias: msg.channel_alias,
        timestamp: msg.timestamp,
        thread_ts: msg.thread_ts,
        interruption_scope_id: msg.interruption_scope_id,
        attachments: msg.attachments.into_iter().map(from_wit_media).collect(),
        subject: msg.subject,
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

// ── Channel trait impl ────────────────────────────────────────────────────────

#[async_trait]
impl Channel for ComponentChannel {
    fn name(&self) -> &str {
        &self.alias
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let wit_msg = to_wit_send(message);
        call_plugin!(
            self,
            "send",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_send(store, &wit_msg)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let channel_name = self.alias.clone();
        let state = Arc::clone(&self.state);
        let plugin_name = self.plugin_name.clone();
        let plugin_version = self.plugin_version.clone();

        zeroclaw_spawn::spawn!(async move {
            const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
            const MAX_BACKOFF: Duration = Duration::from_millis(500);
            let mut backoff = INITIAL_BACKOFF;

            loop {
                let mut guard = state.lock().await;
                let (ref mut store, ref mut bindings) = *guard;
                let f = async move |store, bindings: &mut ChannelPlugin| {
                    bindings
                        .zeroclaw_plugin_channel()
                        .call_poll_message(store)
                        .await
                        .ok()
                        .flatten()
                };
                let result = super::wrap_plugin::wrap_plugin_call(
                    &plugin_name,
                    &plugin_version,
                    "poll_message",
                    f(store, bindings),
                )
                .await;

                match result {
                    Some(wit_msg) => {
                        backoff = INITIAL_BACKOFF;
                        let msg = from_wit_inbound(wit_msg, &channel_name);
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                    _ => {
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
        });

        Ok(())
    }

    // ── Capability-gated overrides ─────────────────────────────────────────

    async fn health_check(&self) -> bool {
        if !self
            .capabilities
            .contains(ChannelCapabilities::HEALTH_CHECK)
        {
            return true;
        }
        call_plugin!(
            self,
            "health_check",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_health_check(store)
                    .await
                    .map_err(anyhow::Error::msg)
                    .unwrap_or(false)
            }
        )
    }

    fn self_handle(&self) -> Option<String> {
        if !self.capabilities.contains(ChannelCapabilities::SELF_HANDLE) {
            return None;
        }
        call_plugin_sync!(
            self,
            "self_handle",
            move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_self_handle(store)
                    .ok()
                    .flatten()
            }
        )
    }

    fn self_addressed_mention(&self) -> Option<String> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::SELF_ADDRESSED_MENTION)
        {
            return None;
        }
        call_plugin_sync!(
            self,
            "self_addressed_mention",
            move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_self_addressed_mention(store)
                    .ok()
                    .flatten()
            }
        )
    }

    fn drop_self_messages(&self, msg: &ChannelMessage) -> bool {
        if !self
            .capabilities
            .contains(ChannelCapabilities::DROP_SELF_MESSAGE)
        {
            // Use the default implementation from the Channel trait.
            let Some(handle) = self.self_handle() else {
                return false;
            };
            let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
            let sender_norm = msg.sender.trim_start_matches('@').to_ascii_lowercase();
            return !handle_norm.is_empty() && handle_norm == sender_norm;
        }
        // Build WIT inbound-message from the Rust ChannelMessage.
        let wit_msg = WitInboundMessage {
            id: msg.id.clone(),
            sender: msg.sender.clone(),
            reply_target: msg.reply_target.clone(),
            content: msg.content.clone(),
            channel: msg.channel.clone(),
            channel_alias: msg.channel_alias.clone(),
            timestamp: msg.timestamp,
            thread_ts: msg.thread_ts.clone(),
            interruption_scope_id: msg.interruption_scope_id.clone(),
            attachments: msg.attachments.iter().map(to_wit_media).collect(),
            subject: msg.subject.clone(),
        };
        call_plugin_sync!(
            self,
            "drop_self_messages",
            move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_drop_self_message(store, &wit_msg)
                    .unwrap_or(false)
            }
        )
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::START_TYPING)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        call_plugin!(
            self,
            "start_typing",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_start_typing(store, &recipient)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        if !self.capabilities.contains(ChannelCapabilities::STOP_TYPING) {
            return Ok(());
        }
        let recipient = recipient.to_string();
        call_plugin!(
            self,
            "stop_typing",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_stop_typing(store, &recipient)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_draft_updates(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_DRAFT_UPDATES)
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if !self.capabilities.contains(ChannelCapabilities::SEND_DRAFT) {
            return Ok(None);
        }
        let wit_msg = to_wit_send(message);
        call_plugin!(
            self,
            "send_draft",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_send_draft(store, &wit_msg)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UPDATE_DRAFT)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        let message_id = message_id.to_string();
        let text = text.to_string();
        call_plugin!(
            self,
            "update_draft",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_update_draft(store, &recipient, &message_id, &text)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UPDATE_DRAFT_PROGRESS)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        let message_id = message_id.to_string();
        let text = text.to_string();
        call_plugin!(
            self,
            "update_draft_progress",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_update_draft_progress(store, &recipient, &message_id, &text)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::FINALIZE_DRAFT)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        let message_id = message_id.to_string();
        let text = text.to_string();
        call_plugin!(
            self,
            "finalize_draft",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_finalize_draft(store, &recipient, &message_id, &text)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::CANCEL_DRAFT)
        {
            return Ok(());
        }
        let recipient = recipient.to_string();
        let message_id = message_id.to_string();
        call_plugin!(
            self,
            "cancel_draft",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_cancel_draft(store, &recipient, &message_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_MULTI_MESSAGE_STREAMING)
    }

    fn multi_message_delay_ms(&self) -> u64 {
        if !self
            .capabilities
            .contains(ChannelCapabilities::MULTI_MESSAGE_DELAY_MS)
        {
            return 800;
        }
        call_plugin_sync!(
            self,
            "multi_message_delay_ms",
            move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_multi_message_delay_ms(store)
                    .unwrap_or(800)
            }
        )
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::ADD_REACTION)
        {
            return Ok(());
        }
        let channel_id = channel_id.to_string();
        let message_id = message_id.to_string();
        let emoji = emoji.to_string();
        call_plugin!(
            self,
            "add_reaction",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_add_reaction(store, &channel_id, &message_id, &emoji)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REMOVE_REACTION)
        {
            return Ok(());
        }
        let channel_id = channel_id.to_string();
        let message_id = message_id.to_string();
        let emoji = emoji.to_string();
        call_plugin!(
            self,
            "remove_reaction",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_remove_reaction(store, &channel_id, &message_id, &emoji)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn pin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        if !self.capabilities.contains(ChannelCapabilities::PIN_MESSAGE) {
            return Ok(());
        }
        let channel_id = channel_id.to_string();
        let message_id = message_id.to_string();
        call_plugin!(
            self,
            "pin_message",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_pin_message(store, &channel_id, &message_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn unpin_message(&self, channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::UNPIN_MESSAGE)
        {
            return Ok(());
        }
        let channel_id = channel_id.to_string();
        let message_id = message_id.to_string();
        call_plugin!(
            self,
            "unpin_message",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_unpin_message(store, &channel_id, &message_id)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn redact_message(
        &self,
        channel_id: &str,
        message_id: &str,
        reason: Option<String>,
    ) -> anyhow::Result<()> {
        if !self
            .capabilities
            .contains(ChannelCapabilities::REDACT_MESSAGE)
        {
            return Ok(());
        }
        let channel_id = channel_id.to_string();
        let message_id = message_id.to_string();
        call_plugin!(
            self,
            "redact_message",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_redact_message(store, &channel_id, &message_id, reason.as_deref())
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
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
            "request_approval",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_request_approval(store, &recipient, &wit_req)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map(|opt| opt.map(from_wit_approval_response))
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    async fn request_choice(
        &self,
        question: &str,
        choices: &[String],
        timeout: std::time::Duration,
    ) -> anyhow::Result<Option<String>> {
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
            "request_choice",
            async move |store, bindings: &mut ChannelPlugin| {
                bindings
                    .zeroclaw_plugin_channel()
                    .call_request_choice(store, &question, &choices, timeout_secs)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .map_err(anyhow::Error::msg)
            }
        )
    }

    fn supports_free_form_ask(&self) -> bool {
        self.capabilities
            .contains(ChannelCapabilities::SUPPORTS_FREE_FORM_ASK)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_capabilities_flags_bitfield_round_trip() {
        // Verify that flag constants exist and can be combined/tested.
        let caps = ChannelCapabilities::HEALTH_CHECK
            | ChannelCapabilities::SEND_DRAFT
            | ChannelCapabilities::ADD_REACTION;

        assert!(caps.contains(ChannelCapabilities::HEALTH_CHECK));
        assert!(caps.contains(ChannelCapabilities::SEND_DRAFT));
        assert!(caps.contains(ChannelCapabilities::ADD_REACTION));
        assert!(!caps.contains(ChannelCapabilities::START_TYPING));
        assert!(!caps.contains(ChannelCapabilities::PIN_MESSAGE));
    }

    #[test]
    fn media_attachment_round_trip() {
        let ma = MediaAttachment {
            file_name: "photo.jpg".into(),
            data: vec![0xFF, 0xD8, 0xFF],
            mime_type: Some("image/jpeg".into()),
        };
        let wit = to_wit_media(&ma);
        let back = from_wit_media(wit);
        assert_eq!(back.file_name, "photo.jpg");
        assert_eq!(back.data, vec![0xFF_u8, 0xD8, 0xFF]);
        assert_eq!(back.mime_type.as_deref(), Some("image/jpeg"));
    }
}
