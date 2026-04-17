use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 500;
const DEFAULT_RETRY_MAX_DELAY_MS: u64 = 30_000;

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
    max_retries: u32,
    retry_base_delay_ms: u64,
    retry_max_delay_ms: u64,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        listen_port: u16,
        listen_path: Option<String>,
        send_url: Option<String>,
        send_method: Option<String>,
        auth_header: Option<String>,
        secret: Option<String>,
        max_retries: Option<u32>,
        retry_base_delay_ms: Option<u64>,
        retry_max_delay_ms: Option<u64>,
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
            max_retries: max_retries.unwrap_or(DEFAULT_MAX_RETRIES),
            retry_base_delay_ms: retry_base_delay_ms.unwrap_or(DEFAULT_RETRY_BASE_DELAY_MS),
            retry_max_delay_ms: retry_max_delay_ms.unwrap_or(DEFAULT_RETRY_MAX_DELAY_MS),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.webhook")
    }

    /// Compute the backoff delay for a given attempt, bounded by `retry_max_delay_ms`
    /// and with ±25% jitter applied. The cap is approximate: jitter is applied after
    /// capping, so the returned delay may exceed `retry_max_delay_ms` by up to 25%.
    fn compute_backoff(&self, attempt: u32) -> Duration {
        let multiplier = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let base = self.retry_base_delay_ms.saturating_mul(multiplier);
        let capped = base.min(self.retry_max_delay_ms);
        let jittered = apply_jitter(capped);
        Duration::from_millis(jittered)
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

/// Apply ±25% jitter to a delay so parallel senders do not thunder-herd.
fn apply_jitter(delay_ms: u64) -> u64 {
    if delay_ms == 0 {
        return 0;
    }
    let jitter_factor = 0.75 + (rand::random::<f64>() * 0.5);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let jittered = ((delay_ms as f64) * jitter_factor) as u64;
    jittered
}

/// Parse a `Retry-After` header value. Supports integer seconds and
/// decimal seconds (truncated to whole seconds). HTTP-date form is not supported.
fn parse_retry_after_ms(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(seconds.saturating_mul(1000));
    }
    let whole = trimmed
        .split_once('.')
        .map(|(whole, _)| whole)
        .unwrap_or(trimmed);
    whole.parse::<u64>().ok().map(|s| s.saturating_mul(1000))
}

/// Outcome of a single send attempt.
enum AttemptOutcome {
    Success,
    RetryAfter(Duration),
    Retry(String),
    Fatal(anyhow::Error),
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

        let total_attempts = self.max_retries.saturating_add(1);

