use async_trait::async_trait;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::StreamMode;

const FEISHU_BASE_URL: &str = "https://open.feishu.cn/open-apis";
const FEISHU_WS_BASE_URL: &str = "https://open.feishu.cn";
const LARK_BASE_URL: &str = "https://open.larksuite.com/open-apis";
const LARK_WS_BASE_URL: &str = "https://open.larksuite.com";

const MAX_LARK_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// Map a unicode emoji used by generic callers of [`Channel::add_reaction`]
/// (e.g. Reply-Intent Precheck, no-reply ack heuristics) to a Lark/Feishu
/// `emoji_type` name recognised by the
/// `POST /im/v1/messages/{id}/reactions` API.
///
/// Returns `None` when no mapping exists; callers should treat that as a
/// best-effort skip rather than an error. The whitelist intentionally
/// covers only the unicode emojis emitted by the inbound-ack policy and
/// related no-reply heuristics today; extend as new callers appear.
fn unicode_to_lark_emoji_type(emoji: &str) -> Option<&'static str> {
    match emoji {
        "👍" => Some("THUMBSUP"),
        "🚫" => Some("No"),
        "⚠️" => Some("Alarm"),
        "👀" => Some("GLANCE"),
        "✅" => Some("DONE"),
        "✔️" => Some("DONE"),
        "❤️" => Some("HEART"),
        "🎉" => Some("PARTY"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LarkPlatform {
    Lark,
    Feishu,
}

impl LarkPlatform {
    fn api_base(self) -> &'static str {
        match self {
            Self::Lark => LARK_BASE_URL,
            Self::Feishu => FEISHU_BASE_URL,
        }
    }

    fn ws_base(self) -> &'static str {
        match self {
            Self::Lark => LARK_WS_BASE_URL,
            Self::Feishu => FEISHU_WS_BASE_URL,
        }
    }

    fn locale_header(self) -> &'static str {
        match self {
            Self::Lark => "en",
            Self::Feishu => "zh",
        }
    }

    fn proxy_service_key(self) -> &'static str {
        match self {
            Self::Lark => "channel.lark",
            Self::Feishu => "channel.feishu",
        }
    }

    fn channel_name(self) -> &'static str {
        match self {
            Self::Lark => "lark",
            Self::Feishu => "feishu",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Feishu WebSocket long-connection: pbbp2.proto frame codec
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, prost::Message)]
struct PbHeader {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(string, tag = "2")]
    pub value: String,
}

/// Feishu WS frame (pbbp2.proto).
/// method=0 → CONTROL (ping/pong)  method=1 → DATA (events)
#[derive(Clone, PartialEq, prost::Message)]
struct PbFrame {
    #[prost(uint64, tag = "1")]
    pub seq_id: u64,
    #[prost(uint64, tag = "2")]
    pub log_id: u64,
    #[prost(int32, tag = "3")]
    pub service: i32,
    #[prost(int32, tag = "4")]
    pub method: i32,
    #[prost(message, repeated, tag = "5")]
    pub headers: Vec<PbHeader>,
    #[prost(bytes = "vec", optional, tag = "8")]
    pub payload: Option<Vec<u8>>,
}

impl PbFrame {
    fn header_value<'a>(&'a self, key: &str) -> &'a str {
        self.headers
            .iter()
            .find(|h| h.key == key)
            .map(|h| h.value.as_str())
            .unwrap_or("")
    }
}

/// Server-sent client config (parsed from pong payload)
#[derive(Debug, serde::Deserialize, Default, Clone)]
struct WsClientConfig {
    #[serde(rename = "PingInterval")]
    ping_interval: Option<u64>,
}

/// POST /callback/ws/endpoint response
#[derive(Debug, serde::Deserialize)]
struct WsEndpointResp {
    code: i32,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Option<WsEndpoint>,
}

#[derive(Debug, serde::Deserialize)]
struct WsEndpoint {
    #[serde(rename = "URL")]
    url: String,
    #[serde(rename = "ClientConfig")]
    client_config: Option<WsClientConfig>,
}

/// LarkEvent envelope (method=1 / type=event payload)
#[derive(Debug, serde::Deserialize)]
struct LarkEvent {
    header: LarkEventHeader,
    event: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct LarkEventHeader {
    event_type: String,
    #[allow(dead_code)]
    event_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct MsgReceivePayload {
    sender: LarkSender,
    message: LarkMessage,
}

#[derive(Debug, serde::Deserialize)]
struct LarkSender {
    sender_id: LarkSenderId,
    #[serde(default)]
    sender_type: String,
}

#[derive(Debug, serde::Deserialize, Default)]
struct LarkSenderId {
    open_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct LarkMessage {
    message_id: String,
    chat_id: String,
    chat_type: String,
    message_type: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    mentions: Vec<serde_json::Value>,
}

/// Heartbeat timeout for WS connection — must be larger than ping_interval (default 120 s).
/// If no binary frame (pong or event) is received within this window, reconnect.
const WS_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh tenant token this many seconds before the announced expiry.
const LARK_TOKEN_REFRESH_SKEW: Duration = Duration::from_secs(120);
/// Fallback tenant token TTL when `expire`/`expires_in` is absent.
const LARK_DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(7200);
/// Feishu/Lark API business code for expired/invalid tenant access token.
const LARK_INVALID_ACCESS_TOKEN_CODE: i64 = 99_991_663;

/// Feishu/Lark API business code returned when a card PATCH (or any draft
/// message edit) is rate-limited. Treated as a soft-failure: we log a warning
/// but never propagate to the caller, since the user-visible decision is
/// already delivered out-of-band via the approval oneshot.
const LARK_DRAFT_RATE_LIMIT_CODE: i64 = 230_020;

/// Max byte size for a single interactive card's markdown content.
/// Lark card payloads have a ~30 KB limit; leave margin for JSON envelope.
const LARK_CARD_MARKDOWN_MAX_BYTES: usize = 28_000;

/// Maximum image size we will download and inline (10 MiB).
const LARK_IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Maximum file size we will download and present as text (512 KiB).
const LARK_FILE_MAX_BYTES: usize = 512 * 1024;

/// Image MIME types we support for inline base64 encoding.
const LARK_SUPPORTED_IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

/// Returns true when the WebSocket frame indicates live traffic that should
/// refresh the heartbeat watchdog.
fn should_refresh_last_recv(msg: &WsMsg) -> bool {
    matches!(msg, WsMsg::Binary(_) | WsMsg::Ping(_) | WsMsg::Pong(_))
}

/// Build an interactive card JSON string with a single markdown element.
/// Uses Card JSON 2.0 structure so that headings, tables, blockquotes,
/// and inline code render correctly.
fn build_card_content(markdown: &str) -> String {
    serde_json::json!({
        "schema": "2.0",
        "body": {
            "elements": [{
                "tag": "markdown",
                "content": markdown
            }]
        }
    })
    .to_string()
}

/// Build an approval-request interactive card (Card JSON 2.0).
///
/// Card 2.0 is required so PATCH-time updates from
/// `build_resolved_approval_card` can re-render the card on the user's
/// client. Feishu's IM PATCH endpoint accepts cross-version PATCH
/// (1.0 send → 2.0 patch) with `code: 0` but does NOT guarantee the
/// client re-renders; the same schema must be used on both sides.
///
/// Each button's `behaviors[0].value.approval_id` round-trips back via
/// the `card.action.trigger` event, parsed by `handle_card_action_event`.
fn build_approval_card(
    approval_id: &str,
    tool_name: &str,
    arguments_summary: &str,
) -> serde_json::Value {
    let make_button = |label: &str, button_type: &str, decision: &str| {
        serde_json::json!({
            "tag": "button",
            "text": { "tag": "plain_text", "content": label },
            "type": button_type,
            "behaviors": [{
                "type": "callback",
                "value": {
                    "approval_id": approval_id,
                    "decision": decision
                }
            }]
        })
    };

    serde_json::json!({
        "schema": "2.0",
        "config": { "wide_screen_mode": true },
        "header": {
            "template": "orange",
            "title": {
                "tag": "plain_text",
                "content": "🔧 Tool approval required"
            }
        },
        "body": {
            "elements": [
                {
                    "tag": "markdown",
                    "content": format!("**Tool:** `{tool_name}`\n\n{arguments_summary}")
                },
                {
                    "tag": "column_set",
                    "flex_mode": "stretch",
                    "columns": [
                        { "tag": "column", "elements": [
                            make_button("✅ Approve", "primary_filled", "approve")
                        ]},
                        { "tag": "column", "elements": [
                            make_button("❌ Deny", "danger_filled", "deny")
                        ]},
                        { "tag": "column", "elements": [
                            make_button("✅✅ Always", "default", "always")
                        ]}
                    ]
                }
            ]
        }
    })
}

/// Resolved-state rendering of the approval card (no buttons, decision banner).
///
/// Uses Card JSON 2.0 schema (matching `build_card_content`) because the
/// Feishu IM PATCH endpoint accepts Card 1.0 envelopes with `code: 0` but
/// silently refuses to re-render the client-side card. Using Card 2.0 (the
/// schema that the production-validated `build_card_content` uses) is what
/// actually causes the visual update to land on the user's screen.
fn build_resolved_approval_card(
    tool_name: &str,
    arguments_summary: &str,
    decision: zeroclaw_api::channel::ChannelApprovalResponse,
) -> serde_json::Value {
    use zeroclaw_api::channel::ChannelApprovalResponse;

    let (banner_emoji, banner_text, header_template) = match decision {
        ChannelApprovalResponse::Approve => ("✅", "Approved", "green"),
        ChannelApprovalResponse::AlwaysApprove => ("✅✅", "Approved (always)", "green"),
        ChannelApprovalResponse::Deny => ("❌", "Denied", "red"),
        ChannelApprovalResponse::DenyWithEdit { .. } => {
            unreachable!("DenyWithEdit is only valid for ACP channels")
        }
    };

    serde_json::json!({
        "schema": "2.0",
        "config": { "wide_screen_mode": true },
        "header": {
            "template": header_template,
            "title": {
                "tag": "plain_text",
                "content": format!("{banner_emoji} Tool approval — {banner_text}")
            }
        },
        "body": {
            "elements": [
                {
                    "tag": "markdown",
                    "content": format!(
                        "**Tool:** `{tool_name}`\n\n{arguments_summary}\n\n---\n\n**{banner_emoji} {banner_text}**"
                    )
                }
            ]
        }
    })
}

/// Build a sanitized copy of a `card.action.trigger` event payload that is
/// safe to emit to structured logs / dashboards / persisted JSONL.
///
/// The raw inbound payload from Lark/Feishu carries tenant-specific
/// identifiers and a callback verification token. These values are
/// classified as PII / callback secrets by the project's privacy policy
/// (see each fixture's `_fixture_note` under `tests/fixtures/lark/` for the
/// authoritative list of fields that must be redacted before any
/// persistence).
///
/// This function replaces the following with deterministic `REDACTED_*`
/// placeholder strings:
///
/// - top-level `token` (Lark callback verification token)
/// - `operator.open_id` / `union_id` / `user_id` / `tenant_key`
/// - `context.open_chat_id` / `context.open_message_id`
///
/// Non-sensitive business fields (`action.*`, `host`, etc.) are preserved
/// verbatim so DEBUG operators can still capture production payload shape
/// for fixture collection.
///
/// The input is borrowed read-only; a fresh owned `Value` is returned. The
/// regression test `sanitize_card_action_payload_redacts_sensitive_fields`
/// is the gate that fails if any of those raw values can leak through this
/// path.
fn sanitize_card_action_payload(event_payload: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;

    let mut sanitized = event_payload.clone();

    // Top-level callback verification token.
    if let Some(token) = sanitized.get_mut("token")
        && !token.is_null()
    {
        *token = Value::String("REDACTED_TOKEN".to_string());
    }

    // operator.* identifiers — only overwrite keys that are actually present
    // so the sanitized payload still reflects production shape (don't
    // invent fields that the real event didn't carry).
    if let Some(Value::Object(operator)) = sanitized.get_mut("operator") {
        for (key, placeholder) in [
            ("open_id", "REDACTED_OPERATOR_OPEN_ID"),
            ("union_id", "REDACTED_OPERATOR_UNION_ID"),
            ("user_id", "REDACTED_OPERATOR_USER_ID"),
            ("tenant_key", "REDACTED_OPERATOR_TENANT_KEY"),
        ] {
            if operator.contains_key(key) {
                operator.insert(key.to_string(), Value::String(placeholder.to_string()));
            }
        }
    }

    // context.open_* identifiers.
    if let Some(Value::Object(context)) = sanitized.get_mut("context") {
        for (key, placeholder) in [
            ("open_chat_id", "REDACTED_OPEN_CHAT_ID"),
            ("open_message_id", "REDACTED_OPEN_MESSAGE_ID"),
        ] {
            if context.contains_key(key) {
                context.insert(key.to_string(), Value::String(placeholder.to_string()));
            }
        }
    }

    sanitized
}

/// Build the full message body for sending an interactive card message.
fn build_interactive_card_body(recipient: &str, markdown: &str) -> serde_json::Value {
    serde_json::json!({
        "receive_id": recipient,
        "msg_type": "interactive",
        "content": build_card_content(markdown),
    })
}

/// Truncate streaming-draft markdown to fit `LARK_CARD_MARKDOWN_MAX_BYTES`.
///
/// When the accumulated content is small, returns it unchanged. When it
/// exceeds the budget we cut at the last UTF-8 boundary that still leaves
/// room for an `…_(updating)_` suffix, so the user sees a visible signal
/// that the card was clipped while updates continue.
fn truncate_card_markdown(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let suffix = "\n\n…_(updating)_";
    let budget = max_bytes.saturating_sub(suffix.len());
    let mut end = 0;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > budget {
            break;
        }
        end = next;
    }
    let mut out = String::with_capacity(end + suffix.len());
    out.push_str(&text[..end]);
    out.push_str(suffix);
    out
}

/// Split markdown content into chunks that fit within the card size limit.
/// Splits on line boundaries to avoid breaking markdown syntax.
fn split_markdown_chunks(text: &str, max_bytes: usize) -> Vec<&str> {
    if text.len() <= max_bytes {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        if start + max_bytes >= text.len() {
            chunks.push(&text[start..]);
            break;
        }

        let end = start + max_bytes;
        let search_region = &text[start..end];
        let split_at = search_region
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        let split_at = if text.is_char_boundary(split_at) {
            split_at
        } else {
            (start..split_at)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(start)
        };

        if split_at <= start {
            let forced = (end..=text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
            chunks.push(&text[start..forced]);
            start = forced;
        } else {
            chunks.push(&text[start..split_at]);
            start = split_at;
        }
    }

    chunks
}

#[derive(Debug, Clone)]
struct CachedTenantToken {
    value: String,
    refresh_after: Instant,
}

fn extract_lark_response_code(body: &serde_json::Value) -> Option<i64> {
    body.get("code").and_then(|c| c.as_i64())
}

fn is_lark_invalid_access_token(body: &serde_json::Value) -> bool {
    extract_lark_response_code(body) == Some(LARK_INVALID_ACCESS_TOKEN_CODE)
}

fn should_refresh_lark_tenant_token(status: reqwest::StatusCode, body: &serde_json::Value) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED || is_lark_invalid_access_token(body)
}

fn extract_lark_token_ttl_seconds(body: &serde_json::Value) -> u64 {
    let ttl = body
        .get("expire")
        .or_else(|| body.get("expires_in"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            body.get("expire")
                .or_else(|| body.get("expires_in"))
                .and_then(|v| v.as_i64())
                .and_then(|v| u64::try_from(v).ok())
        })
        .unwrap_or(LARK_DEFAULT_TOKEN_TTL.as_secs());
    ttl.max(1)
}

fn next_token_refresh_deadline(now: Instant, ttl_seconds: u64) -> Instant {
    let ttl = Duration::from_secs(ttl_seconds.max(1));
    let refresh_in = ttl
        .checked_sub(LARK_TOKEN_REFRESH_SKEW)
        .unwrap_or(Duration::from_secs(1));
    now + refresh_in
}

fn ensure_lark_send_success(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    context: &str,
) -> anyhow::Result<()> {
    if !status.is_success() {
        anyhow::bail!("send failed {context}: status={status}, body={body}");
    }

    let code = extract_lark_response_code(body).unwrap_or(0);
    if code != 0 {
        anyhow::bail!("send failed {context}: code={code}, body={body}");
    }

    Ok(())
}

/// State carried between sending an approval card and the user's click.
///
/// Used to (a) wake the awaiting future via `sender` and (b) re-render
/// the card after the click so the buttons disappear.
struct PendingApproval {
    sender: tokio::sync::oneshot::Sender<zeroclaw_api::channel::ChannelApprovalResponse>,
    /// `data.message_id` returned by the send-card POST. Empty string is a
    /// sentinel meaning "card was sent but message_id was missing from the
    /// response" — handler will skip the post-click PATCH in that case.
    message_id: String,
    tool_name: String,
    arguments_summary: String,
}

/// Lark/Feishu channel.
///
/// Supports two receive modes (configured via `receive_mode` in config):
/// - **`websocket`** (default): persistent WSS long-connection; no public URL needed.
/// - **`webhook`**: HTTP callback server; requires a public HTTPS endpoint.
#[derive(Clone)]
pub struct LarkChannel {
    app_id: String,
    app_secret: String,
    verification_token: String,
    port: Option<u16>,
    /// The alias key under `[channels.lark.<alias>]` this handle is bound to.
    /// Used to scope peer-group writes and resolver lookups. (Pre-V3 Feishu
    /// blocks are folded into `[channels.lark]` with `use_feishu = true`.)
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Bot open_id resolved at runtime via `/bot/v3/info`.
    resolved_bot_open_id: Arc<StdRwLock<Option<String>>>,
    mention_only: bool,
    /// Platform variant: Lark (international) or Feishu (CN).
    platform: LarkPlatform,
    /// How to receive events: WebSocket long-connection or HTTP webhook.
    receive_mode: zeroclaw_config::schema::LarkReceiveMode,
    /// Cached tenant access token
    tenant_token: Arc<RwLock<Option<CachedTenantToken>>>,
    /// Dedup set: WS message_ids seen in last ~30 min to prevent double-dispatch
    ws_seen_ids: Arc<RwLock<HashMap<String, Instant>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
    /// In-flight approval requests keyed by `approval_id` (UUID v4).
    /// Populated by `request_approval`, drained by `handle_card_action_event`.
    pending_approvals: Arc<tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>>,
    /// Seconds to wait for the user's button click before auto-denying.
    /// Set by the orchestrator from
    /// `[channels.lark.<alias>].approval_timeout_secs` via
    /// [`Self::with_approval_timeout_secs`]. Schema default is 300s
    /// (matches the channel-wide standard used by Telegram, Discord, etc.);
    /// `LarkChannel::new()` seeds 120 as a conservative fallback for the
    /// rare construction path that bypasses the builder.
    approval_timeout_secs: u64,
    /// When `true`, [`Self::resolve_sender`] keys group-chat sessions on the
    /// sending user's `open_id` instead of the group's `chat_id`. Default
    /// `false` preserves the existing shared-session behavior. Set via
    /// [`Self::with_per_user_session`] from
    /// `[channels.lark.<alias>].per_user_session`.
    per_user_session: bool,
    /// Cache of `(message_id, unicode_emoji) -> reaction_id` populated by
    /// `add_reaction` so a subsequent `remove_reaction` call can issue
    /// `DELETE /im/v1/messages/{message_id}/reactions/{reaction_id}`
    /// without first re-listing reactions on the message.
    ///
    /// Lifetime: process-local, lost on restart. Reactions added before a
    /// restart are unreachable (acceptable degradation — by then the user
    /// has scrolled past those messages). The cached value is a Feishu
    /// API-returned token (runtime state), not a duplicate of any config
    /// field; SSOT does not apply.
    reaction_ids: Arc<tokio::sync::Mutex<std::collections::HashMap<(String, String), String>>>,
    /// Controls progressive draft-card streaming. `Off` (default) routes
    /// every response through `send()`; `Partial` opens a draft card and
    /// edits it incrementally via `update_draft` / `finalize_draft`.
    /// Set by the orchestrator from `[channels.lark.<alias>].stream_mode`
    /// via [`Self::with_streaming`].
    stream_mode: StreamMode,
    /// Minimum interval between consecutive PATCH edits of the same draft
    /// card. Tunes to Feishu's 5 QPS per-message cap. Set by the
    /// orchestrator from `[channels.lark.<alias>].draft_update_interval_ms`
    /// via [`Self::with_streaming`].
    draft_update_interval_ms: u64,
    /// Per-`message_id` timestamp of the last successful PATCH. Reads /
    /// writes are guarded by an async mutex so concurrent token streams
    /// cooperate on the same draft without racing the rate-limit window.
    /// Runtime state (not a config duplicate per SSOT) — bounded by the
    /// number of in-flight drafts; entries are removed by `finalize_draft`
    /// and `cancel_draft`.
    last_draft_edit: Arc<tokio::sync::Mutex<HashMap<String, Instant>>>,
    #[cfg(test)]
    api_base_override: Option<String>,
}

impl LarkChannel {
    pub fn new(
        app_id: String,
        app_secret: String,
        verification_token: String,
        port: Option<u16>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        mention_only: bool,
    ) -> Self {
        Self::new_with_platform(
            app_id,
            app_secret,
            verification_token,
            port,
            alias,
            peer_resolver,
            mention_only,
            LarkPlatform::Lark,
        )
    }

    /// Return the alias under `[channels.lark.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    fn new_with_platform(
        app_id: String,
        app_secret: String,
        verification_token: String,
        port: Option<u16>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        mention_only: bool,
        platform: LarkPlatform,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            verification_token,
            port,
            alias: alias.into(),
            peer_resolver,
            resolved_bot_open_id: Arc::new(StdRwLock::new(None)),
            mention_only,
            platform,
            receive_mode: zeroclaw_config::schema::LarkReceiveMode::default(),
            tenant_token: Arc::new(RwLock::new(None)),
            ws_seen_ids: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
            pending_approvals: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            approval_timeout_secs: 120,
            per_user_session: false,
            reaction_ids: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: 1000,
            last_draft_edit: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            #[cfg(test)]
            api_base_override: None,
        }
    }

