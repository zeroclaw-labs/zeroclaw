//! Twilio SMS channel — sends via Twilio's REST `Messages` resource and
//! receives via gateway-routed webhooks.
//!
//! Mirrors the gateway-registered pattern used by WhatsApp Cloud, Linq, WATI,
//! and Nextcloud Talk: this channel's `listen()` is a keep-alive no-op; the
//! `zeroclaw-gateway` crate hosts a hardcoded `POST /twilio/sms` route whose
//! handler reads `application/x-www-form-urlencoded` payloads, validates the
//! `X-Twilio-Signature` header, and converts each request into a
//! `ChannelMessage`.
//!
//! # Auth
//! Account SID + Auth Token. Outbound calls use HTTP Basic with the SID as
//! username and the Auth Token as password. Inbound webhooks are
//! authenticated by recomputing Twilio's HMAC-SHA1 over the full request URL
//! concatenated with sorted form parameters and base64-comparing against
//! `X-Twilio-Signature`. The same Auth Token keys both flows.
//!
//! # Outbound
//! `POST https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json`
//! with form fields `From`, `To`, `Body`. Twilio segments long messages
//! transparently up to 1600 characters; bodies above that are split into
//! ≤1600-char chunks at sentence/word boundaries with a `(i/N)` continuation
//! marker.
//!
//! # Inbound
//! The gateway handler verifies the signature, drops senders not on the
//! `allowed_numbers` allowlist, and forwards each message to the agent loop.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use std::collections::BTreeMap;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const TWILIO_API_BASE: &str = "https://api.twilio.com/2010-04-01";
/// Twilio segments outbound messages transparently. The hard ceiling for a
/// single API call is 1600 characters; longer bodies must be split.
const TWILIO_MESSAGE_LIMIT: usize = 1600;
/// Hardcoded inbound webhook path on the gateway. Operators point Twilio's
/// "A MESSAGE COMES IN" setting at `https://{gateway}/twilio/sms`.
pub const TWILIO_WEBHOOK_PATH: &str = "/twilio/sms";

/// Twilio SMS channel — see module docs.
pub struct TwilioChannel {
    account_sid: String,
    auth_token: String,
    from_number: String,
    allowed_numbers: Vec<String>,
}

