//! Plivo SMS channel — sends via Plivo's REST `Message` resource and receives
//! via gateway-routed webhooks.
//!
//! Mirrors the gateway-registered pattern used by WhatsApp Cloud, Linq, WATI,
//! Nextcloud Talk, and Twilio: this channel's `listen()` is a keep-alive
//! no-op; the `zeroclaw-gateway` crate hosts a hardcoded `POST /plivo/sms`
//! route whose handler reads `application/x-www-form-urlencoded` payloads,
//! validates the `X-Plivo-Signature-V3` header, and converts each request
//! into a `ChannelMessage`.
//!
//! # Auth
//! Auth ID + Auth Token. Outbound calls use HTTP Basic with the Auth ID as
//! username and the Auth Token as password. Inbound webhooks are
//! authenticated by recomputing Plivo's HMAC-SHA256 over the full request URL
//! concatenated with the nonce and the raw request body, and base64-comparing
//! against `X-Plivo-Signature-V3`. The same Auth Token keys both flows.
//!
//! # Outbound
//! `POST https://api.plivo.com/v1/Account/{auth_id}/Message/` (note the
//! trailing slash — Plivo requires it) with a JSON body of
//! `{"src": from_number, "dst": to_number, "text": body}`. Plivo segments
//! long messages transparently up to 1600 characters; bodies above that are
//! split into ≤1600-char chunks at sentence/word boundaries with a `(i/N)`
//! continuation marker.
//!
//! # Inbound
//! The gateway handler verifies the V3 signature, drops senders not on the
//! `allowed_numbers` allowlist, and forwards each message to the agent loop.
//!
//! See <https://www.plivo.com/docs/sms/concepts/incoming-sms-message/v3-signature/>.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use std::collections::BTreeMap;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const PLIVO_API_BASE: &str = "https://api.plivo.com/v1";
/// Plivo segments outbound messages transparently. The ceiling for a single
/// API call is 1600 characters; longer bodies must be split.
const PLIVO_MESSAGE_LIMIT: usize = 1600;
/// Hardcoded inbound webhook path on the gateway. Operators point Plivo's
/// "Message URL" setting (in the application configuration) at
/// `https://{gateway}/plivo/sms`.
pub const PLIVO_WEBHOOK_PATH: &str = "/plivo/sms";

/// Plivo SMS channel — see module docs.
pub struct PlivoChannel {
    account_id: String,
    auth_token: String,
    from_number: String,
    allowed_numbers: Vec<String>,
}

impl PlivoChannel {
    pub fn new(
        account_id: String,
        auth_token: String,
        from_number: String,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            account_id,
            auth_token,
            from_number,
            allowed_numbers,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.plivo")
    }

    fn outbound_url(&self) -> String {
        // Trailing slash is mandatory — Plivo returns 404 without it.
        format!("{PLIVO_API_BASE}/Account/{}/Message/", self.account_id)
    }

    /// Configurable base URL hook used by tests. The default path matches
    /// Plivo's production endpoint; tests substitute a wiremock URI by
    /// constructing the channel and overriding via `with_outbound_base`.
    #[cfg(test)]
    fn outbound_url_with_base(&self, base: &str) -> String {
        format!("{base}/v1/Account/{}/Message/", self.account_id)
    }