    /// Build from `LarkConfig` using legacy compatibility:
    /// when `use_feishu=true`, this instance routes to Feishu endpoints.
    pub fn from_config(
        config: &zeroclaw_config::schema::LarkConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        let platform = if config.use_feishu {
            LarkPlatform::Feishu
        } else {
            LarkPlatform::Lark
        };
        let mut ch = Self::new_with_platform(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.verification_token.clone().unwrap_or_default(),
            config.port,
            alias,
            peer_resolver,
            config.mention_only,
            platform,
        );
        ch.receive_mode = config.receive_mode.clone();
        ch.proxy_url = config.proxy_url.clone();
        ch
    }

    /// Override the default approval timeout (300s) — set by the
    /// orchestrator from `[channels.lark.<alias>].approval_timeout_secs`.
    pub fn with_approval_timeout_secs(mut self, secs: u64) -> Self {
        self.approval_timeout_secs = secs;
        self
    }

    /// Configure whether group-chat sessions key on the sender's `open_id`
    /// (per-user isolation) or on `chat_id` (shared session). No effect on
    /// 1-on-1 chats (where `chat_id` is already unique per user-bot pair).
    /// Set by the orchestrator from `[channels.lark.<alias>].per_user_session`.
    pub fn with_per_user_session(mut self, enabled: bool) -> Self {
        self.per_user_session = enabled;
        self
    }

