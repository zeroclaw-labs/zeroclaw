//! Telnyx SMS channel — sends via Telnyx's V2 `Messages` resource and
//! receives via gateway-routed webhooks.
//!
//! Mirrors the gateway-registered pattern used by Twilio, WhatsApp Cloud,
//! Linq, WATI, and Nextcloud Talk: this channel's `listen()` is a keep-alive
//! no-op; the `zeroclaw-gateway` crate hosts a hardcoded `POST /telnyx/sms`
//! route whose handler reads JSON payloads, validates the
//! `telnyx-signature-ed25519` header against an Ed25519 public key, and
//! converts each request into a `ChannelMessage`.
//!
//! # Auth
//! Two distinct values, copied separately from the Telnyx portal:
//! * **V2 API key** — sent as `Authorization: Bearer {api_key}` for all
//!   outbound REST calls.
//! * **Ed25519 public key** — base64-encoded, used to verify inbound webhook
//!   signatures. Telnyx publishes the public key in the portal; operators
//!   must update `public_key` in config when Telnyx rotates it.
//!
//! # Outbound
//! `POST https://api.telnyx.com/v2/messages` with a JSON body containing
//! `from`, `to`, and `text`. When a `messaging_profile_id` is configured,
//! it is included as well. Telnyx accepts up to 1600 characters per request;
//! longer bodies are split into ≤1600-char chunks at sentence/word
//! boundaries with a `(i/N) ` continuation marker.
//!
//! # Inbound
//! The gateway handler verifies the Ed25519 signature over the message bytes
//! `{timestamp}|{raw_body}` (literal pipe separator), enforces a 5-minute
//! timestamp anti-replay window, drops senders not on the `allowed_numbers`
//! allowlist, and forwards each `message.received` event to the agent loop.
//! See <https://developers.telnyx.com/docs/messaging/webhooks>.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const TELNYX_API_BASE: &str = "https://api.telnyx.com";
/// Telnyx accepts up to 1600 characters per outbound API call. Longer bodies
/// must be split.
const TELNYX_MESSAGE_LIMIT: usize = 1600;
/// Anti-replay window for inbound webhooks. Telnyx-signed payloads older or
/// further in the future than this many seconds are rejected outright.
const TELNYX_SIGNATURE_WINDOW_SECS: i64 = 300;
/// Hardcoded inbound webhook path on the gateway. Operators point Telnyx's
/// "Webhook URL" setting at `https://{gateway}/telnyx/sms`.
pub const TELNYX_WEBHOOK_PATH: &str = "/telnyx/sms";

/// Telnyx SMS channel — see module docs.
pub struct TelnyxChannel {
    api_key: String,
    from_number: String,
    messaging_profile_id: Option<String>,
    allowed_numbers: Vec<String>,
    verifying_key: ed25519_dalek::VerifyingKey,
}

