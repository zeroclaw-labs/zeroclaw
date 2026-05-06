//! Sinch SMS channel — sends via Sinch's REST `Batches` resource and receives
//! via gateway-routed webhooks.
//!
//! Mirrors the gateway-registered pattern used by WhatsApp Cloud, Linq, WATI,
//! and Twilio: this channel's `listen()` is a keep-alive no-op; the
//! `zeroclaw-gateway` crate hosts a hardcoded `POST /sinch/sms` route whose
//! handler reads `application/json` payloads, validates the
//! `x-sinch-webhook-signature` header, and converts each request into a
//! [`ChannelMessage`].
//!
//! # Auth
//! Sinch separates outbound and inbound credentials:
//!
//! - **Outbound** uses a service plan ID (public, identifies the project) and
//!   a Bearer API token. The URL is region-scoped:
//!   `https://{region}.sms.api.sinch.com/xms/v1/{service_plan_id}/batches`.
//! - **Inbound webhooks** are signed with a separate `callback_secret`. The
//!   `x-sinch-webhook-signature` header has the format `v1,{nonce},{base64-sig}`
//!   where the signature is HMAC-SHA256 over `nonce_bytes || raw_body` keyed by
//!   `callback_secret`.
//!
//! # Outbound
//! `POST https://{region}.sms.api.sinch.com/xms/v1/{service_plan_id}/batches`
//! with JSON body `{"from": from_number, "to": [to_number], "body": body}`.
//! Long bodies are split into ≤1600-char chunks at sentence/word boundaries
//! with a `(i/N)` continuation marker so recipients can reassemble.
//!
//! # Inbound
//! The gateway handler verifies the signature, drops senders not on the
//! `allowed_numbers` allowlist, and forwards each `mo_text` payload to the
//! agent loop.
//!
//! Reference: <https://developers.sinch.com/docs/sms/api-reference/sms/tag/Batches/>

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// Sinch segments outbound messages transparently. Mirroring Twilio's
/// behaviour: bodies above 1600 characters are split client-side into
/// numbered chunks so a single API call never carries a giant payload.
const SINCH_MESSAGE_LIMIT: usize = 1600;
/// Hardcoded inbound webhook path on the gateway. Operators point Sinch's
/// "Callback URL" at `https://{gateway}/sinch/sms`.
pub const SINCH_WEBHOOK_PATH: &str = "/sinch/sms";

/// Sinch SMS channel — see module docs.
pub struct SinchChannel {
    service_plan_id: String,
    api_token: String,
    region: String,
    from_number: String,
    allowed_numbers: Vec<String>,
    callback_secret: String,
}

