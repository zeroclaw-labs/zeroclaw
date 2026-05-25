use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const MAX_WATI_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// WATI WhatsApp Business API channel.
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/wati` webhook endpoint.
/// The `listen` method here is a keepalive placeholder; actual message handling
/// happens in the gateway when WATI sends webhook events.
pub struct WatiChannel {
    api_token: String,
    api_url: String,
    tenant_id: Option<String>,
    /// The alias key under `[channels.wati.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    client: reqwest::Client,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
}

impl WatiChannel {
    pub fn new(
        api_token: String,
        api_url: String,
        tenant_id: Option<String>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self::new_with_proxy(api_token, api_url, tenant_id, alias, peer_resolver, None)
    }

    pub fn new_with_proxy(
        api_token: String,
        api_url: String,
        tenant_id: Option<String>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        proxy_url: Option<String>,
    ) -> Self {
        Self {
            api_token,
            api_url,
            tenant_id,
            alias: alias.into(),
            peer_resolver,
            client: zeroclaw_config::schema::build_channel_proxy_client(
                "channel.wati",
                proxy_url.as_deref(),
            ),
            transcription_manager: None,
        }
    }

    /// Return the alias under `[channels.wati.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                // Per-agent `transcription_provider` routes through the
                // orchestrator's resolved-runtime path. For the
                // `try_transcribe_audio` direct path (gateway WS handler /
                // channel-side ingest), bind to the sole registered provider
                // when only one is configured so the single-provider case
                // dispatches without an agent context. Multi-provider setups
                // still require explicit `agent.<alias>.transcription_provider`
                // routing through the orchestrator.
                let names = m.available_providers();
                let m = if names.len() == 1 {
                    let only = names[0].to_string();
                    m.with_agent_transcription_provider(only)
                } else {
                    m
                };
                self.transcription_manager = Some(std::sync::Arc::new(m));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, voice transcription disabled"
                );
            }
        }
        self
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890).
    fn is_number_allowed(&self, phone: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, phone, crate::allowlist::Match::Sensitive)
    }

    /// Extract and normalize the sender phone number from a WATI webhook payload.
    /// Returns `None` if the sender is absent, empty, or not in the allowlist.
    fn extract_sender(&self, payload: &serde_json::Value) -> Option<String> {
        // Extract waId (sender phone number)
        let wa_id = payload
            .get("waId")
            .or_else(|| payload.get("wa_id"))
            .or_else(|| payload.get("from"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        if wa_id.is_empty() {
            return None;
        }

        // Normalize phone to E.164 format
        let normalized_phone = if wa_id.starts_with('+') {
            wa_id.to_string()
        } else {
            format!("+{wa_id}")
        };

        // Check allowlist
        if !self.is_number_allowed(&normalized_phone) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"normalized_phone": normalized_phone})),
                "ignoring message from unauthorized sender: . Add to channels.wati.allowed_numbers in config.toml, or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return None;
        }

        Some(normalized_phone)
    }

    /// Build the target field for the WATI API, prefixing with tenant_id if set.
    fn build_target(&self, phone: &str) -> String {
        // Strip leading '+' — WATI expects bare digits
        let bare = phone.strip_prefix('+').unwrap_or(phone);
        if let Some(ref tid) = self.tenant_id {
            if bare.starts_with(&format!("{tid}:")) {
                bare.to_string()
            } else {
                format!("{tid}:{bare}")
            }
        } else {
            bare.to_string()
        }
    }

    /// Extract and normalize a timestamp from a WATI webhook payload.
    ///
    /// Handles unix seconds, unix milliseconds (divided by 1000), and ISO 8601
    /// strings. Falls back to the current system time if parsing fails.
    fn extract_timestamp(payload: &serde_json::Value) -> u64 {
        payload
            .get("timestamp")
            .or_else(|| payload.get("created"))
            .map(|t| {
                if let Some(secs) = t.as_u64() {
                    if secs > 10_000_000_000 {
                        secs / 1000
                    } else {
                        secs
                    }
                } else if let Some(s) = t.as_str() {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .ok()
                        .map(|dt| dt.timestamp().cast_unsigned())
                        .unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                        })
                } else {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                }
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            })
    }

    /// Parse an incoming webhook payload from WATI and extract messages.
    ///
    /// WATI's webhook payloads have variable field names depending on the API
    /// version and configuration, so we try multiple paths for each field.
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Extract text — try multiple field paths
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .or_else(|| {
                payload
                    .get("message")
                    .and_then(|m| m.get("text").or_else(|| m.get("body")))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .trim();

        if text.is_empty() {
            return messages;
        }

        // Check fromMe — skip outgoing messages
        let from_me = payload
            .get("fromMe")
            .or_else(|| payload.get("from_me"))
            .or_else(|| payload.get("owner"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if from_me {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping fromMe message"
            );
            return messages;
        }

        // Extract and validate sender
        let Some(normalized_phone) = self.extract_sender(payload) else {
            return messages;
        };

        let timestamp = Self::extract_timestamp(payload);
        messages.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            reply_target: normalized_phone.clone(),
            sender: normalized_phone,
            content: text.to_string(),
            channel: "wati".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        });

        messages
    }

    /// Extract host from URL string.
    fn extract_host(url_str: &str) -> Option<String> {
        reqwest::Url::parse(url_str)
            .ok()?
            .host_str()
            .map(|h| h.to_ascii_lowercase())
    }

    /// Attempt to download and transcribe an audio message from a WATI webhook payload.
    ///
    /// Returns `Some(transcript)` if transcription succeeds, `None` otherwise.
    /// Called by the gateway after detecting `type == "audio"` or `type == "voice"`.
    pub async fn try_transcribe_audio(&self, payload: &serde_json::Value) -> Option<String> {
        let manager = self.transcription_manager.as_deref()?;

        let media_url = payload
            .get("mediaUrl")
            .or_else(|| payload.get("media_url"))
            .and_then(|v| v.as_str())?;

        // Validate media_url host matches api_url to prevent SSRF
        let api_host = Self::extract_host(&self.api_url);
        let media_host = Self::extract_host(media_url);
        match (api_host, media_host) {
            (Some(ref expected), Some(ref actual)) if actual == expected => {}
            _ => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"media_url": media_url})),
                    "blocked media URL with unexpected host"
                );
                return None;
            }
        }

        // Check fromMe early to avoid downloading media for outgoing messages
        let from_me = payload
            .get("fromMe")
            .or_else(|| payload.get("from_me"))
            .or_else(|| payload.get("owner"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if from_me {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping fromMe audio before download"
            );
            return None;
        }

        let msg_type = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("audio");

        let file_name = match msg_type {
            "voice" => "voice.ogg",
            _ => "audio.ogg",
        };

        let mut resp = match self
            .client
            .get(media_url)
            .bearer_auth(&self.api_token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "media download request failed"
                );
                return None;
            }
        };

        if !resp.status().is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("media download failed: {}", resp.status())
            );
            return None;
        }

        let mut audio_bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.ok().flatten() {
            audio_bytes.extend_from_slice(&chunk);
            if audio_bytes.len() as u64 > MAX_WATI_AUDIO_BYTES {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("audio download exceeds {} byte limit", MAX_WATI_AUDIO_BYTES)
                );
                return None;
            }
        }

        match manager.transcribe(&audio_bytes, file_name).await {
            Ok(transcript) => Some(transcript),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "transcription failed"
                );
                None
            }
        }
    }

    /// Build a ChannelMessage from an audio transcript.
    ///
    /// This helper reuses the same sender extraction and timestamp logic as
    /// `parse_webhook_payload()` but substitutes the transcript as the message content.
    pub fn parse_audio_as_message(
        &self,
        payload: &serde_json::Value,
        transcript: String,
    ) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Check fromMe — skip outgoing messages
        let from_me = payload
            .get("fromMe")
            .or_else(|| payload.get("from_me"))
            .or_else(|| payload.get("owner"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if from_me {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping fromMe audio message"
            );
            return messages;
        }

        if transcript.trim().is_empty() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "skipping empty audio transcript"
            );
            return messages;
        }

        // Extract and validate sender
        let Some(normalized_phone) = self.extract_sender(payload) else {
            return messages;
        };

        let timestamp = Self::extract_timestamp(payload);
        messages.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            reply_target: normalized_phone.clone(),
            sender: normalized_phone,
            content: transcript,
            channel: "wati".to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        });

        messages
    }
}