    /// Whether the inbound `From` number is permitted. Empty allowlist denies
    /// everyone; `"*"` matches anyone; otherwise an exact case-insensitive
    /// E.164 match is required (whitespace stripped).
    pub fn is_number_allowed(&self, phone: &str) -> bool {
        is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Verify a Plivo V3 webhook signature against the request URL, nonce,
    /// and raw body bytes. Plivo's algorithm:
    ///
    /// 1. Concatenate the full URL (the URL the webhook was sent to,
    ///    including scheme/host/path/query), the nonce header value, and the
    ///    raw request body bytes — in that order, as a contiguous byte
    ///    stream.
    /// 2. Compute HMAC-SHA256 of the resulting bytes using the Auth Token as
    ///    the key.
    /// 3. Base64-encode the digest and compare against the
    ///    `X-Plivo-Signature-V3` header (constant-time).
    ///
    /// See <https://www.plivo.com/docs/sms/concepts/incoming-sms-message/v3-signature/>.
    pub fn verify_signature(
        &self,
        full_url: &str,
        nonce: &str,
        raw_body: &[u8],
        header_signature: &str,
    ) -> bool {
        verify_plivo_v3_signature(
            &self.auth_token,
            full_url,
            nonce,
            raw_body,
            header_signature,
        )
    }

    /// Convert an inbound Plivo webhook payload into a `ChannelMessage`.
    /// Returns `None` when the sender isn't on the allowlist, the body is
    /// empty, or the payload is missing required fields.
    pub fn parse_webhook_payload(
        &self,
        form_params: &BTreeMap<String, String>,
    ) -> Option<ChannelMessage> {
        let from = form_params.get("From")?.trim();
        if from.is_empty() {
            return None;
        }
        if !self.is_number_allowed(from) {
            tracing::debug!("Plivo: dropping SMS from {from} (not in allowed_numbers)");
            return None;
        }
        let body = form_params
            .get("Text")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if body.is_empty() {
            tracing::debug!("Plivo: dropping empty-body SMS from {from}");
            return None;
        }
        let message_uuid = form_params
            .get("MessageUUID")
            .cloned()
            .unwrap_or_else(|| format!("plivo-{}", chrono::Utc::now().timestamp_millis()));
        Some(ChannelMessage {
            id: format!("plivo_{message_uuid}"),
            sender: from.to_string(),
            reply_target: from.to_string(),
            content: body,
            channel: "plivo".to_string(),
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
            "src": self.from_number,
            "dst": to,
            "text": body,
        });
        let resp = self
            .http_client()
            .post(&url)
            .basic_auth(&self.account_id, Some(&self.auth_token))
            .json(&payload)
            .send()
            .await
            .context("Plivo Message POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Plivo Message POST returned {status}: {body}");
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for PlivoChannel {
    fn name(&self) -> &str {
        "plivo"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let to = message.recipient.trim();
        if to.is_empty() {
            bail!("Plivo send: empty recipient");
        }
        let chunks = chunk_sms(&message.content, PLIVO_MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        for chunk in chunks {
            self.post_message_chunk(to, &chunk, None).await?;
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Plivo uses webhooks (push-based), not polling. The gateway hosts
        // POST /plivo/sms and routes inbound payloads via parse_webhook_payload.
        tracing::info!(
            "Plivo channel active (webhook mode). Configure your Plivo \
             application's \"Message URL\" to POST to your gateway's \
             {PLIVO_WEBHOOK_PATH} endpoint."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Probe the Account resource — succeeds when Auth ID + Token are valid.
        let url = format!("{PLIVO_API_BASE}/Account/{}/", self.account_id);
        self.http_client()
            .get(&url)
            .basic_auth(&self.account_id, Some(&self.auth_token))
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

fn verify_plivo_v3_signature(
    auth_token: &str,
    full_url: &str,
    nonce: &str,
    raw_body: &[u8],
    header_signature: &str,
) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    if header_signature.is_empty() {
        return false;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(auth_token.as_bytes()) else {
        return false;
    };
    // V3 signs the byte stream URL || nonce || body — no separator, no sort,
    // no form-key concatenation. Body is the raw request bytes.
    mac.update(full_url.as_bytes());
    mac.update(nonce.as_bytes());
    mac.update(raw_body);
    let computed = mac.finalize().into_bytes();
    let expected_b64 = base64_encode(&computed);
    constant_time_eq(expected_b64.as_bytes(), header_signature.as_bytes())
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

    fn ch() -> PlivoChannel {
        PlivoChannel::new(
            "MAtestauthid".into(),
            "test-auth-token".into(),
            "+15555550100".into(),
            vec!["+15555550199".into()],
        )
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
        let chunks = chunk_sms("hi there", PLIVO_MESSAGE_LIMIT);
        assert_eq!(chunks, vec!["hi there"]);
    }

    #[test]
    fn chunk_sms_long_is_split_with_marker() {
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
    }

    #[test]
    fn chunk_sms_empty_returns_no_chunks() {
        let chunks = chunk_sms("   ", PLIVO_MESSAGE_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_sms_preserves_total_content() {
        // Total non-marker content reassembled from chunks should match
        // the original body (minus separators between chunks).
        let body = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let body = body.trim();
        let chunks = chunk_sms(body, 200);
        assert!(chunks.len() >= 2);
        let stripped: String = chunks
            .iter()
            .map(|c| {
                // Strip leading "(i/N) " marker
                if let Some(close) = c.find(") ") {
                    c[close + 2..].to_string()
                } else {
                    c.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        // Whitespace handling means we can't byte-compare, but the char
        // counts should be near-equal (allowing for trim differences at
        // chunk boundaries).
        assert!(
            stripped.chars().count() >= body.chars().count() - chunks.len(),
            "lost content: original={}, reassembled={}",
            body.chars().count(),
            stripped.chars().count()
        );
    }

    #[test]
    fn parse_webhook_drops_outside_allowlist() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550999".into());
        params.insert("Text".into(), "hello".into());
        params.insert("MessageUUID".into(), "uuid-abc".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_body() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550199".into());
        params.insert("Text".into(), "  ".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_sender() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "".into());
        params.insert("Text".into(), "hello".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_returns_message_on_allowed() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550199".into());
        params.insert("Text".into(), "hello world".into());
        params.insert("MessageUUID".into(), "uuid-123".into());
        let msg = ch()
            .parse_webhook_payload(&params)
            .expect("expected message");
        assert_eq!(msg.sender, "+15555550199");
        assert_eq!(msg.reply_target, "+15555550199");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel, "plivo");
        assert_eq!(msg.id, "plivo_uuid-123");
    }

    /// Round-trip the production verifier: build a signature using the same
    /// HMAC-SHA256 over (URL || nonce || body) flow we ship, base64-encode
    /// it, and assert `verify_plivo_v3_signature` accepts it.
    #[test]
    fn verify_signature_round_trips_self_signed() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let token = "plivo-test-token";
        let url = "https://gateway.example.com/plivo/sms";
        let nonce = "abc123nonce";
        let body = b"From=%2B15555550199&Text=hello&MessageUUID=uuid-1&Type=sms";
        let mut mac =
            Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("valid HMAC key length");
        mac.update(url.as_bytes());
        mac.update(nonce.as_bytes());
        mac.update(body);
        let digest = mac.finalize().into_bytes();
        let expected = base64_encode(&digest);
        assert!(
            verify_plivo_v3_signature(token, url, nonce, body, &expected),
            "self-signed signature should round-trip"
        );
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let token = "plivo-test-token";
        let url = "https://gateway.example.com/plivo/sms";
        let nonce = "abc123nonce";
        let body = b"From=%2B15555550199&Text=hello&MessageUUID=uuid-1&Type=sms";
        let mut mac =
            Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("valid HMAC key length");
        mac.update(url.as_bytes());
        mac.update(nonce.as_bytes());
        mac.update(body);
        let expected = base64_encode(&mac.finalize().into_bytes());
        let tampered = b"From=%2B15555550199&Text=goodbye&MessageUUID=uuid-1&Type=sms";
        assert!(!verify_plivo_v3_signature(
            token, url, nonce, tampered, &expected
        ));
    }

    #[test]
    fn verify_signature_rejects_wrong_token() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let url = "https://gateway.example.com/plivo/sms";
        let nonce = "abc123nonce";
        let body = b"hello";
        let mut mac = Hmac::<Sha256>::new_from_slice(b"real-token").expect("valid HMAC key length");
        mac.update(url.as_bytes());
        mac.update(nonce.as_bytes());
        mac.update(body);
        let expected = base64_encode(&mac.finalize().into_bytes());
        assert!(!verify_plivo_v3_signature(
            "wrong-token",
            url,
            nonce,
            body,
            &expected
        ));
    }

    #[test]
    fn verify_signature_rejects_empty_header() {
        assert!(!verify_plivo_v3_signature(
            "token",
            "https://x",
            "nonce",
            b"body",
            ""
        ));
    }

    #[test]
    fn verify_signature_rejects_wrong_nonce() {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let token = "t";
        let url = "https://x/plivo/sms";
        let body = b"body";
        let mut mac = Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("hmac");
        mac.update(url.as_bytes());
        mac.update(b"nonce-A");
        mac.update(body);
        let expected = base64_encode(&mac.finalize().into_bytes());
        assert!(!verify_plivo_v3_signature(
            token, url, "nonce-B", body, &expected
        ));
    }

    mod http_tests {
        use super::*;
        use wiremock::matchers::{body_string_contains, header, header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn send_posts_to_message_endpoint_with_basic_auth_and_json_body() {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/v1/Account/MAtestauthid/Message/"))
                .and(header_exists("authorization"))
                .and(header("content-type", "application/json"))
                .and(body_string_contains("\"src\":\"+15555550100\""))
                .and(body_string_contains("\"dst\":\"+15555550199\""))
                .and(body_string_contains("\"text\":\"hello there\""))
                .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                    "message_uuid": ["uuid-12345"],
                    "api_id": "abc",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let plivo = ch();
            plivo
                .post_message_chunk("+15555550199", "hello there", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v1/Account/MAtestauthid/Message/"))
                .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\": \"auth\"}"))
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
