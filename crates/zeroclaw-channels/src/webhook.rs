use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Generic Webhook channel — receives messages via HTTP POST and sends replies
/// to a configurable outbound URL. This is the "universal adapter" for any system
/// that supports webhooks.
pub struct WebhookChannel {
    listen_port: u16,
    listen_path: String,
    send_url: Option<String>,
    send_method: String,
    auth_header: Option<String>,
    secret: Option<String>,
}

/// Incoming webhook payload format.
#[derive(Debug, Deserialize)]
struct IncomingWebhook {
    sender: String,
    content: String,
    #[serde(default)]
    thread_id: Option<String>,
}

/// Outgoing webhook payload format.
#[derive(Debug, Serialize)]
struct OutgoingWebhook {
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipient: Option<String>,
}

impl WebhookChannel {
    pub fn new(
        listen_port: u16,
        listen_path: Option<String>,
        send_url: Option<String>,
        send_method: Option<String>,
        auth_header: Option<String>,
        secret: Option<String>,
    ) -> Self {
        let path = listen_path.unwrap_or_else(|| "/webhook".to_string());
        // Ensure path starts with /
        let listen_path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };

        Self {
            listen_port,
            listen_path,
            send_url,
            send_method: send_method
                .unwrap_or_else(|| "POST".to_string())
                .to_uppercase(),
            auth_header,
            secret,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.webhook")
    }

    /// Verify an incoming request's signature if a secret is configured.
    #[cfg(test)]
    fn verify_signature(&self, body: &[u8], signature: Option<&str>) -> bool {
        let Some(ref secret) = self.secret else {
            return true; // No secret configured, accept all
        };

        let Some(sig) = signature else {
            return false; // Secret is set but no signature header provided
        };

        // HMAC-SHA256 verification
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
            return false;
        };
        mac.update(body);

        // Signature should be hex-encoded
        let Ok(expected) = hex::decode(sig.trim_start_matches("sha256=")) else {
            return false;
        };

        mac.verify_slice(&expected).is_ok()
    }
}

#[async_trait]
impl Channel for WebhookChannel {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let Some(ref send_url) = self.send_url else {
            tracing::debug!("Webhook channel: no send_url configured, skipping outbound message");
            return Ok(());
        };

        let client = self.http_client();
        let payload = OutgoingWebhook {
            content: message.content.clone(),
            thread_id: message.thread_ts.clone(),
            recipient: if message.recipient.is_empty() {
                None
            } else {
                Some(message.recipient.clone())
            },
        };

        let mut request = match self.send_method.as_str() {
            "PUT" => client.put(send_url),
            _ => client.post(send_url),
        };

        if let Some(ref auth) = self.auth_header {
            request = request.header("Authorization", auth);
        }

        let resp = request.json(&payload).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("Webhook send failed ({status}): {body}");
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        use axum::{
            Router,
            body::Bytes,
            extract::State,
            http::{HeaderMap, StatusCode},
            routing::post,
        };
        use portable_atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        let counter = Arc::new(AtomicU64::new(0));

        struct WebhookState {
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
            secret: Option<String>,
            counter: Arc<AtomicU64>,
        }

        let state = Arc::new(WebhookState {
            tx: tx.clone(),
            secret: self.secret.clone(),
            counter: counter.clone(),
        });

        let listen_path = self.listen_path.clone();

        async fn handle_webhook(
            State(state): State<Arc<WebhookState>>,
            headers: HeaderMap,
            body: Bytes,
        ) -> StatusCode {
            // Verify signature if secret is configured
            if let Some(ref secret) = state.secret {
                use hmac::{Hmac, Mac};
                use sha2::Sha256;
                type HmacSha256 = Hmac<Sha256>;

                let signature = headers
                    .get("x-webhook-signature")
                    .and_then(|v| v.to_str().ok());

                let valid = if let Some(sig) = signature {
                    if let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) {
                        mac.update(&body);
                        let expected =
                            hex::decode(sig.trim_start_matches("sha256=")).unwrap_or_default();
                        mac.verify_slice(&expected).is_ok()
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !valid {
                    tracing::warn!("Webhook: invalid signature, rejecting request");
                    return StatusCode::UNAUTHORIZED;
                }
            }

            let payload: IncomingWebhook = match serde_json::from_slice(&body) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Webhook: invalid JSON payload: {e}");
                    return StatusCode::BAD_REQUEST;
                }
            };

            if payload.content.is_empty() {
                return StatusCode::BAD_REQUEST;
            }

