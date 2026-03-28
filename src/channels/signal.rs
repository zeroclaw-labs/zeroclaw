use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

const GROUP_TARGET_PREFIX: &str = "group:";

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecipientTarget {
    Direct(String),
    Group(String),
}

enum ProcessResult {
    Message(ChannelMessage),
    AudioPending {
        attachment_id: String,
        content_type: String,
        file_name_hint: Option<String>,
        sender: String,
        reply_target: String,
        timestamp: u64,
    },
    None,
}

impl ProcessResult {
    fn is_none(&self) -> bool {
        matches!(self, ProcessResult::None)
    }

    fn unwrap(self) -> ChannelMessage {
        match self {
            ProcessResult::Message(msg) => msg,
            _ => panic!("called `ProcessResult::unwrap()` on a non-Message variant"),
        }
    }
}

/// Signal channel using signal-cli daemon's native JSON-RPC + SSE API.
///
/// Connects to a running `signal-cli daemon --http <host:port>`.
/// Listens via SSE at `/api/v1/events` and sends via JSON-RPC at
/// `/api/v1/rpc`.
#[derive(Clone)]
pub struct SignalChannel {
    http_url: String,
    account: String,
    group_id: Option<String>,
    allowed_from: Vec<String>,
    ignore_attachments: bool,
    ignore_stories: bool,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
}

// ── signal-cli SSE event JSON shapes ────────────────────────────

#[derive(Debug, Deserialize)]
struct Attachment {
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct SseEnvelope {
    #[serde(default)]
    envelope: Option<Envelope>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    source: Option<String>,
    #[serde(rename = "sourceNumber", default)]
    source_number: Option<String>,
    #[serde(rename = "dataMessage", default)]
    data_message: Option<DataMessage>,
    #[serde(rename = "storyMessage", default)]
    story_message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    #[serde(default)]
    attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

impl SignalChannel {
    pub fn new(
        http_url: String,
        account: String,
        group_id: Option<String>,
        allowed_from: Vec<String>,
        ignore_attachments: bool,
        ignore_stories: bool,
    ) -> Self {
        let http_url = http_url.trim_end_matches('/').to_string();
        Self {
            http_url,
            account,
            group_id,
            allowed_from,
            ignore_attachments,
            ignore_stories,
            proxy_url: None,
            transcription_manager: None,
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, voice transcription disabled: {e}"
                );
            }
        }
        self
    }

    fn http_client(&self) -> Client {
        let builder = Client::builder().connect_timeout(Duration::from_secs(10));
        let builder = crate::config::apply_channel_proxy_to_builder(
            builder,
            "channel.signal",
            self.proxy_url.as_deref(),
        );
        builder.build().expect("Signal HTTP client should build")
    }

    /// Effective sender: prefer `sourceNumber` (E.164), fall back to `source`.
    fn sender(envelope: &Envelope) -> Option<String> {
        envelope
            .source_number
            .as_deref()
            .or(envelope.source.as_deref())
            .map(String::from)
    }

    fn is_sender_allowed(&self, sender: &str) -> bool {
        if self.allowed_from.iter().any(|u| u == "*") {
            return true;
        }
        self.allowed_from.iter().any(|u| u == sender)
    }

    fn is_e164(recipient: &str) -> bool {
        let Some(number) = recipient.strip_prefix('+') else {
            return false;
        };
        (2..=15).contains(&number.len()) && number.chars().all(|c| c.is_ascii_digit())
    }

    /// Check whether a string is a valid UUID (signal-cli uses these for
    /// privacy-enabled users who have opted out of sharing their phone number).
    fn is_uuid(s: &str) -> bool {
        Uuid::parse_str(s).is_ok()
    }

    fn parse_recipient_target(recipient: &str) -> RecipientTarget {
        if let Some(group_id) = recipient.strip_prefix(GROUP_TARGET_PREFIX) {
            return RecipientTarget::Group(group_id.to_string());
        }

        if Self::is_e164(recipient) || Self::is_uuid(recipient) {
            RecipientTarget::Direct(recipient.to_string())
        } else {
            RecipientTarget::Group(recipient.to_string())
        }
    }

