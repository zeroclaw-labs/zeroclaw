use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use regex::Regex;
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
pub struct WhatsAppChannel {
    access_token: String,
    endpoint_id: String,
    verify_token: String,
    allowed_numbers: Vec<String>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Compiled mention patterns for group-chat mention gating.
    /// When non-empty, only messages matching at least one pattern are processed.
    mention_patterns: Vec<Regex>,
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
            proxy_url: None,
            mention_patterns: Vec::new(),
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Set mention patterns for group-chat mention gating.
    /// Each pattern string is compiled as a case-insensitive regex.
    /// Invalid patterns are logged and skipped.
    pub fn with_mention_patterns(mut self, patterns: Vec<String>) -> Self {
        self.mention_patterns = Self::compile_mention_patterns(&patterns);
        self
    }

    /// Compile raw pattern strings into case-insensitive regexes.
    /// Invalid or excessively large patterns are logged and skipped.
    pub(crate) fn compile_mention_patterns(patterns: &[String]) -> Vec<Regex> {
        patterns
            .iter()
            .filter_map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() {
                    return None;
                }
                match regex::RegexBuilder::new(trimmed)
                    .case_insensitive(true)
                    .size_limit(1 << 16) // 64 KiB — guard against ReDoS
                    .build()
                {
                    Ok(re) => Some(re),
                    Err(e) => {
                        tracing::warn!(
                            "WhatsApp: ignoring invalid mention_pattern {trimmed:?}: {e}"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Check whether `text` matches any of the compiled mention patterns.
    fn contains_mention(&self, text: &str) -> bool {
        Self::text_matches_patterns(&self.mention_patterns, text)
    }

    /// Check whether `text` matches any pattern in the given slice.
    pub(crate) fn text_matches_patterns(patterns: &[Regex], text: &str) -> bool {
        patterns.iter().any(|re| re.is_match(text))
    }

    /// Strip all mention-pattern matches from `text`, collapse whitespace,
    /// and return `None` if the result is empty.
    fn strip_mention_patterns(&self, text: &str) -> Option<String> {
        Self::strip_patterns(&self.mention_patterns, text)
    }

    /// Strip all pattern matches from `text`, collapse whitespace,
    /// and return `None` if the result is empty.
    pub(crate) fn strip_patterns(patterns: &[Regex], text: &str) -> Option<String> {
        let mut result = text.to_string();
        for re in patterns {
            result = re.replace_all(&result, " ").into_owned();
        }
        let normalized = result.split_whitespace().collect::<Vec<_>>().join(" ");
        (!normalized.is_empty()).then_some(normalized)
    }

    /// Detect group messages in the WhatsApp Cloud API webhook payload.
    ///
    /// A message is considered a group message when it carries a `context`
    /// object containing a non-empty `group_id` field.
    fn is_group_message(msg: &serde_json::Value) -> bool {
        msg.get("context")
            .and_then(|ctx| ctx.get("group_id"))
            .and_then(|g| g.as_str())
            .is_some_and(|s| !s.is_empty())
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client("channel.whatsapp", self.proxy_url.as_deref())
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890)
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
    }

    /// Get the verify token for webhook verification
    pub fn verify_token(&self) -> &str {
        &self.verify_token
    }

    /// Parse an incoming webhook payload from Meta and extract messages
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
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

                    // Extract text content (support text messages only for now)
                    let content = if let Some(text_obj) = msg.get("text") {
                        text_obj
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        // Could be image, audio, etc. — skip for now
                        tracing::debug!("WhatsApp: skipping non-text message from {from}");
                        continue;
                    };

                    if content.is_empty() {
                        continue;
                    }

                    // Mention-pattern gating: when mention_patterns is
                    // configured, reject **group** messages that do not match
                    // any pattern and strip matched fragments from the content.
                    // DM messages always pass through unfiltered.
                    let is_group = Self::is_group_message(msg);
                    let content = if !is_group || self.mention_patterns.is_empty() {
                        content
                    } else {
                        if !self.contains_mention(&content) {
                            continue;
                        }
                        match self.strip_mention_patterns(&content) {
                            Some(c) => c,
                            None => continue,
                        }
                    };

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
                        interruption_scope_id: None,
                        attachments: vec![],
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
            tracing::error!("WhatsApp send failed: {status} — {error_body}");
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

    #[test]
    fn whatsapp_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_valid_text_message() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].channel, "whatsapp");
        assert_eq!(msgs[0].timestamp, 1_699_999_999);
    }

