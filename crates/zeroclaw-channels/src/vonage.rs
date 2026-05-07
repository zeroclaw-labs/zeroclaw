//! Vonage (Nexmo) SMS channel — sends via Vonage's legacy SMS REST API and
//! receives via gateway-routed webhooks.
//!
//! Mirrors the gateway-registered pattern used by Twilio, Plivo, Sinch, and
//! Telnyx: the channel's `listen()` is a keep-alive no-op, and the gateway
//! crate hosts a hardcoded `POST /vonage/sms` route whose handler reads
//! `application/x-www-form-urlencoded` payloads, validates the `sig`
//! parameter (HMAC-SHA256 over alphabetically-sorted params), and converts
//! each request into a `ChannelMessage`.
//!
//! # Auth
//! Two distinct credentials, both configured per Vonage account:
//!
//! 1. `api_secret` — the legacy SMS API password, sent in the outbound POST
//!    body alongside `api_key`. Vonage's `/sms/json` endpoint takes
//!    credentials in the form body (not headers).
//! 2. `signature_secret` — the inbound-webhook HMAC key, configured
//!    separately in the Vonage dashboard's "API settings → Signed messages
//!    → Signature secret" with algorithm "HMAC SHA-256". Mixing this up
//!    with `api_secret` is a common operator footgun, so the docs call it
//!    out explicitly.
//!
//! # Outbound
//! `POST https://rest.nexmo.com/sms/json` with form fields `api_key`,
//! `api_secret`, `from`, `to`, `text`. Bodies over 1600 characters are
//! split at sentence/word boundaries with `(i/N) ` continuation markers.
//!
//! # Inbound
//! The gateway handler recomputes Vonage's signature algorithm and rejects
//! requests where the computed value doesn't match the `sig` parameter:
//!
//! 1. Pop the `sig` parameter from the form body.
//! 2. Sort the remaining parameters alphabetically by key.
//! 3. Concatenate as `&{key}={value}` for each pair (note the leading `&`).
//! 4. Append the `signature_secret` literal at the end.
//! 5. HMAC-SHA256 keyed by `signature_secret`, hex-encoded (lowercase).
//! 6. Constant-time compare against `sig`. Mismatch → drop with 401.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use std::collections::BTreeMap;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const VONAGE_API_BASE: &str = "https://rest.nexmo.com";
/// Vonage's legacy SMS API auto-segments long messages, but bodies above
/// 1600 chars in a single call get awkward; we split here for parity with
/// the other SMS-gateway channels.
const VONAGE_MESSAGE_LIMIT: usize = 1600;
/// Hardcoded inbound webhook path on the gateway. Operators point Vonage's
/// "Inbound SMS Webhook" at `https://{gateway}/vonage/sms` (POST).
pub const VONAGE_WEBHOOK_PATH: &str = "/vonage/sms";

/// Vonage SMS channel — see module docs.
pub struct VonageChannel {
    api_key: String,
    api_secret: String,
    from_number_or_sender_id: String,
    allowed_numbers: Vec<String>,
    signature_secret: String,
}