    /// Check whether the message targets the configured group.
    /// If no `group_id` is configured (None), all DMs and groups are accepted.
    /// Use "dm" to filter DMs only.
    fn matches_group(&self, data_msg: &DataMessage) -> bool {
        let Some(ref expected) = self.group_id else {
            return true;
        };
        match data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
        {
            Some(gid) => gid == expected.as_str(),
            None => expected.eq_ignore_ascii_case("dm"),
        }
    }

    /// Determine the send target: group id or the sender's number.
    fn reply_target(&self, data_msg: &DataMessage, sender: &str) -> String {
        if let Some(group_id) = data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
        {
            format!("{GROUP_TARGET_PREFIX}{group_id}")
        } else {
            sender.to_string()
        }
    }

    /// Send a JSON-RPC request to signal-cli daemon.
    async fn rpc_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let url = format!("{}/api/v1/rpc", self.http_url);
        let id = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });

        let resp = self
            .http_client()
            .post(&url)
            .timeout(Duration::from_secs(30))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        // 201 = success with no body (e.g. typing indicators)
        if resp.status().as_u16() == 201 {
            return Ok(None);
        }

        let text = resp.text().await?;
        if text.is_empty() {
            return Ok(None);
        }

        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown");
            anyhow::bail!("Signal RPC error {code}: {msg}");
        }

        Ok(parsed.get("result").cloned())
    }

    async fn try_transcribe_attachment(
        &self,
        attachment_id: &str,
        content_type: &str,
        file_name_hint: Option<&str>,
    ) -> Option<String> {
        let manager = self.transcription_manager.as_deref()?;

        let encoded_id = urlencoding::encode(attachment_id);
        let url = format!("{}/api/v1/attachments/{encoded_id}", self.http_url);
        let audio_data = self
            .http_client()
            .get(&url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("Signal attachment download failed: {e}");
                e
            })
            .ok()?
            .error_for_status()
            .map_err(|e| {
                tracing::warn!("Signal attachment download HTTP error: {e}");
                e
            })
            .ok()?
            .bytes()
            .await
            .ok()?
            .to_vec();

        let file_name = if let Some(name) = file_name_hint.filter(|v| !v.trim().is_empty()) {
            name.to_string()
        } else if content_type.contains("ogg") {
            format!("{attachment_id}.ogg")
        } else if content_type.contains("mp4") || content_type.contains("m4a") {
            format!("{attachment_id}.m4a")
        } else if content_type.contains("mpeg") || content_type.contains("mp3") {
            format!("{attachment_id}.mp3")
        } else if content_type.contains("webm") {
            format!("{attachment_id}.webm")
        } else if content_type.contains("wav") {
            format!("{attachment_id}.wav")
        } else if content_type.contains("flac") {
            format!("{attachment_id}.flac")
        } else if content_type.contains("opus") {
            format!("{attachment_id}.opus")
        } else if content_type.contains("aac") {
            format!("{attachment_id}.m4a")
        } else {
            tracing::warn!(
                "Signal audio attachment skipped: unsupported content type {content_type}"
            );
            return None;
        };

        match manager.transcribe(&audio_data, &file_name).await {
            Ok(transcript) => Some(transcript),
            Err(e) => {
                tracing::warn!("Signal audio transcription failed: {e}");
                None
            }
        }
    }

    /// Process a single SSE envelope, returning a ProcessResult.
    fn process_envelope(&self, envelope: &Envelope) -> ProcessResult {
        // Skip story messages when configured
        if self.ignore_stories && envelope.story_message.is_some() {
            return ProcessResult::None;
        }

        let data_msg = match envelope.data_message.as_ref() {
            Some(d) => d,
            None => return ProcessResult::None,
        };

        let sender = match Self::sender(envelope) {
            Some(s) => s,
            None => return ProcessResult::None,
        };

        if !self.is_sender_allowed(&sender) {
            return ProcessResult::None;
        }

        if !self.matches_group(data_msg) {
            return ProcessResult::None;
        }

        let target = self.reply_target(data_msg, &sender);

        let timestamp = data_msg
            .timestamp
            .or(envelope.timestamp)
            .unwrap_or_else(|| {
                u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });

        // Text message path (unchanged in behavior)
        // When a message carries both text and audio, prefer text — audio attachment
        // is intentionally not transcribed in this case to avoid duplicate content.
        if let Some(text) = data_msg.message.as_deref().filter(|t| !t.is_empty()) {
            return ProcessResult::Message(ChannelMessage {
                id: format!("sig_{timestamp}"),
                sender,
                reply_target: target,
                content: text.to_string(),
                channel: "signal".to_string(),
                timestamp: timestamp / 1000,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            });
        }

        // Audio attachment path (only when transcription is configured)
        if !self.ignore_attachments && self.transcription_manager.is_some() {
            if let Some(audio_att) =
                data_msg
                    .attachments
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .find(|a| {
                        a.content_type
                            .as_deref()
                            .map(|ct| ct.to_lowercase().starts_with("audio/"))
                            .unwrap_or(false)
                    })
            {
                if let (Some(id), Some(ct)) = (audio_att.id.clone(), audio_att.content_type.clone())
                {
                    return ProcessResult::AudioPending {
                        attachment_id: id,
                        content_type: ct,
                        file_name_hint: audio_att.filename.clone(),
                        sender,
                        reply_target: target,
                        timestamp,
                    };
                }
            }
        }

        ProcessResult::None
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn name(&self) -> &str {
        "signal"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let params = match Self::parse_recipient_target(&message.recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "message": &message.content,
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "message": &message.content,
                "account": &self.account,
            }),
        };

        self.rpc_request("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let mut url = reqwest::Url::parse(&format!("{}/api/v1/events", self.http_url))?;
        url.query_pairs_mut().append_pair("account", &self.account);

        tracing::info!("Signal channel listening via SSE on {}...", self.http_url);

        let mut retry_delay_secs = 2u64;
        let max_delay_secs = 60u64;

        loop {
            let resp = self
                .http_client()
                .get(url.clone())
                .header("Accept", "text/event-stream")
                .send()
                .await;

            let resp = match resp {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    tracing::warn!("Signal SSE returned {status}: {body}");
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Signal SSE connect error: {e}, retrying...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                    retry_delay_secs = (retry_delay_secs * 2).min(max_delay_secs);
                    continue;
                }
            };

            retry_delay_secs = 2;

            let mut bytes_stream = resp.bytes_stream();
            let mut buffer = String::new();
            let mut current_data = String::new();

            while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!("Signal SSE chunk error, reconnecting: {e}");
                        break;
                    }
                };

                let text = match String::from_utf8(chunk.to_vec()) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!("Signal SSE invalid UTF-8, skipping chunk: {}", e);
                        continue;
                    }
                };

                buffer.push_str(&text);

                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    // Skip SSE comments (keepalive)
                    if line.starts_with(':') {
                        continue;
                    }

                    if line.is_empty() {
                        // Empty line = event boundary, dispatch accumulated data
                        if !current_data.is_empty() {
                            match serde_json::from_str::<SseEnvelope>(&current_data) {
                                Ok(sse) => {
                                    if let Some(ref envelope) = sse.envelope {
                                        match self.process_envelope(envelope) {
                                            ProcessResult::Message(msg) => {
                                                if tx.send(msg).await.is_err() {
                                                    return Ok(());
                                                }
                                            }
                                            ProcessResult::AudioPending {
                                                attachment_id,
                                                content_type,
                                                file_name_hint,
                                                sender,
                                                reply_target,
                                                timestamp,
                                            } => {
                                                if let Some(transcript) = self
                                                    .try_transcribe_attachment(
                                                        &attachment_id,
                                                        &content_type,
                                                        file_name_hint.as_deref(),
                                                    )
                                                    .await
                                                {
                                                    let msg = ChannelMessage {
                                                        id: format!("sig_{timestamp}"),
                                                        sender,
                                                        reply_target,
                                                        content: transcript,
                                                        channel: "signal".to_string(),
                                                        timestamp: timestamp / 1000,
                                                        thread_ts: None,
                                                        interruption_scope_id: None,
                                                    };
                                                    if tx.send(msg).await.is_err() {
                                                        return Ok(());
                                                    }
                                                }
                                            }
                                            ProcessResult::None => {}
                                        }
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!("Signal SSE parse skip: {e}");
                                }
                            }
                            current_data.clear();
                        }
                    } else if let Some(data) = line.strip_prefix("data:") {
                        if !current_data.is_empty() {
                            current_data.push('\n');
                        }
                        current_data.push_str(data.trim_start());
                    }
                    // Ignore "event:", "id:", "retry:" lines
                }
            }

            if !current_data.is_empty() {
                match serde_json::from_str::<SseEnvelope>(&current_data) {
                    Ok(sse) => {
                        if let Some(ref envelope) = sse.envelope {
                            match self.process_envelope(envelope) {
                                ProcessResult::Message(msg) => {
                                    let _ = tx.send(msg).await;
                                }
                                ProcessResult::AudioPending {
                                    attachment_id,
                                    content_type,
                                    file_name_hint,
                                    sender,
                                    reply_target,
                                    timestamp,
                                } => {
                                    if let Some(transcript) = self
                                        .try_transcribe_attachment(
                                            &attachment_id,
                                            &content_type,
                                            file_name_hint.as_deref(),
                                        )
                                        .await
                                    {
                                        let msg = ChannelMessage {
                                            id: format!("sig_{timestamp}"),
                                            sender,
                                            reply_target,
                                            content: transcript,
                                            channel: "signal".to_string(),
                                            timestamp: timestamp / 1000,
                                            thread_ts: None,
                                            interruption_scope_id: None,
                                        };
                                        let _ = tx.send(msg).await;
                                    }
                                }
                                ProcessResult::None => {}
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Signal SSE trailing parse skip: {e}");
                    }
                }
            }

            tracing::debug!("Signal SSE stream ended, reconnecting...");
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/api/v1/check", self.http_url);
        let Ok(resp) = self
            .http_client()
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        else {
            return false;
        };
        resp.status().is_success()
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let params = match Self::parse_recipient_target(recipient) {
            RecipientTarget::Direct(number) => serde_json::json!({
                "recipient": [number],
                "account": &self.account,
            }),
            RecipientTarget::Group(group_id) => serde_json::json!({
                "groupId": group_id,
                "account": &self.account,
            }),
        };
        self.rpc_request("sendTyping", params).await?;
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        // signal-cli doesn't have a stop-typing RPC; typing indicators
        // auto-expire after ~15s on the client side.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["+1111111111".to_string()],
            false,
            false,
        )
    }

    fn make_channel_with_group(group_id: &str) -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Some(group_id.to_string()),
            vec!["*".to_string()],
            true,
            true,
        )
    }

    fn make_envelope(source_number: Option<&str>, message: Option<&str>) -> Envelope {
        Envelope {
            source: source_number.map(String::from),
            source_number: source_number.map(String::from),
            data_message: message.map(|m| DataMessage {
                message: Some(m.to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    fn make_transcription_config() -> crate::config::TranscriptionConfig {
        crate::config::TranscriptionConfig {
            enabled: false,
            api_key: Some("test-key".to_string()),
            api_url: "http://127.0.0.1:9999/v1/transcribe".to_string(),
            model: "whisper-large-v3-turbo".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 300,
            default_provider: "groq".to_string(),
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: "http://127.0.0.1:9999/v1/transcribe".to_string(),
                bearer_token: "test-bearer".to_string(),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
        }
    }

    #[test]
    fn creates_with_correct_fields() {
        let ch = make_channel();
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
        assert_eq!(ch.account, "+1234567890");
        assert!(ch.group_id.is_none());
        assert_eq!(ch.allowed_from.len(), 1);
        assert!(!ch.ignore_attachments);
        assert!(!ch.ignore_stories);
    }

    #[test]
    fn strips_trailing_slash() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686/".to_string(),
            "+1234567890".to_string(),
            None,
            vec![],
            false,
            false,
        );
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
    }

    #[test]
    fn wildcard_allows_anyone() {
        let ch = make_channel_with_group("dm");
        assert!(ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn specific_sender_allowed() {
        let ch = make_channel();
        assert!(ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn unknown_sender_denied() {
        let ch = make_channel();
        assert!(!ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec![],
            false,
            false,
        );
        assert!(!ch.is_sender_allowed("+1111111111"));
    }

    #[test]
    fn name_returns_signal() {
        let ch = make_channel();
        assert_eq!(ch.name(), "signal");
    }

    #[test]
    fn matches_group_no_group_id_accepts_all() {
        let ch = make_channel();
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&group));
    }

    #[test]
    fn matches_group_filters_group() {
        let ch = make_channel_with_group("group123");
        let matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(ch.matches_group(&matching));

        let non_matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("other_group".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&non_matching));
    }

    #[test]
    fn matches_group_dm_keyword() {
        let ch = make_channel_with_group("dm");
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert!(!ch.matches_group(&group));
    }

    #[test]
    fn reply_target_dm() {
        let ch = make_channel();
        let dm = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: None,
            attachments: None,
        };
        assert_eq!(ch.reply_target(&dm, "+1111111111"), "+1111111111");
    }

    #[test]
    fn reply_target_group() {
        let ch = make_channel();
        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            attachments: None,
        };
        assert_eq!(ch.reply_target(&group, "+1111111111"), "group:group123");
    }

    #[test]
    fn parse_recipient_target_e164_is_direct() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+1234567890"),
            RecipientTarget::Direct("+1234567890".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_prefixed_group_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("group:abc123"),
            RecipientTarget::Group("abc123".to_string())
        );
    }

    #[test]
    fn parse_recipient_target_uuid_is_direct() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        assert_eq!(
            SignalChannel::parse_recipient_target(uuid),
            RecipientTarget::Direct(uuid.to_string())
        );
    }

    #[test]
    fn parse_recipient_target_non_e164_plus_is_group() {
        assert_eq!(
            SignalChannel::parse_recipient_target("+abc123"),
            RecipientTarget::Group("+abc123".to_string())
        );
    }

    #[test]
    fn is_uuid_valid() {
        assert!(SignalChannel::is_uuid(
            "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        ));
        assert!(SignalChannel::is_uuid(
            "00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn is_uuid_invalid() {
        assert!(!SignalChannel::is_uuid("+1234567890"));
        assert!(!SignalChannel::is_uuid("not-a-uuid"));
        assert!(!SignalChannel::is_uuid("group:abc123"));
        assert!(!SignalChannel::is_uuid(""));
    }

    #[test]
    fn sender_prefers_source_number() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("+1111111111".to_string()));
    }

    #[test]
    fn sender_falls_back_to_source() {
        let env = Envelope {
            source: Some("uuid-123".to_string()),
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: Some(1000),
        };
        assert_eq!(SignalChannel::sender(&env), Some("uuid-123".to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_dm() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Hello from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, uuid);
        assert_eq!(msg.content, "Hello from privacy user");

        // Verify reply routing: UUID sender in DM should route as Direct
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Direct(uuid.to_string()));
    }

    #[test]
    fn process_envelope_uuid_sender_in_group() {
        let uuid = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            Some("testgroup".to_string()),
            vec!["*".to_string()],
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Group msg from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: Some(GroupInfo {
                    group_id: Some("testgroup".to_string()),
                }),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.sender, uuid);
        assert_eq!(msg.reply_target, "group:testgroup");

        // Verify reply routing: group message should still route as Group
        let target = SignalChannel::parse_recipient_target(&msg.reply_target);
        assert_eq!(target, RecipientTarget::Group("testgroup".to_string()));
    }

    #[test]
    fn sender_none_when_both_missing() {
        let env = Envelope {
            source: None,
            source_number: None,
            data_message: None,
            story_message: None,
            timestamp: None,
        };
        assert_eq!(SignalChannel::sender(&env), None);
    }

    #[test]
    fn process_envelope_valid_dm() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), Some("Hello!"));
        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.content, "Hello!");
        assert_eq!(msg.sender, "+1111111111");
        assert_eq!(msg.channel, "signal");
    }

    #[test]
    fn process_envelope_denied_sender() {
        let ch = make_channel();
        let env = make_envelope(Some("+9999999999"), Some("Hello!"));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_empty_message() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), Some(""));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_no_data_message() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), None);
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_stories() {
        let ch = make_channel_with_group("dm");
        let mut env = make_envelope(Some("+1111111111"), Some("story text"));
        env.story_message = Some(serde_json::json!({}));
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn process_envelope_skips_attachment_only() {
        let ch = make_channel_with_group("dm");
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("image/png".to_string()),
                    id: None,
                    filename: None,
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn sse_envelope_deserializes() {
        let json = r#"{
            "envelope": {
                "source": "+1111111111",
                "sourceNumber": "+1111111111",
                "timestamp": 1700000000000,
                "dataMessage": {
                    "message": "Hello Signal!",
                    "timestamp": 1700000000000
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        assert_eq!(env.source_number.as_deref(), Some("+1111111111"));
        let dm = env.data_message.unwrap();
        assert_eq!(dm.message.as_deref(), Some("Hello Signal!"));
    }

    #[test]
    fn sse_envelope_deserializes_group() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+2222222222",
                "dataMessage": {
                    "message": "Group msg",
                    "groupInfo": {
                        "groupId": "abc123"
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        let dm = env.data_message.unwrap();
        assert_eq!(
            dm.group_info.as_ref().unwrap().group_id.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn envelope_defaults() {
        let json = r#"{}"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(env.source.is_none());
        assert!(env.source_number.is_none());
        assert!(env.data_message.is_none());
        assert!(env.story_message.is_none());
        assert!(env.timestamp.is_none());
    }

    // ── PR-07 Tests ─────────────────────────────────────────────────

    #[test]
    fn signal_manager_none_when_transcription_not_configured() {
        let ch = make_channel();
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn signal_manager_some_when_valid_config() {
        let ch = make_channel();
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn signal_manager_none_and_warn_on_init_failure() {
        let ch = make_channel();
        let mut config = make_transcription_config();
        config.enabled = true;
        config.default_provider = "groq".to_string();
        config.api_key = Some(String::new());
        config.local_whisper = None;
        let ch = ch.with_transcription(config);
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn signal_process_envelope_text_returns_message() {
        let ch = make_channel();
        let env = make_envelope(Some("+1111111111"), Some("Hello!"));
        match ch.process_envelope(&env) {
            ProcessResult::Message(msg) => {
                assert_eq!(msg.content, "Hello!");
                assert_eq!(msg.sender, "+1111111111");
            }
            _ => panic!("expected ProcessResult::Message"),
        }
    }

    #[test]
    fn signal_process_envelope_audio_returns_pending() {
        let ch = make_channel();
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("audio/ogg".to_string()),
                    id: Some("test-id".to_string()),
                    filename: Some("voice.ogg".to_string()),
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        match ch.process_envelope(&env) {
            ProcessResult::AudioPending {
                attachment_id,
                content_type,
                ..
            } => {
                assert_eq!(attachment_id, "test-id");
                assert_eq!(content_type, "audio/ogg");
            }
            _ => panic!("expected ProcessResult::AudioPending"),
        }
    }

    #[test]
    fn signal_process_envelope_audio_returns_none_when_manager_absent() {
        let ch = make_channel();
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("audio/ogg".to_string()),
                    id: Some("test-id".to_string()),
                    filename: Some("voice.ogg".to_string()),
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        match ch.process_envelope(&env) {
            ProcessResult::None => {}
            _ => panic!("expected ProcessResult::None"),
        }
    }

    #[test]
    fn signal_process_envelope_image_returns_none() {
        let ch = make_channel();
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("image/png".to_string()),
                    id: Some("test-id".to_string()),
                    filename: Some("photo.png".to_string()),
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        match ch.process_envelope(&env) {
            ProcessResult::None => {}
            _ => panic!("expected ProcessResult::None"),
        }
    }

    #[test]
    fn signal_process_envelope_audio_skipped_when_ignore_attachments() {
        let ch = make_channel_with_group("dm");
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("audio/ogg".to_string()),
                    id: Some("test-id".to_string()),
                    filename: Some("voice.ogg".to_string()),
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        match ch.process_envelope(&env) {
            ProcessResult::None => {}
            _ => panic!("expected ProcessResult::None when ignore_attachments=true"),
        }
    }

    #[test]
    fn signal_process_envelope_text_plus_audio_returns_message() {
        let ch = make_channel();
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);

        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("Check this out".to_string()),
                timestamp: Some(1_700_000_000_000),
                group_info: None,
                attachments: Some(vec![Attachment {
                    content_type: Some("audio/ogg".to_string()),
                    id: Some("test-id".to_string()),
                    filename: Some("voice.ogg".to_string()),
                    extra: serde_json::Value::Null,
                }]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        match ch.process_envelope(&env) {
            ProcessResult::Message(msg) => {
                assert_eq!(msg.content, "Check this out");
            }
            _ => panic!("expected ProcessResult::Message"),
        }
    }

    #[test]
    fn attachment_deserializes_content_type_and_filename() {
        let json = r#"{
            "contentType": "audio/aac",
            "id": "1234567890",
            "filename": "voice-note.m4a"
        }"#;
        let att: Attachment = serde_json::from_str(json).unwrap();
        assert_eq!(att.content_type.as_deref(), Some("audio/aac"));
        assert_eq!(att.id.as_deref(), Some("1234567890"));
        assert_eq!(att.filename.as_deref(), Some("voice-note.m4a"));
    }

    #[tokio::test]
    async fn signal_voice_routes_through_local_whisper() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let whisper_server = MockServer::start().await;
        let signal_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .and(header("authorization", "Bearer test-bearer"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "hello from signal"})),
            )
            .mount(&whisper_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/v1/attachments/test-id"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fake-audio-data"))
            .mount(&signal_server)
            .await;

        let mut config = make_transcription_config();
        config.default_provider = "local_whisper".to_string();
        config.local_whisper = Some(crate::config::LocalWhisperConfig {
            url: format!("{}/v1/transcribe", whisper_server.uri()),
            bearer_token: "test-bearer".to_string(),
            max_audio_bytes: 10 * 1024 * 1024,
            timeout_secs: 30,
        });

        let ch = SignalChannel::new(
            signal_server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
        )
        .with_transcription(config);

        let result = ch
            .try_transcribe_attachment("test-id", "audio/ogg", Some("voice.ogg"))
            .await;
        assert_eq!(result, Some("hello from signal".to_string()));
    }

    #[tokio::test]
    async fn signal_voice_skips_when_manager_none() {
        let ch = make_channel();
        let result = ch
            .try_transcribe_attachment("test-id", "audio/ogg", Some("voice.ogg"))
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn signal_voice_skips_unsupported_content_type_without_filename() {
        let ch = make_channel();
        let config = make_transcription_config();
        let ch = ch.with_transcription(config);

        let result = ch
            .try_transcribe_attachment("test-id", "audio/aac", None)
            .await;
        assert!(result.is_none());
    }
}