    /// Configure progressive draft-card streaming. `stream_mode = Off`
    /// (default) keeps the existing behavior; `Partial` opens a Feishu
    /// interactive card via `send_draft`, edits it via `update_draft`
    /// (rate-limited to `draft_update_interval_ms`), and commits via
    /// `finalize_draft`. Mirrors the `TelegramChannel::with_streaming`
    /// builder pattern; set by the orchestrator from
    /// `[channels.lark.<alias>].{stream_mode, draft_update_interval_ms}`.
    pub fn with_streaming(
        mut self,
        stream_mode: StreamMode,
        draft_update_interval_ms: u64,
    ) -> Self {
        let effective_stream_mode = match stream_mode {
            StreamMode::MultiMessage => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,),
                    "lark: stream_mode=multi_message is not supported by Feishu's editable-card surface; falling back to off (no draft streaming). Use stream_mode=partial for incremental card edits."
                );
                StreamMode::Off
            }
            other => other,
        };
        self.stream_mode = effective_stream_mode;
        self.draft_update_interval_ms = draft_update_interval_ms;
        self
    }

    /// Decide which key to use as the [`ChannelMessage::sender`] field for
    /// an inbound message. When `per_user_session = true`, returns the
    /// sender's `open_id`, falling back to `chat_id` whenever the platform
    /// omits the `open_id` (e.g. composer / edit events) or passes an empty
    /// string. When `per_user_session = false` (default), always returns
    /// `chat_id`, so every message in a chat shares the same agent session.
    /// Pure function: no I/O, lifetime-bound to the inputs so callers can
    /// avoid an extra `to_string()` until the final assembly.
    fn resolve_sender<'a>(&self, chat_id: &'a str, sender_open_id: Option<&'a str>) -> &'a str {
        if self.per_user_session {
            match sender_open_id {
                Some(oid) if !oid.is_empty() => oid,
                _ => chat_id,
            }
        } else {
            chat_id
        }
    }

    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                // Bind the sole registered provider as the agent transcription
                // provider for the channel-direct ingest path. Multi-provider
                // setups still resolve via the orchestrator's per-agent
                // routing (see orchestrator/mod.rs). See wati.rs for full
                // rationale.
                let names = m.available_providers();
                let m = if names.len() == 1 {
                    let only = names[0].to_string();
                    m.with_agent_transcription_provider(only)
                } else {
                    m
                };
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, audio transcription disabled"
                );
            }
        }
        self.transcription = Some(config);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            self.platform.proxy_service_key(),
            self.proxy_url.as_deref(),
        )
    }

    fn channel_name(&self) -> &'static str {
        self.platform.channel_name()
    }

    fn api_base(&self) -> &str {
        #[cfg(test)]
        if let Some(ref url) = self.api_base_override {
            return url.as_str();
        }
        self.platform.api_base()
    }

    fn ws_base(&self) -> &'static str {
        self.platform.ws_base()
    }

    fn tenant_access_token_url(&self) -> String {
        format!("{}/auth/v3/tenant_access_token/internal", self.api_base())
    }

    fn bot_info_url(&self) -> String {
        format!("{}/bot/v3/info", self.api_base())
    }

    fn send_message_url(&self) -> String {
        format!("{}/im/v1/messages?receive_id_type=chat_id", self.api_base())
    }

    /// PATCH endpoint for updating the content of a previously-sent message
    /// (used to flip an approval card from its interactive state to its
    /// resolved/banner state after the user clicks a button).
    fn patch_message_url(&self, message_id: &str) -> String {
        format!("{}/im/v1/messages/{message_id}", self.api_base())
    }

    fn message_reaction_url(&self, message_id: &str) -> String {
        format!("{}/im/v1/messages/{message_id}/reactions", self.api_base())
    }

    fn delete_message_reaction_url(&self, message_id: &str, reaction_id: &str) -> String {
        format!(
            "{}/im/v1/messages/{message_id}/reactions/{reaction_id}",
            self.api_base()
        )
    }

    fn image_resource_url(&self, message_id: &str, image_key: &str) -> String {
        format!(
            "{}/im/v1/messages/{message_id}/resources/{image_key}?type=image",
            self.api_base()
        )
    }

    fn file_download_url(&self, message_id: &str, file_key: &str) -> String {
        format!(
            "{}/im/v1/messages/{message_id}/resources/{file_key}?type=file",
            self.api_base()
        )
    }

    fn resolved_bot_open_id(&self) -> Option<String> {
        self.resolved_bot_open_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn set_resolved_bot_open_id(&self, open_id: Option<String>) {
        if let Ok(mut guard) = self.resolved_bot_open_id.write() {
            *guard = open_id;
        }
    }

    async fn post_message_reaction_with_token(
        &self,
        message_id: &str,
        token: &str,
        emoji_type: &str,
    ) -> anyhow::Result<reqwest::Response> {
        let url = self.message_reaction_url(message_id);
        let body = serde_json::json!({
            "reaction_type": {
                "emoji_type": emoji_type
            }
        });

        let response = self
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await?;

        Ok(response)
    }

    /// POST /callback/ws/endpoint → (wss_url, client_config)
    async fn get_ws_endpoint(&self) -> anyhow::Result<(String, WsClientConfig)> {
        let resp = self
            .http_client()
            .post(format!("{}/callback/ws/endpoint", self.ws_base()))
            .header("locale", self.platform.locale_header())
            .json(&serde_json::json!({
                "AppID": self.app_id,
                "AppSecret": self.app_secret,
            }))
            .send()
            .await?
            .json::<WsEndpointResp>()
            .await?;
        if resp.code != 0 {
            anyhow::bail!(
                "WS endpoint failed: code={} msg={}",
                resp.code,
                resp.msg.as_deref().unwrap_or("(none)")
            );
        }
        let ep = resp.data.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "WS endpoint: empty data"
            );
            anyhow::Error::msg("WS endpoint: empty data")
        })?;
        Ok((ep.url, ep.client_config.unwrap_or_default()))
    }

    /// WS long-connection event loop.  Returns Ok(()) when the connection closes
    /// (the caller reconnects).
    #[allow(clippy::too_many_lines)]
    async fn listen_ws(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        self.ensure_bot_open_id().await;
        let (wss_url, client_config) = self.get_ws_endpoint().await?;
        let service_id = wss_url
            .split('?')
            .nth(1)
            .and_then(|qs| {
                qs.split('&')
                    .find(|kv| kv.starts_with("service_id="))
                    .and_then(|kv| kv.split('=').nth(1))
                    .and_then(|v| v.parse::<i32>().ok())
            })
            .unwrap_or(0);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"wss_url": wss_url})),
            "connecting to"
        );

        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &wss_url,
            "channel.lark",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"service_id": service_id})),
            "WS connected (service_id=)"
        );

        let mut ping_secs = client_config.ping_interval.unwrap_or(120).max(10);
        let mut hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
        let mut timeout_check = tokio::time::interval(Duration::from_secs(10));
        hb_interval.tick().await; // consume immediate tick

        let mut seq: u64 = 0;
        let mut last_recv = Instant::now();

        // Send initial ping immediately (like the official SDK) so the server
        // starts responding with pongs and we can calibrate the ping_interval.
        seq = seq.wrapping_add(1);
        let initial_ping = PbFrame {
            seq_id: seq,
            log_id: 0,
            service: service_id,
            method: 0,
            headers: vec![PbHeader {
                key: "type".into(),
                value: "ping".into(),
            }],
            payload: None,
        };
        if write
            .send(WsMsg::Binary(initial_ping.encode_to_vec().into()))
            .await
            .is_err()
        {
            anyhow::bail!("initial ping failed");
        }
        // message_id → (fragment_slots, created_at) for multi-part reassembly
        type FragEntry = (Vec<Option<Vec<u8>>>, Instant);
        let mut frag_cache: HashMap<String, FragEntry> = HashMap::new();

        loop {
            tokio::select! {
                biased;

                _ = hb_interval.tick() => {
                    seq = seq.wrapping_add(1);
                    let ping = PbFrame {
                        seq_id: seq, log_id: 0, service: service_id, method: 0,
                        headers: vec![PbHeader { key: "type".into(), value: "ping".into() }],
                        payload: None,
                    };
                    if write.send(WsMsg::Binary(ping.encode_to_vec().into())).await.is_err() {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "ping failed, reconnecting");
                        break;
                    }
                    // GC stale fragments > 5 min
                    let cutoff = Instant::now().checked_sub(Duration::from_secs(300)).unwrap_or(Instant::now());
                    frag_cache.retain(|_, (_, ts)| *ts > cutoff);
                }

                _ = timeout_check.tick() => {
                    if last_recv.elapsed() > WS_HEARTBEAT_TIMEOUT {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "heartbeat timeout, reconnecting");
                        break;
                    }
                }

                msg = read.next() => {
                    let raw = match msg {
                        Some(Ok(ws_msg)) => {
                            if should_refresh_last_recv(&ws_msg) {
                                last_recv = Instant::now();
                            }
                            match ws_msg {
                                WsMsg::Binary(b) => b,
                                WsMsg::Ping(d) => { let _ = write.send(WsMsg::Pong(d)).await; continue; }
                                WsMsg::Close(_) => { ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WS closed — reconnecting"); break; }
                                _ => continue,
                            }
                        }
                        None => { ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WS closed — reconnecting"); break; }
                        Some(Err(e)) => { ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "WS read error"); break; }
                    };

                    let frame = match PbFrame::decode(&raw[..]) {
                        Ok(f) => f,
                        Err(e) => { ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "proto decode"); continue; }
                    };

                    // CONTROL frame
                    if frame.method == 0 {
                        if frame.header_value("type") == "pong"
                            && let Some(p) = &frame.payload
                                && let Ok(cfg) = serde_json::from_slice::<WsClientConfig>(p)
                                    && let Some(secs) = cfg.ping_interval {
                                        let secs = secs.max(10);
                                        if secs != ping_secs {
                                            ping_secs = secs;
                                            hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
                                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"ping_secs": ping_secs})), "ping_interval → s");
                                        }
                                    }
                        continue;
                    }

                    // DATA frame
                    let msg_type = frame.header_value("type").to_string();
                    let msg_id   = frame.header_value("message_id").to_string();
                    let sum      = frame.header_value("sum").parse::<usize>().unwrap_or(1);
                    let seq_num  = frame.header_value("seq").parse::<usize>().unwrap_or(0);

                    // ACK immediately (Feishu requires within 3 s)
                    {
                        let mut ack = frame.clone();
                        ack.payload = Some(br#"{"code":200,"headers":{},"data":[]}"#.to_vec());
                        ack.headers.push(PbHeader { key: "biz_rt".into(), value: "0".into() });
                        let _ = write.send(WsMsg::Binary(ack.encode_to_vec().into())).await;
                    }

                    // Fragment reassembly
                    let sum = if sum == 0 { 1 } else { sum };
                    let payload: Vec<u8> = if sum == 1 || msg_id.is_empty() || seq_num >= sum {
                        frame.payload.clone().unwrap_or_default()
                    } else {
                        let entry = frag_cache.entry(msg_id.clone())
                            .or_insert_with(|| (vec![None; sum], Instant::now()));
                        if entry.0.len() != sum { *entry = (vec![None; sum], Instant::now()); }
                        entry.0[seq_num] = frame.payload.clone();
                        if entry.0.iter().all(|s| s.is_some()) {
                            let full: Vec<u8> = entry.0.iter()
                                .flat_map(|s| s.as_deref().unwrap_or(&[]))
                                .copied().collect();
                            frag_cache.remove(&msg_id);
                            full
                        } else { continue; }
                    };

                    if msg_type != "event" { continue; }

                    let event: LarkEvent = match serde_json::from_slice(&payload) {
                        Ok(e) => e,
                        Err(e) => { ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "event JSON"); continue; }
                    };
                    match event.header.event_type.as_str() {
                        "im.message.receive_v1" => {}
                        "card.action.trigger" => {
                            if let Err(e) = self.handle_card_action_event(&event.event).await {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Dispatch
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({"error": e.to_string()})),
                                    "Lark WS: card action dispatch error"
                                );
                            }
                            continue;
                        }
                        _ => continue,
                    }

                    let event_payload = event.event;

                    let recv: MsgReceivePayload = match serde_json::from_value(event_payload.clone()) {
                        Ok(r) => r,
                        Err(e) => { ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "payload parse"); continue; }
                    };

                    if recv.sender.sender_type == "app" || recv.sender.sender_type == "bot" { continue; }

                    let sender_open_id = recv.sender.sender_id.open_id.as_deref().unwrap_or("");
                    if !self.is_user_allowed(sender_open_id) {
                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"sender_open_id": sender_open_id})), "WS: ignoring (not in peer group)");
                        continue;
                    }

                    let lark_msg = &recv.message;

                    // Dedup
                    {
                        let now = Instant::now();
                        let mut seen = self.ws_seen_ids.write().await;
                        // GC
                        seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(30 * 60));
                        if seen.contains_key(&lark_msg.message_id) {
                            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WS: dup {}", lark_msg.message_id));
                            continue;
                        }
                        seen.insert(lark_msg.message_id.clone(), now);
                    }

                    // Decode content by type (mirrors clawdbot-feishu parsing)
                    let (text, post_mentioned_open_ids) = match lark_msg.message_type.as_str() {
                        "text" => {
                            let v: serde_json::Value = match serde_json::from_str(&lark_msg.content) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            match v.get("text").and_then(|t| t.as_str()).filter(|s| !s.is_empty()) {
                                Some(t) => (t.to_string(), Vec::new()),
                                None => continue,
                            }
                        }
                        "post" => match parse_post_content_details(&lark_msg.content) {
                            Some(details) => (details.text, details.mentioned_open_ids),
                            None => continue,
                        },
                        "image" => {
                            let v: serde_json::Value = match serde_json::from_str(&lark_msg.content) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let image_key = match v.get("image_key").and_then(|k| k.as_str()) {
                                Some(k) => k.to_string(),
                                None => { ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WS: image message missing image_key"); continue; }
                            };
                            match self.download_image_as_marker(&lark_msg.message_id, &image_key).await {
                                Some(marker) => (marker, Vec::new()),
                                None => {
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"image_key": image_key})), "WS: failed to download image");
                                    (format!("[IMAGE:{image_key} | download failed]"), Vec::new())
                                }
                            }
                        }
                        "file" => {
                            let v: serde_json::Value = match serde_json::from_str(&lark_msg.content) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let file_key = match v.get("file_key").and_then(|k| k.as_str()) {
                                Some(k) => k.to_string(),
                                None => { ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WS: file message missing file_key"); continue; }
                            };
                            let file_name = v.get("file_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown_file")
                                .to_string();
                            match self.download_file_as_content(&lark_msg.message_id, &file_key, &file_name).await {
                                Some(content) => (content, Vec::new()),
                                None => {
                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"file_key": file_key})), "WS: failed to download file");
                                    (format!("[ATTACHMENT:{file_name} | download failed]"), Vec::new())
                                }
                            }
                        }
                        "audio" => {
                            let Some(manager) = self.transcription_manager.as_deref() else {
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WS: audio message in {} (transcription not configured)", lark_msg.chat_id));
                                continue;
                            };
                            let transcript = self.try_transcribe_audio_message(
                                &lark_msg.message_id,
                                &lark_msg.content,
                                manager,
                            ).await;
                            let Some(text) = transcript else { continue; };
                            (text, Vec::new())
                        }
                        "list" => match parse_list_content(&lark_msg.content) {
                            Some(t) => (t, Vec::new()),
                            None => { ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WS: list message with no extractable text"); continue; }
                        },
                        _ => { ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WS: skipping unsupported type '{}'", lark_msg.message_type)); continue; }
                    };

                    let text = text.trim().to_string();
                    if text.is_empty() { continue; }

                    // Group-chat: only respond when explicitly @-mentioned
                    let bot_open_id = self.resolved_bot_open_id();
                    if lark_msg.chat_type == "group"
                        && !should_respond_in_group(
                            self.mention_only,
                            bot_open_id.as_deref(),
                            &lark_msg.mentions,
                            &post_mentioned_open_ids,
                        )
                    {
                        continue;
                    }

                    // Inbound fast-ack: spawn the 👀 reaction immediately so the
                    // user sees a "received" signal within ~100ms instead of
                    // waiting for the orchestrator's classifier/memory/streaming
                    // pipeline (which can take several seconds before the generic
                    // Channel::add_reaction call would otherwise fire).
                    //
                    // CRITICAL: this spawn MUST go through the trait
                    // `Channel::add_reaction` so that Feishu's returned
                    // reaction_id is written into the shared `reaction_ids`
                    // cache. The trait impl also has a cache-hit dedupe
                    // fast-path, so the later generic orchestrator call to
                    // add_reaction("👀") becomes a no-op instead of a duplicate
                    // POST. This is the "same cached reaction-id contract"
                    // requested by the PR review: fast-ack and generic path
                    // share a single cache, so `remove_reaction("👀")` always
                    // finds the right reaction_id and no orphan 👀 is left
                    // beside the completion marker. See lifecycle regression
                    // tests `lark_inbound_ack_lifecycle_*` and
                    // `lark_fast_ack_and_generic_path_dedupe_on_cache_hit`.
                    let reaction_channel = self.clone();
                    let reaction_message_id = lark_msg.message_id.clone();
                    let reaction_reply_target = lark_msg.chat_id.clone();
                    zeroclaw_spawn::spawn!(async move {
                        if let Err(e) = <LarkChannel as Channel>::add_reaction(
                            &reaction_channel,
                            &reaction_reply_target,
                            &reaction_message_id,
                            "\u{1F440}",
                        )
                        .await
                        {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note,
                                )
                                .with_attrs(::serde_json::json!({
                                    "message_id": reaction_message_id,
                                    "error": format!("{e}"),
                                    "error_key": "lark.inbound_fast_ack.failed",
                                })),
                                "Lark inbound fast-ack failed (soft)"
                            );
                        }
                    });

                    let channel_msg = ChannelMessage {
                        id: lark_msg.message_id.clone(),
                        sender: self
                            .resolve_sender(&lark_msg.chat_id, Some(sender_open_id))
                            .to_string(),
                        reply_target: lark_msg.chat_id.clone(),
                        content: text,
                        channel: self.channel_name().to_string(),
            channel_alias: Some(self.alias.clone()),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                        interruption_scope_id: None,
                    attachments: vec![],
                        subject: None,
                    };

                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WS: message in {}", lark_msg.chat_id));
                    if tx.send(channel_msg).await.is_err() { break; }
                }
            }
        }
        Ok(())
    }

    /// Check if a user open_id is allowed
    fn is_user_allowed(&self, open_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, open_id, crate::allowlist::Match::Sensitive)
    }

    /// Get or refresh tenant access token
    async fn get_tenant_access_token(&self) -> anyhow::Result<String> {
        // Check cache first
        {
            let cached = self.tenant_token.read().await;
            if let Some(ref token) = *cached
                && Instant::now() < token.refresh_after
            {
                return Ok(token.value.clone());
            }
        }

        let url = self.tenant_access_token_url();
        let body = serde_json::json!({
            "app_id": self.app_id,
            "app_secret": self.app_secret,
        });

        let resp = self.http_client().post(&url).json(&body).send().await?;
        let status = resp.status();
        let data: serde_json::Value = resp.json().await?;

        if !status.is_success() {
            anyhow::bail!("tenant_access_token request failed: status={status}, body={data}");
        }

        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = data
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("tenant_access_token failed: {msg}");
        }

        let token = data
            .get("tenant_access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "missing tenant_access_token in response"
                );
                anyhow::Error::msg("missing tenant_access_token in response")
            })?
            .to_string();

        let ttl_seconds = extract_lark_token_ttl_seconds(&data);
        let refresh_after = next_token_refresh_deadline(Instant::now(), ttl_seconds);

        // Cache it with proactive refresh metadata.
        {
            let mut cached = self.tenant_token.write().await;
            *cached = Some(CachedTenantToken {
                value: token.clone(),
                refresh_after,
            });
        }

        Ok(token)
    }

    /// Invalidate cached token (called when API reports an expired tenant token).
    async fn invalidate_token(&self) {
        let mut cached = self.tenant_token.write().await;
        *cached = None;
    }

    /// Download an image from the Lark API and return an `[IMAGE:data:...]` marker string.
    async fn download_image_as_marker(&self, message_id: &str, image_key: &str) -> Option<String> {
        let url = self.image_resource_url(message_id, image_key);
        let mut retried_token = false;

        loop {
            let token = match self.get_tenant_access_token().await {
                Ok(t) => t,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "failed to get token for image download"
                    );
                    return None;
                }
            };

            let resp = match self
                .http_client()
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "image_key": image_key})
                            ),
                        "image download request failed for"
                    );
                    return None;
                }
            };

            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && !retried_token {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"image_key": image_key})),
                    "image download 401, refreshing token and retrying once"
                );
                drop(resp);
                self.invalidate_token().await;
                retried_token = true;
                continue;
            }

            if !resp.status().is_success() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "image download failed for {image_key}: status={}",
                        resp.status()
                    )
                );
                return None;
            }

            if let Some(cl) = resp.content_length()
                && cl > LARK_IMAGE_MAX_BYTES as u64
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"image_key": image_key, "cl": cl})),
                    "image too large for : bytes exceeds limit"
                );
                return None;
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);

            let bytes = match resp.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "image_key": image_key})
                            ),
                        "image body read failed for"
                    );
                    return None;
                }
            };

            if bytes.is_empty() || bytes.len() > LARK_IMAGE_MAX_BYTES {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "image body empty or too large for {image_key}: {} bytes",
                        bytes.len()
                    )
                );
                return None;
            }

            let mime = lark_detect_image_mime(content_type.as_deref(), &bytes)?;
            if !LARK_SUPPORTED_IMAGE_MIMES.contains(&mime.as_str()) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"image_key": image_key, "mime": mime})),
                    "unsupported image MIME for"
                );
                return None;
            }

            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Some(format!("[IMAGE:data:{mime};base64,{encoded}]"));
        }
    }

    /// Download a file from the Lark API and return a text content marker.
    /// For text-like files, the content is inlined. For binary files, a summary is returned.
    async fn download_file_as_content(
        &self,
        message_id: &str,
        file_key: &str,
        file_name: &str,
    ) -> Option<String> {
        let token = match self.get_tenant_access_token().await {
            Ok(t) => t,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "failed to get token for file download"
                );
                return None;
            }
        };

        let url = self.file_download_url(message_id, file_key);
        let resp = match self
            .http_client()
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "file_key": file_key})
                        ),
                    "file download request failed for"
                );
                return None;
            }
        };

        if !resp.status().is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "file download failed for {file_key}: status={}",
                    resp.status()
                )
            );
            return None;
        }

        if let Some(cl) = resp.content_length()
            && cl > LARK_FILE_MAX_BYTES as u64
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"file_key": file_key, "cl": cl})),
                "file too large for : bytes exceeds limit"
            );
            return Some(format!(
                "[ATTACHMENT:{file_name} | size={cl} bytes | too large to inline]"
            ));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "file_key": file_key})
                        ),
                    "file body read failed for"
                );
                return None;
            }
        };

        if bytes.is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"file_key": file_key})),
                "file body is empty for"
            );
            return None;
        }

        // If the content is image-like, return as image marker
        if content_type.starts_with("image/")
            && bytes.len() <= LARK_IMAGE_MAX_BYTES
            && let Some(mime) = lark_detect_image_mime(Some(&content_type), &bytes)
            && LARK_SUPPORTED_IMAGE_MIMES.contains(&mime.as_str())
        {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Some(format!("[IMAGE:data:{mime};base64,{encoded}]"));
        }

        // If the file looks like text, inline it
        if bytes.len() <= LARK_FILE_MAX_BYTES
            && !bytes.contains(&0)
            && (content_type.starts_with("text/")
                || content_type.contains("json")
                || content_type.contains("xml")
                || content_type.contains("yaml")
                || content_type.contains("javascript")
                || content_type.contains("csv")
                || lark_is_text_filename(file_name))
        {
            let text = String::from_utf8_lossy(&bytes);
            let truncated = lark_inline_text_file_preview(text);
            let ext = file_name.rsplit('.').next().unwrap_or("text");
            return Some(format!("[FILE:{file_name}]\n```{ext}\n{truncated}\n```"));
        }

        Some(format!(
            "[ATTACHMENT:{file_name} | mime={content_type} | size={} bytes]",
            bytes.len()
        ))
    }

    async fn fetch_bot_open_id_with_token(
        &self,
        token: &str,
    ) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
        let resp = self
            .http_client()
            .get(self.bot_info_url())
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?;
        let status = resp.status();
        let body = resp
            .json::<serde_json::Value>()
            .await
            .unwrap_or_else(|_| serde_json::json!({}));
        Ok((status, body))
    }

    async fn refresh_bot_open_id(&self) -> anyhow::Result<Option<String>> {
        let token = self.get_tenant_access_token().await?;
        let (status, body) = self.fetch_bot_open_id_with_token(&token).await?;

        let body = if should_refresh_lark_tenant_token(status, &body) {
            self.invalidate_token().await;
            let refreshed = self.get_tenant_access_token().await?;
            let (retry_status, retry_body) = self.fetch_bot_open_id_with_token(&refreshed).await?;
            if !retry_status.is_success() {
                anyhow::bail!(
                    "bot info request failed after token refresh: status={retry_status}, body={retry_body}"
                );
            }
            retry_body
        } else {
            if !status.is_success() {
                anyhow::bail!("bot info request failed: status={status}, body={body}");
            }
            body
        };

        let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!("bot info failed: code={code}, body={body}");
        }

        let bot_open_id = body
            .pointer("/bot/open_id")
            .or_else(|| body.pointer("/data/bot/open_id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);

        self.set_resolved_bot_open_id(bot_open_id.clone());
        Ok(bot_open_id)
    }

    async fn ensure_bot_open_id(&self) {
        if !self.mention_only || self.resolved_bot_open_id().is_some() {
            return;
        }

        match self.refresh_bot_open_id().await {
            Ok(Some(open_id)) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"open_id": open_id})),
                    "resolved bot open_id"
                );
            }
            Ok(None) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "bot open_id missing from /bot/v3/info response; mention_only group messages will be ignored"
                );
            }
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"err": err.to_string()})),
                    "failed to resolve bot open_id: ; mention_only group messages will be ignored"
                );
            }
        }
    }

    async fn stream_audio_bytes(mut resp: reqwest::Response) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > MAX_LARK_AUDIO_BYTES {
                anyhow::bail!("audio download exceeds {} byte limit", MAX_LARK_AUDIO_BYTES);
            }
        }
        Ok(body)
    }

    async fn download_audio_resource(
        &self,
        message_id: &str,
        file_key: &str,
    ) -> anyhow::Result<(Vec<u8>, String)> {
        let url = format!(
            "{}/im/v1/messages/{message_id}/resources/{file_key}?type=file",
            self.api_base()
        );
        let token = self.get_tenant_access_token().await?;
        let resp = self
            .http_client()
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            let body: serde_json::Value =
                serde_json::from_str(&body_text).unwrap_or_else(|_| serde_json::json!({}));

            if should_refresh_lark_tenant_token(status, &body) {
                self.invalidate_token().await;
                let token = self.get_tenant_access_token().await?;
                let resp = self
                    .http_client()
                    .get(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .send()
                    .await?;
                if !resp.status().is_success() {
                    anyhow::bail!(
                        "audio download failed after token refresh: {}",
                        resp.status()
                    );
                }
                let bytes = Self::stream_audio_bytes(resp).await?;
                return Ok((bytes, inferred_audio_filename(file_key)));
            }

            anyhow::bail!("audio download failed: {}", status);
        }
        let bytes = Self::stream_audio_bytes(resp).await?;
        Ok((bytes, inferred_audio_filename(file_key)))
    }

    async fn try_transcribe_audio_message(
        &self,
        message_id: &str,
        content: &str,
        manager: &super::transcription::TranscriptionManager,
    ) -> Option<String> {
        let file_key = serde_json::from_str::<serde_json::Value>(content)
            .ok()
            .and_then(|v| {
                v.get("file_key")
                    .and_then(|k| k.as_str())
                    .map(str::to_owned)
            })?;

        let (audio_data, filename) = match self.download_audio_resource(message_id, &file_key).await
        {
            Ok(result) => result,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "message_id": message_id})
                        ),
                    "audio download failed for"
                );
                return None;
            }
        };

        match manager.transcribe(&audio_data, &filename).await {
            Ok(transcript) => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"message_id": message_id})),
                    "audio transcribed for"
                );
                Some(transcript)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "message_id": message_id})
                        ),
                    "transcription failed for"
                );
                None
            }
        }
    }

    pub async fn parse_event_payload_async(
        &self,
        payload: &serde_json::Value,
    ) -> Vec<ChannelMessage> {
        let event_type = payload
            .pointer("/header/event_type")
            .and_then(|e| e.as_str())
            .unwrap_or("");
        if event_type != "im.message.receive_v1" {
            return vec![];
        }

        let msg_type = payload
            .pointer("/event/message/message_type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        if msg_type != "audio" {
            return self.parse_event_payload(payload).await;
        }

        let Some(manager) = self.transcription_manager.as_deref() else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "webhook: audio message (transcription not configured)"
            );
            return vec![];
        };

        let open_id = payload
            .pointer("/event/sender/sender_id/open_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !self.is_user_allowed(open_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"open_id": open_id})),
                "ignoring audio from unauthorized user"
            );
            return vec![];
        }

        let message_id = payload
            .pointer("/event/message/message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let content = payload
            .pointer("/event/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let chat_id = payload
            .pointer("/event/message/chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or(open_id);

        let chat_type = payload
            .pointer("/event/message/chat_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mentions = payload
            .pointer("/event/message/mentions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let bot_open_id = self.resolved_bot_open_id();
        if chat_type == "group"
            && !should_respond_in_group(
                self.mention_only,
                bot_open_id.as_deref(),
                &mentions,
                &Vec::new(),
            )
        {
            return vec![];
        }

        let Some(text) = self
            .try_transcribe_audio_message(message_id, content, manager)
            .await
        else {
            return vec![];
        };

        let timestamp = payload
            .pointer("/event/message/create_time")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse::<u64>().ok())
            .map(|ms| ms / 1000)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        vec![ChannelMessage {
            id: message_id.to_string(),
            sender: self.resolve_sender(chat_id, Some(open_id)).to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        }]
    }

    async fn send_text_once(
        &self,
        url: &str,
        token: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
        let resp = self
            .http_client()
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({ "raw": raw }));
        Ok((status, parsed))
    }

    /// Parse an event callback payload and extract messages.
    /// Supports text, post, image, and file message types.
    pub async fn parse_event_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Lark event v2 structure:
        // { "header": { "event_type": "im.message.receive_v1" }, "event": { "message": { ... }, "sender": { ... } } }
        let event_type = payload
            .pointer("/header/event_type")
            .and_then(|e| e.as_str())
            .unwrap_or("");

        if event_type != "im.message.receive_v1" {
            return messages;
        }

        let event = match payload.get("event") {
            Some(e) => e,
            None => return messages,
        };

        // Extract sender open_id
        let open_id = event
            .pointer("/sender/sender_id/open_id")
            .and_then(|s| s.as_str())
            .unwrap_or("");

        if open_id.is_empty() {
            return messages;
        }

        // Check allowlist
        if !self.is_user_allowed(open_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"open_id": open_id})),
                "ignoring message from unauthorized user"
            );
            return messages;
        }

        // Extract message content (text and post supported)
        let msg_type = event
            .pointer("/message/message_type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        let chat_type = event
            .pointer("/message/chat_type")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        let mentions = event
            .pointer("/message/mentions")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();

        let content_str = event
            .pointer("/message/content")
            .and_then(|c| c.as_str())
            .unwrap_or("");

        let evt_message_id = event
            .pointer("/message/message_id")
            .and_then(|m| m.as_str())
            .unwrap_or("");

        let (text, post_mentioned_open_ids): (String, Vec<String>) = match msg_type {
            "text" => {
                let extracted = serde_json::from_str::<serde_json::Value>(content_str)
                    .ok()
                    .and_then(|v| {
                        v.get("text")
                            .and_then(|t| t.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                    });
                match extracted {
                    Some(t) => (t, Vec::new()),
                    None => return messages,
                }
            }
            "post" => match parse_post_content_details(content_str) {
                Some(details) => (details.text, details.mentioned_open_ids),
                None => return messages,
            },
            "image" => {
                let image_key = serde_json::from_str::<serde_json::Value>(content_str)
                    .ok()
                    .and_then(|v| {
                        v.get("image_key")
                            .and_then(|k| k.as_str())
                            .map(String::from)
                    });
                match image_key {
                    Some(key) => {
                        let marker = match self.download_image_as_marker(evt_message_id, &key).await
                        {
                            Some(m) => m,
                            None => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"key": key})),
                                    "failed to download image"
                                );
                                format!("[IMAGE:{key} | download failed]")
                            }
                        };
                        (marker, Vec::new())
                    }
                    None => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            "image message missing image_key"
                        );
                        return messages;
                    }
                }
            }
            "file" => {
                let parsed = serde_json::from_str::<serde_json::Value>(content_str).ok();
                let file_key = parsed
                    .as_ref()
                    .and_then(|v| v.get("file_key").and_then(|k| k.as_str()))
                    .map(String::from);
                let file_name = parsed
                    .as_ref()
                    .and_then(|v| v.get("file_name").and_then(|n| n.as_str()))
                    .unwrap_or("unknown_file")
                    .to_string();
                match file_key {
                    Some(key) => {
                        let content = match self
                            .download_file_as_content(evt_message_id, &key, &file_name)
                            .await
                        {
                            Some(c) => c,
                            None => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"key": key})),
                                    "failed to download file"
                                );
                                format!("[ATTACHMENT:{file_name} | download failed]")
                            }
                        };
                        (content, Vec::new())
                    }
                    None => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            "file message missing file_key"
                        );
                        return messages;
                    }
                }
            }
            "list" => match parse_list_content(content_str) {
                Some(t) => (t, Vec::new()),
                None => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "list message with no extractable text"
                    );
                    return messages;
                }
            },
            _ => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"msg_type": msg_type})),
                    "skipping unsupported message type"
                );
                return messages;
            }
        };

        let bot_open_id = self.resolved_bot_open_id();
        if chat_type == "group"
            && !should_respond_in_group(
                self.mention_only,
                bot_open_id.as_deref(),
                &mentions,
                &post_mentioned_open_ids,
            )
        {
            return messages;
        }

        let timestamp = event
            .pointer("/message/create_time")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse::<u64>().ok())
            // Lark timestamps are in milliseconds
            .map(|ms| ms / 1000)
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        let chat_id = event
            .pointer("/message/chat_id")
            .and_then(|c| c.as_str())
            .unwrap_or(open_id);

        messages.push(ChannelMessage {
            id: evt_message_id.to_string(),
            sender: self.resolve_sender(chat_id, Some(open_id)).to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            channel_alias: Some(self.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        });

        messages
    }
}

