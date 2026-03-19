use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use mime_guess::get_mime_extensions_str;
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

const GROUP_TARGET_PREFIX: &str = "group:";

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecipientTarget {
    Direct(String),
    Group(String),
}

/// Signal channel using signal-cli daemon's native JSON-RPC + SSE API.
///
/// Connects to a running `signal-cli daemon --http <host:port>`.
/// Listens via SSE at `/api/v1/events` and sends via JSON-RPC at
/// `/api/v1/rpc`.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct SignalChannel {
    http_url: String,
    account: String,
    group_id: Option<String>,
    allowed_from: Vec<String>,
    mention_only: bool,
    ignore_attachments: bool,
    download_attachments: bool,
    ignore_stories: bool,
}

// ── signal-cli SSE event JSON shapes ────────────────────────────

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
    #[serde(default)]
    quote: Option<Quote>,
    #[serde(rename = "groupInfo", default)]
    group_info: Option<GroupInfo>,
    #[serde(default)]
    mentions: Option<Vec<Mention>>,
    #[serde(default)]
    attachments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct Quote {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(rename = "authorNumber", default)]
    author_number: Option<String>,
    #[serde(default)]
    attachments: Vec<QuotedAttachment>,
}

#[derive(Debug, Deserialize)]
struct QuotedAttachment {
    #[serde(rename = "contentType", default)]
    content_type: Option<String>,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Mention {
    #[serde(default)]
    number: Option<String>,
    #[serde(default)]
    uuid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    #[serde(rename = "groupId", default)]
    group_id: Option<String>,
}

struct ProcessedEnvelope {
    sender: String,
    reply_target: String,
    content_parts: Vec<String>,
    timestamp: u64,
    observe_group: bool,
}

impl ProcessedEnvelope {
    fn into_message(self) -> Option<ChannelMessage> {
        if self.content_parts.is_empty() {
            return None;
        }

        Some(ChannelMessage {
            id: format!("sig_{}", self.timestamp),
            sender: self.sender.clone(),
            reply_target: self.reply_target,
            content: self.content_parts.join("\n\n"),
            channel: "signal".to_string(),
            timestamp: self.timestamp / 1000,
            thread_ts: None,
            observe_group: self.observe_group,
        })
    }
}

impl SignalChannel {
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn new(
        http_url: String,
        account: String,
        group_id: Option<String>,
        allowed_from: Vec<String>,
        mention_only: bool,
        ignore_attachments: bool,
        download_attachments: bool,
        ignore_stories: bool,
    ) -> Self {
        let http_url = http_url.trim_end_matches('/').to_string();
        Self {
            http_url,
            account,
            group_id,
            allowed_from,
            mention_only,
            ignore_attachments,
            download_attachments,
            ignore_stories,
        }
    }