impl SinchChannel {
    pub fn new(
        service_plan_id: String,
        api_token: String,
        region: String,
        from_number: String,
        allowed_numbers: Vec<String>,
        callback_secret: String,
    ) -> Self {
        Self {
            service_plan_id,
            api_token,
            region,
            from_number,
            allowed_numbers,
            callback_secret,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.sinch")
    }

    fn outbound_base(&self) -> String {
        format!("https://{}.sms.api.sinch.com", self.region)
    }

    fn outbound_url(&self) -> String {
        format!(
            "{}/xms/v1/{}/batches",
            self.outbound_base(),
            self.service_plan_id
        )
    }

    /// Configurable base URL hook used by tests. Production uses the
    /// region-scoped Sinch host; tests substitute a wiremock URI.
    #[cfg(test)]
    fn outbound_url_with_base(&self, base: &str) -> String {
        format!("{base}/xms/v1/{}/batches", self.service_plan_id)
    }

    /// Whether the inbound `from` number is permitted. Empty allowlist denies
    /// everyone; `"*"` matches anyone; otherwise an exact case-insensitive
    /// E.164 match is required.
    pub fn is_number_allowed(&self, phone: &str) -> bool {
        is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Verify a Sinch webhook signature. Sinch's algorithm:
    ///
    /// 1. The `x-sinch-webhook-signature` header has three comma-separated
    ///    parts: `v1,{nonce},{base64-sig}`. Reject anything with a different
    ///    version prefix or fewer parts.
    /// 2. Compute HMAC-SHA256 over `nonce_bytes || raw_body` (concatenated
    ///    bytes) using the `callback_secret` as the key.
    /// 3. Base64-encode the digest and constant-time-compare against the
    ///    third header part.
    pub fn verify_signature(&self, raw_body: &[u8], header_value: &str) -> bool {
        verify_sinch_signature(&self.callback_secret, raw_body, header_value)
    }

    /// Convert an inbound Sinch webhook payload into a [`ChannelMessage`].
    /// Returns `None` when the payload isn't a `mo_text` message, the sender
    /// isn't on the allowlist, or required fields are missing/empty.
    pub fn parse_webhook_payload(&self, json: &serde_json::Value) -> Option<ChannelMessage> {
        let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "mo_text" {
            tracing::debug!("Sinch: dropping webhook payload of type {msg_type:?}");
            return None;
        }
        let from = json
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if from.is_empty() {
            return None;
        }
        if !self.is_number_allowed(from) {
            tracing::debug!("Sinch: dropping SMS from {from} (not in allowed_numbers)");
            return None;
        }
        let body = json
            .get("body")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if body.is_empty() {
            tracing::debug!("Sinch: dropping empty-body SMS from {from}");
            return None;
        }
        let id = json
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("sinch-{}", chrono::Utc::now().timestamp_millis()));
        Some(ChannelMessage {
            id: format!("sinch_{id}"),
            sender: from.to_string(),
            reply_target: from.to_string(),
            content: body,
            channel: "sinch".to_string(),
            timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        })
    }

    async fn post_message_chunk(
        &self,
        to: &str,
        body: &str,
        base_override: Option<&str>,
    ) -> Result<()> {
        let url = match base_override {
            #[cfg(test)]
            Some(b) => self.outbound_url_with_base(b),
            #[cfg(not(test))]
            Some(_) => self.outbound_url(),
            None => self.outbound_url(),
        };
        let payload = serde_json::json!({
            "from": self.from_number,
            "to": [to],
            "body": body,
        });
        let resp = self
            .http_client()
            .post(&url)
            .bearer_auth(&self.api_token)
            .json(&payload)
            .send()
            .await
            .context("Sinch Batches POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Sinch Batches POST returned {status}: {body}");
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for SinchChannel {
    fn name(&self) -> &str {
        "sinch"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let to = message.recipient.trim();
        if to.is_empty() {
            bail!("Sinch send: empty recipient");
        }
        let chunks = chunk_sms(&message.content, SINCH_MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        for chunk in chunks {
            self.post_message_chunk(to, &chunk, None).await?;
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Sinch uses webhooks (push-based), not polling. The gateway hosts
        // POST /sinch/sms and routes inbound payloads via parse_webhook_payload.
        tracing::info!(
            "Sinch channel active (webhook mode). Configure Sinch's Callback URL to POST \
             to your gateway's {SINCH_WEBHOOK_PATH} endpoint."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Probe the Batches list — succeeds when service_plan_id + token
        // are valid. `page_size=1` keeps the response trivial.
        let url = format!(
            "{}/xms/v1/{}/batches?page_size=1",
            self.outbound_base(),
            self.service_plan_id
        );
        self.http_client()
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

/// Allowlist matcher with `"*"` wildcard. Comparison is case-insensitive and
/// strips internal whitespace so `"+1 555 555 0199"` round-trips against the
/// canonical E.164 `"+15555550199"`.
pub fn is_number_allowed_for_list(allowlist: &[String], phone: &str) -> bool {
    if allowlist.is_empty() {
        return false;
    }
    if allowlist.iter().any(|n| n == "*") {
        return true;
    }
    let normalized = phone
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    allowlist.iter().any(|entry| {
        let canon = entry
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        canon == normalized
    })
}

fn verify_sinch_signature(callback_secret: &str, raw_body: &[u8], header_value: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    if header_value.is_empty() {
        return false;
    }
    let parts: Vec<&str> = header_value.splitn(3, ',').collect();
    if parts.len() != 3 {
        return false;
    }
    let (version, nonce, expected_sig) = (parts[0], parts[1], parts[2]);
    if version != "v1" {
        return false;
    }
    if nonce.is_empty() {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(callback_secret.as_bytes()) else {
        return false;
    };
    mac.update(nonce.as_bytes());
    mac.update(raw_body);
    let computed = mac.finalize().into_bytes();
    let expected_b64 = base64_encode(&computed);
    constant_time_eq(expected_b64.as_bytes(), expected_sig.as_bytes())
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Split an outbound body into ≤`limit`-character chunks. Single-chunk bodies
/// are returned as-is; multi-chunk bodies receive a `(i/N) ` prefix on each
/// part so the recipient can reassemble. Splits prefer sentence enders, then
/// whitespace, then a hard char cut.
pub fn chunk_sms(body: &str, limit: usize) -> Vec<String> {
    let body = body.trim();
    if body.is_empty() {
        return vec![];
    }
    if body.chars().count() <= limit {
        return vec![body.to_string()];
    }
    const MARKER_RESERVE: usize = 8; // "(99/99) "
    let body_budget = limit.saturating_sub(MARKER_RESERVE).max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut remaining: &str = body;
    while !remaining.is_empty() {
        if remaining.chars().count() <= body_budget {
            chunks.push(remaining.to_string());
            break;
        }
        let split_at = pick_split_point(remaining, body_budget);
        let (head, tail) = remaining.split_at(split_at);
        chunks.push(head.trim_end().to_string());
        remaining = tail.trim_start();
    }
    if chunks.len() == 1 {
        return chunks;
    }
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| format!("({}/{total}) {c}", i + 1))
        .collect()
}

fn pick_split_point(text: &str, char_budget: usize) -> usize {
    let mut budget_idx = text.len();
    for (i, (byte_idx, _)) in text.char_indices().enumerate() {
        if i == char_budget {
            budget_idx = byte_idx;
            break;
        }
    }
    let head = &text[..budget_idx];
    if let Some(idx) = head.rfind(['.', '!', '?', '\n']) {
        return (idx + 1).min(budget_idx);
    }
    if let Some(idx) = head.rfind(char::is_whitespace) {
        return idx + 1;
    }
    budget_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch() -> SinchChannel {
        SinchChannel::new(
            "test-service-plan".into(),
            "test-api-token".into(),
            "us".into(),
            "+15555550100".into(),
            vec!["+15555550199".into()],
            "test-callback-secret".into(),
        )
    }

    /// Compute the signature header a real Sinch webhook would include for
    /// the given (secret, nonce, body) — used to round-trip the verifier.
    fn make_signature_header(secret: &str, nonce: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac =
            Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length is valid");
        mac.update(nonce.as_bytes());
        mac.update(body);
        let computed = mac.finalize().into_bytes();
        let sig = base64_encode(&computed);
        format!("v1,{nonce},{sig}")
    }

    #[test]
    fn allowlist_empty_denies_everyone() {
        assert!(!is_number_allowed_for_list(&[], "+15555550199"));
    }

    #[test]
    fn allowlist_wildcard_allows_anyone() {
        let allow = vec!["*".into()];
        assert!(is_number_allowed_for_list(&allow, "+15555550199"));
        assert!(is_number_allowed_for_list(&allow, "+447700900000"));
    }

    #[test]
    fn allowlist_matches_e164_exact() {
        let allow = vec!["+15555550199".into()];
        assert!(is_number_allowed_for_list(&allow, "+15555550199"));
        assert!(!is_number_allowed_for_list(&allow, "+15555550100"));
    }

    #[test]
    fn allowlist_strips_whitespace() {
        let allow = vec!["+1 555 555 0199".into()];
        assert!(is_number_allowed_for_list(&allow, "+15555550199"));
    }

    #[test]
    fn chunk_sms_short_passes_through() {
        let chunks = chunk_sms("hi there", SINCH_MESSAGE_LIMIT);
        assert_eq!(chunks, vec!["hi there"]);
    }

    #[test]
    fn chunk_sms_long_is_split_with_marker_and_preserves_content() {
        let body = "alpha beta gamma. ".repeat(120);
        let chunks = chunk_sms(&body, 100);
        assert!(chunks.len() >= 2, "expected ≥2 chunks, got {chunks:?}");
        for (i, c) in chunks.iter().enumerate() {
            assert!(
                c.starts_with(&format!("({}/", i + 1)),
                "missing marker on chunk {i}: {c:?}"
            );
            assert!(
                c.chars().count() <= 100,
                "chunk {i} exceeds limit: {c:?} ({} chars)",
                c.chars().count()
            );
        }
        // Total reassembled content (after stripping markers) covers every
        // original word — markers reserve space, never drop characters.
        let mut joined = String::new();
        for c in &chunks {
            // Strip "(i/N) " prefix.
            let stripped = c.split_once(' ').map(|(_, rest)| rest).unwrap_or(c);
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(stripped);
        }
        let original_words: usize = body.split_whitespace().count();
        let joined_words: usize = joined.split_whitespace().count();
        assert_eq!(
            joined_words, original_words,
            "chunked body lost or duplicated words"
        );
    }

    #[test]
    fn chunk_sms_empty_returns_no_chunks() {
        let chunks = chunk_sms("   ", SINCH_MESSAGE_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_webhook_drops_outside_allowlist() {
        let payload = serde_json::json!({
            "type": "mo_text",
            "from": "+15555550999",
            "to": "+15555550100",
            "body": "hello",
            "id": "abc123",
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_body() {
        let payload = serde_json::json!({
            "type": "mo_text",
            "from": "+15555550199",
            "to": "+15555550100",
            "body": "  ",
            "id": "abc123",
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_drops_non_mo_text_type() {
        let payload = serde_json::json!({
            "type": "delivery_report_sms",
            "from": "+15555550199",
            "to": "+15555550100",
            "body": "hello",
            "id": "abc123",
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_returns_message_on_allowed_mo_text() {
        let payload = serde_json::json!({
            "type": "mo_text",
            "from": "+15555550199",
            "to": "+15555550100",
            "body": "hello world",
            "id": "01HXXXMSGID",
        });
        let msg = ch()
            .parse_webhook_payload(&payload)
            .expect("expected message");
        assert_eq!(msg.sender, "+15555550199");
        assert_eq!(msg.reply_target, "+15555550199");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel, "sinch");
        assert_eq!(msg.id, "sinch_01HXXXMSGID");
    }

    #[test]
    fn verify_signature_round_trip() {
        let secret = "test-callback-secret";
        let body = b"{\"type\":\"mo_text\",\"from\":\"+15555550199\"}";
        let header = make_signature_header(secret, "nonce-xyz-1", body);
        assert!(verify_sinch_signature(secret, body, &header));
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let secret = "test-callback-secret";
        let body = b"{\"type\":\"mo_text\",\"from\":\"+15555550199\"}";
        let header = make_signature_header(secret, "nonce-xyz-1", body);
        let tampered = b"{\"type\":\"mo_text\",\"from\":\"+15555559999\"}";
        assert!(!verify_sinch_signature(secret, tampered, &header));
    }

    #[test]
    fn verify_signature_rejects_wrong_secret() {
        let body = b"hello";
        let header = make_signature_header("real-secret", "nonce-1", body);
        assert!(!verify_sinch_signature("wrong-secret", body, &header));
    }

    #[test]
    fn verify_signature_rejects_wrong_version_prefix() {
        let secret = "test-callback-secret";
        let body = b"hello";
        // Compute a v1 header but mutate the prefix to v0.
        let valid = make_signature_header(secret, "nonce-1", body);
        let mut parts = valid.splitn(3, ',');
        let _ = parts.next();
        let nonce = parts.next().unwrap_or("");
        let sig = parts.next().unwrap_or("");
        let mutated = format!("v0,{nonce},{sig}");
        assert!(!verify_sinch_signature(secret, body, &mutated));
    }

    #[test]
    fn verify_signature_rejects_missing_nonce() {
        let secret = "test-callback-secret";
        let body = b"hello";
        // Header with empty nonce slot.
        let header = "v1,,abcd1234";
        assert!(!verify_sinch_signature(secret, body, header));
    }

    #[test]
    fn verify_signature_rejects_empty_header() {
        assert!(!verify_sinch_signature("secret", b"body", ""));
    }

    #[test]
    fn verify_signature_rejects_too_few_parts() {
        let secret = "test-callback-secret";
        // Only two comma-separated parts.
        assert!(!verify_sinch_signature(secret, b"body", "v1,nonce"));
        // No commas at all.
        assert!(!verify_sinch_signature(secret, b"body", "v1noncesig"));
    }

    mod http_tests {
        use super::*;
        use wiremock::matchers::{body_json_string, header, header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn send_posts_to_batches_endpoint_with_bearer_and_array_to() {
            let server = MockServer::start().await;

            let expected_body = serde_json::json!({
                "from": "+15555550100",
                "to": ["+15555550199"],
                "body": "hello there",
            });

            Mock::given(method("POST"))
                .and(path("/xms/v1/test-service-plan/batches"))
                .and(header_exists("authorization"))
                .and(header("content-type", "application/json"))
                .and(body_json_string(expected_body.to_string()))
                .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "id": "01HXX-batch-id",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let sinch = ch();
            sinch
                .post_message_chunk("+15555550199", "hello there", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/xms/v1/test-service-plan/batches"))
                .respond_with(
                    ResponseTemplate::new(401).set_body_string("{\"code\": \"unauthorized\"}"),
                )
                .mount(&server)
                .await;

            let err = ch()
                .post_message_chunk("+15555550199", "hi", Some(&server.uri()))
                .await
                .expect_err("expected error");
            let msg = format!("{err:#}");
            assert!(msg.contains("401"), "missing 401 in error: {msg}");
        }
    }
}