impl VonageChannel {
    pub fn new(
        api_key: String,
        api_secret: String,
        from_number_or_sender_id: String,
        allowed_numbers: Vec<String>,
        signature_secret: String,
    ) -> Self {
        Self {
            api_key,
            api_secret,
            from_number_or_sender_id,
            allowed_numbers,
            signature_secret,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.vonage")
    }

    fn outbound_url(&self) -> String {
        format!("{VONAGE_API_BASE}/sms/json")
    }

    /// Configurable base URL hook used by tests. The default points at
    /// Vonage's production endpoint; tests substitute a wiremock URI.
    #[cfg(test)]
    fn outbound_url_with_base(&self, base: &str) -> String {
        format!("{base}/sms/json")
    }

    /// Whether the inbound `msisdn` is permitted. Empty allowlist denies
    /// everyone; `"*"` matches anyone; otherwise an exact case-insensitive
    /// E.164 match is required.
    pub fn is_number_allowed(&self, phone: &str) -> bool {
        is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Verify a Vonage inbound-webhook signature. The caller must pass the
    /// already-parsed form parameters as a `BTreeMap` so the iteration
    /// order matches Vonage's "alphabetically by key" requirement.
    ///
    /// Algorithm — see <https://developer.vonage.com/en/messaging/sms/concepts/signed-messages>:
    ///
    /// 1. Pop `sig` from `form_params` (it's not part of the canonical string).
    /// 2. Sort remaining keys alphabetically. (BTreeMap iteration handles this.)
    /// 3. Build the canonical string by concatenating `&{k}={v}` for each pair.
    /// 4. Compute `HMAC-SHA256(canonical, signature_secret)`, lowercase hex.
    /// 5. Constant-time compare against the popped `sig` value.
    pub fn verify_signature(&self, form_params: &BTreeMap<String, String>) -> bool {
        verify_vonage_signature(&self.signature_secret, form_params)
    }

    /// Convert an inbound Vonage webhook payload into a `ChannelMessage`.
    /// Returns `None` when the sender isn't on the allowlist, the body is
    /// empty, or required fields are missing.
    pub fn parse_webhook_payload(
        &self,
        form_params: &BTreeMap<String, String>,
    ) -> Option<ChannelMessage> {
        let from = form_params.get("msisdn")?.trim();
        if from.is_empty() {
            return None;
        }
        if !self.is_number_allowed(from) {
            tracing::debug!("Vonage: dropping SMS from {from} (not in allowed_numbers)");
            return None;
        }
        let body = form_params
            .get("text")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if body.is_empty() {
            tracing::debug!("Vonage: dropping empty-body SMS from {from}");
            return None;
        }
        let message_id = form_params
            .get("messageId")
            .cloned()
            .unwrap_or_else(|| format!("vonage-{}", chrono::Utc::now().timestamp_millis()));
        Some(ChannelMessage {
            id: format!("vonage_{message_id}"),
            sender: from.to_string(),
            reply_target: from.to_string(),
            content: body,
            channel: "vonage".to_string(),
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
        let resp = self
            .http_client()
            .post(&url)
            .form(&[
                ("api_key", self.api_key.as_str()),
                ("api_secret", self.api_secret.as_str()),
                ("from", self.from_number_or_sender_id.as_str()),
                ("to", to),
                ("text", body),
            ])
            .send()
            .await
            .context("Vonage /sms/json POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Vonage /sms/json POST returned {status}: {body}");
        }
        // Vonage returns 200 even when the message itself was rejected;
        // the per-message status lives in a JSON body. Surface that as an
        // error so the caller sees it.
        let payload: VonageSmsResponse = resp
            .json()
            .await
            .context("Vonage /sms/json returned non-JSON body")?;
        for message in &payload.messages {
            // status="0" means OK; anything else is an error
            // (https://developer.vonage.com/en/api/sms#sms-response-codes).
            if message.status != "0" {
                bail!(
                    "Vonage SMS rejected: status={} error_text={}",
                    message.status,
                    message.error_text.as_deref().unwrap_or("(none)"),
                );
            }
        }
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct VonageSmsResponse {
    #[serde(default)]
    messages: Vec<VonageSmsMessage>,
}

#[derive(serde::Deserialize)]
struct VonageSmsMessage {
    /// `"0"` = success, anything else is an error code (string-typed in the
    /// JSON, not numeric).
    status: String,
    #[serde(default, rename = "error-text")]
    error_text: Option<String>,
}

#[async_trait]
impl Channel for VonageChannel {
    fn name(&self) -> &str {
        "vonage"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let to = message.recipient.trim();
        if to.is_empty() {
            bail!("Vonage send: empty recipient");
        }
        let chunks = chunk_sms(&message.content, VONAGE_MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        for chunk in chunks {
            self.post_message_chunk(to, &chunk, None).await?;
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Vonage uses webhooks (push-based), not polling. The gateway hosts
        // POST /vonage/sms and routes inbound payloads via parse_webhook_payload.
        tracing::info!(
            "Vonage channel active (webhook mode). Configure Vonage's \
             \"Inbound SMS Webhook\" to POST to your gateway's {VONAGE_WEBHOOK_PATH} endpoint."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Probe the account-balance endpoint — succeeds when api_key +
        // api_secret are valid. (No JSON parsing needed; just status check.)
        let url = format!("{VONAGE_API_BASE}/account/get-balance");
        self.http_client()
            .get(&url)
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("api_secret", self.api_secret.as_str()),
            ])
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }
}

/// Allowlist matcher with `"*"` wildcard. Comparison is case-insensitive
/// and strips internal whitespace so `"+1 555 555 0199"` round-trips
/// against the canonical E.164 `"+15555550199"`.
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

/// Verify Vonage's `sig` webhook parameter. See `VonageChannel::verify_signature`
/// for the algorithm. Returns `false` on any of: missing `sig`, bad hex
/// decode, signature length mismatch, or HMAC mismatch.
fn verify_vonage_signature(signature_secret: &str, form_params: &BTreeMap<String, String>) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let Some(provided_sig) = form_params.get("sig") else {
        return false;
    };
    if provided_sig.is_empty() {
        return false;
    }

    // Build canonical string: alphabetically sorted &k=v pairs, excluding
    // the `sig` parameter itself, with the `signature_secret` appended.
    // BTreeMap iterates sorted by key, which matches Vonage's spec.
    let mut canonical = String::with_capacity(form_params.len() * 32);
    for (k, v) in form_params.iter().filter(|(k, _)| k.as_str() != "sig") {
        canonical.push('&');
        canonical.push_str(k);
        canonical.push('=');
        canonical.push_str(v);
    }
    canonical.push_str(signature_secret);

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(signature_secret.as_bytes()) else {
        return false;
    };
    mac.update(canonical.as_bytes());
    let computed = mac.finalize().into_bytes();
    let computed_hex = hex::encode(computed);
    constant_time_eq(computed_hex.as_bytes(), provided_sig.as_bytes())
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

/// Split an outbound body into ≤`limit`-character chunks. Single-chunk
/// bodies are returned as-is; multi-chunk bodies receive a `(i/N) ` prefix
/// on each part. Splits prefer sentence enders, then whitespace, then a
/// hard char cut.
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

    fn ch() -> VonageChannel {
        VonageChannel::new(
            "TESTKEY".into(),
            "test-secret".into(),
            "+15555550100".into(),
            vec!["+15555550199".into()],
            "sig-secret".into(),
        )
    }

    /// Compute the canonical signature for a fixture, exercising the same
    /// path the production validator uses but as the "trusted" producer
    /// side. Tests sign with this and then validate via `verify_signature`
    /// to round-trip the algorithm.
    fn compute_vonage_sig(secret: &str, params: &BTreeMap<String, String>) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut canonical = String::new();
        for (k, v) in params.iter().filter(|(k, _)| k.as_str() != "sig") {
            canonical.push('&');
            canonical.push_str(k);
            canonical.push('=');
            canonical.push_str(v);
        }
        canonical.push_str(secret);
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn signed_form(secret: &str, body: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut map: BTreeMap<String, String> = body
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let sig = compute_vonage_sig(secret, &map);
        map.insert("sig".into(), sig);
        map
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
        let chunks = chunk_sms("hi there", VONAGE_MESSAGE_LIMIT);
        assert_eq!(chunks, vec!["hi there"]);
    }

    #[test]
    fn chunk_sms_long_is_split_with_marker() {
        let body = "alpha beta gamma. ".repeat(120);
        let chunks = chunk_sms(&body, 100);
        assert!(chunks.len() >= 2);
        for (i, c) in chunks.iter().enumerate() {
            assert!(c.starts_with(&format!("({}/", i + 1)));
            assert!(c.chars().count() <= 100);
        }
    }

    #[test]
    fn chunk_sms_empty_returns_no_chunks() {
        let chunks = chunk_sms("   ", VONAGE_MESSAGE_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_webhook_drops_outside_allowlist() {
        let mut params = BTreeMap::new();
        params.insert("msisdn".into(), "+15555550999".into());
        params.insert("text".into(), "hi".into());
        params.insert("messageId".into(), "M1".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_body() {
        let mut params = BTreeMap::new();
        params.insert("msisdn".into(), "+15555550199".into());
        params.insert("text".into(), "  ".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_returns_message_on_allowed() {
        let mut params = BTreeMap::new();
        params.insert("msisdn".into(), "+15555550199".into());
        params.insert("text".into(), "hello world".into());
        params.insert("messageId".into(), "M123".into());
        let msg = ch()
            .parse_webhook_payload(&params)
            .expect("expected message");
        assert_eq!(msg.sender, "+15555550199");
        assert_eq!(msg.reply_target, "+15555550199");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel, "vonage");
        assert_eq!(msg.id, "vonage_M123");
    }

    #[test]
    fn verify_signature_round_trips_against_self_computed_value() {
        let params = signed_form(
            "sig-secret",
            &[
                ("msisdn", "+15555550199"),
                ("to", "+15555550100"),
                ("text", "hello world"),
                ("messageId", "MABC"),
                ("type", "text"),
            ],
        );
        assert!(verify_vonage_signature("sig-secret", &params));
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let mut params = signed_form(
            "sig-secret",
            &[("msisdn", "+15555550199"), ("text", "hello")],
        );
        // Tamper with the body AFTER the signature was computed.
        params.insert("text".into(), "hello (modified)".into());
        assert!(!verify_vonage_signature("sig-secret", &params));
    }

    #[test]
    fn verify_signature_rejects_wrong_secret() {
        let params = signed_form(
            "sig-secret",
            &[("msisdn", "+15555550199"), ("text", "hello")],
        );
        assert!(!verify_vonage_signature("WRONG-SECRET", &params));
    }

    #[test]
    fn verify_signature_rejects_missing_sig() {
        let mut params = BTreeMap::new();
        params.insert("msisdn".into(), "+15555550199".into());
        params.insert("text".into(), "hello".into());
        // No `sig` key inserted.
        assert!(!verify_vonage_signature("sig-secret", &params));
    }

    #[test]
    fn verify_signature_rejects_empty_sig() {
        let mut params = BTreeMap::new();
        params.insert("msisdn".into(), "+15555550199".into());
        params.insert("text".into(), "hello".into());
        params.insert("sig".into(), String::new());
        assert!(!verify_vonage_signature("sig-secret", &params));
    }

    #[test]
    fn verify_signature_ignores_sig_in_canonical_string() {
        // The canonical string excludes `sig`. If our implementation
        // accidentally included it, the round-trip would still pass
        // (because both sides would include it), but rotating sig to a
        // garbage value should still fail. This test confirms we skip
        // `sig` when building the canonical string by checking that the
        // round-trip works regardless of whether we recompute or trust the
        // already-attached value.
        let params = signed_form("sig-secret", &[("a", "1"), ("b", "2"), ("c", "3")]);
        // Round-trip succeeds.
        assert!(verify_vonage_signature("sig-secret", &params));
        // Mutating `sig` to a different valid-looking value must fail.
        let mut tampered = params.clone();
        tampered.insert("sig".into(), "0".repeat(64));
        assert!(!verify_vonage_signature("sig-secret", &tampered));
    }

    mod http_tests {
        use super::*;
        use serde_json::json;
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn send_posts_form_body_with_credentials() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/sms/json"))
                .and(header("content-type", "application/x-www-form-urlencoded"))
                .and(body_string_contains("api_key=TESTKEY"))
                .and(body_string_contains("api_secret=test-secret"))
                .and(body_string_contains("from=%2B15555550100"))
                .and(body_string_contains("to=%2B15555550199"))
                .and(body_string_contains("text=hello+there"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "message-count": "1",
                    "messages": [{"to": "+15555550199", "message-id": "M1", "status": "0"}]
                })))
                .expect(1)
                .mount(&server)
                .await;

            let twilio_like = ch();
            twilio_like
                .post_message_chunk("+15555550199", "hello there", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/sms/json"))
                .respond_with(
                    ResponseTemplate::new(401).set_body_string("{\"error\":\"unauthorized\"}"),
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

        #[tokio::test]
        async fn send_surfaces_per_message_failure() {
            // Vonage returns 200 with an error status inside the JSON when
            // the per-message send fails (e.g. invalid destination, throttle).
            // Validator must surface that as Err, not silently succeed.
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/sms/json"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "message-count": "1",
                    "messages": [{
                        "to": "+15555550199",
                        "status": "5",
                        "error-text": "Server Error"
                    }]
                })))
                .mount(&server)
                .await;

            let err = ch()
                .post_message_chunk("+15555550199", "hi", Some(&server.uri()))
                .await
                .expect_err("expected per-message-failure error");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("status=5") && msg.contains("Server Error"),
                "missing per-message status/error in: {msg}"
            );
        }
    }
}