    fn http_client(&self) -> Client {
        let builder = Client::builder().connect_timeout(Duration::from_secs(10));
        let builder = crate::config::apply_runtime_proxy_to_builder(builder, "channel.signal");
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

    fn is_group_message(data_msg: &DataMessage) -> bool {
        data_msg
            .group_info
            .as_ref()
            .and_then(|g| g.group_id.as_deref())
            .is_some()
    }

    fn is_account_mentioned(&self, data_msg: &DataMessage) -> bool {
        data_msg.mentions.as_ref().is_some_and(|mentions| {
            mentions.iter().any(|mention| {
                mention.number.as_deref() == Some(self.account.as_str())
                    || mention.uuid.as_deref() == Some(self.account.as_str())
            })
        })
    }

    /// Check whether the quoted message was authored by this bot's account.
    fn is_reply_to_self(&self, data_msg: &DataMessage) -> bool {
        data_msg.quote.as_ref().is_some_and(|quote| {
            quote
                .author_number
                .as_deref()
                .or(quote.author.as_deref())
                .is_some_and(|author| author == self.account)
        })
    }

    fn extract_mentions(data_msg: &DataMessage) -> Option<String> {
        let mentions = data_msg
            .mentions
            .as_ref()
            .filter(|mentions| !mentions.is_empty())?;
        let mention_list: Vec<&str> = mentions
            .iter()
            .filter_map(|mention| mention.number.as_deref().or(mention.uuid.as_deref()))
            .collect();

        if mention_list.is_empty() {
            return None;
        }

        Some(format!("[Mentioned: {}]", mention_list.join(", ")))
    }

    fn extract_reply_context(data_msg: &DataMessage) -> Option<String> {
        let quote = data_msg.quote.as_ref()?;
        let preview = Self::quoted_preview(quote).unwrap_or_else(|| "[Quoted message]".to_string());
        let author_info = quote
            .author_number
            .as_deref()
            .or(quote.author.as_deref())
            .map(|author| format!(" (from {author})"))
            .unwrap_or_default();
        Some(format!("[Replying to: \"{preview}\"{author_info}]"))
    }

    fn quoted_preview(quote: &Quote) -> Option<String> {
        if let Some(text) = quote
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return Some(truncate_reply_preview(text, 200));
        }

        let attachment = quote.attachments.first()?;
        let preview = match attachment.content_type.as_deref() {
            Some(content_type) if content_type.starts_with("image/") => "[Photo]".to_string(),
            Some(content_type) if content_type.starts_with("video/") => "[Video]".to_string(),
            Some(content_type) if content_type.starts_with("audio/") => "[Audio]".to_string(),
            Some(content_type) if content_type.starts_with("application/") => attachment
                .filename
                .as_deref()
                .map(|filename| format!("[Document: {}]", truncate_reply_preview(filename, 120)))
                .unwrap_or_else(|| "[Document]".to_string()),
            _ => attachment
                .filename
                .as_deref()
                .map(|filename| format!("[Attachment: {}]", truncate_reply_preview(filename, 120)))
                .unwrap_or_else(|| "[Attachment]".to_string()),
        };

        Some(preview)
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

    fn attachment_content_type(attachment: &serde_json::Value) -> Option<&str> {
        attachment
            .get("contentType")
            .and_then(serde_json::Value::as_str)
    }

    fn attachment_id(attachment: &serde_json::Value) -> Option<&str> {
        attachment.get("id").and_then(serde_json::Value::as_str)
    }

    fn attachment_file_name(attachment: &serde_json::Value) -> Option<&str> {
        attachment
            .get("fileName")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                attachment
                    .get("filename")
                    .and_then(serde_json::Value::as_str)
            })
    }

    fn attachment_file_path(attachment: &serde_json::Value) -> Option<PathBuf> {
        let path = attachment.get("file").and_then(serde_json::Value::as_str)?;
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        let path = PathBuf::from(path);
        path.exists().then_some(path)
    }