impl ::zeroclaw_api::attribution::Attributable for LarkChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Lark)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for LarkChannel {
    fn name(&self) -> &str {
        self.channel_name()
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let token = self.get_tenant_access_token().await?;
        let url = self.send_message_url();

        let chunks = split_markdown_chunks(&message.content, LARK_CARD_MARKDOWN_MAX_BYTES);
        for chunk in &chunks {
            let body = build_interactive_card_body(&message.recipient, chunk);

            let (status, response) = self.send_text_once(&url, &token, &body).await?;

            if should_refresh_lark_tenant_token(status, &response) {
                // Token expired/invalid, invalidate and retry once.
                self.invalidate_token().await;
                let new_token = self.get_tenant_access_token().await?;
                let (retry_status, retry_response) =
                    self.send_text_once(&url, &new_token, &body).await?;

                if should_refresh_lark_tenant_token(retry_status, &retry_response) {
                    anyhow::bail!(
                        "send failed after token refresh: status={retry_status}, body={retry_response}"
                    );
                }

                ensure_lark_send_success(retry_status, &retry_response, "after token refresh")?;
            } else {
                ensure_lark_send_success(status, &response, "without token refresh")?;
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        use zeroclaw_config::schema::LarkReceiveMode;
        match self.receive_mode {
            LarkReceiveMode::Websocket => self.listen_ws(tx).await,
            LarkReceiveMode::Webhook => self.listen_http(tx).await,
        }
    }

    async fn health_check(&self) -> bool {
        self.get_tenant_access_token().await.is_ok()
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() {
            return Ok(());
        }

        // Cache-hit dedupe: if this (message_id, emoji) pair already has a
        // cached reaction_id, the reaction is already on the message and a
        // second POST would either be silently de-duped by Feishu (no
        // reaction_id returned, leaving a cache hole) or returned as a
        // non-zero business code. Either way it is a no-op the orchestrator
        // does not need. This fast-path is what lets the Lark-local
        // inbound-ack spawn and the generic orchestrator add_reaction call
        // share the same reaction_ids cache without racing each other.
        {
            let cache = self.reaction_ids.lock().await;
            if cache.contains_key(&(message_id.to_string(), emoji.to_string())) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "emoji": emoji,
                            "error_key": "lark.add_reaction.cache_hit_dedupe",
                        })),
                    "Lark add_reaction: cache hit, skipping duplicate POST"
                );
                return Ok(());
            }
        }

        let emoji_type = match unicode_to_lark_emoji_type(emoji) {
            Some(t) => t,
            None => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "emoji": emoji,
                            "error_key": "lark.add_reaction.no_emoji_mapping",
                        })),
                    "Lark add_reaction: no emoji_type mapping for unicode, skipping"
                );
                return Ok(());
            }
        };

        let mut token = self.get_tenant_access_token().await?;

        let mut retried = false;
        loop {
            let response = self
                .post_message_reaction_with_token(message_id, &token, emoji_type)
                .await?;

            if response.status().as_u16() == 401 && !retried {
                self.invalidate_token().await;
                token = self.get_tenant_access_token().await?;
                retried = true;
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                let err_body = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "Lark add_reaction failed for {message_id}: status={status}, body={err_body}"
                );
            }

            let payload: serde_json::Value = response.json().await?;
            let code = payload.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            if code != 0 {
                let msg = payload
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "code": code,
                            "message_id": message_id,
                            "msg": msg,
                            "error_key": "lark.add_reaction.non_zero_code",
                        })),
                    "Lark add_reaction returned non-zero code"
                );
            } else if let Some(reaction_id) = payload
                .pointer("/data/reaction_id")
                .and_then(|v| v.as_str())
            {
                self.reaction_ids.lock().await.insert(
                    (message_id.to_string(), emoji.to_string()),
                    reaction_id.to_string(),
                );
            }
            return Ok(());
        }
    }

    /// Remove a reaction this bot previously added via `add_reaction`.
    ///
    /// Looks up the cached `reaction_id` written by `add_reaction` (Feishu's
    /// POST response already contains it) and calls
    /// `DELETE /im/v1/messages/{message_id}/reactions/{reaction_id}`. On
    /// cache miss this is a silent no-op so the orchestrator's
    /// `let _ = channel.remove_reaction(...)` pattern keeps working after a
    /// restart loses the cache.
    ///
    /// All failure paths (transport / 401 / Feishu non-zero codes) soft-fail
    /// via [`zeroclaw_log::record!`] at WARN (or DEBUG for expected
    /// stale-state codes). Errors never propagate because the orchestrator
    /// caller discards the `Result` anyway.
    async fn remove_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() {
            return Ok(());
        }

        let reaction_id = {
            let mut cache = self.reaction_ids.lock().await;
            cache.remove(&(message_id.to_string(), emoji.to_string()))
        };
        let Some(reaction_id) = reaction_id else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "emoji": emoji,
                    })),
                "Lark remove_reaction: cache miss, skipping"
            );
            return Ok(());
        };

        let mut token = self.get_tenant_access_token().await?;
        let url = self.delete_message_reaction_url(message_id, &reaction_id);

        let mut retried = false;
        loop {
            let response = self
                .http_client()
                .delete(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await?;

            if response.status().as_u16() == 401 && !retried {
                self.invalidate_token().await;
                token = self.get_tenant_access_token().await?;
                retried = true;
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                let err_body = response.text().await.unwrap_or_default();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "reaction_id": reaction_id,
                            "status": status.as_u16(),
                            "body": err_body,
                            "error_key": "lark.remove_reaction.http_failure",
                        })),
                    "Lark remove_reaction failed"
                );
                return Ok(());
            }

            let payload: serde_json::Value = response.json().await?;
            let code = payload.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            match code {
                0 => {}
                231_003 | 231_007 | 231_010 | 231_011 => {
                    let msg = payload
                        .get("msg")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error");
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "code": code,
                                "msg": msg,
                                "message_id": message_id,
                            })),
                        "Lark remove_reaction: server-side stale state"
                    );
                }
                _ => {
                    let msg = payload
                        .get("msg")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "code": code,
                                "message_id": message_id,
                                "msg": msg,
                                "error_key": "lark.remove_reaction.non_zero_code",
                            })),
                        "Lark remove_reaction returned non-zero code"
                    );
                }
            }
            return Ok(());
        }
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        let approval_id = Uuid::new_v4().to_string();
        let card =
            build_approval_card(&approval_id, &request.tool_name, &request.arguments_summary);

        let token = self.get_tenant_access_token().await?;
        let url = self.send_message_url();
        let body = serde_json::json!({
            "receive_id": recipient,
            "receive_id_type": "chat_id",
            "msg_type": "interactive",
            "content": serde_json::to_string(&card)?,
        });

        let response_body = {
            let (status, resp) = self.send_text_once(&url, &token, &body).await?;
            if should_refresh_lark_tenant_token(status, &resp) {
                self.invalidate_token().await;
                let new_token = self.get_tenant_access_token().await?;
                let (retry_status, retry_body) =
                    self.send_text_once(&url, &new_token, &body).await?;
                ensure_lark_send_success(retry_status, &retry_body, "approval retry")?;
                retry_body
            } else {
                ensure_lark_send_success(status, &resp, "approval")?;
                resp
            }
        };

        let message_id = response_body
            .pointer("/data/message_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(
                        module_path!(),
                        ::zeroclaw_log::Action::Note
                    )
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"approval_id": approval_id})),
                    "Lark: approval card sent but no data.message_id in response — post-click card update will be skipped"
                );
                String::new()
            });

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                message_id,
                tool_name: request.tool_name.clone(),
                arguments_summary: request.arguments_summary.clone(),
            },
        );

        Ok(Some(self.wait_for_decision(rx, &approval_id).await))
    }

    fn supports_draft_updates(&self) -> bool {
        !matches!(self.stream_mode, StreamMode::Off)
    }

    /// Open a streaming draft card. Returns `Ok(None)` (caller must
    /// degrade to `send()`) when streaming is disabled, the initial POST
    /// fails, or Feishu replies with non-zero `code`. The returned
    /// `String` is the Feishu `message_id` used by subsequent
    /// `update_draft` / `finalize_draft` PATCH calls.
    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if matches!(self.stream_mode, StreamMode::Off) {
            return Ok(None);
        }

        let placeholder = truncate_card_markdown(
            if message.content.is_empty() {
                "_processing…_"
            } else {
                message.content.as_str()
            },
            LARK_CARD_MARKDOWN_MAX_BYTES,
        );
        let body = build_interactive_card_body(&message.recipient, &placeholder);
        let url = self.send_message_url();

        let (status, response) = match self.patch_or_send_once(&url, &body, false).await {
            Ok(r) => r,
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": format!("{err}")})),
                    "Lark: send_draft failed, falling back to send()"
                );
                return Ok(None);
            }
        };

        if !status.is_success() || extract_lark_response_code(&response).unwrap_or(0) != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "status": status.as_u16(),
                        "body": response,
                    })),
                "Lark: send_draft non-success, falling back to send()"
            );
            return Ok(None);
        }

        let message_id = response
            .pointer("/data/message_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        Ok(message_id)
    }

    /// Edit a previously-opened draft card with the latest accumulated
    /// content. Per-`message_id` rate-limited via `last_draft_edit` so we
    /// stay under Feishu's 5 QPS PATCH cap; calls inside the cooldown window
    /// are silently dropped (the next caller will catch up). Soft-fails on
    /// transport / token-refresh / 230020 rate-limit code so streaming token
    /// loops never abort because of a single edit hiccup.
    async fn update_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() {
            return Ok(());
        }

        {
            let mut guard = self.last_draft_edit.lock().await;
            if let Some(last) = guard.get(message_id) {
                let elapsed_ms = u64::try_from(last.elapsed().as_millis()).unwrap_or(u64::MAX);
                if elapsed_ms < self.draft_update_interval_ms {
                    return Ok(());
                }
            }
            guard.insert(message_id.to_string(), Instant::now());
        }

        let rendered = truncate_card_markdown(text, LARK_CARD_MARKDOWN_MAX_BYTES);
        self.patch_card_content(message_id, &rendered).await
    }

    /// Same wire shape as `update_draft`; kept as a separate trait method so
    /// callers can later distinguish progress chrome from response content
    /// without changing the calling sites.
    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.update_draft(recipient, message_id, text).await
    }

    /// Commit the final response into the draft card. The first chunk is
    /// PATCH-applied to the existing message_id; any overflow chunks are
    /// posted as fresh interactive cards (with a single token-refresh retry
    /// each) so long responses still land in full.
    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() {
            return self.send(&SendMessage::new(text, recipient)).await;
        }

        self.last_draft_edit.lock().await.remove(message_id);

        let chunks = split_markdown_chunks(text, LARK_CARD_MARKDOWN_MAX_BYTES);
        let first = chunks.first().copied().unwrap_or("");
        self.patch_card_content(message_id, first).await?;

        if chunks.len() > 1 {
            let token = self.get_tenant_access_token().await?;
            let url = self.send_message_url();
            for chunk in &chunks[1..] {
                let body = build_interactive_card_body(recipient, chunk);
                let (status, response) = self.send_text_once(&url, &token, &body).await?;
                if should_refresh_lark_tenant_token(status, &response) {
                    self.invalidate_token().await;
                    let new_token = self.get_tenant_access_token().await?;
                    let (retry_status, retry_response) =
                        self.send_text_once(&url, &new_token, &body).await?;
                    ensure_lark_send_success(
                        retry_status,
                        &retry_response,
                        "after token refresh (finalize_draft)",
                    )?;
                } else {
                    ensure_lark_send_success(status, &response, "finalize_draft chunk")?;
                }
            }
        }

        Ok(())
    }

    /// Replace the draft body with a "cancelled" marker. Feishu does not
    /// expose an official "delete-draft" endpoint, so the closest faithful
    /// signal is a one-line PATCH that overwrites the card content. We
    /// best-effort emit the marker, then unconditionally evict the
    /// `last_draft_edit` rate-limit entry so the per-message_id slot is
    /// reclaimed even when the PATCH itself fails (matching the
    /// `finalize_draft` cleanup contract — see the field doc on
    /// `last_draft_edit`).
    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        let result = self
            .update_draft(recipient, message_id, "_(cancelled)_")
            .await;
        self.last_draft_edit.lock().await.remove(message_id);
        result
    }
}

impl LarkChannel {
    /// PATCH the draft card body with new markdown content.
    ///
    /// Used by both `update_draft` (per-token streaming) and
    /// `finalize_draft` (last-chunk commit). Soft-fails on every error
    /// path — transport (reqwest), token-refresh-still-401, the explicit
    /// 230020 frequency-limit code, and any other non-zero Feishu business
    /// code — because the streaming caller cannot meaningfully recover
    /// from a single missed edit and dropping the error keeps the token
    /// loop alive. The signature still returns `anyhow::Result<()>` for
    /// caller-shape compatibility, but it never returns `Err`; every
    /// failure path is logged at WARN/DEBUG with a stable `error_key`
    /// and the function returns `Ok(())`.
    async fn patch_card_content(&self, message_id: &str, markdown: &str) -> anyhow::Result<()> {
        let url = self.patch_message_url(message_id);
        let body = serde_json::json!({
            "content": build_card_content(markdown),
        });

        // First PATCH attempt — soft-fail transport errors instead of
        // propagating them. The streaming caller invokes this per token,
        // so a single transport hiccup must not break the token loop.
        let (status, response) = match self.patch_or_send_once(&url, &body, true).await {
            Ok(pair) => pair,
            Err(err) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "error": format!("{err}"),
                            "error_key": "lark.draft_patch.transport_failure",
                        })),
                    "Lark: draft PATCH transport-failed (soft)"
                );
                return Ok(());
            }
        };

        let body_for_inspect = if should_refresh_lark_tenant_token(status, &response) {
            self.invalidate_token().await;
            // Retry PATCH after token refresh — same soft-fail discipline:
            // a transport error on the retry must not propagate.
            let (retry_status, retry_response) = match self
                .patch_or_send_once(&url, &body, true)
                .await
            {
                Ok(pair) => pair,
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note,)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "message_id": message_id,
                                "error": format!("{err}"),
                                "error_key": "lark.draft_patch.transport_failure_on_retry",
                            })),
                        "Lark: draft PATCH retry transport-failed (soft)"
                    );
                    return Ok(());
                }
            };
            if should_refresh_lark_tenant_token(retry_status, &retry_response) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "body": retry_response,
                            "error_key": "lark.draft_patch.unauthorized_after_refresh",
                        })),
                    "Lark: draft PATCH still unauthorized after token refresh"
                );
                return Ok(());
            }
            retry_response
        } else {
            response
        };

        let code = extract_lark_response_code(&body_for_inspect).unwrap_or(0);
        if code == LARK_DRAFT_RATE_LIMIT_CODE {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "error_key": "lark.draft_patch.rate_limited",
                    })),
                "Lark: draft PATCH rate-limited (code=230020)"
            );
            return Ok(());
        }
        if code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "code": code,
                        "body": body_for_inspect,
                        "error_key": "lark.draft_patch.non_zero_code",
                    })),
                "Lark: draft PATCH soft-failed"
            );
        }
        Ok(())
    }
}