impl TelnyxChannel {
    /// Build a Telnyx channel. The base64-encoded `public_key_b64` must
    /// decode to a 32-byte Ed25519 verifying key; `Err` is returned otherwise
    /// so the daemon can log + skip rather than panic on a config typo.
    pub fn new(
        api_key: String,
        from_number: String,
        messaging_profile_id: Option<String>,
        allowed_numbers: Vec<String>,
        public_key_b64: &str,
    ) -> Result<Self> {
        use base64::Engine;
        let trimmed = public_key_b64.trim();
        if trimmed.is_empty() {
            bail!(
                "Telnyx public_key is empty — copy the Ed25519 public key from the Telnyx portal"
            );
        }
        let raw = base64::engine::general_purpose::STANDARD
            .decode(trimmed)
            .with_context(|| "Telnyx public_key is not valid base64")?;
        let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!(
                "Telnyx public_key must decode to 32 bytes (got {})",
                raw.len()
            )
        })?;
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&bytes).map_err(|e| {
            anyhow::anyhow!("Telnyx public_key is not a valid Ed25519 verifying key: {e}")
        })?;
        Ok(Self {
            api_key,
            from_number,
            messaging_profile_id,
            allowed_numbers,
            verifying_key,
        })
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_runtime_proxy_client("channel.telnyx")
    }

    fn outbound_url(&self) -> String {
        format!("{TELNYX_API_BASE}/v2/messages")
    }

    /// Configurable base URL hook used by tests. The default path matches
    /// Telnyx's production endpoint; tests substitute a wiremock URI.
    #[cfg(test)]
    fn outbound_url_with_base(&self, base: &str) -> String {
        format!("{base}/v2/messages")
    }

    fn whoami_url(&self) -> String {
        format!("{TELNYX_API_BASE}/v2/whoami")
    }

    /// Whether the inbound `from` number is permitted. Empty allowlist
    /// denies everyone; `"*"` matches anyone; otherwise an exact
    /// case-insensitive E.164 match (with whitespace stripped) is required.
    pub fn is_number_allowed(&self, phone: &str) -> bool {
        is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Verify a Telnyx webhook signature.
    ///
    /// Telnyx's algorithm:
    /// 1. Read `telnyx-timestamp` (unix epoch seconds, as a string) and
    ///    `telnyx-signature-ed25519` (base64-encoded 64-byte signature).
    /// 2. Reject if `|now - timestamp| > 300s` (5-minute anti-replay window).
    /// 3. Construct the signed message bytes: `{timestamp}|{raw_body}`
    ///    (literal pipe separator).
    /// 4. Verify the Ed25519 signature against the operator-configured
    ///    public key using strict verification (`verify_strict`).
    ///
    /// `now_unix` is taken as a parameter so the gateway can pass
    /// `chrono::Utc::now().timestamp()` while tests can use deterministic
    /// values.
    ///
    /// See <https://developers.telnyx.com/docs/messaging/webhooks>.
    pub fn verify_signature(
        &self,
        timestamp_str: &str,
        raw_body: &[u8],
        signature_b64: &str,
        now_unix: i64,
    ) -> bool {
        verify_telnyx_signature(
            &self.verifying_key,
            timestamp_str,
            raw_body,
            signature_b64,
            now_unix,
        )
    }

    /// Convert an inbound Telnyx webhook payload into a `ChannelMessage`.
    /// Returns `None` when the event is not a `message.received`, the sender
    /// isn't on the allowlist, or the body is empty.
    pub fn parse_webhook_payload(&self, json: &serde_json::Value) -> Option<ChannelMessage> {
        let data = json.get("data")?;
        let event_type = data
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if event_type != "message.received" {
            tracing::debug!("Telnyx: dropping non-message event type '{event_type}'");
            return None;
        }
        let payload = data.get("payload")?;
        let from = payload
            .get("from")
            .and_then(|v| v.get("phone_number"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if from.is_empty() {
            return None;
        }
        if !self.is_number_allowed(from) {
            tracing::debug!("Telnyx: dropping SMS from {from} (not in allowed_numbers)");
            return None;
        }
        let body = payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if body.is_empty() {
            tracing::debug!("Telnyx: dropping empty-body SMS from {from}");
            return None;
        }
        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("telnyx-{}", chrono::Utc::now().timestamp_millis()));
        Some(ChannelMessage {
            id: format!("telnyx_{id}"),
            sender: from.to_string(),
            reply_target: from.to_string(),
            content: body,
            channel: "telnyx".to_string(),
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
        let mut payload = serde_json::json!({
            "from": self.from_number,
            "to": to,
            "text": body,
        });
        if let Some(profile_id) = self.messaging_profile_id.as_deref()
            && !profile_id.is_empty()
            && let Some(obj) = payload.as_object_mut()
        {
            obj.insert(
                "messaging_profile_id".to_string(),
                serde_json::Value::String(profile_id.to_string()),
            );
        }
        let resp = self
            .http_client()
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .context("Telnyx Messages POST failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("Telnyx Messages POST returned {status}: {body}");
        }
        Ok(())
    }
}

#[async_trait]
impl Channel for TelnyxChannel {
    fn name(&self) -> &str {
        "telnyx"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let to = message.recipient.trim();
        if to.is_empty() {
            bail!("Telnyx send: empty recipient");
        }
        let chunks = chunk_sms(&message.content, TELNYX_MESSAGE_LIMIT);
        if chunks.is_empty() {
            return Ok(());
        }
        for chunk in chunks {
            self.post_message_chunk(to, &chunk, None).await?;
        }
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Telnyx uses webhooks (push-based), not polling. The gateway hosts
        // POST /telnyx/sms and routes inbound payloads via parse_webhook_payload.
        tracing::info!(
            "Telnyx channel active (webhook mode). Configure Telnyx's webhook URL \
             to POST to your gateway's {TELNYX_WEBHOOK_PATH} endpoint."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Probe an authenticated endpoint — succeeds when the V2 API key is
        // valid. `/v2/whoami` returns the authenticated user info and is the
        // cheapest authenticated GET available.
        self.http_client()
            .get(self.whoami_url())
            .bearer_auth(&self.api_key)
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

fn verify_telnyx_signature(
    verifying_key: &ed25519_dalek::VerifyingKey,
    timestamp_str: &str,
    raw_body: &[u8],
    signature_b64: &str,
    now_unix: i64,
) -> bool {
    use base64::Engine;
    if signature_b64.is_empty() || timestamp_str.is_empty() {
        return false;
    }
    let timestamp: i64 = match timestamp_str.parse() {
        Ok(t) => t,
        Err(_) => return false,
    };
    if (now_unix - timestamp).abs() > TELNYX_SIGNATURE_WINDOW_SECS {
        return false;
    }
    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(signature_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig_arr: [u8; 64] = match sig_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let signature = ed25519_dalek::Signature::from_bytes(&sig_arr);
    let mut message = format!("{timestamp_str}|").into_bytes();
    message.extend_from_slice(raw_body);
    verifying_key.verify_strict(&message, &signature).is_ok()
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

    /// Deterministic Ed25519 keypair seed used across signature tests. The
    /// `[1u8; 32]` seed is arbitrary — what matters is that the verifying
    /// key derived from it matches the signing key used to sign the test
    /// payloads, so no real Telnyx credentials are involved.
    const TEST_SEED: [u8; 32] = [1u8; 32];

    fn test_signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&TEST_SEED)
    }

    fn test_public_key_b64() -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .encode(test_signing_key().verifying_key().as_bytes())
    }

    fn ch() -> TelnyxChannel {
        TelnyxChannel::new(
            "test-api-key".into(),
            "+15555550100".into(),
            None,
            vec!["+15555550199".into()],
            &test_public_key_b64(),
        )
        .expect("test channel construction succeeds")
    }

    fn ch_with_profile() -> TelnyxChannel {
        TelnyxChannel::new(
            "test-api-key".into(),
            "+15555550100".into(),
            Some("mp_test_profile".into()),
            vec!["+15555550199".into()],
            &test_public_key_b64(),
        )
        .expect("test channel with profile construction succeeds")
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
    fn new_rejects_empty_public_key() {
        let result = TelnyxChannel::new("k".into(), "+15555550100".into(), None, vec![], "");
        let Err(err) = result else {
            panic!("empty public_key should error");
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("empty"), "error should mention empty: {msg}");
    }

    #[test]
    fn new_rejects_invalid_base64() {
        let result = TelnyxChannel::new(
            "k".into(),
            "+15555550100".into(),
            None,
            vec![],
            "!!!not-base64!!!",
        );
        let Err(err) = result else {
            panic!("malformed base64 should error");
        };
        let msg = format!("{err:#}");
        assert!(msg.contains("base64"), "error should mention base64: {msg}");
    }

    #[test]
    fn new_rejects_wrong_length() {
        use base64::Engine;
        let too_short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        let result =
            TelnyxChannel::new("k".into(), "+15555550100".into(), None, vec![], &too_short);
        let Err(err) = result else {
            panic!("16-byte key should error");
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("32 bytes"),
            "error should mention 32 bytes: {msg}"
        );
    }

    #[test]
    fn chunk_sms_short_passes_through() {
        let chunks = chunk_sms("hi there", TELNYX_MESSAGE_LIMIT);
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
        let chunks = chunk_sms("   ", TELNYX_MESSAGE_LIMIT);
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_sms_preserves_total_content() {
        let body = "alpha beta gamma. ".repeat(120);
        let chunks = chunk_sms(&body, 100);
        // Strip "(i/N) " markers, concat, and ensure no characters got lost.
        let recombined: String = chunks
            .iter()
            .map(|c| {
                if let Some(rest) = c.split_once(") ") {
                    rest.1.to_string()
                } else {
                    c.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let stripped_orig: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
        let stripped_recomb: String = recombined.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(stripped_orig, stripped_recomb);
    }

    #[test]
    fn parse_webhook_drops_outside_allowlist() {
        let payload = serde_json::json!({
            "data": {
                "event_type": "message.received",
                "id": "abc",
                "payload": {
                    "from": { "phone_number": "+15555550999" },
                    "to": [{ "phone_number": "+15555550100" }],
                    "text": "hello",
                }
            }
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_drops_empty_body() {
        let payload = serde_json::json!({
            "data": {
                "event_type": "message.received",
                "id": "abc",
                "payload": {
                    "from": { "phone_number": "+15555550199" },
                    "to": [{ "phone_number": "+15555550100" }],
                    "text": "   ",
                }
            }
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_drops_non_message_received_event() {
        let payload = serde_json::json!({
            "data": {
                "event_type": "message.sent",
                "id": "abc",
                "payload": {
                    "from": { "phone_number": "+15555550199" },
                    "text": "hello",
                }
            }
        });
        assert!(ch().parse_webhook_payload(&payload).is_none());
    }

    #[test]
    fn parse_webhook_returns_message_on_allowed() {
        let payload = serde_json::json!({
            "data": {
                "event_type": "message.received",
                "id": "msg_abc123",
                "payload": {
                    "from": { "phone_number": "+15555550199" },
                    "to": [{ "phone_number": "+15555550100" }],
                    "text": "hello world",
                }
            }
        });
        let msg = ch()
            .parse_webhook_payload(&payload)
            .expect("expected message");
        assert_eq!(msg.sender, "+15555550199");
        assert_eq!(msg.reply_target, "+15555550199");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.channel, "telnyx");
        assert_eq!(msg.id, "telnyx_msg_abc123");
    }

    /// Build a valid Telnyx-style signature header for the given timestamp +
    /// raw body using the deterministic test signing key.
    fn sign_b64(timestamp: &str, raw_body: &[u8]) -> String {
        use base64::Engine;
        use ed25519_dalek::Signer;
        let mut msg = format!("{timestamp}|").into_bytes();
        msg.extend_from_slice(raw_body);
        let sig = test_signing_key().sign(&msg);
        base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
    }

    #[test]
    fn verify_signature_accepts_valid_payload() {
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        let body = b"{\"data\":{\"event_type\":\"message.received\"}}";
        let sig = sign_b64(&ts, body);
        assert!(ch().verify_signature(&ts, body, &sig, now));
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        let body = b"original body";
        let sig = sign_b64(&ts, body);
        let tampered = b"different body";
        assert!(!ch().verify_signature(&ts, tampered, &sig, now));
    }

    #[test]
    fn verify_signature_rejects_tampered_timestamp() {
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        let body = b"hello";
        let sig = sign_b64(&ts, body);
        // Move ts by 1 second — signature was for `1_700_000_000|hello`, now
        // we present `1_700_000_001|hello`.
        let bumped = (now + 1).to_string();
        assert!(!ch().verify_signature(&bumped, body, &sig, now + 1));
    }

    #[test]
    fn verify_signature_rejects_far_future_timestamp() {
        let now: i64 = 1_700_000_000;
        let future = now + 600; // 10 min ahead — outside 5-min window
        let ts = future.to_string();
        let body = b"hello";
        let sig = sign_b64(&ts, body);
        assert!(!ch().verify_signature(&ts, body, &sig, now));
    }

    #[test]
    fn verify_signature_rejects_far_past_timestamp() {
        let now: i64 = 1_700_000_000;
        let past = now - 600; // 10 min ago — outside 5-min window
        let ts = past.to_string();
        let body = b"hello";
        let sig = sign_b64(&ts, body);
        assert!(!ch().verify_signature(&ts, body, &sig, now));
    }

    #[test]
    fn verify_signature_rejects_wrong_signature_length() {
        use base64::Engine;
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        let body = b"hello";
        // 32 bytes encoded — wrong length, real signatures are 64 bytes.
        let too_short = base64::engine::general_purpose::STANDARD.encode([0u8; 32]);
        assert!(!ch().verify_signature(&ts, body, &too_short, now));
    }

    #[test]
    fn verify_signature_rejects_malformed_base64() {
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        let body = b"hello";
        assert!(!ch().verify_signature(&ts, body, "!!!not-base64!!!", now));
    }

    #[test]
    fn verify_signature_rejects_empty_signature() {
        let now: i64 = 1_700_000_000;
        let ts = now.to_string();
        assert!(!ch().verify_signature(&ts, b"hello", "", now));
    }

    #[test]
    fn verify_signature_rejects_unparseable_timestamp() {
        let now: i64 = 1_700_000_000;
        let body = b"hello";
        let sig = sign_b64("1700000000", body);
        assert!(!ch().verify_signature("not-a-number", body, &sig, now));
    }

    mod http_tests {
        use super::*;
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn send_posts_json_with_bearer_auth() {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/v2/messages"))
                .and(header("authorization", "Bearer test-api-key"))
                .and(header("content-type", "application/json"))
                .and(body_string_contains("\"from\":\"+15555550100\""))
                .and(body_string_contains("\"to\":\"+15555550199\""))
                .and(body_string_contains("\"text\":\"hello there\""))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": { "id": "msg_abc" },
                })))
                .expect(1)
                .mount(&server)
                .await;

            let telnyx = ch();
            telnyx
                .post_message_chunk("+15555550199", "hello there", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_includes_messaging_profile_id_when_set() {
            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/v2/messages"))
                .and(body_string_contains(
                    "\"messaging_profile_id\":\"mp_test_profile\"",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "data": { "id": "msg_abc" },
                })))
                .expect(1)
                .mount(&server)
                .await;

            let telnyx = ch_with_profile();
            telnyx
                .post_message_chunk("+15555550199", "with profile", Some(&server.uri()))
                .await
                .expect("send succeeds");
        }

        #[tokio::test]
        async fn send_surfaces_4xx_as_error() {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/v2/messages"))
                .respond_with(
                    ResponseTemplate::new(401)
                        .set_body_string("{\"errors\":[{\"code\":\"10003\"}]}"),
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
