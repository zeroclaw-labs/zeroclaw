//! Gmail Pub/Sub push notification channel.
//!
//! Instead of polling via IMAP, this channel uses Google's Gmail Pub/Sub push
//! notifications.  Google sends a POST to our webhook endpoint whenever the
//! user's mailbox changes.  The notification body contains a base64-encoded
//! JSON payload with `emailAddress` and `historyId`; we then call the Gmail
//! History API to fetch newly arrived messages.
//!
//! ## Setup
//!
//! 1. Create a Google Cloud Pub/Sub topic and grant `gmail-api-push@system.gserviceaccount.com`
//!    the **Pub/Sub Publisher** role on that topic.
//! 2. Create a push subscription pointing to `https://<your-domain>/webhook/gmail`.
//! 3. Configure `[channels_config.gmail_push]` in `config.toml` with `topic` and
//!    `oauth_token` (or set `GMAIL_PUSH_OAUTH_TOKEN` env var).
//!
//! The channel automatically calls `users.watch` to register the subscription
//! and renews it before the 7-day expiry.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use super::traits::{Channel, ChannelMessage, SendMessage};

// ── Configuration ────────────────────────────────────────────────

/// Gmail Pub/Sub push notification channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, zeroclaw_macros::Configurable)]
#[prefix = "channels.gmail"]
pub struct GmailPushConfig {
    /// Enable the Gmail push channel. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Google Cloud Pub/Sub topic in the form `projects/<project>/topics/<topic>`.
    pub topic: String,
    /// Gmail labels to watch. Default: `["INBOX"]`.
    #[serde(default = "default_label_filter")]
    pub label_filter: Vec<String>,
    /// OAuth2 access token for the Gmail API.
    /// Falls back to `GMAIL_PUSH_OAUTH_TOKEN` env var.
    #[serde(default)]
    #[secret]
    pub oauth_token: String,
    /// Allowed sender addresses/domains. Empty = deny all, `["*"]` = allow all.
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Webhook URL that Google Pub/Sub should POST to.
    /// Usually `https://<your-domain>/webhook/gmail`.
    /// If empty, watch registration is skipped (useful when using external subscription management).
    #[serde(default)]
    pub webhook_url: String,
    /// Shared secret for webhook authentication. If set, incoming webhook
    /// requests must include `Authorization: Bearer <secret>`.
    /// Falls back to `GMAIL_PUSH_WEBHOOK_SECRET` env var.
    #[serde(default)]
    pub webhook_secret: String,
}

fn default_label_filter() -> Vec<String> {
    vec!["INBOX".into()]
}

impl crate::config::traits::ChannelConfig for GmailPushConfig {
    fn name() -> &'static str {
        "Gmail Push"
    }
    fn desc() -> &'static str {
        "Gmail Pub/Sub real-time push notifications"
    }
}

impl Default for GmailPushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            topic: String::new(),
            label_filter: default_label_filter(),
            oauth_token: String::new(),
            allowed_senders: Vec::new(),
            webhook_url: String::new(),
            webhook_secret: String::new(),
        }
    }
}

// ── Pub/Sub notification payload ─────────────────────────────────

/// The outer JSON envelope that Google Pub/Sub POSTs to the push endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct PubSubEnvelope {
    pub message: PubSubMessage,
    /// Subscription name (informational).
    #[serde(default)]
    pub subscription: String,
}

/// A single Pub/Sub message inside the envelope.
#[derive(Debug, Deserialize, Serialize)]
pub struct PubSubMessage {
    /// Base64-encoded JSON data from Gmail.
    pub data: String,
    /// Pub/Sub message ID.
    #[serde(default, rename = "messageId")]
    pub message_id: String,
    /// Publish timestamp (RFC 3339).
    #[serde(default, rename = "publishTime")]
    pub publish_time: String,
}