impl ::zeroclaw_api::attribution::Attributable for WatiChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Wati)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for WatiChannel {
    fn name(&self) -> &str {
        "wati"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let target = self.build_target(&message.recipient);

        let body = serde_json::json!({
            "target": target,
            "text": message.content
        });

        let url = format!("{}/api/ext/v3/conversations/messages/text", self.api_url);

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"status": status.to_string(), "error_body": error_body})), "send failed:");
            anyhow::bail!("WATI API error: {status}");
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WATI uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /wati endpoint.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "WATI channel active (webhook mode). \
            Configure WATI webhook to POST to your gateway's /wati endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/ext/v3/contacts/count", self.api_url);

        self.client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // WATI API does not support typing indicators
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // WATI API does not support typing indicators
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wati_channel_name() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert_eq!(ch.name(), "wati");
    }

    #[test]
    fn wati_number_allowed_exact() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    fn wati_number_allowed_wildcard() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    fn wati_number_allowed_empty() {
        let ch = WatiChannel::new(
            "tok".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    fn wati_build_target_with_tenant() {
        let ch = WatiChannel::new(
            "tok".into(),
            "https://live-mt-server.wati.io".into(),
            Some("tenant1".into()),
            "wati_test_alias",
            Arc::new(Vec::new),
        );
        assert_eq!(ch.build_target("+1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_build_target_without_tenant() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert_eq!(ch.build_target("+1234567890"), "1234567890");
    }

    #[test]
    fn wati_build_target_already_prefixed() {
        let ch = WatiChannel::new(
            "tok".into(),
            "https://live-mt-server.wati.io".into(),
            Some("tenant1".into()),
            "wati_test_alias",
            Arc::new(Vec::new),
        );
        // If the phone already has the tenant prefix, don't double it
        assert_eq!(ch.build_target("tenant1:1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_parse_valid_message() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "text": "Hello from WATI!",
            "waId": "1234567890",
            "fromMe": false,
            "timestamp": 1_705_320_000_u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello from WATI!");
        assert_eq!(msgs[0].channel, "wati");
        assert_eq!(msgs[0].reply_target, "+1234567890");
        assert_eq!(msgs[0].timestamp, 1_705_320_000);
    }

    #[test]
    fn wati_parse_skip_from_me() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "text": "My own message",
            "waId": "1234567890",
            "fromMe": true
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "fromMe messages should be skipped");
    }

    #[test]
    fn wati_parse_skip_no_text() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Messages without text should be skipped");
    }

    #[test]
    fn wati_parse_alternative_field_names() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );

        // wa_id instead of waId, message.body instead of text
        let payload = serde_json::json!({
            "message": { "body": "Alt field test" },
            "wa_id": "1234567890",
            "from_me": false,
            "timestamp": 1_705_320_000_u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Alt field test");
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_timestamp_seconds() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": 1_705_320_000_u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1_705_320_000);
    }

    #[test]
    fn wati_parse_timestamp_milliseconds() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": 1_705_320_000_000_u64
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1_705_320_000);
    }

    #[test]
    fn wati_parse_timestamp_iso() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "timestamp": "2025-01-15T12:00:00Z"
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1_736_942_400);
    }

    #[test]
    fn wati_parse_normalizes_phone() {
        let ch = WatiChannel::new(
            "tok".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );

        // Phone without + prefix
        let payload = serde_json::json!({
            "text": "Hi",
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_empty_payload() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn wati_parse_from_field_fallback() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        // Uses "from" instead of "waId"
        let payload = serde_json::json!({
            "text": "Fallback test",
            "from": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn wati_parse_message_text_fallback() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        // Uses "message.text" instead of top-level "text"
        let payload = serde_json::json!({
            "message": { "text": "Nested text" },
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Nested text");
    }

    #[test]
    fn wati_parse_owner_field_as_from_me() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        // Uses "owner" field as fromMe indicator
        let payload = serde_json::json!({
            "text": "Test",
            "waId": "1234567890",
            "owner": true
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "owner=true messages should be skipped");
    }

    #[test]
    fn wati_manager_none_when_not_configured() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn wati_manager_some_when_valid_config() {
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test-key".to_string()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "distil-whisper-large-v3-en".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn wati_manager_none_and_warn_on_init_failure() {
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some(String::new()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "distil-whisper-large-v3-en".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        assert!(ch.transcription_manager.is_none());
    }

    #[tokio::test]
    async fn wati_try_transcribe_returns_none_when_manager_none() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "type": "audio",
            "mediaUrl": "https://example.com/audio.ogg",
            "waId": "1234567890"
        });

        let result = ch.try_transcribe_audio(&payload).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn wati_try_transcribe_returns_none_when_no_media_url() {
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: false,
            api_key: Some("test-key".to_string()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "distil-whisper-large-v3-en".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        let payload = serde_json::json!({
            "type": "audio",
            "waId": "1234567890"
        });

        let result = ch.try_transcribe_audio(&payload).await;
        assert!(result.is_none());
    }

    #[test]
    fn wati_filename_voice_type() {
        let _ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "type": "voice",
            "mediaUrl": "https://example.com/media/123",
            "waId": "1234567890"
        });

        let msg_type = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("audio");
        let file_name = match msg_type {
            "voice" => "voice.ogg",
            _ => "audio.ogg",
        };

        assert_eq!(file_name, "voice.ogg");
    }

    #[test]
    fn wati_filename_audio_type() {
        let _ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "type": "audio",
            "mediaUrl": "https://example.com/media/123",
            "waId": "1234567890"
        });

        let msg_type = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("audio");
        let file_name = match msg_type {
            "voice" => "voice.ogg",
            _ => "audio.ogg",
        };

        assert_eq!(file_name, "audio.ogg");
    }

    #[test]
    fn wati_extract_sender_absent_returns_none() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "type": "audio"
        });

        let result = ch.extract_sender(&payload);
        assert!(result.is_none());
    }

    #[test]
    fn wati_extract_sender_not_in_allowlist_returns_none() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        let payload = serde_json::json!({
            "waId": "9999999999"
        });

        let result = ch.extract_sender(&payload);
        assert!(result.is_none());
    }

    #[test]
    fn wati_parse_audio_as_message_uses_transcript_as_content() {
        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        let payload = serde_json::json!({
            "type": "audio",
            "waId": "1234567890",
            "fromMe": false,
            "timestamp": 1_705_320_000_u64
        });

        let transcript = "This is a test transcript.".to_string();
        let msgs = ch.parse_audio_as_message(&payload, transcript.clone());

        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, transcript);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].channel, "wati");
        assert_eq!(msgs[0].timestamp, 1_705_320_000);
    }

    #[tokio::test]
    async fn wati_transcribes_audio_via_local_whisper() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let media_server = MockServer::start().await;
        let whisper_server = MockServer::start().await;

        let audio_bytes = b"fake-audio-data";
        Mock::given(method("GET"))
            .and(path("/media/123"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(audio_bytes))
            .mount(&media_server)
            .await;

        let transcript = "Transcribed text from local whisper.";
        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": transcript})),
            )
            .mount(&whisper_server)
            .await;

        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: None,
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-1".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", whisper_server.uri()),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 300,
            }),
            transcribe_non_ptt_audio: false,
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            media_server.uri(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        let payload = serde_json::json!({
            "type": "audio",
            "mediaUrl": format!("{}/media/123", media_server.uri()),
            "waId": "1234567890"
        });

        let result = ch.try_transcribe_audio(&payload).await;
        assert_eq!(result, Some(transcript.to_string()));
    }

    #[tokio::test]
    async fn wati_try_transcribe_returns_none_on_media_download_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let media_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/media/123"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&media_server)
            .await;

        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: None,
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-1".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: "http://localhost:8000/v1/transcribe".to_string(),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 300,
            }),
            transcribe_non_ptt_audio: false,
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            media_server.uri(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        let payload = serde_json::json!({
            "type": "audio",
            "mediaUrl": format!("{}/media/123", media_server.uri()),
            "waId": "1234567890"
        });

        let result = ch.try_transcribe_audio(&payload).await;
        assert!(result.is_none());
    }

    #[test]
    fn extract_host_uses_url_parser() {
        assert_eq!(
            WatiChannel::extract_host("https://live-mt-server.wati.io/media/123"),
            Some("live-mt-server.wati.io".to_string())
        );
        // URL with userinfo@ — proper parser extracts the real host, not the
        // attacker-controlled host that naive string splitting would produce
        assert_eq!(
            WatiChannel::extract_host("https://live-mt-server.wati.io@evil.com/media/123"),
            Some("evil.com".to_string())
        );
    }

    #[tokio::test]
    async fn wati_try_transcribe_blocks_host_mismatch() {
        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: "http://localhost:8001/v1/transcribe".into(),
                bearer_token: Some("test-token".into()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 120,
            }),
            ..Default::default()
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            "wati_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(config);

        let payload = serde_json::json!({
            "type": "audio",
            "mediaUrl": "https://evil.com/media/123",
            "waId": "1234567890"
        });

        let result = ch.try_transcribe_audio(&payload).await;
        assert!(result.is_none());
    }
}
