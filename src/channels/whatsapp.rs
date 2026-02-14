use async_trait::async_trait;
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use super::traits::{Channel, ChannelMessage};

const WHATSAPP_API_BASE: &str = "https://graph.facebook.com/v18.0";

/// `WhatsApp` channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    pub phone_number_id: String,
    pub access_token: String,
    pub verify_token: String,
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
    #[serde(default = "default_webhook_path")]
    pub webhook_path: String,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
}

fn default_webhook_path() -> String { "/webhook/whatsapp".into() }
fn default_rate_limit() -> u32 { 60 }

impl Default for WhatsAppConfig {
    fn default() -> Self {
        Self {
            phone_number_id: String::new(),
            access_token: String::new(),
            verify_token: String::new(),
            allowed_numbers: Vec::new(),
            webhook_path: default_webhook_path(),
            rate_limit_per_minute: default_rate_limit(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WebhookEntry { changes: Vec<WebhookChange> }
#[derive(Debug, Deserialize)]
struct WebhookChange { value: WebhookValue }
#[derive(Debug, Deserialize)]
struct WebhookValue {
    messages: Option<Vec<WebhookMessage>>,
    statuses: Option<Vec<MessageStatus>>,
}
#[derive(Debug, Deserialize)]
struct WebhookMessage {
    from: String, id: String, timestamp: String,
    text: Option<MessageText>,
    image: Option<MediaMessage>,
    document: Option<MediaMessage>,
}
#[derive(Debug, Deserialize)]
struct MessageText { body: String }
#[derive(Debug, Deserialize)]
struct MediaMessage { id: String, mime_type: Option<String>, filename: Option<String> }
#[derive(Debug, Deserialize)]
struct MessageStatus { id: String, status: String, timestamp: String, recipient_id: String }

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    messaging_product: String, to: String,
    #[serde(rename = "type")] message_type: String,
    text: MessageTextBody,
}
#[derive(Debug, Serialize)]
struct MessageTextBody { body: String }

pub struct WhatsAppChannel {
    pub config: WhatsAppConfig,
    client: Client,
    rate_limiter: Arc<RwLock<HashMap<String, Vec<u64>>>>,
}

impl WhatsAppChannel {
    pub fn new(config: WhatsAppConfig) -> Self {
        Self {
            config,
            client: Client::builder().timeout(std::time::Duration::from_secs(30)).build().unwrap(),
            rate_limiter: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn verify_webhook(&self, mode: &str, token: &str, challenge: &str) -> Result<String> {
        if mode == "subscribe" && token == self.config.verify_token {
            Ok(challenge.to_string())
        } else {
            Err(anyhow!("Webhook verification failed"))
        }
    }

    pub async fn process_webhook(&self, payload: Value, tx: &mpsc::Sender<ChannelMessage>) -> Result<()> {
        let webhook: HashMap<String, Value> = serde_json::from_value(payload)?;
        if let Some(entry_array) = webhook.get("entry") {
            if let Some(entries) = entry_array.as_array() {
                for entry in entries {
                    if let Ok(e) = serde_json::from_value::<WebhookEntry>(entry.clone()) {
                        for change in e.changes {
                            if let Some(messages) = change.value.messages {
                                for msg in messages {
                                    let _ = self.process_message(msg, tx).await;
                                }
                            }
                            if let Some(statuses) = change.value.statuses {
                                for s in statuses {
                                    debug!("Status {}: {} for {}", s.id, s.status, s.recipient_id);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn process_message(&self, message: WebhookMessage, tx: &mpsc::Sender<ChannelMessage>) -> Result<()> {
        if !self.is_sender_allowed(&message.from) {
            warn!("Blocked WhatsApp from {}", message.from);
            return Ok(());
        }
        if !self.check_rate_limit(&message.from).await {
            warn!("Rate limited: {}", message.from);
            return Ok(());
        }
        let content = if let Some(text) = message.text { text.body }
            else if message.image.is_some() { "[Image]".into() }
            else if message.document.is_some() { "[Document]".into() }
            else { "[Unsupported]".into() };

        let timestamp = message.timestamp.parse::<u64>().unwrap_or_else(|_| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
        });

        let _ = tx.send(ChannelMessage {
            id: message.id, sender: message.from, content,
            channel: "whatsapp".into(), timestamp,
        }).await;
        Ok(())
    }

    pub fn is_sender_allowed(&self, phone: &str) -> bool {
        fn normalize(p: &str) -> String {
            p.trim_start_matches('+').trim_start_matches('0').to_string()
        }
        if self.config.allowed_numbers.is_empty() { return false; }
        if self.config.allowed_numbers.iter().any(|a| a == "*") { return true; }
        // Normalize phone numbers for comparison (strip + and leading zeros)
        let phone_norm = normalize(phone);
        self.config.allowed_numbers.iter().any(|a| {
            let a_norm = normalize(a);
            a_norm == phone_norm || phone_norm.ends_with(&a_norm) || a_norm.ends_with(&phone_norm)
        })
    }

    pub async fn check_rate_limit(&self, phone: &str) -> bool {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut limiter = self.rate_limiter.write().await;
        let timestamps = limiter.entry(phone.to_string()).or_default();
        timestamps.retain(|&t| now - t < 60);
        if timestamps.len() >= self.config.rate_limit_per_minute as usize { return false; }
        timestamps.push(now);
        true
    }
}

#[async_trait]
impl Channel for WhatsAppChannel {
    fn name(&self) -> &str { "whatsapp" }

    async fn send(&self, message: &str, recipient: &str) -> Result<()> {
        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, self.config.phone_number_id);
        let body = json!({
            "messaging_product": "whatsapp", "to": recipient,
            "type": "text", "text": {"body": message}
        });
        let resp = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.config.access_token))
            .json(&body).send().await?;
        if !resp.status().is_success() {
            let err = resp.text().await?;
            return Err(anyhow!("WhatsApp API: {err}"));
        }
        info!("WhatsApp sent to {}", recipient);
        Ok(())
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        info!("WhatsApp webhook path: {}", self.config.webhook_path);
        // Webhooks handled by gateway HTTP server — process_webhook() called externally
        // Keep task alive to prevent channel bus from closing
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/{}", WHATSAPP_API_BASE, self.config.phone_number_id);
        self.client.get(&url)
            .header("Authorization", format!("Bearer {}", self.config.access_token))
            .send().await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_module_compiles() {
        // This test should always pass if the module compiles
        assert!(true);
    }

    fn wildcard() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: "123".into(), access_token: "tok".into(),
            verify_token: "verify".into(), allowed_numbers: vec!["*".into()],
            ..Default::default()
        }
    }

    #[test]
    fn name() {
        assert_eq!(WhatsAppChannel::new(wildcard()).name(), "whatsapp");
    }
    #[test]
    fn allow_wildcard() {
        assert!(WhatsAppChannel::new(wildcard()).is_sender_allowed("any"));
    }
    #[test]
    fn deny_empty() {
        let mut c = wildcard();
        c.allowed_numbers = vec![];
        assert!(!WhatsAppChannel::new(c).is_sender_allowed("any"));
    }
    #[tokio::test]
    async fn verify_ok() {
        let ch = WhatsAppChannel::new(wildcard());
        assert_eq!(
            ch.verify_webhook("subscribe", "verify", "ch")
                .await
                .unwrap(),
            "ch"
        );
    }
    #[tokio::test]
    async fn verify_bad() {
        assert!(WhatsAppChannel::new(wildcard())
            .verify_webhook("subscribe", "wrong", "c")
            .await
            .is_err());
    }
    #[tokio::test]
    async fn rate_limit() {
        let mut c = wildcard();
        c.rate_limit_per_minute = 2;
        let ch = WhatsAppChannel::new(c);
        assert!(ch.check_rate_limit("+1").await);
        assert!(ch.check_rate_limit("+1").await);
        assert!(!ch.check_rate_limit("+1").await);
    }
    #[tokio::test]
    async fn text_msg() {
        let ch = WhatsAppChannel::new(wildcard());
        let (tx, mut rx) = mpsc::channel(10);
        ch.process_webhook(
            json!({"entry":[{"changes":[{"value":{"messages":[{
                "from":"123","id":"m1","timestamp":"100","text":{"body":"hi"}
            }]}}]}]}),
            &tx,
        )
        .await
        .unwrap();
        let m = rx.recv().await.unwrap();
        assert_eq!(m.content, "hi");
        assert_eq!(m.channel, "whatsapp");
    }
}