    fn attachment_marker_label(content_type: Option<&str>) -> &'static str {
        match content_type {
            Some(content_type) if content_type.starts_with("image/") => "Image file",
            Some(content_type) if content_type.starts_with("video/") => "Video file",
            Some(content_type) if content_type.starts_with("audio/") => "Audio file",
            Some(content_type)
                if content_type.starts_with("application/")
                    || content_type.starts_with("text/") =>
            {
                "Document file"
            }
            _ => "Attachment file",
        }
    }

    fn sanitize_attachment_filename(file_name: &str) -> Option<String> {
        let basename = Path::new(file_name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_name.to_string());
        let basename = basename.trim();

        if basename.is_empty() || basename == "." || basename == ".." {
            return None;
        }

        let sanitized: String = basename
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect();

        if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
            None
        } else {
            Some(sanitized)
        }
    }

    fn ensure_attachment_extension(file_name: String, content_type: Option<&str>) -> String {
        if Path::new(&file_name).extension().is_some() {
            return file_name;
        }

        let Some(content_type) = content_type else {
            return file_name;
        };
        let Some(extensions) = get_mime_extensions_str(content_type) else {
            return file_name;
        };
        let Some(extension) = extensions.first() else {
            return file_name;
        };

        format!("{file_name}.{extension}")
    }

    fn attachment_storage_path(
        &self,
        attachment: &serde_json::Value,
        attachment_id: &str,
    ) -> PathBuf {
        let file_name = Self::attachment_file_name(attachment)
            .and_then(Self::sanitize_attachment_filename)
            .or_else(|| Self::sanitize_attachment_filename(attachment_id))
            .unwrap_or_else(|| "attachment".to_string());
        let file_name =
            Self::ensure_attachment_extension(file_name, Self::attachment_content_type(attachment));

        std::env::temp_dir()
            .join("zeroclaw-signal")
            .join(format!("{}-{file_name}", Uuid::new_v4()))
    }

    async fn download_attachment(
        &self,
        envelope: &Envelope,
        data_msg: &DataMessage,
        attachment: &serde_json::Value,
    ) -> anyhow::Result<Option<PathBuf>> {
        if let Some(path) = Self::attachment_file_path(attachment) {
            return Ok(Some(path));
        }

        let Some(attachment_id) = Self::attachment_id(attachment).map(str::trim) else {
            return Ok(None);
        };
        if attachment_id.is_empty() {
            return Ok(None);
        }

        let mut params = serde_json::json!({
            "account": &self.account,
            "id": attachment_id,
        });

        if let Some(group_id) = data_msg
            .group_info
            .as_ref()
            .and_then(|group| group.group_id.as_deref())
        {
            params["groupId"] = serde_json::json!(group_id);
        } else if let Some(sender) = Self::sender(envelope) {
            params["recipient"] = serde_json::json!(sender);
        } else {
            return Ok(None);
        }

        let result = self
            .rpc_request("getAttachment", params)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Signal getAttachment returned no result"))?;
        let encoded = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Signal getAttachment result was not a string"))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|err| anyhow::anyhow!("invalid base64 attachment payload: {err}"))?;

        let path = self.attachment_storage_path(attachment, attachment_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, bytes).await?;

        Ok(Some(path))
    }

    async fn attachment_marker(
        &self,
        envelope: &Envelope,
        data_msg: &DataMessage,
        attachment: &serde_json::Value,
    ) -> Option<String> {
        match self
            .download_attachment(envelope, data_msg, attachment)
            .await
        {
            Ok(Some(path)) => Some(format!(
                "[{}: {}]",
                Self::attachment_marker_label(Self::attachment_content_type(attachment)),
                path.display()
            )),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!("Signal attachment download failed: {err}");
                None
            }
        }
    }

    fn process_envelope_parts(&self, envelope: &Envelope) -> Option<ProcessedEnvelope> {
        // Skip story messages when configured
        if self.ignore_stories && envelope.story_message.is_some() {
            return None;
        }

        let data_msg = envelope.data_message.as_ref()?;
        let has_attachments = data_msg.attachments.as_ref().is_some_and(|a| !a.is_empty());

        // Skip attachment-only messages when configured
        if self.ignore_attachments && has_attachments && data_msg.message.is_none() {
            return None;
        }

        let text = data_msg.message.as_deref().filter(|t| !t.is_empty());
        let sender = Self::sender(envelope)?;
        let mut content_parts = Vec::new();
        if let Some(reply_context) = Self::extract_reply_context(data_msg) {
            content_parts.push(reply_context);
        }
        if let Some(mentions) = Self::extract_mentions(data_msg) {
            content_parts.push(mentions);
        }
        if let Some(text) = text {
            content_parts.push(text.to_string());
        }
        if content_parts.is_empty()
            && !(self.download_attachments && !self.ignore_attachments && has_attachments)
        {
            return None;
        }

        if !self.is_sender_allowed(&sender) {
            return None;
        }

        if !self.matches_group(data_msg) {
            return None;
        }

        let is_group = Self::is_group_message(data_msg);
        let is_mentioned = self.is_account_mentioned(data_msg);
        let is_reply_to_bot = self.is_reply_to_self(data_msg);

        let target = self.reply_target(data_msg, &sender);

        if self.mention_only && is_group && !is_mentioned && !is_reply_to_bot {
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

            return Some(ProcessedEnvelope {
                sender: sender.clone(),
                reply_target: target,
                content_parts,
                timestamp,
                observe_group: true,
            });
        }

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

        Some(ProcessedEnvelope {
            sender,
            reply_target: target,
            content_parts,
            timestamp,
            observe_group: false,
        })
    }

    /// Process a single SSE envelope, returning a ChannelMessage if valid.
    fn process_envelope(&self, envelope: &Envelope) -> Option<ChannelMessage> {
        self.process_envelope_parts(envelope)?.into_message()
    }

    async fn process_envelope_with_attachments(
        &self,
        envelope: &Envelope,
    ) -> Option<ChannelMessage> {
        let data_msg = envelope.data_message.as_ref()?;
        let mut processed = self.process_envelope_parts(envelope)?;

        if self.download_attachments && !self.ignore_attachments {
            if let Some(attachments) = data_msg.attachments.as_ref() {
                for attachment in attachments {
                    if let Some(marker) =
                        self.attachment_marker(envelope, data_msg, attachment).await
                    {
                        processed.content_parts.push(marker);
                    }
                }
            }
        }

        processed.into_message()
    }
}