/// The decoded payload inside `PubSubMessage.data`.
#[derive(Debug, Deserialize, Serialize)]
pub struct GmailNotification {
    /// Email address of the affected mailbox.
    #[serde(rename = "emailAddress")]
    pub email_address: String,
    /// History ID to use as `startHistoryId` for incremental sync.
    #[serde(rename = "historyId")]
    pub history_id: u64,
}

// ── Gmail API response types ─────────────────────────────────────

/// Response from `GET /gmail/v1/users/me/history`.
#[derive(Debug, Deserialize)]
pub struct HistoryResponse {
    pub history: Option<Vec<HistoryRecord>>,
    #[serde(default, rename = "historyId")]
    pub history_id: u64,
    #[serde(default, rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

/// A single history record containing messages added to the mailbox.
#[derive(Debug, Deserialize)]
pub struct HistoryRecord {
    #[serde(default, rename = "messagesAdded")]
    pub messages_added: Vec<MessageAdded>,
}

/// Wrapper for a newly added message reference.
#[derive(Debug, Deserialize)]
pub struct MessageAdded {
    pub message: MessageRef,
}

/// Minimal message reference returned by the history API.
#[derive(Debug, Deserialize)]
pub struct MessageRef {
    pub id: String,
    #[serde(default, rename = "threadId")]
    pub thread_id: String,
}

/// Full message returned by `GET /gmail/v1/users/me/messages/{id}`.
#[derive(Debug, Deserialize)]
pub struct GmailMessage {
    pub id: String,
    #[serde(default, rename = "threadId")]
    pub thread_id: String,
    #[serde(default)]
    pub snippet: String,
    pub payload: Option<MessagePayload>,
    #[serde(default, rename = "internalDate")]
    pub internal_date: String,
}

/// Message payload with headers and parts.
#[derive(Debug, Deserialize)]
pub struct MessagePayload {
    #[serde(default)]
    pub headers: Vec<MessageHeader>,
    pub body: Option<MessageBody>,
    #[serde(default)]
    pub parts: Vec<MessagePart>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
}

/// A single email header (name/value pair).
#[derive(Debug, Deserialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

/// Message body with optional base64-encoded data.
#[derive(Debug, Deserialize)]
pub struct MessageBody {
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub size: u64,
}

/// A MIME part of a multipart message.
#[derive(Debug, Deserialize)]
pub struct MessagePart {
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    pub body: Option<MessageBody>,
    #[serde(default)]
    pub parts: Vec<MessagePart>,
    #[serde(default)]
    pub filename: String,
}

/// Response from `POST /gmail/v1/users/me/watch`.
#[derive(Debug, Deserialize)]
pub struct WatchResponse {
    #[serde(default, rename = "historyId")]
    pub history_id: u64,
    #[serde(default)]
    pub expiration: String,
}

// ── Channel implementation ───────────────────────────────────────

/// Gmail Pub/Sub push notification channel.
///
/// Incoming messages arrive via webhook (`POST /webhook/gmail`) and are
/// dispatched to the agent.  The `listen` method registers the Gmail watch
/// subscription and periodically renews it.
pub struct GmailPushChannel {
    pub config: GmailPushConfig,
    http: Client,
    last_history_id: Arc<Mutex<u64>>,
    /// Sender half injected by the gateway to forward webhook-received messages.
    pub tx: Arc<Mutex<Option<mpsc::Sender<ChannelMessage>>>>,
}

impl GmailPushChannel {
    pub fn new(config: GmailPushConfig) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self {
            config,
            http,
            last_history_id: Arc::new(Mutex::new(0)),
            tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Resolve the webhook secret from config or environment.
    pub fn resolve_webhook_secret(&self) -> String {
        if !self.config.webhook_secret.is_empty() {
            return self.config.webhook_secret.clone();
        }
        std::env::var("GMAIL_PUSH_WEBHOOK_SECRET").unwrap_or_default()
    }

    /// Resolve the OAuth token from config or environment.
    pub fn resolve_oauth_token(&self) -> String {
        if !self.config.oauth_token.is_empty() {
            return self.config.oauth_token.clone();
        }
        std::env::var("GMAIL_PUSH_OAUTH_TOKEN").unwrap_or_default()
    }

    /// Register a Gmail watch subscription via `POST /gmail/v1/users/me/watch`.
    pub async fn register_watch(&self) -> Result<WatchResponse> {
        let token = self.resolve_oauth_token();
        if token.is_empty() {
            return Err(anyhow!("Gmail OAuth token is not configured"));
        }

        let body = serde_json::json!({
            "topicName": self.config.topic,
            "labelIds": self.config.label_filter,
        });

        let resp = self
            .http
            .post("https://gmail.googleapis.com/gmail/v1/users/me/watch")
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Gmail watch registration failed ({}): {}",
                status,
                text
            ));
        }

