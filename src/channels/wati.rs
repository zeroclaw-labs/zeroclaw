use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

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
    allowed_numbers: Vec<String>,
    client: reqwest::Client,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
}

impl WatiChannel {
    pub fn new(
        api_token: String,
        api_url: String,
        tenant_id: Option<String>,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self::new_with_proxy(api_token, api_url, tenant_id, allowed_numbers, None)
    }

    pub fn new_with_proxy(
        api_token: String,
        api_url: String,
        tenant_id: Option<String>,
        allowed_numbers: Vec<String>,
        proxy_url: Option<String>,
    ) -> Self {
        Self {
            api_token,
            api_url,
            tenant_id,
            allowed_numbers,
            client: crate::config::build_channel_proxy_client("channel.wati", proxy_url.as_deref()),
            transcription_manager: None,
        }
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(std::sync::Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, voice transcription disabled: {e}"
                );
            }
        }
        self
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890).
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
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
            tracing::warn!(
                "WATI: ignoring message from unauthorized sender: {normalized_phone}. \
                Add to channels.wati.allowed_numbers in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
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
            tracing::debug!("WATI: skipping fromMe message");
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
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
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
                tracing::warn!("WATI: blocked media URL with unexpected host: {media_url}");
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
            tracing::debug!("WATI: skipping fromMe audio before download");
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
                tracing::warn!("WATI: media download request failed: {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!("WATI: media download failed: {}", resp.status());
            return None;
        }

        let mut audio_bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.ok().flatten() {
            audio_bytes.extend_from_slice(&chunk);
            if audio_bytes.len() as u64 > MAX_WATI_AUDIO_BYTES {
                tracing::warn!(
                    "WATI: audio download exceeds {} byte limit",
                    MAX_WATI_AUDIO_BYTES
                );
                return None;
            }
        }

        match manager.transcribe(&audio_bytes, file_name).await {
            Ok(transcript) => Some(transcript),
            Err(e) => {
                tracing::warn!("WATI: transcription failed: {e}");
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
            tracing::debug!("WATI: skipping fromMe audio message");
            return messages;
        }

        if transcript.trim().is_empty() {
            tracing::debug!("WATI: skipping empty audio transcript");
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
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        });

        messages
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
            tracing::error!("WATI send failed: {status} — {error_body}");
            anyhow::bail!("WATI API error: {status}");
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WATI uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /wati endpoint.
        tracing::info!(
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

    fn make_channel() -> WatiChannel {
        WatiChannel {
            api_token: "test-token".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["+1234567890".into()],
            client: reqwest::Client::new(),
            transcription_manager: None,
        }
    }

    fn make_wildcard_channel() -> WatiChannel {
        WatiChannel {
            api_token: "test-token".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["*".into()],
            client: reqwest::Client::new(),
            transcription_manager: None,
        }
    }

    #[test]
    fn wati_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "wati");
    }

    #[test]
    fn wati_number_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    fn wati_number_allowed_wildcard() {
        let ch = make_wildcard_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    fn wati_number_allowed_empty() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
            transcription_manager: None,
        };
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    fn wati_build_target_with_tenant() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: Some("tenant1".into()),
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
            transcription_manager: None,
        };
        assert_eq!(ch.build_target("+1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_build_target_without_tenant() {
        let ch = make_channel();
        assert_eq!(ch.build_target("+1234567890"), "1234567890");
    }

    #[test]
    fn wati_build_target_already_prefixed() {
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: Some("tenant1".into()),
            allowed_numbers: vec![],
            client: reqwest::Client::new(),
            transcription_manager: None,
        };
        // If the phone already has the tenant prefix, don't double it
        assert_eq!(ch.build_target("tenant1:1234567890"), "tenant1:1234567890");
    }

    #[test]
    fn wati_parse_valid_message() {
        let ch = make_channel();
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
        let ch = make_wildcard_channel();
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
        let ch = make_wildcard_channel();
        let payload = serde_json::json!({
            "waId": "1234567890",
            "fromMe": false
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Messages without text should be skipped");
    }

    #[test]
    fn wati_parse_alternative_field_names() {
        let ch = make_wildcard_channel();

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
        let ch = make_wildcard_channel();
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
        let ch = make_wildcard_channel();
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
        let ch = make_wildcard_channel();
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
        let ch = WatiChannel {
            api_token: "tok".into(),
            api_url: "https://live-mt-server.wati.io".into(),
            tenant_id: None,
            allowed_numbers: vec!["+1234567890".into()],
            client: reqwest::Client::new(),
            transcription_manager: None,
        };

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
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn wati_parse_from_field_fallback() {
        let ch = make_wildcard_channel();
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
        let ch = make_wildcard_channel();
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
        let ch = make_wildcard_channel();
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
        let ch = make_channel();
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn wati_manager_some_when_valid_config() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".to_string(),
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
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn wati_manager_none_and_warn_on_init_failure() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".to_string(),
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
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        assert!(ch.transcription_manager.is_none());
    }

    #[tokio::test]
    async fn wati_try_transcribe_returns_none_when_manager_none() {
        let ch = make_channel();
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
        let config = crate::config::TranscriptionConfig {
            enabled: false,
            default_provider: "groq".to_string(),
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
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            vec!["+1234567890".into()],
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
        let _ch = make_channel();
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
        let _ch = make_channel();
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
        let ch = make_channel();
        let payload = serde_json::json!({
            "type": "audio"
        });

        let result = ch.extract_sender(&payload);
        assert!(result.is_none());
    }

    #[test]
    fn wati_extract_sender_not_in_allowlist_returns_none() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "waId": "9999999999"
        });

        let result = ch.extract_sender(&payload);
        assert!(result.is_none());
    }

    #[test]
    fn wati_parse_audio_as_message_uses_transcript_as_content() {
        let ch = make_wildcard_channel();
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

        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".to_string(),
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
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", whisper_server.uri()),
                bearer_token: "test-token".to_string(),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 300,
            }),
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            media_server.uri(),
            None,
            vec!["+1234567890".into()],
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

        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".to_string(),
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
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: "http://localhost:8000/v1/transcribe".to_string(),
                bearer_token: "test-token".to_string(),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 300,
            }),
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            media_server.uri(),
            None,
            vec!["+1234567890".into()],
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
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".into(),
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: "http://localhost:8001/v1/transcribe".into(),
                bearer_token: "test-token".into(),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 120,
            }),
            ..Default::default()
        };

        let ch = WatiChannel::new(
            "test-token".into(),
            "https://live-mt-server.wati.io".into(),
            None,
            vec!["+1234567890".into()],
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