        for attempt in 0..total_attempts {
            let outcome = self.attempt_send(&client, send_url, &payload).await;

            match outcome {
                AttemptOutcome::Success => return Ok(()),
                AttemptOutcome::Fatal(err) => return Err(err),
                AttemptOutcome::RetryAfter(delay) => {
                    if attempt + 1 >= total_attempts {
                        bail!(
                            "Webhook send failed after {total_attempts} attempt(s); last error: rate limited / server error with Retry-After"
                        );
                    }
                    let capped = delay.min(Duration::from_millis(self.retry_max_delay_ms));
                    tracing::warn!(
                        "Webhook send: server requested retry after {}ms (attempt {}/{}), waiting...",
                        capped.as_millis(),
                        attempt + 1,
                        total_attempts
                    );
                    tokio::time::sleep(capped).await;
                }
                AttemptOutcome::Retry(err_msg) => {
                    if attempt + 1 >= total_attempts {
                        bail!(
                            "Webhook send failed after {total_attempts} attempt(s); last error: {err_msg}"
                        );
                    }
                    let delay = self.compute_backoff(attempt);
                    tracing::warn!(
                        "Webhook send failed (attempt {}/{}): {}; retrying in {}ms",
                        attempt + 1,
                        total_attempts,
                        err_msg,
                        delay.as_millis()
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }

        unreachable!("send loop exits via return or bail on the final attempt")
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

impl WebhookChannel {
    async fn attempt_send(
        &self,
        client: &reqwest::Client,
        send_url: &str,
        payload: &OutgoingWebhook,
    ) -> AttemptOutcome {
        let mut request = match self.send_method.as_str() {
            "PUT" => client.put(send_url),
            _ => client.post(send_url),
        };

        if let Some(ref auth) = self.auth_header {
            request = request.header("Authorization", auth);
        }

        let resp = match request.json(payload).send().await {
            Ok(r) => r,
            Err(e) => return AttemptOutcome::Retry(format!("network error: {e}")),
        };

        let status = resp.status();
        if status.is_success() {
            return AttemptOutcome::Success;
        }

        let code = status.as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after_ms);

        let body = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response: {e}>"));

        // 429 and 503 may include Retry-After; honor it if present. 429 appears here
        // *and* in the branch below: here we take the server-supplied delay, below we
        // fall back to exponential backoff when no Retry-After header was sent.
        if (code == 429 || code == 503)
            && let Some(ms) = retry_after
        {
            return AttemptOutcome::RetryAfter(Duration::from_millis(ms));
        }

        // Retry 429 (rate limit) and 5xx (server errors).
        if code == 429 || (500..600).contains(&code) {
            return AttemptOutcome::Retry(format!("Webhook send failed ({status}): {body}"));
        }

        // Other 4xx → do not retry.
        AttemptOutcome::Fatal(anyhow::anyhow!("Webhook send failed ({status}): {body}"))
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
            None,
            None,
            None,
        )
    }

    fn make_channel_to(url: &str) -> WebhookChannel {
        // Fast retries to keep tests snappy.
        WebhookChannel::new(
            8080,
            None,
            Some(url.into()),
            None,
            None,
            None,
            Some(2),
            Some(10),
            Some(100),
        )
    }

    #[test]
    fn default_path() {
        let ch = WebhookChannel::new(8080, None, None, None, None, None, None, None, None);
        assert_eq!(ch.listen_path, "/webhook");
    }

    #[test]
    fn path_normalized() {
        let ch = WebhookChannel::new(
            8080,
            Some("hooks/incoming".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
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
            None,
            None,
            None,
        );
        assert_eq!(ch.send_method, "PUT");
    }

    #[test]
    fn retry_defaults_applied() {
        let ch = make_channel();
        assert_eq!(ch.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(ch.retry_base_delay_ms, DEFAULT_RETRY_BASE_DELAY_MS);
        assert_eq!(ch.retry_max_delay_ms, DEFAULT_RETRY_MAX_DELAY_MS);
    }

    #[test]
    fn retry_overrides_applied() {
        let ch = WebhookChannel::new(
            8080,
            None,
            Some("https://example.com".into()),
            None,
            None,
            None,
            Some(0),
            Some(50),
            Some(1_000),
        );
        assert_eq!(ch.max_retries, 0);
        assert_eq!(ch.retry_base_delay_ms, 50);
        assert_eq!(ch.retry_max_delay_ms, 1_000);
    }

    #[test]
    fn backoff_capped_by_max_delay() {
        let ch = WebhookChannel::new(
            8080,
            None,
            Some("https://example.com".into()),
            None,
            None,
            None,
            Some(5),
            Some(1_000),
            Some(2_000),
        );
        let d = ch.compute_backoff(10); // 1_000 * 2^10 = 1_024_000, capped at 2_000
        // With ±25% jitter: [1500, 2500]
        assert!(d.as_millis() >= 1_500 && d.as_millis() <= 2_500);
    }

    #[test]
    fn parse_retry_after_integer_seconds() {
        assert_eq!(parse_retry_after_ms("5"), Some(5_000));
    }

    #[test]
    fn parse_retry_after_decimal_seconds() {
        assert_eq!(parse_retry_after_ms("2.9"), Some(2_000));
    }

    #[test]
    fn parse_retry_after_rejects_non_numeric() {
        assert_eq!(parse_retry_after_ms("later"), None);
    }

    #[test]
    fn parse_retry_after_empty() {
        assert_eq!(parse_retry_after_ms("  "), None);
    }

    #[test]
    fn parse_retry_after_zero() {
        assert_eq!(parse_retry_after_ms("0"), Some(0));
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

    fn test_message() -> SendMessage {
        SendMessage::new("hello", "zeroclaw_user")
    }

    #[tokio::test]
    async fn send_happy_path_returns_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        let ch = make_channel_to(&format!("{}/cb", mock.uri()));
        ch.send(&test_message()).await.unwrap();
    }

    #[tokio::test]
    async fn send_retries_on_5xx_then_succeeds() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        let ch = make_channel_to(&format!("{}/cb", mock.uri()));
        ch.send(&test_message()).await.unwrap();
    }

    #[tokio::test]
    async fn send_does_not_retry_on_4xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1) // exactly one call — must not retry
            .mount(&mock)
            .await;

        let ch = make_channel_to(&format!("{}/cb", mock.uri()));
        let err = ch.send(&test_message()).await.unwrap_err();
        assert!(err.to_string().contains("400"));
    }

    #[tokio::test]
    async fn send_retries_on_429_then_exhausts() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(429))
            .expect(3) // max_retries=2 → 3 total attempts
            .mount(&mock)
            .await;

        let ch = make_channel_to(&format!("{}/cb", mock.uri()));
        let err = ch.send(&test_message()).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("3 attempt"),
            "expected attempt count in error: {msg}"
        );
    }

    #[tokio::test]
    async fn send_honors_retry_after_header() {
        use std::time::Instant;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        // Use a channel whose retry_max_delay_ms is high enough to let us actually
        // wait the full Retry-After (cap at 2s so we honor the 1s instruction).
        let ch = WebhookChannel::new(
            8080,
            None,
            Some(format!("{}/cb", mock.uri())),
            None,
            None,
            None,
            Some(2),
            Some(10),
            Some(2_000),
        );

        let start = Instant::now();
        ch.send(&test_message()).await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "expected to wait ~1s for Retry-After, elapsed = {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn send_honors_retry_after_on_503() {
        use std::time::Instant;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", "1"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&mock)
            .await;

        let ch = WebhookChannel::new(
            8080,
            None,
            Some(format!("{}/cb", mock.uri())),
            None,
            None,
            None,
            Some(2),
            Some(10),
            Some(2_000),
        );

        let start = Instant::now();
        ch.send(&test_message()).await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "expected to wait ~1s for Retry-After on 503, elapsed = {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn send_max_retries_zero_disables_retry() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cb"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1) // only one attempt when max_retries=0
            .mount(&mock)
            .await;

        let ch = WebhookChannel::new(
            8080,
            None,
            Some(format!("{}/cb", mock.uri())),
            None,
            None,
            None,
            Some(0),
            Some(10),
            Some(100),
        );
        assert!(ch.send(&test_message()).await.is_err());
    }
}