            let seq = state.counter.fetch_add(1, Ordering::Relaxed);

            #[allow(clippy::cast_possible_truncation)]
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let reply_target = payload
                .thread_id
                .clone()
                .unwrap_or_else(|| payload.sender.clone());

            let msg = ChannelMessage {
                id: format!("webhook_{seq}"),
                sender: payload.sender,
                reply_target,
                content: payload.content,
                channel: "webhook".to_string(),
                timestamp,
                thread_ts: payload.thread_id,
                interruption_scope_id: None,
                metadata: serde_json::Value::Null,
                attachments: vec![],
            };

            if state.tx.send(msg).await.is_err() {
                return StatusCode::SERVICE_UNAVAILABLE;
            }

            StatusCode::OK
        }

        let app = Router::new()
            .route(&listen_path, post(handle_webhook))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.listen_port));
        tracing::info!(
            "Webhook channel listening on http://0.0.0.0:{}{} ...",
            self.listen_port,
            self.listen_path
        );

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("Webhook server error: {e}"))?;

        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Webhook channel is healthy if the port can be bound (basic check).
        // In practice, once listen() starts the server is running.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> WebhookChannel {
        WebhookChannel::new(
            8080,
            Some("/webhook".into()),
            Some("https://example.com/callback".into()),
            None,
            None,
            None,
        )
    }

    fn make_channel_with_secret() -> WebhookChannel {
        WebhookChannel::new(
            8080,
            None,
            Some("https://example.com/callback".into()),
            None,
            None,
            Some("mysecret".into()),
        )
    }

    #[test]
    fn default_path() {
        let ch = WebhookChannel::new(8080, None, None, None, None, None);
        assert_eq!(ch.listen_path, "/webhook");
    }

    #[test]
    fn path_normalized() {
        let ch = WebhookChannel::new(8080, Some("hooks/incoming".into()), None, None, None, None);
        assert_eq!(ch.listen_path, "/hooks/incoming");
    }

    #[test]
    fn send_method_default() {
        let ch = make_channel();
        assert_eq!(ch.send_method, "POST");
    }

    #[test]
    fn send_method_put() {
        let ch = WebhookChannel::new(
            8080,
            None,
            Some("https://example.com".into()),
            Some("put".into()),
            None,
            None,
        );
        assert_eq!(ch.send_method, "PUT");
    }

    #[test]
    fn incoming_payload_deserializes_all_fields() {
        let json = r#"{"sender": "zeroclaw_user", "content": "hello", "thread_id": "t1"}"#;
        let payload: IncomingWebhook = serde_json::from_str(json).unwrap();
        assert_eq!(payload.sender, "zeroclaw_user");
        assert_eq!(payload.content, "hello");
        assert_eq!(payload.thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn incoming_payload_without_thread() {
        let json = r#"{"sender": "bob", "content": "hi"}"#;
        let payload: IncomingWebhook = serde_json::from_str(json).unwrap();
        assert_eq!(payload.sender, "bob");
        assert_eq!(payload.content, "hi");
        assert!(payload.thread_id.is_none());
    }

    #[test]
    fn outgoing_payload_serializes_content() {
        let payload = OutgoingWebhook {
            content: "response".into(),
            thread_id: Some("t1".into()),
            recipient: Some("zeroclaw_user".into()),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["content"], "response");
        assert_eq!(json["thread_id"], "t1");
        assert_eq!(json["recipient"], "zeroclaw_user");
    }

    #[test]
    fn outgoing_payload_omits_none_fields() {
        let payload = OutgoingWebhook {
            content: "response".into(),
            thread_id: None,
            recipient: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["content"], "response");
        assert!(json.get("thread_id").is_none());
        assert!(json.get("recipient").is_none());
    }

    #[test]
    fn verify_signature_no_secret() {
        let ch = make_channel();
        assert!(ch.verify_signature(b"body", None));
    }

    #[test]
    fn verify_signature_missing_header() {
        let ch = make_channel_with_secret();
        assert!(!ch.verify_signature(b"body", None));
    }

    #[test]
    fn verify_signature_valid() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let ch = make_channel_with_secret();
        let body = b"test body";

        let mut mac = HmacSha256::new_from_slice(b"mysecret").unwrap();
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());

        assert!(ch.verify_signature(body, Some(&sig)));
    }

    #[test]
    fn verify_signature_invalid() {
        let ch = make_channel_with_secret();
        assert!(!ch.verify_signature(b"body", Some("badhex")));
    }
}