impl LarkChannel {
    /// Wait for the user's approval click; on timeout, evict the pending entry
    /// and synthesize a `Deny` response. Never panics.
    async fn wait_for_decision(
        &self,
        rx: tokio::sync::oneshot::Receiver<zeroclaw_api::channel::ChannelApprovalResponse>,
        approval_id: &str,
    ) -> zeroclaw_api::channel::ChannelApprovalResponse {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
            Ok(Ok(response)) => response,
            _ => {
                self.pending_approvals.lock().await.remove(approval_id);
                ChannelApprovalResponse::Deny
            }
        }
    }

    /// PATCH an approval card to its resolved state. Soft-fails on every error
    /// path (transport / token refresh / rate-limited / non-zero code) — never
    /// propagates to the caller, since the user-visible decision is already
    /// delivered via the oneshot.
    async fn patch_approval_card_resolved(
        &self,
        message_id: &str,
        tool_name: &str,
        arguments_summary: &str,
        decision: zeroclaw_api::channel::ChannelApprovalResponse,
    ) {
        let card = build_resolved_approval_card(tool_name, arguments_summary, decision.clone());
        let url = self.patch_message_url(message_id);
        let body = serde_json::json!({
            "content": card.to_string(),
        });

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "message_id": message_id,
                    "decision": format!("{decision:?}"),
                })),
            "Lark: approval card PATCH dispatching"
        );

        let (status, response) = match self.patch_or_send_once(&url, &body, true).await {
            Ok(pair) => pair,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "message_id": message_id,
                            "error": e.to_string(),
                        })),
                    "Lark: approval card PATCH transport error"
                );
                return;
            }
        };

        let final_body = if should_refresh_lark_tenant_token(status, &response) {
            self.invalidate_token().await;
            match self.patch_or_send_once(&url, &body, true).await {
                Ok((retry_status, retry_response)) => {
                    if should_refresh_lark_tenant_token(retry_status, &retry_response) {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Send
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "message_id": message_id,
                                "body": retry_response.to_string(),
                            })),
                            "Lark: approval card PATCH still unauthorized after token refresh"
                        );
                        return;
                    }
                    retry_response
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "message_id": message_id,
                                "error": e.to_string(),
                            })),
                        "Lark: approval card PATCH retry transport error"
                    );
                    return;
                }
            }
        } else {
            response
        };

        let code = extract_lark_response_code(&final_body).unwrap_or(0);
        if code == LARK_DRAFT_RATE_LIMIT_CODE {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "code": LARK_DRAFT_RATE_LIMIT_CODE,
                    })),
                "Lark: approval card PATCH rate-limited"
            );
        } else if code != 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "code": code,
                        "status": status.to_string(),
                        "body": final_body.to_string(),
                    })),
                "Lark: approval card PATCH soft-failed"
            );
        } else {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "message_id": message_id,
                        "status": status.to_string(),
                    })),
                "Lark: approval card PATCH succeeded"
            );
        }
    }

    /// Single-shot HTTP request used by `patch_approval_card_resolved`. Builds
    /// PATCH (when `is_patch=true`) or POST request with current tenant token,
    /// returns parsed JSON body and the HTTP status. Caller decides whether to
    /// retry on token refresh.
    async fn patch_or_send_once(
        &self,
        url: &str,
        body: &serde_json::Value,
        is_patch: bool,
    ) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
        let token = self.get_tenant_access_token().await?;
        let builder = if is_patch {
            self.http_client().patch(url)
        } else {
            self.http_client().post(url)
        };
        let resp = builder
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({ "raw": raw }));
        Ok((status, parsed))
    }

    /// Handle a `card.action.trigger` event: parse `approval_id` + `decision`
    /// from `event.action.value` (or `event.action.behaviors[0].value` for
    /// Card 2.0 button click events), resolve the pending oneshot, and
    /// forward the response. Unknown / expired approval IDs are silently
    /// dropped (info-log only).
    async fn handle_card_action_event(
        &self,
        event_payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        // Diagnostic: emit a SANITIZED copy of the inbound payload at DEBUG
        // so operators can capture real Lark/Feishu `card.action.trigger`
        // shape evidence for fixture collection WITHOUT leaking
        // tenant-specific identifiers (token, operator.*, context.open_*)
        // to runtime logs / dashboards / persisted JSONL.
        //
        // `sanitize_card_action_payload` replaces those fields with
        // deterministic `REDACTED_*` placeholders before the value reaches
        // `record!`. The regression test
        // `sanitize_card_action_payload_redacts_sensitive_fields` will fail
        // if any of those raw values can leak through this path again.
        //
        // Default production RUST_LOG (=info) leaves this off, so it costs
        // nothing at runtime; opt in with:
        //
        //   RUST_LOG=info,zeroclaw_log_event=debug
        //
        // Captured payloads should land in
        // `crates/zeroclaw-channels/tests/fixtures/lark/` and are replayed
        // by the integration test in `tests/lark_approval_live_evidence.rs`.
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Receive).with_attrs(
                ::serde_json::json!({
                    "sanitized_payload": sanitize_card_action_payload(event_payload),
                })
            ),
            "card.action.trigger sanitized payload"
        );

        // Feishu Card 2.0 button click events MAY round-trip the button value at
        // `event.action.behaviors[0].value` instead of `event.action.value`
        // (the Card 1.0 path). Both pointers are accepted for forward-compat;
        // captured fixtures under `tests/fixtures/lark/` lock the shape that
        // production currently emits.
        let value = event_payload
            .pointer("/action/value")
            .or_else(|| event_payload.pointer("/action/behaviors/0/value"))
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "card.action.trigger: missing event.action.value or event.action.behaviors[0].value"
                );
                anyhow::Error::msg(
                    "card.action.trigger: missing event.action.value or event.action.behaviors[0].value",
                )
            })?;

        let approval_id = value
            .get("approval_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "card.action.trigger: missing approval_id in value"
                );
                anyhow::Error::msg("card.action.trigger: missing approval_id in value")
            })?;

        let decision_str = value
            .get("decision")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "card.action.trigger: missing decision in value"
                );
                anyhow::Error::msg("card.action.trigger: missing decision in value")
            })?;

        let decision = match decision_str {
            "approve" => ChannelApprovalResponse::Approve,
            "deny" => ChannelApprovalResponse::Deny,
            "always" => ChannelApprovalResponse::AlwaysApprove,
            other => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"decision_str": other})),
                    "Lark: unknown approval decision — treating as deny"
                );
                ChannelApprovalResponse::Deny
            }
        };

        let pending = self.pending_approvals.lock().await.remove(approval_id);
        let Some(pending) = pending else {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "approval_id": approval_id,
                        "decision": format!("{decision:?}"),
                    })),
                "Lark: card action for unknown/expired approval_id"
            );
            return Ok(());
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Receive)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "approval_id": approval_id,
                    "decision": format!("{decision:?}"),
                    "message_id": pending.message_id,
                    "has_message_id": !pending.message_id.is_empty(),
                })),
            "Lark: card action received"
        );

        let _ = pending.sender.send(decision.clone());

        if !pending.message_id.is_empty() {
            self.patch_approval_card_resolved(
                &pending.message_id,
                &pending.tool_name,
                &pending.arguments_summary,
                decision,
            )
            .await;
        }

        Ok(())
    }
}

impl LarkChannel {
    /// HTTP callback server (legacy — requires a public endpoint).
    /// Use `listen()` (WS long-connection) for new deployments.
    pub async fn listen_http(
        &self,
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        self.ensure_bot_open_id().await;
        use axum::{Json, Router, extract::State, routing::post};

        #[derive(Clone)]
        struct AppState {
            verification_token: String,
            channel: Arc<LarkChannel>,
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        }

        async fn handle_event(
            State(state): State<AppState>,
            Json(payload): Json<serde_json::Value>,
        ) -> axum::response::Response {
            use axum::http::StatusCode;
            use axum::response::IntoResponse;

            // URL verification challenge
            if let Some(challenge) = payload.get("challenge").and_then(|c| c.as_str()) {
                // Verify token if present
                let token_ok = payload
                    .get("token")
                    .and_then(|t| t.as_str())
                    .is_none_or(|t| t == state.verification_token);

                if !token_ok {
                    return (StatusCode::FORBIDDEN, "invalid token").into_response();
                }

                let resp = serde_json::json!({ "challenge": challenge });
                return (StatusCode::OK, Json(resp)).into_response();
            }

            // Card button click events are not message events — route them
            // through the approval-card resolver and short-circuit before the
            // generic message parser sees them.
            let event_type = payload
                .pointer("/header/event_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if event_type == "card.action.trigger"
                && let Some(inner) = payload.get("event")
            {
                if let Err(e) = state.channel.handle_card_action_event(inner).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(
                            module_path!(),
                            ::zeroclaw_log::Action::Dispatch
                        )
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                        "Lark webhook: card action dispatch error"
                    );
                }
                return (StatusCode::OK, "ok").into_response();
            }

            // Parse event messages first; then issue an inbound fast-ack via
            // the same trait-level Channel::add_reaction path that the generic
            // orchestrator uses. The trait impl writes Feishu's returned
            // reaction_id into the shared reaction_ids cache and dedupes
            // subsequent duplicate POSTs via a cache-hit fast-path, so the
            // later generic orchestrator add_reaction("👀") call becomes a
            // no-op and remove_reaction("👀") always finds the right id (no
            // orphan reaction). See lark.rs `add_reaction` impl and the
            // `lark_fast_ack_and_generic_path_dedupe_on_cache_hit` test.
            let messages = state.channel.parse_event_payload_async(&payload).await;
            if !messages.is_empty()
                && let Some(message_id) = payload
                    .pointer("/event/message/message_id")
                    .and_then(|m| m.as_str())
            {
                let reaction_channel = Arc::clone(&state.channel);
                let reaction_message_id = message_id.to_string();
                // Prefer the first parsed message's reply_target as the
                // ack target; parse_event_payload_async already filtered
                // out unauthorized senders and non-text payloads.
                let reaction_reply_target = messages[0].reply_target.clone();
                zeroclaw_spawn::spawn!(async move {
                    if let Err(e) = <LarkChannel as Channel>::add_reaction(
                        &reaction_channel,
                        &reaction_reply_target,
                        &reaction_message_id,
                        "\u{1F440}",
                    )
                    .await
                    {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note,
                            )
                            .with_attrs(::serde_json::json!({
                                "message_id": reaction_message_id,
                                "error": format!("{e}"),
                                "error_key": "lark.inbound_fast_ack.failed",
                            })),
                            "Lark inbound fast-ack failed (soft, webhook path)"
                        );
                    }
                });
            }

            for msg in messages {
                if state.tx.send(msg).await.is_err() {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "message channel closed"
                    );
                    break;
                }
            }

            (StatusCode::OK, "ok").into_response()
        }

        let port = self.port.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"mode": "webhook", "missing": "port"})),
                "lark: webhook mode requires port"
            );
            anyhow::Error::msg("webhook mode requires `port` to be set in [channels_config.lark]")
        })?;

        let state = AppState {
            verification_token: self.verification_token.clone(),
            channel: Arc::new(self.clone()),
            tx,
        };

        let app = Router::new()
            .route("/lark", post(handle_event))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"addr": addr})),
            "event callback server listening on"
        );

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WS helper functions
// ─────────────────────────────────────────────────────────────────────────────

fn inferred_audio_filename(file_key: &str) -> String {
    const SUPPORTED_EXTENSIONS: &[&str] = &[".m4a", ".ogg", ".mp3", ".aac", ".wav"];
    let file_key_lower = file_key.to_lowercase();
    if SUPPORTED_EXTENSIONS
        .iter()
        .any(|ext| file_key_lower.ends_with(ext))
    {
        file_key.to_string()
    } else {
        "voice.m4a".to_string()
    }
}

/// Detect image MIME type from magic bytes, falling back to Content-Type header.
fn lark_detect_image_mime(content_type: Option<&str>, bytes: &[u8]) -> Option<String> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        return Some("image/png".to_string());
    }
    if bytes.len() >= 3 && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg".to_string());
    }
    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    if bytes.len() >= 2 && bytes.starts_with(b"BM") {
        return Some("image/bmp".to_string());
    }
    content_type
        .and_then(|ct| ct.split(';').next())
        .map(|ct| ct.trim().to_lowercase())
        .filter(|ct| ct.starts_with("image/"))
}

/// Check if a filename looks like a text file based on extension.
fn lark_is_text_filename(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "txt"
            | "md"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
            | "rb"
            | "sh"
            | "bash"
            | "zsh"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "xml"
            | "html"
            | "css"
            | "sql"
            | "csv"
            | "tsv"
            | "log"
            | "cfg"
            | "ini"
            | "conf"
            | "env"
            | "dockerfile"
            | "makefile"
    )
}

fn lark_inline_text_file_preview(text: Cow<'_, str>) -> String {
    if text.len() > 50_000 {
        let end = crate::util::floor_char_boundary(text.as_ref(), 50_000);
        format!("{}...\n[truncated]", &text[..end])
    } else {
        text.into_owned()
    }
}

/// Flatten a Feishu `post` rich-text message to plain text.
///
/// Returns `None` when the content cannot be parsed or yields no usable text,
/// so callers can simply `continue` rather than forwarding a meaningless
/// placeholder string to the agent.
struct ParsedPostContent {
    text: String,
    mentioned_open_ids: Vec<String>,
}

fn parse_post_content_details(content: &str) -> Option<ParsedPostContent> {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let locale = parsed
        .get("zh_cn")
        .or_else(|| parsed.get("en_us"))
        .or_else(|| {
            parsed
                .as_object()
                .and_then(|m| m.values().find(|v| v.is_object()))
        })?;

    let mut text = String::new();
    let mut mentioned_open_ids = Vec::new();

    if let Some(title) = locale
        .get("title")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
    {
        text.push_str(title);
        text.push_str("\n\n");
    }

    if let Some(paragraphs) = locale.get("content").and_then(|c| c.as_array()) {
        for para in paragraphs {
            if let Some(elements) = para.as_array() {
                for el in elements {
                    match el.get("tag").and_then(|t| t.as_str()).unwrap_or("") {
                        "text" => {
                            if let Some(t) = el.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                        "a" => {
                            text.push_str(
                                el.get("text")
                                    .and_then(|t| t.as_str())
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| el.get("href").and_then(|h| h.as_str()))
                                    .unwrap_or(""),
                            );
                        }
                        "at" => {
                            let n = el
                                .get("user_name")
                                .and_then(|n| n.as_str())
                                .or_else(|| el.get("user_id").and_then(|i| i.as_str()))
                                .unwrap_or("user");
                            text.push('@');
                            text.push_str(n);
                            if let Some(open_id) = el
                                .get("user_id")
                                .and_then(|i| i.as_str())
                                .map(str::trim)
                                .filter(|id| !id.is_empty())
                            {
                                mentioned_open_ids.push(open_id.to_string());
                            }
                        }
                        _ => {
                            // Some Feishu rich-text tags (for example `md`) still carry useful
                            // human text in a `text` field. Keep that text instead of dropping
                            // the whole message as empty.
                            if let Some(t) = el.get("text").and_then(|t| t.as_str()) {
                                text.push_str(t);
                            }
                        }
                    }
                }
                text.push('\n');
            }
        }
    }

    let result = text.trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(ParsedPostContent {
            text: result,
            mentioned_open_ids,
        })
    }
}