impl TwilioChannel {
    pub fn new(
        account_sid: String,
        auth_token: String,
        from_number: String,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            account_sid,
            auth_token,
            from_number,
            allowed_numbers,
        }
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.twilio")
    }

    fn outbound_url(&self) -> String {
        format!(
            "{TWILIO_API_BASE}/Accounts/{}/Messages.json",
            self.account_sid
        )
    }

    /// Configurable base URL hook used by tests. The default path matches
    /// Twilio's production endpoint; tests substitute a wiremock URI by
    /// constructing the channel and overriding via `with_outbound_base`.
    #[cfg(test)]
    fn outbound_url_with_base(&self, base: &str) -> String {
        format!(
            "{base}/2010-04-01/Accounts/{}/Messages.json",
            self.account_sid
        )
    }

    /// Whether the inbound `From` number is permitted. Empty allowlist denies
    /// everyone; `"*"` matches anyone; otherwise an exact case-insensitive
    /// E.164 match is required.
    pub fn is_number_allowed(&self, phone: &str) -> bool {
        is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Verify a Twilio webhook signature against the request URL and form
    /// parameters. Twilio's algorithm:
    ///
    /// 1. Take the full URL the request was sent to (including scheme, host,
    ///    path, and any query string).
    /// 2. If the request is a POST with `application/x-www-form-urlencoded`,
    ///    sort the form parameters by key, then concatenate `key + value` for
    ///    each pair and append the result to the URL.
    /// 3. Compute HMAC-SHA1 of the resulting string using the Auth Token as
    ///    the key.
    /// 4. Base64-encode the digest and compare against the
    ///    `X-Twilio-Signature` header (constant-time).
    ///
    /// See <https://www.twilio.com/docs/usage/webhooks/webhooks-security>.
    pub fn verify_signature(
        &self,
        full_url: &str,
        form_params: &BTreeMap<String, String>,
        header_signature: &str,
    ) -> bool {
        verify_twilio_signature(&self.auth_token, full_url, form_params, header_signature)
    }

    /// Convert an inbound Twilio webhook payload into a `ChannelMessage`.
    /// Returns `None` when the sender isn't on the allowlist or the body is
    /// empty/non-text.
    pub fn parse_webhook_payload(
        &self,
        form_params: &BTreeMap<String, String>,
    ) -> Option<ChannelMessage> {
        let from = form_params.get("From")?.trim();
        if from.is_empty() {
            return None;
        }
        if !self.is_number_allowed(from) {
            tracing::debug!("Twilio: dropping SMS from {from} (not in allowed_numbers)");
            return None;
        }
        let body = form_params
            .get("Body")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if body.is_empty() {
            tracing::debug!("Twilio: dropping empty-body SMS from {from}");
            return None;
        }
        let message_sid = form_params
            .get("MessageSid")
            .cloned()
            .unwrap_or_else(|| format!("twilio-{}", chrono::Utc::now().timestamp_millis()));
        Some(ChannelMessage {
            id: format!("twilio_{message_sid}"),
            sender: from.to_string(),
            reply_target: from.to_string(),
            content: body,
            channel: "twilio".to_string(),
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
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&[
                ("From", self.from_number.as_str()),
                ("To", to),
                ("Body", body),
            ])
            .send()
            .await
            .context("Twilio Messages POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Twilio Messages POST returned {status}: {body}");
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for TwilioChannel {
    fn name(&self) -> &str {
        "twilio"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let to = message.recipient.trim();
        if to.is_empty() {
            bail!("Twilio send: empty recipient");
        }
        let chunks = chunk_sms(&message.content, TWILIO_MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        for chunk in chunks {
            self.post_message_chunk(to, &chunk, None).await?;
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Twilio uses webhooks (push-based), not polling. The gateway hosts
        // POST /twilio/sms and routes inbound payloads via parse_webhook_payload.
        tracing::info!(
            "Twilio channel active (webhook mode). Configure Twilio's \"A MESSAGE \
             COMES IN\" setting to POST to your gateway's {TWILIO_WEBHOOK_PATH} endpoint."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Probe the Account resource — succeeds when SID + token are valid.
        let url = format!("{TWILIO_API_BASE}/Accounts/{}.json", self.account_sid);
        self.http_client()
            .get(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
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

fn verify_twilio_signature(
    auth_token: &str,
    full_url: &str,
    form_params: &BTreeMap<String, String>,
    header_signature: &str,
) -> bool {
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    if header_signature.is_empty() {
        return false;
    }
    let mut composed = String::with_capacity(full_url.len() + form_params.len() * 32);
    composed.push_str(full_url);
    // BTreeMap iteration is sorted by key — exactly what Twilio asks for.
    for (k, v) in form_params {
        composed.push_str(k);
        composed.push_str(v);
    }
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(auth_token.as_bytes()) else {
        return false;
    };
    mac.update(composed.as_bytes());
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

    fn ch() -> TwilioChannel {
        TwilioChannel::new(
            "ACtestsid".into(),
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
        let chunks = chunk_sms("hi there", TWILIO_MESSAGE_LIMIT);
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
        let chunks = chunk_sms("   ", TWILIO_MESSAGE_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_webhook_drops_outside_allowlist() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550999".into());
        params.insert("Body".into(), "hello".into());
        params.insert("MessageSid".into(), "SMabc".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_body() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550199".into());
        params.insert("Body".into(), "  ".into());
        assert!(ch().parse_webhook_payload(&params).is_none());
    }

    #[test]
    fn parse_webhook_returns_message_on_allowed() {
        let mut params = BTreeMap::new();
        params.insert("From".into(), "+15555550199".into());
        params.insert("Body".into(), "hello world".into());
        params.insert("MessageSid".into(), "SM123".into());
        let msg = ch()
            .parse_webhook_payload(&params)
            .expect("expected message");
        assert_eq!(msg.sender, "+15555550199");
        assert_eq!(msg.reply_target, "+15555550199");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel, "twilio");
        assert_eq!(msg.id, "twilio_SM123");
    }

    /// Test vector hand-derived from Twilio's published algorithm at
    /// <https://www.twilio.com/docs/usage/webhooks/webhooks-security>.
    /// Inputs:
    ///   url: "https://mycompany.com/myapp.php?foo=1&bar=2"
    ///   params: {Digits: "1234", To: "+18005551212", From: "+14158675309",
    ///            Caller: "+14158675309", CallSid: "CA1234567890ABCDE"}
    ///   token: "12345"
    /// Expected base64 signature: "RSOYDt4T1cUTdK1PDd93/VVr8B8="
    #[test]
    fn verify_signature_matches_documented_vector() {
        let mut params = BTreeMap::new();
        params.insert("CallSid".into(), "CA1234567890ABCDE".into());
        params.insert("Caller".into(), "+14158675309".into());
        params.insert("Digits".into(), "1234".into());
        params.insert("From".into(), "+14158675309".into());
        params.insert("To".into(), "+18005551212".into());
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        let auth_token = "12345";
        let expected = "RSOYDt4T1cUTdK1PDd93/VVr8B8=";
        assert!(
            verify_twilio_signature(auth_token, url, &params, expected),
            "documented Twilio vector should validate"
        );
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let mut params = BTreeMap::new();
        params.insert("CallSid".into(), "CA1234567890ABCDE".into());
        params.insert("Digits".into(), "1234".into());
        params.insert("From".into(), "+14158675309".into());
        params.insert("To".into(), "+18005551212".into());
        // Caller intentionally altered.
        params.insert("Caller".into(), "+19998675309".into());
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        assert!(!verify_twilio_signature(
            "12345",
            url,
            &params,
            "RSOYDt4T1cUTdK1PDd93/VVr8B8="
        ));
    }

    #[test]
    fn verify_signature_rejects_empty_header() {
        let params = BTreeMap::new();
        assert!(!verify_twilio_signature("12345", "https://x", &params, ""));
    }

    #[test]
    fn verify_signature_rejects_wrong_token() {
        let mut params = BTreeMap::new();
        params.insert("CallSid".into(), "CA1234567890ABCDE".into());
        params.insert("Caller".into(), "+14158675309".into());
        params.insert("Digits".into(), "1234".into());
        params.insert("From".into(), "+14158675309".into());
        params.insert("To".into(), "+18005551212".into());
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        assert!(!verify_twilio_signature(
            "wrong-token",
            url,
            &params,
            "RSOYDt4T1cUTdK1PDd93/VVr8B8="
        ));
    }

    mod http_tests {
        use super::*;
        use wiremock::matchers::{body_string_contains, header, header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn send_posts_to_messages_endpoint_with_basic_auth() {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/2010-04-01/Accounts/ACtestsid/Messages.json"))
                .and(header_exists("authorization"))
                .and(header("content-type", "application/x-www-form-urlencoded"))
                .and(body_string_contains("From=%2B15555550100"))
                .and(body_string_contains("To=%2B15555550199"))
                .and(body_string_contains("Body=hello+there"))
                .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                    "sid": "SM12345",
                })))
                .expect(1)
                .mount(&server)
                .await;

            let twilio = ch();
            twilio
                .post_message_chunk("+15555550199", "hello there", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/2010-04-01/Accounts/ACtestsid/Messages.json"))
                .respond_with(ResponseTemplate::new(401).set_body_string("{\"code\": 20003}"))
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