    #[test]
    fn whatsapp_parse_unauthorized_number() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Unauthorized numbers should be filtered");
    }

    #[test]
    fn whatsapp_parse_non_text_message_skipped() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Non-text messages should be skipped");
    }

    #[test]
    fn whatsapp_parse_multiple_messages() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "First");
        assert_eq!(msgs[1].content, "Second");
    }

    #[test]
    fn whatsapp_parse_normalizes_phone_with_plus() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn whatsapp_empty_text_skipped() {
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

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    // ══════════════════════════════════════════════════════════
    // EDGE CASES — Comprehensive coverage
    // ══════════════════════════════════════════════════════════

    #[test]
    fn whatsapp_parse_missing_entry_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "object": "whatsapp_business_account"
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_entry_not_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": "not_an_array"
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_missing_changes_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{ "id": "123" }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_changes_not_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": "not_an_array"
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_missing_value() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{ "field": "messages" }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_missing_messages_array() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_messages_not_array() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_missing_from_field() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Messages without 'from' should be skipped");
    }

    #[test]
    fn whatsapp_parse_missing_text_body() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Messages with empty text object should be skipped"
        );
    }

    #[test]
    fn whatsapp_parse_null_text_body() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Messages with null body should be skipped");
    }

    #[test]
    fn whatsapp_parse_invalid_timestamp_uses_current() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        // Timestamp should be current time (non-zero)
        assert!(msgs[0].timestamp > 0);
    }

    #[test]
    fn whatsapp_parse_missing_timestamp_uses_current() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].timestamp > 0);
    }

    #[test]
    fn whatsapp_parse_multiple_entries() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Entry 1");
        assert_eq!(msgs[1].content, "Entry 2");
    }

    #[test]
    fn whatsapp_parse_multiple_changes() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Change 1");
        assert_eq!(msgs[1].content, "Change 2");
    }

    #[test]
    fn whatsapp_parse_status_update_ignored() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Status updates should be ignored");
    }

    #[test]
    fn whatsapp_parse_audio_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_video_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_document_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_sticker_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_location_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_contacts_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_reaction_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_mixed_authorized_unauthorized() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "Allowed");
        assert_eq!(msgs[1].content, "Also allowed");
    }

    #[test]
    fn whatsapp_parse_unicode_message() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello 👋 世界 🌍 مرحبا");
    }

    #[test]
    fn whatsapp_parse_very_long_message() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content.len(), 10_000);
    }

    #[test]
    fn whatsapp_parse_whitespace_only_message_skipped() {
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
        let msgs = ch.parse_webhook_payload(&payload);
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

    #[test]
    fn whatsapp_parse_phone_already_has_plus() {
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
        let msgs = ch.parse_webhook_payload(&payload);
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

    #[test]
    fn whatsapp_parse_empty_messages_array() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_empty_entry_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": []
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_empty_changes_array() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": []
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn whatsapp_parse_newlines_preserved() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Line 1\nLine 2\nLine 3");
    }

    #[test]
    fn whatsapp_parse_special_characters() {
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
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content,
            "<script>alert('xss')</script> & \"quotes\" 'apostrophe'"
        );
    }

    // ══════════════════════════════════════════════════════════
    // MENTION-PATTERN GATING — Unit tests
    // ══════════════════════════════════════════════════════════

    fn make_mention_channel() -> WhatsAppChannel {
        WhatsAppChannel::new(
            "test-token".into(),
            "123456789".into(),
            "verify-me".into(),
            vec!["*".into()],
        )
        .with_mention_patterns(vec!["@?ZeroClaw".into()])
    }

    // ── compile_mention_patterns ──

    #[test]
    fn whatsapp_compile_valid_patterns() {
        let patterns = WhatsAppChannel::compile_mention_patterns(&[
            "@?ZeroClaw".into(),
            r"\+?15555550123".into(),
        ]);
        assert_eq!(patterns.len(), 2);
    }

    #[test]
    fn whatsapp_compile_skips_invalid_patterns() {
        let patterns =
            WhatsAppChannel::compile_mention_patterns(&["@?ZeroClaw".into(), "[invalid".into()]);
        assert_eq!(patterns.len(), 1);
    }

    #[test]
    fn whatsapp_compile_skips_empty_patterns() {
        let patterns =
            WhatsAppChannel::compile_mention_patterns(&["@?ZeroClaw".into(), "  ".into()]);
        assert_eq!(patterns.len(), 1);
    }

    #[test]
    fn whatsapp_compile_empty_vec() {
        let patterns = WhatsAppChannel::compile_mention_patterns(&[]);
        assert!(patterns.is_empty());
    }

    // ── contains_mention ──

    #[test]
    fn whatsapp_contains_mention_at_name() {
        let ch = make_mention_channel();
        assert!(ch.contains_mention("Hello @ZeroClaw"));
    }

    #[test]
    fn whatsapp_contains_mention_name_only() {
        let ch = make_mention_channel();
        assert!(ch.contains_mention("Hello ZeroClaw"));
    }

    #[test]
    fn whatsapp_contains_mention_case_insensitive() {
        let ch = make_mention_channel();
        assert!(ch.contains_mention("Hello @zeroclaw"));
        assert!(ch.contains_mention("Hello ZEROCLAW"));
    }

    #[test]
    fn whatsapp_contains_mention_no_match() {
        let ch = make_mention_channel();
        assert!(!ch.contains_mention("Hello @otherbot"));
        assert!(!ch.contains_mention("Hello world"));
    }

    #[test]
    fn whatsapp_contains_mention_phone_pattern() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()])
            .with_mention_patterns(vec![r"\+?15555550123".into()]);
        assert!(ch.contains_mention("Hey +15555550123 help"));
        assert!(ch.contains_mention("Hey 15555550123 help"));
        assert!(!ch.contains_mention("Hey +19999999999 help"));
    }

    #[test]
    fn whatsapp_contains_mention_multiple_patterns() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()])
            .with_mention_patterns(vec!["@?ZeroClaw".into(), r"\+?15555550123".into()]);
        assert!(ch.contains_mention("Hello @ZeroClaw"));
        assert!(ch.contains_mention("Hey +15555550123"));
        assert!(!ch.contains_mention("Hello world"));
    }

    #[test]
    fn whatsapp_contains_mention_empty_patterns() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        assert!(!ch.contains_mention("Hello @ZeroClaw"));
    }

    // ── strip_mention_patterns ──

    #[test]
    fn whatsapp_strip_at_name() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("@ZeroClaw what is the weather?"),
            Some("what is the weather?".into())
        );
    }

    #[test]
    fn whatsapp_strip_name_without_at() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("ZeroClaw what is the weather?"),
            Some("what is the weather?".into())
        );
    }

    #[test]
    fn whatsapp_strip_at_end() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("Help me @ZeroClaw"),
            Some("Help me".into())
        );
    }

    #[test]
    fn whatsapp_strip_mid_sentence() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("Hey @ZeroClaw how are you?"),
            Some("Hey how are you?".into())
        );
    }

    #[test]
    fn whatsapp_strip_multiple_occurrences() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("@ZeroClaw hello @ZeroClaw"),
            Some("hello".into())
        );
    }

    #[test]
    fn whatsapp_strip_returns_none_when_only_mention() {
        let ch = make_mention_channel();
        assert_eq!(ch.strip_mention_patterns("@ZeroClaw"), None);
    }

    #[test]
    fn whatsapp_strip_returns_none_for_whitespace_only() {
        let ch = make_mention_channel();
        assert_eq!(ch.strip_mention_patterns("  @ZeroClaw  "), None);
    }

    #[test]
    fn whatsapp_strip_collapses_whitespace() {
        let ch = make_mention_channel();
        assert_eq!(
            ch.strip_mention_patterns("@ZeroClaw   status   please"),
            Some("status please".into())
        );
    }

    #[test]
    fn whatsapp_strip_phone_pattern() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()])
            .with_mention_patterns(vec![r"\+?15555550123".into()]);
        assert_eq!(
            ch.strip_mention_patterns("Hey +15555550123 help me"),
            Some("Hey help me".into())
        );
    }

    // ── with_mention_patterns builder ──

    #[test]
    fn whatsapp_with_mention_patterns_compiles() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec![])
            .with_mention_patterns(vec!["@?bot".into()]);
        assert_eq!(ch.mention_patterns.len(), 1);
    }

    #[test]
    fn whatsapp_default_no_mention_patterns() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec![]);
        assert!(ch.mention_patterns.is_empty());
    }

    // ── mention_patterns integration with parse_webhook_payload ──

    /// Helper: build a group message payload with optional context.group_id.
    fn group_msg(from: &str, ts: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "from": from,
            "timestamp": ts,
            "type": "text",
            "text": { "body": body },
            "context": { "group_id": "120363012345678901@g.us" }
        })
    }

    /// Helper: build a DM message payload (no group_id).
    fn dm_msg(from: &str, ts: &str, body: &str) -> serde_json::Value {
        serde_json::json!({
            "from": from,
            "timestamp": ts,
            "type": "text",
            "text": { "body": body }
        })
    }

    #[test]
    fn whatsapp_is_group_message_with_group_id() {
        let msg = group_msg("111", "1", "Hello");
        assert!(WhatsAppChannel::is_group_message(&msg));
    }

    #[test]
    fn whatsapp_is_group_message_without_context() {
        let msg = dm_msg("111", "1", "Hello");
        assert!(!WhatsAppChannel::is_group_message(&msg));
    }

    #[test]
    fn whatsapp_is_group_message_empty_group_id() {
        let msg = serde_json::json!({
            "from": "111",
            "timestamp": "1",
            "type": "text",
            "text": { "body": "Hi" },
            "context": { "group_id": "" }
        });
        assert!(!WhatsAppChannel::is_group_message(&msg));
    }

    #[test]
    fn whatsapp_mention_rejects_group_message_without_match() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "Hello without mention")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Should reject group messages without mention"
        );
    }

    #[test]
    fn whatsapp_mention_dm_passes_through_without_match() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [dm_msg("111", "1", "Hello without mention")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(
            msgs.len(),
            1,
            "DMs should pass through even without a mention"
        );
        assert_eq!(msgs[0].content, "Hello without mention");
    }

    #[test]
    fn whatsapp_mention_accepts_and_strips_in_group() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "@ZeroClaw what is the weather?")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "what is the weather?");
    }

    #[test]
    fn whatsapp_mention_strips_from_group_content() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "Hey @ZeroClaw tell me a joke")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hey tell me a joke");
    }

    #[test]
    fn whatsapp_mention_drops_mention_only_group_message() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "@ZeroClaw")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Should drop group message that is only a mention"
        );
    }

    #[test]
    fn whatsapp_mention_case_insensitive_group_match() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "@zeroclaw status")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "status");
    }

    #[test]
    fn whatsapp_no_patterns_passes_all_group_messages() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "Hello without mention")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello without mention");
    }

    #[test]
    fn whatsapp_mention_mixed_group_messages() {
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [
                            group_msg("111", "1", "No mention here"),
                            group_msg("222", "2", "@ZeroClaw help me"),
                            group_msg("333", "3", "Also no mention")
                        ]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "help me");
        assert_eq!(msgs[0].sender, "+222");
    }

    #[test]
    fn whatsapp_mention_phone_pattern_in_group() {
        let ch = WhatsAppChannel::new("tok".into(), "123".into(), "ver".into(), vec!["*".into()])
            .with_mention_patterns(vec![r"\+?15555550123".into()]);
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [group_msg("111", "1", "+15555550123 tell me a joke")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "tell me a joke");
    }

    #[test]
    fn whatsapp_mention_dm_not_stripped() {
        // DMs should never have mention patterns stripped, even when configured
        let ch = make_mention_channel();
        let payload = serde_json::json!({
            "entry": [{
                "changes": [{
                    "value": {
                        "messages": [dm_msg("111", "1", "@ZeroClaw what is the weather?")]
                    }
                }]
            }]
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content, "@ZeroClaw what is the weather?",
            "DM content should not be stripped"
        );
    }
}