/// Parse Feishu `list` message content into plain-text bullet lines.
///
/// Feishu sends list/bullet content as a JSON structure with nested items,
/// each containing inline elements (text, links, etc.).  We flatten them
/// into `"- item"` lines separated by newlines.
fn parse_list_content(content: &str) -> Option<String> {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok()?;

    // The top-level structure may contain an "items" array directly, or the
    // items might be under a "content" key.  Walk both shapes.
    let items = parsed
        .get("items")
        .and_then(|v| v.as_array())
        .or_else(|| parsed.get("content").and_then(|v| v.as_array()))?;

    let mut lines = Vec::new();
    collect_list_items(items, &mut lines, 0);

    let result = lines.join("\n").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Recursively collect list item text.  Each item may itself contain nested
/// sub-lists via a `"children"` field.
fn collect_list_items(items: &[serde_json::Value], lines: &mut Vec<String>, depth: usize) {
    let indent = "  ".repeat(depth);
    for item in items {
        // Each item can be an array of inline elements, or an object with
        // "content" (inline elements array) and optional "children" (sub-items).
        let (inline_elements, children) = if let Some(arr) = item.as_array() {
            (arr.as_slice(), None)
        } else if let Some(obj) = item.as_object() {
            let inlines = obj
                .get("content")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let kids = obj.get("children").and_then(|v| v.as_array());
            (inlines, kids)
        } else {
            continue;
        };

        let mut text = String::new();
        for el in inline_elements {
            // Handle flat inline elements or nested arrays of inline elements
            if let Some(inner_arr) = el.as_array() {
                for inner_el in inner_arr {
                    extract_inline_text(inner_el, &mut text);
                }
            } else {
                extract_inline_text(el, &mut text);
            }
        }

        let trimmed = text.trim();
        if !trimmed.is_empty() {
            lines.push(format!("{indent}- {trimmed}"));
        }

        if let Some(kids) = children {
            collect_list_items(kids, lines, depth + 1);
        }
    }
}

/// Extract text from a single Feishu inline element (text, link, at-mention).
fn extract_inline_text(el: &serde_json::Value, out: &mut String) {
    match el.get("tag").and_then(|t| t.as_str()).unwrap_or("") {
        "text" => {
            if let Some(t) = el.get("text").and_then(|t| t.as_str()) {
                out.push_str(t);
            }
        }
        "a" => {
            out.push_str(
                el.get("text")
                    .and_then(|t| t.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| el.get("href").and_then(|h| h.as_str()))
                    .unwrap_or(""),
            );
        }
        "at" => {
            let n = el
                .get("user_name")
                .and_then(|n| n.as_str())
                .or_else(|| el.get("user_id").and_then(|i| i.as_str()))
                .unwrap_or("user");
            out.push('@');
            out.push_str(n);
        }
        _ => {}
    }
}

fn mention_matches_bot_open_id(mention: &serde_json::Value, bot_open_id: &str) -> bool {
    mention
        .pointer("/id/open_id")
        .or_else(|| mention.pointer("/open_id"))
        .and_then(|v| v.as_str())
        .is_some_and(|value| value == bot_open_id)
}

/// In group chats, only respond when the bot is explicitly @-mentioned.
fn should_respond_in_group(
    mention_only: bool,
    bot_open_id: Option<&str>,
    mentions: &[serde_json::Value],
    post_mentioned_open_ids: &[String],
) -> bool {
    if !mention_only {
        return true;
    }
    let Some(bot_open_id) = bot_open_id.filter(|id| !id.is_empty()) else {
        return false;
    };
    if mentions.is_empty() && post_mentioned_open_ids.is_empty() {
        return false;
    }
    mentions
        .iter()
        .any(|mention| mention_matches_bot_open_id(mention, bot_open_id))
        || post_mentioned_open_ids
            .iter()
            .any(|id| id.as_str() == bot_open_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_bot_open_id(ch: LarkChannel, bot_open_id: &str) -> LarkChannel {
        ch.set_resolved_bot_open_id(Some(bot_open_id.to_string()));
        ch
    }

    fn resolver_from(peers: Vec<String>) -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(move || peers.clone())
    }

    fn make_channel() -> LarkChannel {
        with_bot_open_id(
            LarkChannel::new(
                "cli_test_app_id".into(),
                "test_app_secret".into(),
                "test_verification_token".into(),
                None,
                "lark_test_alias",
                resolver_from(vec!["ou_testuser123".into()]),
                true,
            ),
            "ou_bot",
        )
    }

    #[test]
    fn lark_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "lark");
    }

    #[test]
    fn lark_ws_activity_refreshes_heartbeat_watchdog() {
        assert!(should_refresh_last_recv(&WsMsg::Binary(
            vec![1, 2, 3].into()
        )));
        assert!(should_refresh_last_recv(&WsMsg::Ping(vec![9, 9].into())));
        assert!(should_refresh_last_recv(&WsMsg::Pong(vec![8, 8].into())));
    }

    #[test]
    fn lark_ws_non_activity_frames_do_not_refresh_heartbeat_watchdog() {
        assert!(!should_refresh_last_recv(&WsMsg::Text("hello".into())));
        assert!(!should_refresh_last_recv(&WsMsg::Close(None)));
    }

    #[test]
    fn lark_group_response_requires_matching_bot_mention_when_ids_available() {
        let mentions = vec![serde_json::json!({
            "id": { "open_id": "ou_other" }
        })];
        assert!(!should_respond_in_group(
            true,
            Some("ou_bot"),
            &mentions,
            &[]
        ));

        let mentions = vec![serde_json::json!({
            "id": { "open_id": "ou_bot" }
        })];
        assert!(should_respond_in_group(
            true,
            Some("ou_bot"),
            &mentions,
            &[]
        ));
    }

    #[test]
    fn lark_group_response_requires_resolved_open_id_when_mention_only_enabled() {
        let mentions = vec![serde_json::json!({
            "id": { "open_id": "ou_any" }
        })];
        assert!(!should_respond_in_group(true, None, &mentions, &[]));
    }

    #[test]
    fn lark_group_response_allows_post_mentions_for_bot_open_id() {
        assert!(should_respond_in_group(
            true,
            Some("ou_bot"),
            &[],
            &[String::from("ou_bot")]
        ));
    }

    #[test]
    fn lark_should_refresh_token_on_http_401() {
        let body = serde_json::json!({ "code": 0 });
        assert!(should_refresh_lark_tenant_token(
            reqwest::StatusCode::UNAUTHORIZED,
            &body
        ));
    }

    #[test]
    fn lark_should_refresh_token_on_body_code_99991663() {
        let body = serde_json::json!({
            "code": LARK_INVALID_ACCESS_TOKEN_CODE,
            "msg": "Invalid access token for authorization."
        });
        assert!(should_refresh_lark_tenant_token(
            reqwest::StatusCode::OK,
            &body
        ));
    }

    #[test]
    fn lark_should_not_refresh_token_on_success_body() {
        let body = serde_json::json!({ "code": 0, "msg": "ok" });
        assert!(!should_refresh_lark_tenant_token(
            reqwest::StatusCode::OK,
            &body
        ));
    }

    #[test]
    fn lark_extract_token_ttl_seconds_supports_expire_and_expires_in() {
        let body_expire = serde_json::json!({ "expire": 7200 });
        let body_expires_in = serde_json::json!({ "expires_in": 3600 });
        let body_missing = serde_json::json!({});
        assert_eq!(extract_lark_token_ttl_seconds(&body_expire), 7200);
        assert_eq!(extract_lark_token_ttl_seconds(&body_expires_in), 3600);
        assert_eq!(
            extract_lark_token_ttl_seconds(&body_missing),
            LARK_DEFAULT_TOKEN_TTL.as_secs()
        );
    }

    #[test]
    fn lark_next_token_refresh_deadline_reserves_refresh_skew() {
        let now = Instant::now();
        let regular = next_token_refresh_deadline(now, 7200);
        let short_ttl = next_token_refresh_deadline(now, 60);

        assert_eq!(regular.duration_since(now), Duration::from_secs(7080));
        assert_eq!(short_ttl.duration_since(now), Duration::from_secs(1));
    }

    #[test]
    fn lark_ensure_send_success_rejects_non_zero_code() {
        let ok = serde_json::json!({ "code": 0 });
        let bad = serde_json::json!({ "code": 12345, "msg": "bad request" });

        assert!(ensure_lark_send_success(reqwest::StatusCode::OK, &ok, "test").is_ok());
        assert!(ensure_lark_send_success(reqwest::StatusCode::OK, &bad, "test").is_err());
    }

    #[test]
    fn lark_user_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_user_allowed("ou_testuser123"));
        assert!(!ch.is_user_allowed("ou_other"));
    }

    #[test]
    fn lark_user_allowed_wildcard() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        assert!(ch.is_user_allowed("ou_anyone"));
    }

    #[test]
    fn lark_user_denied_empty() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec![]),
            true,
        );
        assert!(!ch.is_user_allowed("ou_anyone"));
    }

    #[tokio::test]
    async fn lark_parse_challenge() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "challenge": "abc123",
            "token": "test_verification_token",
            "type": "url_verification"
        });
        // Challenge payloads should not produce messages
        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_valid_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_testuser123"
                    }
                },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"Hello ZeroClaw!\"}",
                    "chat_id": "oc_chat123",
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].sender, "oc_chat123");
        assert_eq!(msgs[0].channel, "lark");
        assert_eq!(msgs[0].timestamp, 1_699_999_999);
    }

    #[tokio::test]
    async fn lark_parse_unauthorized_user() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_unauthorized" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"spam\"}",
                    "chat_id": "oc_chat",
                    "create_time": "1000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_unsupported_message_type_skipped() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "sticker",
                    "content": "{}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_list_content_flat_items() {
        // Flat structure: items is an array of arrays of inline elements
        let content = r#"{"items":[[{"tag":"text","text":"first item"}],[{"tag":"text","text":"second item"}]]}"#;
        let result = parse_list_content(content).unwrap();
        assert_eq!(result, "- first item\n- second item");
    }

    #[test]
    fn parse_list_content_nested_children() {
        // Nested structure: items are objects with content + children
        let content = r#"{"items":[{"content":[[{"tag":"text","text":"parent"}]],"children":[{"content":[[{"tag":"text","text":"child"}]]}]}]}"#;
        let result = parse_list_content(content).unwrap();
        assert_eq!(result, "- parent\n  - child");
    }

    #[test]
    fn parse_list_content_with_links() {
        let content = r#"{"items":[[{"tag":"text","text":"see "},{"tag":"a","text":"docs","href":"https://example.com"}]]}"#;
        let result = parse_list_content(content).unwrap();
        assert_eq!(result, "- see docs");
    }

    #[test]
    fn parse_list_content_empty_returns_none() {
        let content = r#"{"items":[]}"#;
        assert!(parse_list_content(content).is_none());
    }

    #[test]
    fn parse_list_content_invalid_json_returns_none() {
        assert!(parse_list_content("not json").is_none());
    }

    #[tokio::test]
    async fn lark_parse_list_message_type() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "list",
                    "content": "{\"items\":[[{\"tag\":\"text\",\"text\":\"buy milk\"}],[{\"tag\":\"text\",\"text\":\"buy eggs\"}]]}",
                    "chat_id": "oc_chat",
                    "create_time": "1000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].content.contains("buy milk"));
        assert!(msgs[0].content.contains("buy eggs"));
    }

    #[tokio::test]
    async fn lark_parse_image_missing_key_skipped() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "image",
                    "content": "{}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_file_missing_key_skipped() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "file",
                    "content": "{}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_empty_text_skipped() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"\"}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_wrong_event_type() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.chat.disbanded_v1" },
            "event": {}
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_missing_sender() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_unicode_message() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"Hello world 🌍\"}",
                    "chat_id": "oc_chat",
                    "create_time": "1000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello world 🌍");
    }

    #[tokio::test]
    async fn lark_parse_missing_event() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_invalid_content_json() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "not valid json",
                    "chat_id": "oc_chat"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_config_serde() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};
        let lc = LarkConfig {
            enabled: true,
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::default(),
            port: None,
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 300,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };
        let json = serde_json::to_string(&lc).unwrap();
        let parsed: LarkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.app_id, "cli_app123");
        assert_eq!(parsed.app_secret, "secret456");
        assert_eq!(parsed.verification_token.as_deref(), Some("vtoken789"));
    }

    #[test]
    fn lark_config_toml_roundtrip() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};
        let lc = LarkConfig {
            enabled: true,
            app_id: "app".into(),
            app_secret: "secret".into(),
            encrypt_key: None,
            verification_token: Some("tok".into()),
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 300,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };
        let toml_str = toml::to_string(&lc).unwrap();
        let parsed: LarkConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.app_id, "app");
        assert_eq!(parsed.verification_token.as_deref(), Some("tok"));
    }

    #[test]
    fn lark_config_defaults_optional_fields() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};
        let json = r#"{"app_id":"a","app_secret":"s"}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.verification_token.is_none());
        assert!(!parsed.mention_only);
        assert_eq!(parsed.receive_mode, LarkReceiveMode::Websocket);
        assert!(parsed.port.is_none());
    }

    #[test]
    fn lark_from_config_preserves_mode_and_region() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};

        let cfg = LarkConfig {
            enabled: true,
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 300,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };

        let ch = LarkChannel::from_config(&cfg, "lark_test_alias", resolver_from(vec!["*".into()]));

        assert_eq!(ch.api_base(), LARK_BASE_URL);
        assert_eq!(ch.ws_base(), LARK_WS_BASE_URL);
        assert_eq!(ch.receive_mode, LarkReceiveMode::Webhook);
        assert_eq!(ch.port, Some(9898));
    }

    #[test]
    fn lark_from_config_with_use_feishu_routes_to_feishu() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};

        let cfg = LarkConfig {
            enabled: true,
            app_id: "cli_feishu_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            mention_only: false,
            use_feishu: true,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 300,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };

        let ch =
            LarkChannel::from_config(&cfg, "feishu_test_alias", resolver_from(vec!["*".into()]));

        assert_eq!(ch.api_base(), FEISHU_BASE_URL);
        assert_eq!(ch.ws_base(), FEISHU_WS_BASE_URL);
        assert_eq!(ch.name(), "feishu");
    }

    #[test]
    fn lark_with_approval_timeout_secs_propagates_value() {
        use zeroclaw_config::schema::{LarkConfig, LarkReceiveMode};

        let cfg = LarkConfig {
            enabled: true,
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 456,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };

        let ch = LarkChannel::from_config(&cfg, "lark_test_alias", resolver_from(vec!["*".into()]))
            .with_approval_timeout_secs(cfg.approval_timeout_secs);

        assert_eq!(ch.approval_timeout_secs, 456);
    }

    #[test]
    fn lark_with_per_user_session_propagates_value() {
        let ch_on = make_channel().with_per_user_session(true);
        assert!(ch_on.per_user_session);
        let ch_off = make_channel().with_per_user_session(false);
        assert!(!ch_off.per_user_session);
    }

    #[test]
    fn supports_draft_updates_reflects_stream_mode() {
        let off = make_channel();
        assert!(!off.supports_draft_updates());

        let partial = make_channel().with_streaming(StreamMode::Partial, 500);
        assert!(partial.supports_draft_updates());
    }

    #[tokio::test]
    async fn update_draft_rate_limits_within_interval() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-rate",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        let patch_mock = Mock::given(method("PATCH"))
            .and(path_regex("/im/v1/messages/om_draft_rl"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "code": 0 })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel().with_streaming(StreamMode::Partial, 5_000);
        ch.api_base_override = Some(server.uri());

        ch.update_draft("oc_chat1", "om_draft_rl", "first")
            .await
            .expect("first update_draft ok");
        ch.update_draft("oc_chat1", "om_draft_rl", "second")
            .await
            .expect("second update_draft ok");

        drop(patch_mock);
    }

    #[tokio::test]
    async fn update_draft_proceeds_after_interval() {
        use std::time::Duration as StdDuration;
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-proceed",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        let patch_mock = Mock::given(method("PATCH"))
            .and(path_regex("/im/v1/messages/om_draft_go"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "code": 0 })),
            )
            .expect(2)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel().with_streaming(StreamMode::Partial, 50);
        ch.api_base_override = Some(server.uri());

        ch.update_draft("oc_chat1", "om_draft_go", "first")
            .await
            .expect("first update_draft ok");
        tokio::time::sleep(StdDuration::from_millis(80)).await;
        ch.update_draft("oc_chat1", "om_draft_go", "second")
            .await
            .expect("second update_draft ok");

        drop(patch_mock);
    }

    #[test]
    fn lark_resolve_sender_respects_per_user_session_flag() {
        let mut ch = make_channel();

        assert!(!ch.per_user_session);
        assert_eq!(ch.resolve_sender("oc_chat", Some("ou_user")), "oc_chat");
        assert_eq!(ch.resolve_sender("oc_chat", None), "oc_chat");
        assert_eq!(ch.resolve_sender("oc_chat", Some("")), "oc_chat");

        ch.per_user_session = true;
        assert_eq!(ch.resolve_sender("oc_chat", Some("ou_user")), "ou_user");
        assert_eq!(ch.resolve_sender("oc_chat", None), "oc_chat");
        assert_eq!(ch.resolve_sender("oc_chat", Some("")), "oc_chat");
    }

    #[tokio::test]
    async fn lark_parse_fallback_sender_to_open_id() {
        // When chat_id is missing, sender should fall back to open_id
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            true,
        );
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "create_time": "1000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "ou_user");
    }

    #[tokio::test]
    async fn lark_parse_group_message_requires_bot_mention_when_enabled() {
        let ch = with_bot_open_id(
            LarkChannel::new(
                "cli_app123".into(),
                "secret".into(),
                "token".into(),
                None,
                "lark_test_alias",
                resolver_from(vec!["*".into()]),
                true,
            ),
            "ou_bot_123",
        );

        let no_mention_payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "group",
                    "chat_id": "oc_chat",
                    "mentions": []
                }
            }
        });
        assert!(ch.parse_event_payload(&no_mention_payload).await.is_empty());

        let wrong_mention_payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "group",
                    "chat_id": "oc_chat",
                    "mentions": [{ "id": { "open_id": "ou_other" } }]
                }
            }
        });
        assert!(
            ch.parse_event_payload(&wrong_mention_payload)
                .await
                .is_empty()
        );

        let bot_mention_payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "group",
                    "chat_id": "oc_chat",
                    "mentions": [{ "id": { "open_id": "ou_bot_123" } }]
                }
            }
        });
        assert_eq!(ch.parse_event_payload(&bot_mention_payload).await.len(), 1);
    }

    #[tokio::test]
    async fn lark_parse_group_post_message_accepts_at_when_top_level_mentions_empty() {
        let ch = with_bot_open_id(
            LarkChannel::new(
                "cli_app123".into(),
                "secret".into(),
                "token".into(),
                None,
                "lark_test_alias",
                resolver_from(vec!["*".into()]),
                true,
            ),
            "ou_bot_123",
        );

        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "post",
                    "chat_type": "group",
                    "chat_id": "oc_chat",
                    "mentions": [],
                    "content": "{\"zh_cn\":{\"title\":\"\",\"content\":[[{\"tag\":\"at\",\"user_id\":\"ou_bot_123\",\"user_name\":\"Bot\"},{\"tag\":\"text\",\"text\":\" hi\"}]]}}"
                }
            }
        });

        assert_eq!(ch.parse_event_payload(&payload).await.len(), 1);
    }

    #[tokio::test]
    async fn lark_parse_post_message_accepts_md_tag_text_content() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_testuser123" } },
                "message": {
                    "message_type": "post",
                    "chat_type": "p2p",
                    "chat_id": "oc_chat",
                    "mentions": [],
                    "content": "{\"zh_cn\":{\"title\":\"\",\"content\":[[{\"tag\":\"md\",\"text\":\"* 1\\n* 2\"}]]}}"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "* 1\n* 2");
    }

    #[tokio::test]
    async fn lark_parse_group_message_allows_without_mention_when_disabled() {
        let ch = LarkChannel::new(
            "cli_app123".into(),
            "secret".into(),
            "token".into(),
            None,
            "lark_test_alias",
            resolver_from(vec!["*".into()]),
            false,
        );

        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_type": "group",
                    "chat_id": "oc_chat",
                    "mentions": []
                }
            }
        });

        assert_eq!(ch.parse_event_payload(&payload).await.len(), 1);
    }

    #[test]
    fn lark_reaction_url_matches_region() {
        let ch_lark = make_channel();
        assert_eq!(
            ch_lark.message_reaction_url("om_test_message_id"),
            "https://open.larksuite.com/open-apis/im/v1/messages/om_test_message_id/reactions"
        );

        let feishu_cfg = zeroclaw_config::schema::LarkConfig {
            enabled: true,
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            mention_only: false,
            use_feishu: true,
            receive_mode: zeroclaw_config::schema::LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
            excluded_tools: vec![],
            approval_timeout_secs: 300,
            per_user_session: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
        };
        let ch_feishu = LarkChannel::from_config(
            &feishu_cfg,
            "feishu_test_alias",
            resolver_from(vec!["*".into()]),
        );
        assert_eq!(
            ch_feishu.message_reaction_url("om_test_message_id"),
            "https://open.feishu.cn/open-apis/im/v1/messages/om_test_message_id/reactions"
        );
    }

    #[test]
    fn lark_image_max_bytes_is_10_mib() {
        assert_eq!(LARK_IMAGE_MAX_BYTES, 10 * 1024 * 1024);
    }

    #[test]
    fn lark_file_download_url_matches_region() {
        let ch = make_channel();
        assert_eq!(
            ch.file_download_url("om_msg123", "file_abc"),
            "https://open.larksuite.com/open-apis/im/v1/messages/om_msg123/resources/file_abc?type=file"
        );
    }

    #[test]
    fn lark_detect_image_mime_from_magic_bytes() {
        let png = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        assert_eq!(
            lark_detect_image_mime(None, &png).as_deref(),
            Some("image/png")
        );

        let jpeg = [0xff, 0xd8, 0xff, 0xe0];
        assert_eq!(
            lark_detect_image_mime(None, &jpeg).as_deref(),
            Some("image/jpeg")
        );

        let gif = b"GIF89a...";
        assert_eq!(
            lark_detect_image_mime(None, gif).as_deref(),
            Some("image/gif")
        );

        // Unknown bytes should fall back to content-type header
        let unknown = [0x00, 0x01, 0x02];
        assert_eq!(
            lark_detect_image_mime(Some("image/webp"), &unknown).as_deref(),
            Some("image/webp")
        );

        // Non-image content-type should be rejected
        assert_eq!(lark_detect_image_mime(Some("text/html"), &unknown), None);

        // No info at all should return None
        assert_eq!(lark_detect_image_mime(None, &unknown), None);
    }

    #[test]
    fn lark_is_text_filename_recognizes_common_extensions() {
        assert!(lark_is_text_filename("script.py"));
        assert!(lark_is_text_filename("config.toml"));
        assert!(lark_is_text_filename("data.csv"));
        assert!(lark_is_text_filename("README.md"));
        assert!(!lark_is_text_filename("image.png"));
        assert!(!lark_is_text_filename("archive.zip"));
        assert!(!lark_is_text_filename("binary.exe"));
    }

    #[test]
    fn lark_inline_text_file_preview_truncates_on_utf8_boundary() {
        let prefix = "a".repeat(49_999);
        let text = format!("{prefix}{}tail", "😀");
        let preview = lark_inline_text_file_preview(Cow::Borrowed(&text));

        assert_eq!(preview, format!("{prefix}...\n[truncated]"));
    }

    #[test]
    fn build_interactive_card_body_produces_correct_structure() {
        let body = build_interactive_card_body("oc_chat123", "**Hello** world");
        assert_eq!(body["receive_id"], "oc_chat123");
        assert_eq!(body["msg_type"], "interactive");

        let content: serde_json::Value =
            serde_json::from_str(body["content"].as_str().unwrap()).unwrap();
        assert_eq!(content["schema"], "2.0");
        let elements = content["body"]["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0]["tag"], "markdown");
        assert_eq!(elements[0]["content"], "**Hello** world");
    }

    #[test]
    fn build_card_content_produces_valid_json() {
        let content = build_card_content("# Title\n\n**Bold** text");
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["schema"], "2.0");
        assert_eq!(parsed["body"]["elements"][0]["tag"], "markdown");
        assert_eq!(
            parsed["body"]["elements"][0]["content"],
            "# Title\n\n**Bold** text"
        );
    }

    #[test]
    fn split_markdown_chunks_single_chunk_for_small_content() {
        let text = "Hello world";
        let chunks = split_markdown_chunks(text, LARK_CARD_MARKDOWN_MAX_BYTES);
        assert_eq!(chunks, vec!["Hello world"]);
    }

    #[test]
    fn split_markdown_chunks_splits_on_newline_boundaries() {
        let line = "abcdefghij\n"; // 11 bytes per line
        let text = line.repeat(10); // 110 bytes total
        let chunks = split_markdown_chunks(&text, 33); // ~3 lines per chunk
        assert_eq!(chunks.len(), 4);
        for chunk in &chunks[..3] {
            assert!(chunk.len() <= 33);
            assert!(chunk.ends_with('\n'));
        }
    }

    #[test]
    fn split_markdown_chunks_handles_no_newlines() {
        let text = "a".repeat(100);
        let chunks = split_markdown_chunks(&text, 30);
        assert!(chunks.len() > 1);
        let reassembled: String = chunks.concat();
        assert_eq!(reassembled, text);
    }

    #[test]
    fn split_markdown_chunks_exact_boundary() {
        let text = "abc";
        let chunks = split_markdown_chunks(text, 3);
        assert_eq!(chunks, vec!["abc"]);
    }

    #[test]
    fn lark_manager_none_when_transcription_not_configured() {
        let ch = make_channel();
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn lark_manager_none_when_disabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        };
        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn lark_manager_none_and_warn_on_init_failure() {
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some(String::new()),
            ..Default::default()
        };
        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription_manager.is_none());
        assert!(ch.transcription.is_some());
    }

    #[test]
    fn lark_audio_extensionless_file_key_falls_back_to_m4a() {
        assert_eq!(inferred_audio_filename("abc123"), "voice.m4a");
        assert_eq!(inferred_audio_filename("file_without_ext"), "voice.m4a");
    }

    #[test]
    fn lark_audio_extensionless_file_key_preserves_existing_extension() {
        assert_eq!(inferred_audio_filename("abc.m4a"), "abc.m4a");
        assert_eq!(inferred_audio_filename("voice.ogg"), "voice.ogg");
        assert_eq!(inferred_audio_filename("audio.mp3"), "audio.mp3");
        assert_eq!(inferred_audio_filename("note.aac"), "note.aac");
        assert_eq!(inferred_audio_filename("file.wav"), "file.wav");
    }

    #[tokio::test]
    async fn lark_parse_audio_message_type_skipped_without_manager() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_testuser123"
                    }
                },
                "message": {
                    "message_id": "om_audio123",
                    "message_type": "audio",
                    "content": "{\"file_key\":\"audio_file_key\"}",
                    "chat_id": "oc_chat123",
                    "chat_type": "p2p",
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_text_still_works_via_async_path() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_testuser123"
                    }
                },
                "message": {
                    "message_id": "om_text123",
                    "message_type": "text",
                    "content": "{\"text\":\"Hello async!\"}",
                    "chat_id": "oc_chat123",
                    "chat_type": "p2p",
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello async!");
    }

    #[tokio::test]
    async fn lark_audio_group_without_mention_skips_before_download() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_testuser123"
                    }
                },
                "message": {
                    "message_id": "om_audio_group",
                    "message_type": "audio",
                    "content": "{\"file_key\":\"audio_file_key\"}",
                    "chat_id": "oc_group123",
                    "chat_type": "group",
                    "mentions": [],
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_feishu_audio_uses_feishu_api_base() {
        let ch = LarkChannel::new_with_platform(
            "app_id".into(),
            "secret".into(),
            "token".into(),
            None,
            "feishu_test_alias",
            resolver_from(vec![]),
            false,
            LarkPlatform::Feishu,
        );
        assert_eq!(ch.api_base(), FEISHU_BASE_URL);
    }

    #[tokio::test]
    async fn lark_audio_file_key_missing_returns_none() {
        let ch = make_channel();
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: "http://localhost:0/v1/transcribe".to_string(),
                bearer_token: Some("unused".to_string()),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
            ..Default::default()
        };
        let ch = ch.with_transcription(tc);
        let manager = ch.transcription_manager.as_deref().unwrap();

        let result = ch
            .try_transcribe_audio_message("om_123", "{}", manager)
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn lark_audio_skips_when_manager_none() {
        let ch = make_channel();
        assert!(ch.transcription_manager.is_none());

        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_testuser123" }
                },
                "message": {
                    "message_id": "om_audio_1",
                    "message_type": "audio",
                    "content": "{\"file_key\":\"fk_abc123\"}",
                    "chat_id": "oc_chat1",
                    "chat_type": "p2p",
                    "mentions": [],
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn lark_audio_routes_through_transcription_manager() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock the tenant access token endpoint
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "test-tenant-token",
                "expire": 7200
            })))
            .mount(&mock_server)
            .await;

        // Mock the audio resource download endpoint
        Mock::given(method("GET"))
            .and(path_regex("/im/v1/messages/.+/resources/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 128]))
            .mount(&mock_server)
            .await;

        // Mock whisper transcription endpoint
        let whisper_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "test transcript"})),
            )
            .mount(&whisper_server)
            .await;

        let config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", whisper_server.uri()),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
            ..Default::default()
        };

        let mut ch = make_channel();
        ch.api_base_override = Some(mock_server.uri());
        let ch = ch.with_transcription(config);

        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_testuser123" }
                },
                "message": {
                    "message_id": "om_audio_2",
                    "message_type": "audio",
                    "content": "{\"file_key\":\"fk_abc123\"}",
                    "chat_id": "oc_chat1",
                    "chat_type": "p2p",
                    "mentions": [],
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "test transcript");
    }

    #[tokio::test]
    async fn lark_audio_token_refresh_on_invalid_token_response() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Token endpoint always returns valid token
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "refreshed-token",
                "expire": 7200
            })))
            .mount(&mock_server)
            .await;

        // Resource endpoint: first call returns 401, second returns audio bytes
        Mock::given(method("GET"))
            .and(path_regex("/im/v1/messages/.+/resources/.+"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "code": 99_991_663,
                "msg": "token invalid"
            })))
            .up_to_n_times(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path_regex("/im/v1/messages/.+/resources/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 64]))
            .mount(&mock_server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(mock_server.uri());

        let result = ch.download_audio_resource("om_msg_1", "fk_audio_key").await;
        assert!(result.is_ok());
        let (bytes, filename) = result.unwrap();
        assert_eq!(bytes.len(), 64);
        assert_eq!(filename, "voice.m4a");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Card 2.0 approval card tests
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn build_approval_card_contains_all_three_buttons() {
        let card = build_approval_card("test-id", "shell", "rm -rf /tmp/foo");

        // Card 2.0 schema lock — guard against future regressions where the
        // send-side schema drifts back to 1.0 (which Feishu's PATCH endpoint
        // silently refuses to re-render after the click).
        assert_eq!(
            card.get("schema").and_then(|v| v.as_str()),
            Some("2.0"),
            "approval card must use Card JSON 2.0 schema"
        );

        let columns = card
            .pointer("/body/elements/1/columns")
            .and_then(|v| v.as_array())
            .expect("column_set with columns missing");
        assert_eq!(
            columns.len(),
            3,
            "expected 3 button columns (Approve/Deny/Always)"
        );

        let decisions: Vec<&str> = columns
            .iter()
            .filter_map(|c| {
                c.pointer("/elements/0/behaviors/0/value/decision")
                    .and_then(|d| d.as_str())
            })
            .collect();
        assert_eq!(decisions, vec!["approve", "deny", "always"]);
    }

    #[test]
    fn build_approval_card_round_trips_approval_id_in_all_buttons() {
        let card = build_approval_card("approval-abc-123", "tool", "args");
        let columns = card["body"]["elements"][1]["columns"]
            .as_array()
            .expect("columns array");
        for column in columns {
            assert_eq!(
                column["elements"][0]["behaviors"][0]["value"]["approval_id"],
                "approval-abc-123"
            );
        }
    }

    #[test]
    fn build_approval_card_and_resolved_card_share_schema_version() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let send_card = build_approval_card("id", "shell", "args");
        let patch_card =
            build_resolved_approval_card("shell", "args", ChannelApprovalResponse::Approve);

        let send_schema = send_card.get("schema").and_then(|v| v.as_str());
        let patch_schema = patch_card.get("schema").and_then(|v| v.as_str());

        assert_eq!(
            send_schema, patch_schema,
            "send-time approval card and PATCH-time resolved card MUST use the same Card JSON schema; \
             Feishu's IM PATCH endpoint silently fails to re-render on the client when send/patch \
             schema versions differ"
        );
        assert_eq!(send_schema, Some("2.0"));
    }

    #[test]
    fn build_resolved_approval_card_uses_decision_specific_banner() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        for (decision, expected_template, expected_text_fragment) in [
            (ChannelApprovalResponse::Approve, "green", "Approved"),
            (
                ChannelApprovalResponse::AlwaysApprove,
                "green",
                "Approved (always)",
            ),
            (ChannelApprovalResponse::Deny, "red", "Denied"),
        ] {
            let card = build_resolved_approval_card("shell", "args", decision.clone());
            assert_eq!(
                card.pointer("/header/template").and_then(|v| v.as_str()),
                Some(expected_template),
                "decision={decision:?} should use header template {expected_template}"
            );
            let title = card
                .pointer("/header/title/content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert!(
                title.contains(expected_text_fragment),
                "decision={decision:?} header title `{title}` should contain `{expected_text_fragment}`"
            );
        }
    }

    #[test]
    fn sanitize_card_action_payload_redacts_sensitive_fields() {
        let raw = serde_json::json!({
            "action": {
                "tag": "button",
                "value": {
                    "approval_id": "2ecbcc0f-59f0-4216-ba1c-5b6f4deaf7c7",
                    "decision": "approve"
                }
            },
            "context": {
                "open_chat_id": "oc_real_chat_id_LEAKED",
                "open_message_id": "om_real_msg_id_LEAKED"
            },
            "host": "im_message",
            "operator": {
                "open_id": "ou_real_user_id_LEAKED",
                "tenant_key": "real_tenant_key_LEAKED",
                "union_id": "on_real_union_id_LEAKED",
                "user_id": "real_user_id_LEAKED"
            },
            "token": "c-real_callback_token_LEAKED"
        });

        let sanitized = sanitize_card_action_payload(&raw);
        let dumped = serde_json::to_string(&sanitized).expect("sanitized must serialize");

        for forbidden in [
            "oc_real_chat_id_LEAKED",
            "om_real_msg_id_LEAKED",
            "ou_real_user_id_LEAKED",
            "real_tenant_key_LEAKED",
            "on_real_union_id_LEAKED",
            "real_user_id_LEAKED",
            "c-real_callback_token_LEAKED",
        ] {
            assert!(
                !dumped.contains(forbidden),
                "sanitized payload must not contain raw value {forbidden:?}; got {dumped}"
            );
        }

        assert_eq!(sanitized["token"], "REDACTED_TOKEN");
        assert_eq!(
            sanitized["operator"]["open_id"],
            "REDACTED_OPERATOR_OPEN_ID"
        );
        assert_eq!(
            sanitized["operator"]["union_id"],
            "REDACTED_OPERATOR_UNION_ID"
        );
        assert_eq!(
            sanitized["operator"]["user_id"],
            "REDACTED_OPERATOR_USER_ID"
        );
        assert_eq!(
            sanitized["operator"]["tenant_key"],
            "REDACTED_OPERATOR_TENANT_KEY"
        );
        assert_eq!(
            sanitized["context"]["open_chat_id"],
            "REDACTED_OPEN_CHAT_ID"
        );
        assert_eq!(
            sanitized["context"]["open_message_id"],
            "REDACTED_OPEN_MESSAGE_ID"
        );

        assert_eq!(
            sanitized["action"]["value"]["approval_id"],
            "2ecbcc0f-59f0-4216-ba1c-5b6f4deaf7c7"
        );
        assert_eq!(sanitized["action"]["value"]["decision"], "approve");
        assert_eq!(sanitized["action"]["tag"], "button");
        assert_eq!(sanitized["host"], "im_message");

        assert_eq!(raw["token"], "c-real_callback_token_LEAKED");
        assert_eq!(raw["operator"]["open_id"], "ou_real_user_id_LEAKED");
    }

    #[test]
    fn sanitize_card_action_payload_handles_missing_optional_fields() {
        let raw = serde_json::json!({
            "action": { "value": { "approval_id": "x", "decision": "approve" } }
        });
        let sanitized = sanitize_card_action_payload(&raw);
        assert!(sanitized.get("token").is_none());
        assert!(sanitized.get("operator").is_none());
        assert!(sanitized.get("context").is_none());
        assert_eq!(sanitized["action"]["value"]["decision"], "approve");
    }

    #[test]
    fn sanitize_card_action_payload_redacts_committed_fixtures() {
        let fixtures: [(&str, &str); 3] = [
            (
                "card_action_approve.json",
                include_str!("../tests/fixtures/lark/card_action_approve.json"),
            ),
            (
                "card_action_deny.json",
                include_str!("../tests/fixtures/lark/card_action_deny.json"),
            ),
            (
                "card_action_always.json",
                include_str!("../tests/fixtures/lark/card_action_always.json"),
            ),
        ];
        for (name, raw_text) in fixtures {
            let raw: serde_json::Value = serde_json::from_str(raw_text)
                .unwrap_or_else(|e| panic!("parse fixture {name}: {e}"));
            let sanitized = sanitize_card_action_payload(&raw);
            let dumped =
                serde_json::to_string(&sanitized).expect("sanitized fixture must serialize");
            for placeholder_field in [
                "REDACTED_TOKEN",
                "REDACTED_OPERATOR_OPEN_ID",
                "REDACTED_OPEN_CHAT_ID",
            ] {
                assert!(
                    dumped.contains(placeholder_field),
                    "sanitizer output for {name} must contain {placeholder_field}; got {dumped}"
                );
            }
        }
    }

    #[tokio::test]
    async fn handle_card_action_event_routes_approve_to_pending_sender() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        let ch = make_channel();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let approval_id = "test-approval-1".to_string();
        ch.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                message_id: String::new(),
                tool_name: String::new(),
                arguments_summary: String::new(),
            },
        );

        let event = serde_json::json!({
            "action": {
                "value": { "approval_id": approval_id, "decision": "approve" },
                "tag": "button"
            }
        });
        ch.handle_card_action_event(&event)
            .await
            .expect("handler ok");
        let result = rx.await.expect("oneshot delivered");
        assert_eq!(result, ChannelApprovalResponse::Approve);
    }

    #[tokio::test]
    async fn handle_card_action_event_parses_card_v2_behaviors_value_payload() {
        use zeroclaw_api::channel::ChannelApprovalResponse;

        // Card 2.0 button click events MAY round-trip via
        // event.action.behaviors[0].value instead of event.action.value.
        // Verify the dual-pointer fallback.
        let ch = make_channel();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let approval_id = "test-v2-approval".to_string();
        ch.pending_approvals.lock().await.insert(
            approval_id.clone(),
            PendingApproval {
                sender: tx,
                message_id: String::new(),
                tool_name: String::new(),
                arguments_summary: String::new(),
            },
        );

        let event = serde_json::json!({
            "action": {
                "tag": "button",
                "behaviors": [{
                    "type": "callback",
                    "value": { "approval_id": approval_id, "decision": "always" }
                }]
            }
        });
        ch.handle_card_action_event(&event)
            .await
            .expect("handler ok");
        let result = rx.await.expect("oneshot delivered");
        assert_eq!(result, ChannelApprovalResponse::AlwaysApprove);
    }

    #[tokio::test]
    async fn handle_card_action_event_for_unknown_approval_is_not_an_error() {
        let ch = make_channel();
        let event = serde_json::json!({
            "action": {
                "value": { "approval_id": "never-existed", "decision": "deny" }
            }
        });
        // Unknown approval IDs are dropped silently (info-log only); the
        // handler must NOT propagate an error to the caller, since stray
        // clicks (resent after restart) are routine.
        ch.handle_card_action_event(&event)
            .await
            .expect("unknown approval id should not error");
    }
    async fn mount_lark_token_and_send_mocks(mock_server: &wiremock::MockServer) {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, ResponseTemplate};

        Mock::given(method("POST"))
            .and(path("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "test-tenant-token",
                "expire": 7200
            })))
            .mount(mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/im/v1/messages"))
            .and(query_param("receive_id_type", "chat_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": { "message_id": "om_test_message_id" }
            })))
            .expect(1)
            .mount(mock_server)
            .await;
    }

    async fn assert_send_body_matches_recipient_and_text(
        mock_server: &wiremock::MockServer,
        expected_recipient: &str,
        expected_text: &str,
    ) {
        let requests = mock_server
            .received_requests()
            .await
            .expect("mock server should record requests");
        let send_request = requests
            .iter()
            .find(|r| r.url.path() == "/im/v1/messages")
            .expect("expected at least one POST /im/v1/messages");
        assert_eq!(
            send_request.url.query(),
            Some("receive_id_type=chat_id"),
            "send URL must carry receive_id_type=chat_id query param"
        );
        let body: serde_json::Value =
            serde_json::from_slice(&send_request.body).expect("send body should be valid JSON");
        assert_eq!(
            body["receive_id"].as_str(),
            Some(expected_recipient),
            "receive_id must match the SendMessage recipient; full body: {body}"
        );
        assert_eq!(
            body["msg_type"].as_str(),
            Some("interactive"),
            "msg_type must be 'interactive'; full body: {body}"
        );
        let content_str = body["content"]
            .as_str()
            .expect("content must be a JSON string per Lark interactive-card spec");
        assert!(
            content_str.contains(expected_text),
            "card content should embed the message text {expected_text:?}; got: {content_str}"
        );
    }

    #[tokio::test]
    async fn lark_send_via_from_config_emits_post_to_messages_endpoint() {
        let mock_server = wiremock::MockServer::start().await;
        mount_lark_token_and_send_mocks(&mock_server).await;

        let config = zeroclaw_config::schema::LarkConfig {
            enabled: true,
            use_feishu: false,
            app_id: "cli_test_app_id".to_string(),
            app_secret: "test_app_secret".to_string(),
            approval_timeout_secs: 300,
            ..Default::default()
        };
        let mut ch = LarkChannel::from_config(&config, "test_alias", resolver_from(vec![]));
        ch.api_base_override = Some(mock_server.uri());

        assert_eq!(
            ch.name(),
            "lark",
            "use_feishu=false must keep the channel identity as 'lark'"
        );

        let message = SendMessage::new("hi from cron", "oc_test_chat_id");
        Channel::send(&ch, &message)
            .await
            .expect("Channel::send should succeed against mocked Lark endpoint");

        assert_send_body_matches_recipient_and_text(
            &mock_server,
            "oc_test_chat_id",
            "hi from cron",
        )
        .await;
    }

    #[tokio::test]
    async fn feishu_send_via_from_config_emits_post_to_messages_endpoint() {
        let mock_server = wiremock::MockServer::start().await;
        mount_lark_token_and_send_mocks(&mock_server).await;

        let config = zeroclaw_config::schema::LarkConfig {
            enabled: true,
            use_feishu: true,
            app_id: "cli_test_app_id".to_string(),
            app_secret: "test_app_secret".to_string(),
            approval_timeout_secs: 300,
            ..Default::default()
        };
        let mut ch = LarkChannel::from_config(&config, "test_alias", resolver_from(vec![]));
        ch.api_base_override = Some(mock_server.uri());

        assert_eq!(
            ch.name(),
            "feishu",
            "use_feishu=true must surface the channel identity as 'feishu' \
             (registry key alignment — see orchestrator::deliver_announcement)"
        );

        let message = SendMessage::new("hi from cron", "oc_test_chat_id");
        Channel::send(&ch, &message)
            .await
            .expect("Channel::send should succeed against mocked Feishu endpoint");

        assert_send_body_matches_recipient_and_text(
            &mock_server,
            "oc_test_chat_id",
            "hi from cron",
        )
        .await;
    }

    #[test]
    fn unicode_to_lark_emoji_type_covers_known_noreply_emojis() {
        assert_eq!(unicode_to_lark_emoji_type("👍"), Some("THUMBSUP"));
        assert_eq!(unicode_to_lark_emoji_type("🚫"), Some("No"));
        assert_eq!(unicode_to_lark_emoji_type("⚠️"), Some("Alarm"));
        assert_eq!(unicode_to_lark_emoji_type("👀"), Some("GLANCE"));
        assert_eq!(unicode_to_lark_emoji_type("✅"), Some("DONE"));
        assert_eq!(unicode_to_lark_emoji_type("🎉"), Some("PARTY"));
        assert_eq!(unicode_to_lark_emoji_type("🙉"), None);
        assert_ne!(unicode_to_lark_emoji_type("🚫"), Some("NO"));
    }

    /// Regression guard: ChannelMessage.id MUST equal the Feishu om_xxx
    /// message_id so that the orchestrator's add_reaction calls (which
    /// pass msg.id straight to `/im/v1/messages/{message_id}/reactions`)
    /// succeed instead of returning HTTP 400 / code 99992354
    /// "Invalid ids: [<uuid>]". Replacing the inbound id with
    /// `Uuid::new_v4()` silently breaks the 👀/✅ ack/done reaction flow.
    #[tokio::test]
    async fn lark_inbound_channel_message_id_is_om_xxx_not_uuid() {
        let ch = make_channel();
        let om_id = "om_ack_reaction_compat_xyz";
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_testuser123"
                    }
                },
                "message": {
                    "message_id": om_id,
                    "message_type": "text",
                    "content": "{\"text\":\"ack test\"}",
                    "chat_id": "oc_chat123",
                    "chat_type": "p2p",
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].id, om_id,
            "ChannelMessage.id must equal the Feishu om_xxx message_id; \
             otherwise add_reaction returns 99992354 (id not exist). \
             Got: {:?}",
            msgs[0].id
        );

        // Belt-and-suspenders: explicitly assert msg.id is NOT a
        // UUID-v4 shape (8-4-4-4-12 hex with hyphens). Future "let's
        // just use UUID" PRs will fail this and prompt a re-read.
        fn looks_like_uuid_v4(s: &str) -> bool {
            let bytes = s.as_bytes();
            if bytes.len() != 36 {
                return false;
            }
            for (i, &b) in bytes.iter().enumerate() {
                let is_hyphen_pos = i == 8 || i == 13 || i == 18 || i == 23;
                if is_hyphen_pos {
                    if b != b'-' {
                        return false;
                    }
                } else if !b.is_ascii_hexdigit() {
                    return false;
                }
            }
            true
        }
        assert!(
            !looks_like_uuid_v4(&msgs[0].id),
            "ChannelMessage.id must NOT be a UUID-v4 shape — Feishu \
             add_reaction requires the native om_xxx open_message_id. \
             Got: {:?}",
            msgs[0].id
        );
    }

    #[tokio::test]
    async fn remove_reaction_caches_id_from_add_and_deletes() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-rm-ok",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        let post_mock = Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_test/reactions$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_xyz",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000000",
                    "reaction_type": { "emoji_type": "GLANCE" }
                }
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let delete_mock = Mock::given(method("DELETE"))
            .and(path_regex("/im/v1/messages/om_test/reactions/r_xyz$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "code": 0 })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        ch.add_reaction("oc_chat", "om_test", "\u{1F440}")
            .await
            .expect("add_reaction should succeed");
        ch.remove_reaction("oc_chat", "om_test", "\u{1F440}")
            .await
            .expect("remove_reaction should succeed");

        let cache = ch.reaction_ids.lock().await;
        assert!(
            cache.is_empty(),
            "reaction_ids cache should be empty after remove, got {} entries",
            cache.len()
        );

        drop(post_mock);
        drop(delete_mock);
    }

    #[tokio::test]
    async fn remove_reaction_silent_on_cache_miss() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-rm-miss",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        let delete_mock = Mock::given(method("DELETE"))
            .and(path_regex("/im/v1/messages/.*/reactions/.*"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        ch.remove_reaction("oc_chat", "om_never_added", "\u{1F440}")
            .await
            .expect("cache miss must not error");

        drop(delete_mock);
    }

    #[tokio::test]
    async fn remove_reaction_tolerates_server_stale_codes() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-rm-stale",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_stale/reactions$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_stale",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000000",
                    "reaction_type": { "emoji_type": "GLANCE" }
                }
            })))
            .mount(&server)
            .await;

        let delete_mock = Mock::given(method("DELETE"))
            .and(path_regex("/im/v1/messages/om_stale/reactions/r_stale$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 231_007,
                "msg": "operator has no permission to delete this reaction"
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        ch.add_reaction("oc_chat", "om_stale", "\u{1F440}")
            .await
            .expect("add_reaction should succeed");
        ch.remove_reaction("oc_chat", "om_stale", "\u{1F440}")
            .await
            .expect("stale-state code must not propagate as error");

        let cache = ch.reaction_ids.lock().await;
        assert!(
            cache.is_empty(),
            "reaction_ids cache should be empty after stale-state DELETE"
        );

        drop(delete_mock);
    }

    #[tokio::test]
    async fn add_reaction_caches_glance_under_unicode_key() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-glance",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        let post_mock = Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_glance/reactions$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_glance_xyz",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000000",
                    "reaction_type": { "emoji_type": "GLANCE" }
                }
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        ch.add_reaction("oc_chat", "om_glance", "\u{1F440}")
            .await
            .expect("add_reaction should succeed");

        let cache = ch.reaction_ids.lock().await;
        let stored = cache
            .get(&("om_glance".to_string(), "\u{1F440}".to_string()))
            .cloned();
        assert_eq!(
            stored.as_deref(),
            Some("r_glance_xyz"),
            "reaction_id must be cached under unicode 👀 key, got {stored:?}"
        );
        assert!(
            cache
                .get(&("om_glance".to_string(), "GLANCE".to_string()))
                .is_none(),
            "reaction_id must NOT be cached under Feishu emoji_type 'GLANCE'"
        );

        drop(post_mock);
    }

    /// End-to-end regression for the inbound-ack lifecycle:
    ///   add 👀 → remove 👀 → add ✅
    ///
    /// Asserts the "shared cached reaction-id contract" that the PR review
    /// requested. The Lark-local inbound fast-ack spawn (in `listen_ws` /
    /// `listen_http`) and the generic orchestrator `Channel::add_reaction`
    /// call BOTH go through the same trait impl, which writes Feishu's
    /// returned `reaction_id` into `reaction_ids` and dedupes duplicate
    /// POSTs via a cache-hit fast-path. As a result `remove_reaction("👀")`
    /// always finds the right id and no orphan 👀 is left beside the
    /// completion marker.
    ///
    /// The two strong assertions:
    ///   1. The mock counts EXACTLY one POST per emoji and EXACTLY one
    ///      DELETE on the cached `reaction_id`. This is the
    ///      shared-cache invariant — even though both the inbound fast-ack
    ///      and the orchestrator may call `add_reaction("👀")` for the
    ///      same message, the second call is a cache hit and does NOT
    ///      issue a second POST (see
    ///      `lark_fast_ack_and_generic_path_dedupe_on_cache_hit` for the
    ///      explicit dedupe test).
    ///   2. The final `reaction_ids` cache shape contains ONLY ✅ —
    ///      i.e. the 👀 entry was removed and no orphan was left behind.
    #[tokio::test]
    async fn lark_inbound_ack_lifecycle_swaps_glance_to_done_with_no_orphan() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-lifecycle",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        // POST 👀 (GLANCE) — must be invoked EXACTLY once.
        // If a regression re-adds a Lark-local fast-ack spawn alongside
        // the generic orchestrator add_reaction call, this mock would see
        // a second POST and the assertion below would fail.
        let post_glance_mock = Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_lifecycle/reactions$"))
            .and(wiremock::matchers::body_string_contains("GLANCE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_glance_lifecycle",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000000",
                    "reaction_type": { "emoji_type": "GLANCE" }
                }
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        // DELETE on the cached GLANCE reaction_id — must be invoked
        // EXACTLY once. Cache-miss path would silently skip the DELETE
        // (see `remove_reaction` doc) and this expect(1) would fail.
        let delete_glance_mock = Mock::given(method("DELETE"))
            .and(path_regex(
                "/im/v1/messages/om_lifecycle/reactions/r_glance_lifecycle$",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "code": 0 })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        // POST ✅ (DONE) — must be invoked EXACTLY once.
        let post_done_mock = Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_lifecycle/reactions$"))
            .and(wiremock::matchers::body_string_contains("DONE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_done_lifecycle",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000001",
                    "reaction_type": { "emoji_type": "DONE" }
                }
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        // Drive the lifecycle through the public Channel trait — the
        // same surface the generic orchestrator uses in production.
        ch.add_reaction("oc_chat", "om_lifecycle", "\u{1F440}")
            .await
            .expect("add 👀 should succeed");
        ch.remove_reaction("oc_chat", "om_lifecycle", "\u{1F440}")
            .await
            .expect("remove 👀 should succeed");
        ch.add_reaction("oc_chat", "om_lifecycle", "\u{2705}")
            .await
            .expect("add ✅ should succeed");

        // Cache shape: ✅ present, 👀 gone, no orphans.
        let cache = ch.reaction_ids.lock().await;
        assert_eq!(
            cache.len(),
            1,
            "after lifecycle the cache must contain exactly 1 entry (✅), got {}: {:?}",
            cache.len(),
            cache.keys().collect::<Vec<_>>()
        );
        assert!(
            cache
                .get(&("om_lifecycle".to_string(), "\u{1F440}".to_string()))
                .is_none(),
            "the 👀 entry must be gone after remove_reaction; \
             orphan presence indicates a parallel ack path bypassed the cache"
        );
        assert_eq!(
            cache
                .get(&("om_lifecycle".to_string(), "\u{2705}".to_string()))
                .map(String::as_str),
            Some("r_done_lifecycle"),
            "✅ reaction_id must be cached under its unicode key"
        );

        // Mock-scope drop verifies the .expect(N) counts. A regression
        // that POSTs 👀 twice (fast-ack + generic) makes post_glance_mock
        // fail with 'received 2 requests, expected 1'.
        drop(post_glance_mock);
        drop(delete_glance_mock);
        drop(post_done_mock);
    }

    /// Shared-cache dedupe contract: when the Lark-local inbound fast-ack
    /// has already POSTed `add_reaction(om_xxx, "👀")` and written
    /// `(om_xxx, "👀") → R1` into `reaction_ids`, a subsequent
    /// `add_reaction(om_xxx, "👀")` call from the generic orchestrator
    /// path MUST be a cache-hit no-op — NO second POST is issued, and
    /// the cached reaction_id is preserved so `remove_reaction("👀")` can
    /// still DELETE it correctly.
    ///
    /// This is the precise invariant the PR review asked for ("make the
    /// Lark-local ack use the same cached reaction-id contract as the
    /// generic path"). Without the cache-hit fast-path in `add_reaction`
    /// the generic call would issue a second POST: Feishu would either
    /// silently dedupe and return no reaction_id (leaving R1 cached but
    /// an unverifiable duplicate POST on the wire) OR return a non-zero
    /// business code; in either case `remove_reaction` would still find
    /// R1 in cache, but the wire-level duplicate POST violates the
    /// contract. This test asserts the wire stays clean: ONE POST 👀,
    /// then ONE DELETE on R1.
    #[tokio::test]
    async fn lark_fast_ack_and_generic_path_dedupe_on_cache_hit() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use zeroclaw_api::channel::Channel;

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path_regex("/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "tenant_access_token": "t-dedupe",
                "expire": 7200
            })))
            .mount(&server)
            .await;

        // POST 👀 — MUST be invoked EXACTLY once across BOTH calls.
        // The first call is the fast-ack; the second call (simulating
        // the generic orchestrator path) MUST hit the cache and skip
        // the POST entirely. expect(1) catches a regression where the
        // dedupe fast-path is missing or broken.
        let post_glance_mock = Mock::given(method("POST"))
            .and(path_regex("/im/v1/messages/om_dedupe/reactions$"))
            .and(wiremock::matchers::body_string_contains("GLANCE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0,
                "data": {
                    "reaction_id": "r_dedupe_fast_ack",
                    "operator": { "operator_id": "cli_test", "operator_type": "app" },
                    "action_time": "1700000000000",
                    "reaction_type": { "emoji_type": "GLANCE" }
                }
            })))
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        // DELETE on the cached reaction_id from the FAST-ACK POST — proves
        // that fast-ack's reaction_id survived through the dedupe path
        // and is still usable for cleanup.
        let delete_glance_mock = Mock::given(method("DELETE"))
            .and(path_regex(
                "/im/v1/messages/om_dedupe/reactions/r_dedupe_fast_ack$",
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "code": 0 })),
            )
            .expect(1)
            .mount_as_scoped(&server)
            .await;

        let mut ch = make_channel();
        ch.api_base_override = Some(server.uri());

        // Step 1: fast-ack POSTs 👀 and writes (om_dedupe, "👀") → R1.
        ch.add_reaction("oc_chat", "om_dedupe", "\u{1F440}")
            .await
            .expect("fast-ack add 👀 should succeed");

        // Sanity: cache populated.
        {
            let cache = ch.reaction_ids.lock().await;
            assert_eq!(
                cache
                    .get(&("om_dedupe".to_string(), "\u{1F440}".to_string()))
                    .map(String::as_str),
                Some("r_dedupe_fast_ack"),
                "fast-ack must populate cache under unicode 👀 key"
            );
        }

        // Step 2: generic orchestrator path tries to add 👀 again.
        // The cache-hit fast-path in add_reaction MUST return Ok(())
        // without issuing a second POST. If a regression removes the
        // dedupe check, post_glance_mock will receive 2 requests and
        // its expect(1) will fail.
        ch.add_reaction("oc_chat", "om_dedupe", "\u{1F440}")
            .await
            .expect("generic-path add 👀 must be cache-hit no-op, not error");

        // Cache must still hold the SAME reaction_id from the fast-ack —
        // the dedupe path must not overwrite it.
        {
            let cache = ch.reaction_ids.lock().await;
            assert_eq!(
                cache
                    .get(&("om_dedupe".to_string(), "\u{1F440}".to_string()))
                    .map(String::as_str),
                Some("r_dedupe_fast_ack"),
                "cache value must remain the fast-ack reaction_id after dedupe \
                 (no overwrite)"
            );
        }

        // Step 3: cleanup. DELETE must hit the cached fast-ack reaction_id.
        // If the dedupe path had wrongly issued a second POST and Feishu
        // had returned a different reaction_id that overwrote the cache,
        // delete_glance_mock's path-match on r_dedupe_fast_ack would
        // miss and the assertion would fail.
        ch.remove_reaction("oc_chat", "om_dedupe", "\u{1F440}")
            .await
            .expect("remove 👀 should DELETE the fast-ack reaction_id");

        // Cache must be empty after remove.
        {
            let cache = ch.reaction_ids.lock().await;
            assert!(
                cache.is_empty(),
                "cache must be empty after remove_reaction, got {} entries",
                cache.len()
            );
        }

        drop(post_glance_mock);
        drop(delete_glance_mock);
    }
}