fn truncate_reply_preview(text: &str, max_chars: usize) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        normalized
    } else {
        let truncated: String = normalized.chars().take(max_chars).collect();
        format!("{truncated}...")
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
                                        if let Some(msg) =
                                            self.process_envelope_with_attachments(envelope).await
                                        {
                                            if tx.send(msg).await.is_err() {
                                                return Ok(());
                                            }
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
                            if let Some(msg) =
                                self.process_envelope_with_attachments(envelope).await
                            {
                                let _ = tx.send(msg).await;
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
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_channel() -> SignalChannel {
        SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["+1111111111".to_string()],
            false,
            false,
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
            false,
            true,
            false,
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
                quote: None,
                group_info: None,
                mentions: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        }
    }

    #[test]
    fn creates_with_correct_fields() {
        let ch = make_channel();
        assert_eq!(ch.http_url, "http://127.0.0.1:8686");
        assert_eq!(ch.account, "+1234567890");
        assert!(ch.group_id.is_none());
        assert_eq!(ch.allowed_from.len(), 1);
        assert!(!ch.mention_only);
        assert!(!ch.ignore_attachments);
        assert!(!ch.download_attachments);
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
            quote: None,
            group_info: None,
            mentions: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            mentions: None,
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
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            mentions: None,
            attachments: None,
        };
        assert!(ch.matches_group(&matching));

        let non_matching = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("other_group".to_string()),
            }),
            mentions: None,
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
            quote: None,
            group_info: None,
            mentions: None,
            attachments: None,
        };
        assert!(ch.matches_group(&dm));

        let group = DataMessage {
            message: Some("hi".to_string()),
            timestamp: Some(1000),
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            mentions: None,
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
            quote: None,
            group_info: None,
            mentions: None,
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
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            mentions: None,
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
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Hello from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: None,
                mentions: None,
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
            false,
            false,
        );
        let env = Envelope {
            source: Some(uuid.to_string()),
            source_number: None,
            data_message: Some(DataMessage {
                message: Some("Group msg from privacy user".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: Some(GroupInfo {
                    group_id: Some("testgroup".to_string()),
                }),
                mentions: None,
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
    fn extract_mentions_uses_numbers_and_uuids() {
        let data_msg = DataMessage {
            message: Some("hello".to_string()),
            timestamp: Some(1_700_000_000_000),
            quote: None,
            group_info: Some(GroupInfo {
                group_id: Some("group123".to_string()),
            }),
            mentions: Some(vec![
                Mention {
                    number: Some("+1234567890".to_string()),
                    uuid: None,
                },
                Mention {
                    number: None,
                    uuid: Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string()),
                },
            ]),
            attachments: None,
        };

        assert_eq!(
            SignalChannel::extract_mentions(&data_msg),
            Some("[Mentioned: +1234567890, a1b2c3d4-e5f6-7890-abcd-ef1234567890]".to_string())
        );
    }

    #[test]
    fn extract_reply_context_uses_quoted_text() {
        let data_msg = DataMessage {
            message: Some("Thanks".to_string()),
            timestamp: Some(1_700_000_000_000),
            quote: Some(Quote {
                text: Some("Original\nmessage".to_string()),
                author: Some("+2222222222".to_string()),
                author_number: Some("+2222222222".to_string()),
                attachments: Vec::new(),
            }),
            group_info: None,
            mentions: None,
            attachments: None,
        };

        assert_eq!(
            SignalChannel::extract_reply_context(&data_msg),
            Some("[Replying to: \"Original message\" (from +2222222222)]".to_string())
        );
    }

    #[test]
    fn extract_reply_context_uses_attachment_preview() {
        let data_msg = DataMessage {
            message: Some("See this".to_string()),
            timestamp: Some(1_700_000_000_000),
            quote: Some(Quote {
                text: None,
                author: Some("legacy-author".to_string()),
                author_number: None,
                attachments: vec![QuotedAttachment {
                    content_type: Some("image/png".to_string()),
                    filename: Some("cat.png".to_string()),
                }],
            }),
            group_info: None,
            mentions: None,
            attachments: None,
        };

        assert_eq!(
            SignalChannel::extract_reply_context(&data_msg),
            Some("[Replying to: \"[Photo]\" (from legacy-author)]".to_string())
        );
    }

    #[test]
    fn extract_reply_context_uses_document_preview() {
        let data_msg = DataMessage {
            message: Some("See this".to_string()),
            timestamp: Some(1_700_000_000_000),
            quote: Some(Quote {
                text: None,
                author: Some("+3333333333".to_string()),
                author_number: Some("+3333333333".to_string()),
                attachments: vec![QuotedAttachment {
                    content_type: Some("application/pdf".to_string()),
                    filename: Some("report.pdf".to_string()),
                }],
            }),
            group_info: None,
            mentions: None,
            attachments: None,
        };

        assert_eq!(
            SignalChannel::extract_reply_context(&data_msg),
            Some("[Replying to: \"[Document: report.pdf]\" (from +3333333333)]".to_string())
        );
    }

    #[test]
    fn process_envelope_prepends_reply_context() {
        let ch = make_channel();
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("Thanks".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: Some(Quote {
                    text: Some("Earlier note".to_string()),
                    author: Some("+4444444444".to_string()),
                    author_number: Some("+4444444444".to_string()),
                    attachments: Vec::new(),
                }),
                group_info: None,
                mentions: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(
            msg.content,
            "[Replying to: \"Earlier note\" (from +4444444444)]\n\nThanks"
        );
    }

    #[test]
    fn process_envelope_keeps_reply_context_without_body() {
        let ch = make_channel();
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                quote: Some(Quote {
                    text: None,
                    author: Some("+5555555555".to_string()),
                    author_number: Some("+5555555555".to_string()),
                    attachments: vec![QuotedAttachment {
                        content_type: Some("image/jpeg".to_string()),
                        filename: None,
                    }],
                }),
                group_info: None,
                mentions: None,
                attachments: Some(vec![serde_json::json!({"contentType": "image/jpeg"})]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(msg.content, "[Replying to: \"[Photo]\" (from +5555555555)]");
    }

    #[test]
    fn process_envelope_group_prepends_mentions() {
        let ch = make_channel_with_group("group123");
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hello team".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: Some(GroupInfo {
                    group_id: Some("group123".to_string()),
                }),
                mentions: Some(vec![
                    Mention {
                        number: Some("+1234567890".to_string()),
                        uuid: None,
                    },
                    Mention {
                        number: None,
                        uuid: Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string()),
                    },
                ]),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch.process_envelope(&env).unwrap();
        assert_eq!(
            msg.content,
            "[Mentioned: +1234567890, a1b2c3d4-e5f6-7890-abcd-ef1234567890]\n\nhello team"
        );
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
                quote: None,
                group_info: None,
                mentions: None,
                attachments: Some(vec![serde_json::json!({"contentType": "image/png"})]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        assert!(ch.process_envelope(&env).is_none());
    }

    #[test]
    fn mention_only_group_without_mention_is_observed_only() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            true,
            false,
            false,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hello group".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: Some(GroupInfo {
                    group_id: Some("group123".to_string()),
                }),
                mentions: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch
            .process_envelope(&env)
            .expect("group msg should be observed");
        assert_eq!(msg.reply_target, "group:group123");
        assert!(msg.observe_group);
    }

    #[test]
    fn mention_only_group_with_matching_number_passes() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            true,
            false,
            false,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hello @bot".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: Some(GroupInfo {
                    group_id: Some("group123".to_string()),
                }),
                mentions: Some(vec![Mention {
                    number: Some("+1234567890".to_string()),
                    uuid: None,
                }]),
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch
            .process_envelope(&env)
            .expect("mentioned msg should pass");
        assert!(!msg.observe_group);
    }

    #[test]
    fn mention_only_direct_message_bypasses_gate() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            true,
            false,
            false,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("hello in dm".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: None,
                mentions: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch.process_envelope(&env).expect("dm should pass");
        assert!(!msg.observe_group);
    }

    #[test]
    fn mention_only_reply_to_bot_triggers_response() {
        let ch = SignalChannel::new(
            "http://127.0.0.1:8686".to_string(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            true,
            false,
            false,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("thanks for the info".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: Some(Quote {
                    text: Some("Here is the answer".to_string()),
                    author: None,
                    author_number: Some("+1234567890".to_string()),
                    attachments: Vec::new(),
                }),
                group_info: Some(GroupInfo {
                    group_id: Some("group123".to_string()),
                }),
                mentions: None,
                attachments: None,
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };
        let msg = ch
            .process_envelope(&env)
            .expect("reply to bot should trigger response");
        assert!(!msg.observe_group);
    }

    fn extract_attachment_path<'a>(marker: &'a str, prefix: &str) -> &'a str {
        marker
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(']'))
            .expect("attachment marker should have the expected format")
    }

    #[tokio::test]
    async fn process_envelope_with_attachments_downloads_and_exposes_image_path() {
        let server = MockServer::start().await;
        let payload = base64::engine::general_purpose::STANDARD.encode(b"image-bytes");

        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_string_contains("\"method\":\"getAttachment\""))
            .and(body_string_contains("\"account\":\"+1234567890\""))
            .and(body_string_contains("\"id\":\"photo-id\""))
            .and(body_string_contains("\"recipient\":\"+1111111111\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"jsonrpc":"2.0","id":"1","result":"{payload}"}}"#
            )))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            None,
            vec!["*".to_string()],
            false,
            false,
            true,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: None,
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: None,
                mentions: None,
                attachments: Some(vec![serde_json::json!({
                    "id": "photo-id",
                    "fileName": "photo.jpg",
                    "contentType": "image/jpeg"
                })]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch
            .process_envelope_with_attachments(&env)
            .await
            .expect("attachment-only dm should be exposed");
        let path = extract_attachment_path(&msg.content, "[Image file: ");

        assert!(Path::new(path).exists());
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"image-bytes");

        tokio::fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn process_envelope_with_attachments_uses_group_id_for_group_messages() {
        let server = MockServer::start().await;
        let payload = base64::engine::general_purpose::STANDARD.encode(b"report-bytes");

        Mock::given(method("POST"))
            .and(path("/api/v1/rpc"))
            .and(body_string_contains("\"method\":\"getAttachment\""))
            .and(body_string_contains("\"account\":\"+1234567890\""))
            .and(body_string_contains("\"id\":\"report-id\""))
            .and(body_string_contains("\"groupId\":\"group123\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"jsonrpc":"2.0","id":"1","result":"{payload}"}}"#
            )))
            .mount(&server)
            .await;

        let ch = SignalChannel::new(
            server.uri(),
            "+1234567890".to_string(),
            Some("group123".to_string()),
            vec!["*".to_string()],
            false,
            false,
            true,
            false,
        );
        let env = Envelope {
            source: Some("+1111111111".to_string()),
            source_number: Some("+1111111111".to_string()),
            data_message: Some(DataMessage {
                message: Some("report".to_string()),
                timestamp: Some(1_700_000_000_000),
                quote: None,
                group_info: Some(GroupInfo {
                    group_id: Some("group123".to_string()),
                }),
                mentions: None,
                attachments: Some(vec![serde_json::json!({
                    "id": "report-id",
                    "fileName": "report.pdf",
                    "contentType": "application/pdf"
                })]),
            }),
            story_message: None,
            timestamp: Some(1_700_000_000_000),
        };

        let msg = ch
            .process_envelope_with_attachments(&env)
            .await
            .expect("group attachment should be exposed");
        let marker = msg
            .content
            .lines()
            .last()
            .expect("attachment marker should be appended");
        let path = extract_attachment_path(marker, "[Document file: ");

        assert_eq!(msg.content.lines().next(), Some("report"));
        assert!(Path::new(path).exists());
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"report-bytes");

        tokio::fs::remove_file(path).await.unwrap();
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
                    "timestamp": 1700000000000,
                    "quote": {
                        "id": 1699999999999,
                        "author": "+2222222222",
                        "authorNumber": "+2222222222",
                        "text": "Earlier message",
                        "attachments": []
                    }
                }
            }
        }"#;
        let sse: SseEnvelope = serde_json::from_str(json).unwrap();
        let env = sse.envelope.unwrap();
        assert_eq!(env.source_number.as_deref(), Some("+1111111111"));
        let dm = env.data_message.unwrap();
        assert_eq!(dm.message.as_deref(), Some("Hello Signal!"));
        assert_eq!(
            dm.quote.as_ref().and_then(|quote| quote.text.as_deref()),
            Some("Earlier message")
        );
        assert_eq!(
            dm.quote
                .as_ref()
                .and_then(|quote| quote.author_number.as_deref()),
            Some("+2222222222")
        );
    }

    #[test]
    fn sse_envelope_deserializes_group() {
        let json = r#"{
            "envelope": {
                "sourceNumber": "+2222222222",
                "dataMessage": {
                    "message": "Group msg",
                    "quote": {
                        "id": 1699999999999,
                        "author": "+3333333333",
                        "authorNumber": "+3333333333",
                        "attachments": [
                            {
                                "contentType": "image/jpeg",
                                "filename": "photo.jpg"
                            }
                        ]
                    },
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
        assert_eq!(
            SignalChannel::extract_reply_context(&dm),
            Some("[Replying to: \"[Photo]\" (from +3333333333)]".to_string())
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
}