        let watch: WatchResponse = resp.json().await?;
        let mut last_id = self.last_history_id.lock().await;
        if *last_id == 0 {
            *last_id = watch.history_id;
        }
        info!(
            "Gmail watch registered — historyId={}, expiration={}",
            watch.history_id, watch.expiration
        );
        Ok(watch)
    }

    /// Fetch new messages since the given `start_history_id` using the History API.
    pub async fn fetch_history(&self, start_history_id: u64) -> Result<Vec<String>> {
        let mut last_id = self.last_history_id.lock().await;
        self.fetch_history_inner(start_history_id, &mut last_id)
            .await
    }

    /// Inner history fetch that takes an already-locked history ID reference.
    /// This allows callers that already hold the lock to avoid deadlock.
    async fn fetch_history_inner(
        &self,
        start_history_id: u64,
        last_id: &mut u64,
    ) -> Result<Vec<String>> {
        let token = self.resolve_oauth_token();
        if token.is_empty() {
            return Err(anyhow!("Gmail OAuth token is not configured"));
        }

        let mut message_ids = Vec::new();
        let mut page_token: Option<String> = None;

        loop {
            let mut url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/history?startHistoryId={}&historyTypes=messageAdded",
                start_history_id
            );
            if let Some(ref pt) = page_token {
                let _ = write!(url, "&pageToken={pt}");
            }

            let resp = self.http.get(&url).bearer_auth(&token).send().await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(anyhow!("Gmail history fetch failed ({}): {}", status, text));
            }

            let history_resp: HistoryResponse = resp.json().await?;

            if let Some(records) = history_resp.history {
                for record in records {
                    for added in record.messages_added {
                        message_ids.push(added.message.id);
                    }
                }
            }

            // Update tracked history ID
            if history_resp.history_id > 0 && history_resp.history_id > *last_id {
                *last_id = history_resp.history_id;
            }

            match history_resp.next_page_token {
                Some(token) => page_token = Some(token),
                None => break,
            }
        }

        Ok(message_ids)
    }

    /// Fetch a full message by ID from the Gmail API.
    pub async fn fetch_message(&self, message_id: &str) -> Result<GmailMessage> {
        let token = self.resolve_oauth_token();
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}?format=full",
            message_id
        );

        let resp = self.http.get(&url).bearer_auth(&token).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Gmail message fetch failed ({}): {}", status, text));
        }

        Ok(resp.json().await?)
    }

    /// Check if a sender email is in the allowlist.
    pub fn is_sender_allowed(&self, email: &str) -> bool {
        if self.config.allowed_senders.is_empty() {
            return false;
        }
        if self.config.allowed_senders.iter().any(|a| a == "*") {
            return true;
        }
        let email_lower = email.to_lowercase();
        self.config.allowed_senders.iter().any(|allowed| {
            if allowed.starts_with('@') {
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                allowed.eq_ignore_ascii_case(email)
            } else {
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        })
    }

    /// Process a Pub/Sub push notification and dispatch new messages to the agent.
    pub async fn handle_notification(&self, envelope: &PubSubEnvelope) -> Result<()> {
        let notification = parse_notification(&envelope.message)?;
        debug!(
            "Gmail push notification: email={}, historyId={}",
            notification.email_address, notification.history_id
        );

        // Hold the lock across read-fetch-update to prevent duplicate
        // processing when concurrent webhook notifications arrive.
        let mut last_id = self.last_history_id.lock().await;

        if *last_id == 0 {
            // First notification — just record the history ID.
            *last_id = notification.history_id;
            info!(
                "Gmail push: first notification, seeding historyId={}",
                notification.history_id
            );
            return Ok(());
        }

        let start_id = *last_id;
        let message_ids = self.fetch_history_inner(start_id, &mut last_id).await?;
        // Explicitly drop the lock before doing network-heavy message fetching.
        drop(last_id);

        if message_ids.is_empty() {
            debug!("Gmail push: no new messages in history");
            return Ok(());
        }

        info!(
            "Gmail push: {} new message(s) to process",
            message_ids.len()
        );

        // Clone the sender and drop the mutex immediately to avoid holding it
        // across network calls.
        let tx = {
            let tx_guard = self.tx.lock().await;
            match tx_guard.clone() {
                Some(tx) => tx,
                None => {
                    warn!("Gmail push: no listener registered, dropping messages");
                    return Ok(());
                }
            }
        };

        for msg_id in message_ids {
            match self.fetch_message(&msg_id).await {
                Ok(gmail_msg) => {
                    let sender = extract_header(&gmail_msg, "From").unwrap_or_default();
                    let sender_email = extract_email_from_header(&sender);

                    if !self.is_sender_allowed(&sender_email) {
                        warn!("Gmail push: blocked message from {}", sender_email);
                        continue;
                    }

                    let subject = extract_header(&gmail_msg, "Subject").unwrap_or_default();
                    let body_text = extract_body_text(&gmail_msg);

                    let content = format!("Subject: {subject}\n\n{body_text}");
                    let timestamp = gmail_msg
                        .internal_date
                        .parse::<u64>()
                        .map(|ms| ms / 1000)
                        .unwrap_or_else(|_| {
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0)
                        });

                    let channel_msg = ChannelMessage {
                        id: format!("gmail_{}", gmail_msg.id),
                        reply_target: sender_email.clone(),
                        sender: sender_email,
                        content,
                        channel: "gmail_push".to_string(),
                        timestamp,
                        thread_ts: Some(gmail_msg.thread_id),
                        interruption_scope_id: None,
                        attachments: Vec::new(),
                    };

                    if tx.send(channel_msg).await.is_err() {
                        debug!("Gmail push: listener channel closed");
                        return Ok(());
                    }
                }
                Err(e) => {
                    error!("Gmail push: failed to fetch message {}: {}", msg_id, e);
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Channel for GmailPushChannel {
    fn name(&self) -> &str {
        "gmail_push"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Send via Gmail API (drafts.send or messages.send)
        let token = self.resolve_oauth_token();
        if token.is_empty() {
            return Err(anyhow!("Gmail OAuth token is not configured for sending"));
        }

        let subject = message.subject.as_deref().unwrap_or("ZeroClaw Message");
        // Sanitize headers to prevent CRLF injection attacks.
        let safe_recipient = sanitize_header_value(&message.recipient);
        let safe_subject = sanitize_header_value(subject);
        let rfc2822 = format!(
            "To: {}\r\nSubject: {}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}",
            safe_recipient, safe_subject, message.content
        );
        let encoded = BASE64.encode(rfc2822.as_bytes());
        // Gmail API uses URL-safe base64 with no padding
        let url_safe = encoded.replace('+', "-").replace('/', "_").replace('=', "");

        let body = serde_json::json!({
            "raw": url_safe,
        });

        let resp = self
            .http
            .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Gmail send failed ({}): {}", status, text));
        }

        info!("Gmail message sent to {}", message.recipient);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Store the sender for webhook-driven message dispatch
        {
            let mut tx_guard = self.tx.lock().await;
            *tx_guard = Some(tx);
        }

        info!("Gmail push channel started — registering watch subscription");

        // Register initial watch
        if !self.config.webhook_url.is_empty() {
            if let Err(e) = self.register_watch().await {
                error!("Gmail watch registration failed: {e:#}");
                // Non-fatal — external subscription management may be in use
            }
        }

        // Renewal loop: Gmail watch subscriptions expire after 7 days.
        // Re-register every 6 days to maintain continuous coverage.
        let renewal_interval = Duration::from_secs(6 * 24 * 60 * 60); // 6 days
        loop {
            tokio::time::sleep(renewal_interval).await;
            info!("Gmail push: renewing watch subscription");
            if let Err(e) = self.register_watch().await {
                error!("Gmail watch renewal failed: {e:#}");
            }
        }
    }

    async fn health_check(&self) -> bool {
        let token = self.resolve_oauth_token();
        if token.is_empty() {
            return false;
        }

        match self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
            .bearer_auth(&token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }
}

// ── Helper functions ─────────────────────────────────────────────

/// Parse and decode the Gmail notification from a Pub/Sub message.
pub fn parse_notification(msg: &PubSubMessage) -> Result<GmailNotification> {
    let decoded = BASE64
        .decode(&msg.data)
        .map_err(|e| anyhow!("Invalid base64 in Pub/Sub message: {e}"))?;
    let notification: GmailNotification = serde_json::from_slice(&decoded)
        .map_err(|e| anyhow!("Invalid JSON in Gmail notification: {e}"))?;
    Ok(notification)
}

/// Extract a header value from a Gmail message by name.
pub fn extract_header(msg: &GmailMessage, name: &str) -> Option<String> {
    msg.payload.as_ref().and_then(|p| {
        p.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.clone())
    })
}

/// Extract the plain email address from a `From` header value like `"Name <email@example.com>"`.
pub fn extract_email_from_header(from: &str) -> String {
    if let Some(start) = from.find('<') {
        // Use rfind to find the matching '>' after '<', preventing panic
        // when malformed headers have '>' before '<'.
        if let Some(end) = from.rfind('>') {
            if end > start + 1 {
                return from[start + 1..end].to_string();
            }
        }
    }
    from.trim().to_string()
}

/// Sanitize a string for use in an RFC 2822 header value.
/// Removes CR and LF characters to prevent header injection attacks.
pub fn sanitize_header_value(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

/// Extract the plain-text body from a Gmail message.
///
/// Walks MIME parts looking for `text/plain`; falls back to `text/html`
/// with basic tag stripping; finally falls back to the `snippet`.
pub fn extract_body_text(msg: &GmailMessage) -> String {
    if let Some(ref payload) = msg.payload {
        // Single-part message
        if payload.mime_type == "text/plain" {
            if let Some(text) = decode_body(payload.body.as_ref()) {
                return text;
            }
        }

        // Multipart — walk parts
        if let Some(text) = find_text_in_parts(&payload.parts, "text/plain") {
            return text;
        }
        if let Some(html) = find_text_in_parts(&payload.parts, "text/html") {
            return strip_html(&html);
        }
    }

    // Fallback to snippet
    msg.snippet.clone()
}

/// Recursively search MIME parts for a given content type.
fn find_text_in_parts(parts: &[MessagePart], mime_type: &str) -> Option<String> {
    for part in parts {
        if part.mime_type == mime_type {
            if let Some(text) = decode_body(part.body.as_ref()) {
                return Some(text);
            }
        }
        // Recurse into nested parts
        if let Some(text) = find_text_in_parts(&part.parts, mime_type) {
            return Some(text);
        }
    }
    None
}

/// Decode a base64url-encoded Gmail message body.
fn decode_body(body: Option<&MessageBody>) -> Option<String> {
    body.and_then(|b| {
        b.data.as_ref().and_then(|data| {
            // Gmail API uses URL-safe base64 without padding
            let standard = data.replace('-', "+").replace('_', "/");
            // Restore padding stripped by Gmail API
            let padded = match standard.len() % 4 {
                2 => format!("{standard}=="),
                3 => format!("{standard}="),
                _ => standard,
            };
            BASE64
                .decode(&padded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
    })
}

/// Basic HTML tag stripper (reuses the pattern from email_channel).
fn strip_html(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    let mut normalized = String::with_capacity(result.len());
    for word in result.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    normalized
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Notification parsing ─────────────────────────────────────

    #[test]
    fn parse_notification_valid() {
        let payload = serde_json::json!({
            "emailAddress": "user@example.com",
            "historyId": 12345
        });
        let encoded = BASE64.encode(serde_json::to_vec(&payload).unwrap());

        let msg = PubSubMessage {
            data: encoded,
            message_id: "msg-1".into(),
            publish_time: "2026-03-21T08:00:00Z".into(),
        };

        let notification = parse_notification(&msg).unwrap();
        assert_eq!(notification.email_address, "user@example.com");
        assert_eq!(notification.history_id, 12345);
    }

    #[test]
    fn parse_notification_invalid_base64() {
        let msg = PubSubMessage {
            data: "!!!not-base64!!!".into(),
            message_id: "msg-2".into(),
            publish_time: String::new(),
        };
        assert!(parse_notification(&msg).is_err());
    }

    #[test]
    fn parse_notification_invalid_json() {
        let encoded = BASE64.encode(b"not json at all");
        let msg = PubSubMessage {
            data: encoded,
            message_id: "msg-3".into(),
            publish_time: String::new(),
        };
        assert!(parse_notification(&msg).is_err());
    }

    // ── Envelope deserialization ─────────────────────────────────

    #[test]
    fn pubsub_envelope_deserialize() {
        let payload = serde_json::json!({
            "emailAddress": "test@gmail.com",
            "historyId": 999
        });
        let encoded = BASE64.encode(serde_json::to_vec(&payload).unwrap());

        let json = serde_json::json!({
            "message": {
                "data": encoded,
                "messageId": "pubsub-1",
                "publishTime": "2026-03-21T10:00:00Z"
            },
            "subscription": "projects/my-project/subscriptions/gmail-push"
        });

        let envelope: PubSubEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(envelope.message.message_id, "pubsub-1");
        assert_eq!(
            envelope.subscription,
            "projects/my-project/subscriptions/gmail-push"
        );

        let notification = parse_notification(&envelope.message).unwrap();
        assert_eq!(notification.email_address, "test@gmail.com");
        assert_eq!(notification.history_id, 999);
    }

    // ── Email extraction from From header ────────────────────────

    #[test]
    fn extract_email_from_header_angle_brackets() {
        assert_eq!(
            extract_email_from_header("John Doe <john@example.com>"),
            "john@example.com"
        );
    }

    #[test]
    fn extract_email_from_header_bare_email() {
        assert_eq!(
            extract_email_from_header("user@example.com"),
            "user@example.com"
        );
    }

    #[test]
    fn extract_email_from_header_empty() {
        assert_eq!(extract_email_from_header(""), "");
    }

    #[test]
    fn extract_email_with_quotes() {
        assert_eq!(
            extract_email_from_header("\"Doe, John\" <john@example.com>"),
            "john@example.com"
        );
    }

    #[test]
    fn extract_email_malformed_angle_brackets() {
        // '>' before '<' with no proper closing — falls back to full trimmed string
        assert_eq!(
            extract_email_from_header("attacker> <victim@example.com"),
            "attacker> <victim@example.com"
        );
        // Properly closed after the second '<'
        assert_eq!(
            extract_email_from_header("attacker> <victim@example.com>"),
            "victim@example.com"
        );
        // No closing '>' at all
        assert_eq!(extract_email_from_header("Name <broken"), "Name <broken");
    }

    #[test]
    fn sanitize_header_strips_crlf() {
        assert_eq!(
            sanitize_header_value("normal@example.com"),
            "normal@example.com"
        );
        assert_eq!(
            sanitize_header_value("evil@example.com\r\nBcc: spy@evil.com"),
            "evil@example.comBcc: spy@evil.com"
        );
        assert_eq!(
            sanitize_header_value("inject\nSubject: fake"),
            "injectSubject: fake"
        );
    }

    // ── Header extraction ────────────────────────────────────────

    #[test]
    fn extract_header_found() {
        let msg = GmailMessage {
            id: "msg-1".into(),
            thread_id: "thread-1".into(),
            snippet: String::new(),
            payload: Some(MessagePayload {
                headers: vec![
                    MessageHeader {
                        name: "From".into(),
                        value: "sender@example.com".into(),
                    },
                    MessageHeader {
                        name: "Subject".into(),
                        value: "Test Subject".into(),
                    },
                ],
                body: None,
                parts: Vec::new(),
                mime_type: String::new(),
            }),
            internal_date: "0".into(),
        };

        assert_eq!(
            extract_header(&msg, "Subject"),
            Some("Test Subject".to_string())
        );
        assert_eq!(
            extract_header(&msg, "from"), // case-insensitive
            Some("sender@example.com".to_string())
        );
        assert_eq!(extract_header(&msg, "X-Missing"), None);
    }

    #[test]
    fn extract_header_no_payload() {
        let msg = GmailMessage {
            id: "msg-2".into(),
            thread_id: String::new(),
            snippet: String::new(),
            payload: None,
            internal_date: "0".into(),
        };
        assert_eq!(extract_header(&msg, "Subject"), None);
    }

    // ── Body text extraction ─────────────────────────────────────

    #[test]
    fn extract_body_text_plain() {
        let plain_b64 = BASE64
            .encode(b"Hello, world!")
            .replace('+', "-")
            .replace('/', "_")
            .replace('=', "");

        let msg = GmailMessage {
            id: "msg-3".into(),
            thread_id: String::new(),
            snippet: "snippet".into(),
            payload: Some(MessagePayload {
                headers: Vec::new(),
                body: Some(MessageBody {
                    data: Some(plain_b64),
                    size: 13,
                }),
                parts: Vec::new(),
                mime_type: "text/plain".into(),
            }),
            internal_date: "0".into(),
        };

        assert_eq!(extract_body_text(&msg), "Hello, world!");
    }

    #[test]
    fn extract_body_text_multipart() {
        let html_b64 = BASE64
            .encode(b"<p>Hello</p>")
            .replace('+', "-")
            .replace('/', "_")
            .replace('=', "");

        let msg = GmailMessage {
            id: "msg-4".into(),
            thread_id: String::new(),
            snippet: "snippet".into(),
            payload: Some(MessagePayload {
                headers: Vec::new(),
                body: None,
                parts: vec![MessagePart {
                    mime_type: "text/html".into(),
                    body: Some(MessageBody {
                        data: Some(html_b64),
                        size: 12,
                    }),
                    parts: Vec::new(),
                    filename: String::new(),
                }],
                mime_type: "multipart/alternative".into(),
            }),
            internal_date: "0".into(),
        };

        assert_eq!(extract_body_text(&msg), "Hello");
    }

    #[test]
    fn extract_body_text_fallback_to_snippet() {
        let msg = GmailMessage {
            id: "msg-5".into(),
            thread_id: String::new(),
            snippet: "My snippet text".into(),
            payload: Some(MessagePayload {
                headers: Vec::new(),
                body: None,
                parts: Vec::new(),
                mime_type: "multipart/mixed".into(),
            }),
            internal_date: "0".into(),
        };

        assert_eq!(extract_body_text(&msg), "My snippet text");
    }

    // ── Sender allowlist ─────────────────────────────────────────

    #[test]
    fn sender_allowed_empty_denies() {
        let ch = GmailPushChannel::new(GmailPushConfig::default());
        assert!(!ch.is_sender_allowed("anyone@example.com"));
    }

    #[test]
    fn sender_allowed_wildcard() {
        let ch = GmailPushChannel::new(GmailPushConfig {
            allowed_senders: vec!["*".into()],
            ..Default::default()
        });
        assert!(ch.is_sender_allowed("anyone@example.com"));
    }

    #[test]
    fn sender_allowed_specific_email() {
        let ch = GmailPushChannel::new(GmailPushConfig {
            allowed_senders: vec!["user@example.com".into()],
            ..Default::default()
        });
        assert!(ch.is_sender_allowed("user@example.com"));
        assert!(!ch.is_sender_allowed("other@example.com"));
    }

    #[test]
    fn sender_allowed_domain_with_at() {
        let ch = GmailPushChannel::new(GmailPushConfig {
            allowed_senders: vec!["@example.com".into()],
            ..Default::default()
        });
        assert!(ch.is_sender_allowed("user@example.com"));
        assert!(ch.is_sender_allowed("admin@example.com"));
        assert!(!ch.is_sender_allowed("user@other.com"));
    }

    #[test]
    fn sender_allowed_domain_without_at() {
        let ch = GmailPushChannel::new(GmailPushConfig {
            allowed_senders: vec!["example.com".into()],
            ..Default::default()
        });
        assert!(ch.is_sender_allowed("user@example.com"));
        assert!(!ch.is_sender_allowed("user@other.com"));
    }

    // ── Strip HTML ───────────────────────────────────────────────

    #[test]
    fn strip_html_basic() {
        assert_eq!(strip_html("<p>Hello</p>"), "Hello");
    }

    #[test]
    fn strip_html_nested() {
        assert_eq!(
            strip_html("<div><p>Hello <b>World</b></p></div>"),
            "Hello World"
        );
    }

    // ── Config defaults ──────────────────────────────────────────

    #[test]
    fn config_default_values() {
        let config = GmailPushConfig::default();
        assert!(!config.enabled);
        assert!(config.topic.is_empty());
        assert_eq!(config.label_filter, vec!["INBOX"]);
        assert!(config.oauth_token.is_empty());
        assert!(config.allowed_senders.is_empty());
        assert!(config.webhook_url.is_empty());
    }

    #[test]
    fn config_deserialize_with_defaults() {
        let json = r#"{"topic": "projects/my-proj/topics/gmail"}"#;
        let config: GmailPushConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert_eq!(config.topic, "projects/my-proj/topics/gmail");
        assert_eq!(config.label_filter, vec!["INBOX"]);
    }

    #[test]
    fn config_serialize_roundtrip() {
        let config = GmailPushConfig {
            enabled: true,
            topic: "projects/test/topics/gmail".into(),
            label_filter: vec!["INBOX".into(), "IMPORTANT".into()],
            oauth_token: "test-token".into(),
            allowed_senders: vec!["@example.com".into()],
            webhook_url: "https://example.com/webhook/gmail".into(),
            webhook_secret: "my-secret".into(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: GmailPushConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.topic, config.topic);
        assert_eq!(deserialized.label_filter, config.label_filter);
        assert_eq!(deserialized.webhook_url, config.webhook_url);
    }

    // ── Channel name ─────────────────────────────────────────────

    #[test]
    fn channel_name() {
        let ch = GmailPushChannel::new(GmailPushConfig::default());
        assert_eq!(ch.name(), "gmail_push");
    }

    // ── Decode body ──────────────────────────────────────────────

    #[test]
    fn decode_body_none() {
        assert!(decode_body(None).is_none());
    }

    #[test]
    fn decode_body_empty_data() {
        let body = MessageBody {
            data: None,
            size: 0,
        };
        assert!(decode_body(Some(&body)).is_none());
    }

    #[test]
    fn decode_body_valid() {
        let b64 = BASE64
            .encode(b"test content")
            .replace('+', "-")
            .replace('/', "_")
            .replace('=', "");
        let body = MessageBody {
            data: Some(b64),
            size: 12,
        };
        assert_eq!(decode_body(Some(&body)), Some("test content".to_string()));
    }
}
