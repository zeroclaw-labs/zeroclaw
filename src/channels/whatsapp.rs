use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

/// `WhatsApp` channel — uses `WhatsApp` Business Cloud API
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/whatsapp` webhook endpoint.
/// The `listen` method here is a no-op placeholder; actual message handling
/// happens in the gateway when Meta sends webhook events.
fn ensure_https(url: &str) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!(
            "Refusing to transmit sensitive data over non-HTTPS URL: URL scheme must be https"
        );
    }
    Ok(())
}

///
/// # Runtime Negotiation
///
/// This Cloud API channel is automatically selected when `phone_number_id` is set in the config.
/// Use `WhatsAppWebChannel` (with `session_path`) for native Web mode.
/// Maximum file size for inbound WhatsApp Cloud API media downloads (20 MiB).
const WA_CLOUD_MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

/// Derive a file extension from a MIME type for incoming Cloud API media.
fn wa_cloud_mime_to_ext(mime: &str) -> &str {
    let base = mime.split(';').next().unwrap_or("").trim();
    match base.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/ogg" | "audio/oga" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/aac" => "m4a",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

/// Check whether a file path has a recognized image extension.
fn wa_cloud_is_image_ext(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
        .unwrap_or(false)
}

pub struct WhatsAppChannel {
    access_token: String,
    endpoint_id: String,
    verify_token: String,
    allowed_numbers: Vec<String>,
    /// Workspace directory for saving downloaded media attachments.
    workspace_dir: Option<std::path::PathBuf>,
}

impl WhatsAppChannel {
    pub fn new(
        access_token: String,
        endpoint_id: String,
        verify_token: String,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            access_token,
            endpoint_id,
            verify_token,
            allowed_numbers,
            workspace_dir: None,
        }
    }

    /// Configure workspace directory for saving downloaded media attachments.
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.whatsapp")
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890)
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
    }

    /// Get the verify token for webhook verification
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Download media from WhatsApp Cloud API using a 2-step process:
    /// 1. GET /v18.0/{media_id} → JSON with `url` field
    /// 2. GET that URL with bearer token → binary data
    async fn download_media_by_id(&self, media_id: &str) -> anyhow::Result<(Vec<u8>, Option<String>)> {
        let meta_url = format!("https://graph.facebook.com/v18.0/{media_id}");
        ensure_https(&meta_url)?;

        let meta_resp = self
            .http_client()
            .get(&meta_url)
            .bearer_auth(&self.access_token)
            .send()
            .await?;

        if !meta_resp.status().is_success() {
            let status = meta_resp.status();
            anyhow::bail!("WhatsApp media metadata request failed: {status}");
        }

        let meta_json: serde_json::Value = meta_resp.json().await?;
        let download_url = meta_json
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("WhatsApp media response missing 'url' field"))?;
        let mime_type = meta_json
            .get("mime_type")
            .and_then(|m| m.as_str())
            .map(String::from);
        let file_size = meta_json
            .get("file_size")
            .and_then(|s| s.as_u64());

        if let Some(size) = file_size {
            if size > WA_CLOUD_MAX_MEDIA_BYTES {
                anyhow::bail!(
                    "WhatsApp media too large ({size} bytes > {WA_CLOUD_MAX_MEDIA_BYTES})"
                );
            }
        }

        ensure_https(download_url)?;

        let data_resp = self
            .http_client()
            .get(download_url)
            .bearer_auth(&self.access_token)
            .send()
            .await?;

        if !data_resp.status().is_success() {
            let status = data_resp.status();
            anyhow::bail!("WhatsApp media download failed: {status}");
        }

        let bytes = data_resp.bytes().await?.to_vec();
        Ok((bytes, mime_type))
    }

    /// Try to download and save a media attachment from a webhook message.
    /// Returns the formatted content string with `[IMAGE:]` or `[Document:]` markers,
    /// or `None` if the message has no media or download fails.
    async fn try_process_media_message(
        &self,
        msg: &serde_json::Value,
        from: &str,
    ) -> Option<String> {
        let workspace = self.workspace_dir.as_ref()?;
        let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");

        let (media_obj_key, default_ext, is_image_type) = match msg_type {
            "image" => ("image", "jpg", true),
            "document" => ("document", "bin", false),
            "video" => ("video", "mp4", false),
            "sticker" => ("sticker", "webp", true),
            _ => return None,
        };

        let media_obj = msg.get(media_obj_key)?;
        let media_id = media_obj.get("id").and_then(|i| i.as_str())?;
        let caption = media_obj
            .get("caption")
            .and_then(|c| c.as_str())
            .map(String::from);
        let file_name = media_obj
            .get("filename")
            .and_then(|f| f.as_str())
            .map(String::from);
        let mime_from_payload = media_obj
            .get("mime_type")
            .and_then(|m| m.as_str())
            .map(String::from);

        let (bytes, mime_from_download) = match self.download_media_by_id(media_id).await {
            Ok(result) => result,
            Err(e) => {
                tracing::warn!("WhatsApp: failed to download {msg_type} media {media_id}: {e}");
                return None;
            }
        };

        let mime = mime_from_payload
            .as_deref()
            .or(mime_from_download.as_deref());
        let ext = if let Some(ref name) = file_name {
            std::path::Path::new(name)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_else(|| mime.map(wa_cloud_mime_to_ext).unwrap_or(default_ext))
        } else {
            mime.map(wa_cloud_mime_to_ext).unwrap_or(default_ext)
        };

        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let save_filename = format!("wa_{msg_type}_{from}_{ts}.{ext}");
        let save_dir = workspace.join("whatsapp_files");

        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp: failed to create save dir: {e}");
            return None;
        }
        let path = save_dir.join(&save_filename);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!("WhatsApp: failed to save {msg_type}: {e}");
            return None;
        }

        tracing::info!(
            "WhatsApp: saved inbound {msg_type} to {} ({} bytes)",
            path.display(),
            bytes.len()
        );

        let display_name = file_name.as_deref().unwrap_or(&save_filename);
        let mut content = if is_image_type || wa_cloud_is_image_ext(&path) {
            format!("[IMAGE:{}]", path.display())
        } else {
            format!("[Document: {}] {}", display_name, path.display())
        };

        if let Some(ref cap) = caption {
            if !cap.trim().is_empty() {
                content = format!("{}\n\n{content}", cap.trim());
            }
        }

        Some(content)
    }

    /// Parse an incoming webhook payload from Meta and extract messages.
    ///
    /// This method is async because media messages (image, document, video,
    /// sticker) require downloading from the WhatsApp Cloud API.
    pub async fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // WhatsApp Cloud API webhook structure:
        // { "object": "whatsapp_business_account", "entry": [...] }
        let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) else {
            return messages;
        };

        for entry in entries {
            let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) else {
                continue;
            };

            for change in changes {
                let Some(value) = change.get("value") else {
                    continue;
                };

                // Extract messages array
                let Some(msgs) = value.get("messages").and_then(|m| m.as_array()) else {
                    continue;
                };

                for msg in msgs {
                    // Get sender phone number
                    let Some(from) = msg.get("from").and_then(|f| f.as_str()) else {
                        continue;
                    };

                    // Check allowlist
                    let normalized_from = if from.starts_with('+') {
                        from.to_string()
                    } else {
                        format!("+{from}")
                    };

                    if !self.is_number_allowed(&normalized_from) {
                        tracing::warn!(
                            "WhatsApp: ignoring message from unauthorized number: {normalized_from}. \
                            Add to channels.whatsapp.allowed_numbers in config.toml, \
                            or run `zeroclaw onboard --channels-only` to configure interactively."
                        );
                        continue;
                    }

                    // Extract content: text messages directly, media messages
                    // via download + [IMAGE:]/[Document:] markers.
                    let content = if let Some(text_obj) = msg.get("text") {
                        text_obj
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else if let Some(media_content) =
                        self.try_process_media_message(msg, from).await
                    {
                        media_content
                    } else {
                        let msg_type = msg
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("unknown");
                        tracing::debug!(
                            "WhatsApp: skipping unsupported message type '{msg_type}' from {from}"
                        );
                        continue;
                    };

                    if content.is_empty() {
                        continue;
                    }

                    // Get timestamp
                    let timestamp = msg
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|t| t.parse::<u64>().ok())
                        .unwrap_or_else(|| {
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                        });

                    messages.push(ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        reply_target: normalized_from.clone(),
                        sender: normalized_from,
                        content,
                        channel: "whatsapp".to_string(),
                        timestamp,
                        thread_ts: None,
                    });
                }
            }
        }

        messages
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // WhatsApp Cloud API: POST to /v18.0/{phone_number_id}/messages
        let url = format!(
            "https://graph.facebook.com/v18.0/{}/messages",
            self.endpoint_id
        );

        // Normalize recipient (remove leading + if present for API)
        let to = message
            .recipient
            .strip_prefix('+')
            .unwrap_or(&message.recipient);

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": to,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": message.content
            }
        });

        ensure_https(&url)?;

        let resp = self
            .http_client()
            .post(&url)
            .bearer_auth(&self.access_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let error_body = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&error_body);
            tracing::error!("WhatsApp send failed: {status} — {sanitized}");
            anyhow::bail!("WhatsApp API error: {status}");
        }

        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // WhatsApp uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /whatsapp endpoint.
        // This method keeps the channel "alive" but doesn't actively poll.
        tracing::info!(
            "WhatsApp channel active (webhook mode). \
            Configure Meta webhook to POST to your gateway's /whatsapp endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Check if we can reach the WhatsApp API
        let url = format!("https://graph.facebook.com/v18.0/{}", self.endpoint_id);

        if ensure_https(&url).is_err() {
            return false;
        }

        self.http_client()
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> WhatsAppChannel {
        WhatsAppChannel::new(
            "test-token".into(),
            "123456789".into(),
            "verify-me".into(),
            vec!["+1234567890".into()],
        )
    }

    #[test]
    fn whatsapp_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "whatsapp");
    }

    #[test]
    fn whatsapp_verify_token() {
        let ch = make_channel();
        assert_eq!(ch.verify_token(), "verify-me");
    }

    #[test]
    fn whatsapp_number_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    fn whatsapp_number_allowed_wildcard() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    fn whatsapp_number_denied_empty() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec![]);
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[tokio::test]
    async fn whatsapp_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_valid_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "id": "123",
                "changes": [{
                    "value": {
                        "messaging_product": "whatsapp",
                        "metadata": {
                            "display_phone_number": "15551234567",
                            "phone_number_id": "123456789"
                        },
                        "messages": [{
                            "from": "1234567890",
                            "id": "wamid.xxx",
                            "timestamp": "1699999999",
                            "type": "text",
                            "text": {
                                "body": "Hello ZeroClaw!"
                            }
                        }]
                    },
                    "field": "messages"
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].channel, "whatsapp");
        assert_eq!(msgs[0].timestamp, 1_699_999_999);
    }

    #[tokio::test]
    async fn whatsapp_parse_unauthorized_number() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "object": "whatsapp_business_account",
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "9999999999",
                            "timestamp": "1699999999",
                            "type": "text",
                            "text": { "body": "Spam" }
                        }]
                    }
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Unauthorized numbers should be filtered");
    }

    #[tokio::test]
    async fn whatsapp_parse_non_text_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "1234567890",
                            "timestamp": "1699999999",
                            "type": "image",
                            "image": { "id": "img123" }
                        }]
                    }
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Non-text messages should be skipped");
    }

    #[tokio::test]
    async fn whatsapp_parse_multiple_messages() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [
                            { "from": "111", "timestamp": "1", "type": "text", "text": { "body": "First" } },
                            { "from": "222", "timestamp": "2", "type": "text", "text": { "body": "Second" } }
                        ]
                    }
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "First");
        assert_eq!(msgs[1].content, "Second");
    }

    #[tokio::test]
    async fn whatsapp_parse_normalizes_phone_with_plus() {
        let ch = WhatsAppChannel::new(
            "tok".into(),
            "123".into(),
            "ver".into(),
            vec!["+1234567890".into()],
        );
        // API sends without +, but we normalize to +
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "1234567890",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "Hi" }
                        }]
                    }
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[tokio::test]
    async fn whatsapp_empty_text_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "" }
                        }]
                    }
                }]
            }]
        });

        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    // ══════════════════════════════════════════════════════════
    // EDGE CASES — Comprehensive coverage
    // ══════════════════════════════════════════════════════════

    #[tokio::test]
    async fn whatsapp_parse_missing_entry_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "object": "whatsapp_business_account"
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_entry_not_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": "not_an_array"
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_changes_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{ "id": "123" }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_changes_not_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": "not_an_array"
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_value() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{ "field": "messages" }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_messages_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "metadata": {}
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_messages_not_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": "not_an_array"
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_from_field() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "No sender" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Messages without 'from' should be skipped");
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_text_body() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": {}
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(
            msgs.is_empty(),
            "Messages with empty text object should be skipped"
        );
    }

    #[tokio::test]
    async fn whatsapp_parse_null_text_body() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": null }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Messages with null body should be skipped");
    }

    #[tokio::test]
    async fn whatsapp_parse_invalid_timestamp_uses_current() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "not_a_number",
                            "type": "text",
                            "text": { "body": "Hello" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        // Timestamp should be current time (non-zero)
        assert!(msgs[0].timestamp > 0);
    }

    #[tokio::test]
    async fn whatsapp_parse_missing_timestamp_uses_current() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "type": "text",
                            "text": { "body": "Hello" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].timestamp > 0);
    }

    #[tokio::test]
    async fn whatsapp_parse_multiple_entries() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [
                {
                    "changes": [{
                        "value": {
                            "messages": [{
                                "from": "111",
                                "timestamp": "1",
                                "type": "text",
                                "text": { "body": "Entry 1" }
                            }]
                        }
                    }]
                },
                {
                    "changes": [{
                        "value": {
                            "messages": [{
                                "from": "222",
                                "timestamp": "2",
                                "type": "text",
                                "text": { "body": "Entry 2" }
                            }]
                        }
                    }]
                }
            ]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Entry 1");
        assert_eq!(msgs[1].content, "Entry 2");
    }

    #[tokio::test]
    async fn whatsapp_parse_multiple_changes() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [
                    {
                        "value": {
                            "messages": [{
                                "from": "111",
                                "timestamp": "1",
                                "type": "text",
                                "text": { "body": "Change 1" }
                            }]
                        }
                    },
                    {
                        "value": {
                            "messages": [{
                                "from": "222",
                                "timestamp": "2",
                                "type": "text",
                                "text": { "body": "Change 2" }
                            }]
                        }
                    }
                ]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Change 1");
        assert_eq!(msgs[1].content, "Change 2");
    }

    #[tokio::test]
    async fn whatsapp_parse_status_update_ignored() {
        // Status updates have "statuses" instead of "messages"
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "statuses": [{
                            "id": "wamid.xxx",
                            "status": "delivered",
                            "timestamp": "1699999999"
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Status updates should be ignored");
    }

    #[tokio::test]
    async fn whatsapp_parse_audio_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "audio",
                            "audio": { "id": "audio123", "mime_type": "audio/ogg" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_video_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "video",
                            "video": { "id": "video123" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_document_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "document",
                            "document": { "id": "doc123", "filename": "file.pdf" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_sticker_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "sticker",
                            "sticker": { "id": "sticker123" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_location_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "location",
                            "location": { "latitude": 40.7128, "longitude": -74.0060 }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_contacts_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "contacts",
                            "contacts": [{ "name": { "formatted_name": "John" } }]
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_reaction_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "reaction",
                            "reaction": { "message_id": "wamid.xxx", "emoji": "👍" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_mixed_authorized_unauthorized() {
        let ch = WhatsAppChannel::new(
            "tok".into(),
            "123".into(),
            "ver".into(),
            vec!["+1111111111".into()],
        );
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [
                            { "from": "1111111111", "timestamp": "1", "type": "text", "text": { "body": "Allowed" } },
                            { "from": "9999999999", "timestamp": "2", "type": "text", "text": { "body": "Blocked" } },
                            { "from": "1111111111", "timestamp": "3", "type": "text", "text": { "body": "Also allowed" } }
                        ]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Allowed");
        assert_eq!(msgs[1].content, "Also allowed");
    }

    #[tokio::test]
    async fn whatsapp_parse_unicode_message() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "Hello 👋 世界 🌍 مرحبا" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello 👋 世界 🌍 مرحبا");
    }

    #[tokio::test]
    async fn whatsapp_parse_very_long_message() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let long_text = "A".repeat(10_000);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": long_text }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.len(), 10_000);
    }

    #[tokio::test]
    async fn whatsapp_parse_whitespace_only_message_skipped() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "   " }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        // Whitespace-only is NOT empty, so it passes through
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "   ");
    }

    #[test]
    fn whatsapp_number_allowed_multiple_numbers() {
        let ch = WhatsAppChannel::new(
            "tok".into(),
            "123".into(),
            "ver".into(),
            vec![
                "+1111111111".into(),
                "+2222222222".into(),
                "+3333333333".into(),
            ],
        );
        assert!(ch.is_number_allowed("+1111111111"));
        assert!(ch.is_number_allowed("+2222222222"));
        assert!(ch.is_number_allowed("+3333333333"));
        assert!(!ch.is_number_allowed("+4444444444"));
    }

    #[test]
    fn whatsapp_number_allowed_case_sensitive() {
        // Phone numbers should be exact match
        let ch = WhatsAppChannel::new(
            "tok".into(),
            "123".into(),
            "ver".into(),
            vec!["+1234567890".into()],
        );
        assert!(ch.is_number_allowed("+1234567890"));
        // Different number should not match
        assert!(!ch.is_number_allowed("+1234567891"));
    }

    #[tokio::test]
    async fn whatsapp_parse_phone_already_has_plus() {
        let ch = WhatsAppChannel::new(
            "tok".into(),
            "123".into(),
            "ver".into(),
            vec!["+1234567890".into()],
        );
        // If API sends with +, we should still handle it
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "+1234567890",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "Hi" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn whatsapp_channel_fields_stored_correctly() {
        let ch = WhatsAppChannel::new(
            "my-access-token".into(),
            "phone-id-123".into(),
            "my-verify-token".into(),
            vec!["+111".into(), "+222".into()],
        );
        assert_eq!(ch.verify_token(), "my-verify-token");
        assert!(ch.is_number_allowed("+111"));
        assert!(ch.is_number_allowed("+222"));
        assert!(!ch.is_number_allowed("+333"));
    }

    #[tokio::test]
    async fn whatsapp_parse_empty_messages_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": []
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_empty_entry_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": []
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_empty_changes_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": []
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn whatsapp_parse_newlines_preserved() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "Line 1\nLine 2\nLine 3" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Line 1\nLine 2\nLine 3");
    }

    #[tokio::test]
    async fn whatsapp_parse_special_characters() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "text",
                            "text": { "body": "<script>alert('xss')</script> & \"quotes\" 'apostrophe'" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content,
            "<script>alert('xss')</script> & \"quotes\" 'apostrophe'"
        );
    }

    // ══════════════════════════════════════════════════════════
    // INBOUND MEDIA — Helper function tests
    // ══════════════════════════════════════════════════════════

    #[test]
    fn wa_cloud_mime_to_ext_known_types() {
        assert_eq!(wa_cloud_mime_to_ext("image/jpeg"), "jpg");
        assert_eq!(wa_cloud_mime_to_ext("image/png"), "png");
        assert_eq!(wa_cloud_mime_to_ext("image/webp"), "webp");
        assert_eq!(wa_cloud_mime_to_ext("image/gif"), "gif");
        assert_eq!(wa_cloud_mime_to_ext("video/mp4"), "mp4");
        assert_eq!(wa_cloud_mime_to_ext("video/3gpp"), "3gp");
        assert_eq!(wa_cloud_mime_to_ext("audio/ogg"), "ogg");
        assert_eq!(wa_cloud_mime_to_ext("audio/mpeg"), "mp3");
        assert_eq!(wa_cloud_mime_to_ext("audio/mp4"), "m4a");
        assert_eq!(wa_cloud_mime_to_ext("application/pdf"), "pdf");
    }

    #[test]
    fn wa_cloud_mime_to_ext_unknown_fallback() {
        assert_eq!(wa_cloud_mime_to_ext("application/octet-stream"), "bin");
        assert_eq!(wa_cloud_mime_to_ext("text/plain"), "bin");
        assert_eq!(wa_cloud_mime_to_ext(""), "bin");
    }

    #[test]
    fn wa_cloud_mime_to_ext_with_params() {
        assert_eq!(wa_cloud_mime_to_ext("image/jpeg; codecs=foo"), "jpg");
        assert_eq!(wa_cloud_mime_to_ext("audio/ogg; codecs=opus"), "ogg");
    }

    #[test]
    fn wa_cloud_is_image_ext_recognizes_images() {
        assert!(wa_cloud_is_image_ext(std::path::Path::new("photo.png")));
        assert!(wa_cloud_is_image_ext(std::path::Path::new("photo.jpg")));
        assert!(wa_cloud_is_image_ext(std::path::Path::new("photo.jpeg")));
        assert!(wa_cloud_is_image_ext(std::path::Path::new("photo.gif")));
        assert!(wa_cloud_is_image_ext(std::path::Path::new("photo.webp")));
        assert!(wa_cloud_is_image_ext(std::path::Path::new("PHOTO.PNG")));
    }

    #[test]
    fn wa_cloud_is_image_ext_rejects_non_images() {
        assert!(!wa_cloud_is_image_ext(std::path::Path::new("file.pdf")));
        assert!(!wa_cloud_is_image_ext(std::path::Path::new("file.txt")));
        assert!(!wa_cloud_is_image_ext(std::path::Path::new("file.mp4")));
        assert!(!wa_cloud_is_image_ext(std::path::Path::new("file")));
    }

    #[tokio::test]
    async fn whatsapp_media_skipped_without_workspace_dir() {
        // Without workspace_dir, media messages should still be skipped
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [{
                            "from": "111",
                            "timestamp": "1",
                            "type": "image",
                            "image": { "id": "img123", "mime_type": "image/jpeg" }
                        }]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload).await;
        assert!(msgs.is_empty(), "Media without workspace_dir should be skipped");
    }
}
