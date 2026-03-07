use super::ack_reaction::{select_ack_reaction, AckReactionContext, AckReactionContextChatType};
use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;

const FEISHU_BASE_URL: &str = "https://open.feishu.cn/open-apis";
const FEISHU_WS_BASE_URL: &str = "https://open.feishu.cn";
const LARK_BASE_URL: &str = "https://open.larksuite.com/open-apis";
const LARK_WS_BASE_URL: &str = "https://open.larksuite.com";

const LARK_ACK_REACTIONS_ZH_CN: &[&str] = &[
    "OK", "JIAYI", "APPLAUSE", "THUMBSUP", "MUSCLE", "SMILE", "DONE",
];
const LARK_ACK_REACTIONS_ZH_TW: &[&str] = &[
    "OK",
    "JIAYI",
    "APPLAUSE",
    "THUMBSUP",
    "FINGERHEART",
    "SMILE",
    "DONE",
];
const LARK_ACK_REACTIONS_EN: &[&str] = &[
    "OK",
    "THUMBSUP",
    "THANKS",
    "MUSCLE",
    "FINGERHEART",
    "APPLAUSE",
    "SMILE",
    "DONE",
];
const LARK_ACK_REACTIONS_JA: &[&str] = &[
    "OK",
    "THUMBSUP",
    "THANKS",
    "MUSCLE",
    "FINGERHEART",
    "APPLAUSE",
    "SMILE",
    "DONE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LarkAckLocale {
    ZhCn,
    ZhTw,
    En,
    Ja,
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
    content: serde_json::Value,
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
/// Retention window for seen event/message dedupe keys.
const LARK_EVENT_DEDUP_TTL: Duration = Duration::from_secs(30 * 60);
/// Periodic cleanup interval for the dedupe cache.
const LARK_EVENT_DEDUP_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT: &str =
    "[Image message received but could not be downloaded]";

/// Returns true when the WebSocket frame indicates live traffic that should
/// refresh the heartbeat watchdog.
fn should_refresh_last_recv(msg: &WsMsg) -> bool {
    matches!(msg, WsMsg::Binary(_) | WsMsg::Ping(_) | WsMsg::Pong(_))
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

fn normalize_message_content(content: &serde_json::Value) -> Option<serde_json::Value> {
    match content {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(trimmed).ok()
        }
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => Some(content.clone()),
        _ => None,
    }
}

fn extract_text_message_content(content: &serde_json::Value) -> Option<String> {
    let normalized = normalize_message_content(content)?;
    match normalized {
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|text| text.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned),
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    }
}

fn parse_image_key(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| {
            value
                .get("image_key")
                .and_then(|key| key.as_str())
                .map(str::to_string)
        })
}

fn parse_image_key_value(content: &serde_json::Value) -> Option<String> {
    let normalized = normalize_message_content(content)?;
    match normalized {
        serde_json::Value::Object(map) => map
            .get("image_key")
            .and_then(|key| key.as_str())
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned),
        serde_json::Value::String(raw) => parse_image_key(&raw),
        _ => None,
    }
}

fn is_image_filename(path_like: &str) -> bool {
    let normalized = path_like
        .split('?')
        .next()
        .unwrap_or(path_like)
        .split('#')
        .next()
        .unwrap_or(path_like)
        .to_ascii_lowercase();

    normalized.ends_with(".png")
        || normalized.ends_with(".jpg")
        || normalized.ends_with(".jpeg")
        || normalized.ends_with(".gif")
        || normalized.ends_with(".webp")
        || normalized.ends_with(".bmp")
        || normalized.ends_with(".heic")
        || normalized.ends_with(".heif")
        || normalized.ends_with(".svg")
}

fn parse_image_marker_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let marker = trimmed.strip_prefix("[IMAGE:")?.strip_suffix(']')?.trim();
    if marker.is_empty() {
        return None;
    }
    Some(marker)
}

fn is_data_image_uri(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    lower.starts_with("data:image/") && lower.contains(";base64,")
}

fn extract_local_image_path_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed.trim_matches(|c| matches!(c, '`' | '"' | '\''));
    let candidate = candidate.strip_prefix("file://").unwrap_or(candidate);
    if candidate.is_empty() || candidate.contains('\0') {
        return None;
    }

    if !is_image_filename(candidate) {
        return None;
    }

    let path = Path::new(candidate);
    if !path.is_file() {
        return None;
    }

    Some(candidate.to_string())
}

fn parse_outgoing_content(content: &str) -> (String, Vec<String>) {
    let mut text_lines = Vec::new();
    let mut image_targets = Vec::new();

    for line in content.lines() {
        if let Some(marker_target) = parse_image_marker_line(line) {
            image_targets.push(marker_target.to_string());
            continue;
        }

        let trimmed = line.trim();
        if is_data_image_uri(trimmed) {
            image_targets.push(trimmed.to_string());
            continue;
        }

        if let Some(local_path) = extract_local_image_path_line(line) {
            image_targets.push(local_path);
            continue;
        }

        text_lines.push(line);
    }

    (text_lines.join("\n").trim().to_string(), image_targets)
}

fn decode_data_image_uri(source: &str) -> anyhow::Result<(Vec<u8>, String)> {
    let trimmed = source.trim();
    let (header, payload) = trimmed
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("invalid data URI: missing comma separator"))?;

    let lower_header = header.to_ascii_lowercase();
    if !lower_header.starts_with("data:image/") {
        anyhow::bail!("unsupported data URI mime (expected image/*): {header}");
    }
    if !lower_header.contains(";base64") {
        anyhow::bail!("unsupported data URI encoding (expected base64): {header}");
    }

    let mime = header
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or("image/png")
        .trim()
        .to_ascii_lowercase();

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| anyhow::anyhow!("invalid data URI base64 payload: {e}"))?;
    if bytes.is_empty() {
        anyhow::bail!("image payload is empty");
    }

    Ok((bytes, mime))
}

fn image_extension_from_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/heic" => "heic",
        "image/heif" => "heif",
        _ => "png",
    }
}

fn display_image_target(target: &str) -> String {
    let trimmed = target.trim();
    if is_data_image_uri(trimmed) {
        "[inline image data]".to_string()
    } else {
        trimmed.to_string()
    }
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

fn sanitize_lark_body(body: &serde_json::Value) -> String {
    crate::providers::sanitize_api_error(&body.to_string())
}

fn ensure_lark_send_success(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    context: &str,
) -> anyhow::Result<()> {
    if !status.is_success() {
        let sanitized = sanitize_lark_body(body);
        anyhow::bail!("Lark send failed {context}: status={status}, body={sanitized}");
    }

    let code = extract_lark_response_code(body).unwrap_or(0);
    if code != 0 {
        let sanitized = sanitize_lark_body(body);
        anyhow::bail!("Lark send failed {context}: code={code}, body={sanitized}");
    }

    Ok(())
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
    allowed_users: Vec<String>,
    group_reply_allowed_sender_ids: Vec<String>,
    /// Bot open_id resolved at runtime via `/bot/v3/info`.
    resolved_bot_open_id: Arc<StdRwLock<Option<String>>>,
    mention_only: bool,
    platform: LarkPlatform,
    /// How to receive events: WebSocket long-connection or HTTP webhook.
    receive_mode: crate::config::schema::LarkReceiveMode,
    /// Cached tenant access token
    tenant_token: Arc<RwLock<Option<CachedTenantToken>>>,
    /// Dedup set for recently seen event/message keys across WS + webhook paths.
    recent_event_keys: Arc<RwLock<HashMap<String, Instant>>>,
    /// Last time we ran TTL cleanup over the dedupe cache.
    recent_event_cleanup_at: Arc<RwLock<Instant>>,
    ack_reaction: Option<crate::config::AckReactionConfig>,
}

impl LarkChannel {
    pub fn new(
        app_id: String,
        app_secret: String,
        verification_token: String,
        port: Option<u16>,
        allowed_users: Vec<String>,
        mention_only: bool,
    ) -> Self {
        Self::new_with_platform(
            app_id,
            app_secret,
            verification_token,
            port,
            allowed_users,
            mention_only,
            LarkPlatform::Lark,
        )
    }

    fn new_with_platform(
        app_id: String,
        app_secret: String,
        verification_token: String,
        port: Option<u16>,
        allowed_users: Vec<String>,
        mention_only: bool,
        platform: LarkPlatform,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            verification_token,
            port,
            allowed_users,
            group_reply_allowed_sender_ids: Vec::new(),
            resolved_bot_open_id: Arc::new(StdRwLock::new(None)),
            mention_only,
            platform,
            receive_mode: crate::config::schema::LarkReceiveMode::default(),
            tenant_token: Arc::new(RwLock::new(None)),
            recent_event_keys: Arc::new(RwLock::new(HashMap::new())),
            recent_event_cleanup_at: Arc::new(RwLock::new(Instant::now())),
            ack_reaction: None,
        }
    }

    /// Configure ACK reaction policy.
    pub fn with_ack_reaction(
        mut self,
        ack_reaction: Option<crate::config::AckReactionConfig>,
    ) -> Self {
        self.ack_reaction = ack_reaction;
        self
    }

    /// Build from `LarkConfig` using legacy compatibility:
    /// when `use_feishu=true`, this instance routes to Feishu endpoints.
    pub fn from_config(config: &crate::config::schema::LarkConfig) -> Self {
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
            config.allowed_users.clone(),
            config.effective_group_reply_mode().requires_mention(),
            platform,
        );
        ch.group_reply_allowed_sender_ids =
            normalize_group_reply_allowed_sender_ids(config.group_reply_allowed_sender_ids());
        ch.receive_mode = config.receive_mode.clone();
        ch
    }

    pub fn from_lark_config(config: &crate::config::schema::LarkConfig) -> Self {
        let mut ch = Self::new_with_platform(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.verification_token.clone().unwrap_or_default(),
            config.port,
            config.allowed_users.clone(),
            config.effective_group_reply_mode().requires_mention(),
            LarkPlatform::Lark,
        );
        ch.group_reply_allowed_sender_ids =
            normalize_group_reply_allowed_sender_ids(config.group_reply_allowed_sender_ids());
        ch.receive_mode = config.receive_mode.clone();
        ch
    }

    pub fn from_feishu_config(config: &crate::config::schema::FeishuConfig) -> Self {
        let mut ch = Self::new_with_platform(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.verification_token.clone().unwrap_or_default(),
            config.port,
            config.allowed_users.clone(),
            config.effective_group_reply_mode().requires_mention(),
            LarkPlatform::Feishu,
        );
        ch.group_reply_allowed_sender_ids =
            normalize_group_reply_allowed_sender_ids(config.group_reply_allowed_sender_ids());
        ch.receive_mode = config.receive_mode.clone();
        ch
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client(self.platform.proxy_service_key())
    }

    fn channel_name(&self) -> &'static str {
        self.platform.channel_name()
    }

    fn api_base(&self) -> &'static str {
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

    fn message_reaction_url(&self, message_id: &str) -> String {
        format!("{}/im/v1/messages/{message_id}/reactions", self.api_base())
    }

    fn image_resource_url(&self, message_id: &str, image_key: &str) -> String {
        format!(
            "{}/im/v1/messages/{message_id}/resources/{image_key}",
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

    fn dedupe_event_key(event_id: Option<&str>, message_id: Option<&str>) -> Option<String> {
        let normalized_event = event_id.map(str::trim).filter(|value| !value.is_empty());
        if let Some(event_id) = normalized_event {
            return Some(format!("event:{event_id}"));
        }

        let normalized_message = message_id.map(str::trim).filter(|value| !value.is_empty());
        normalized_message.map(|message_id| format!("message:{message_id}"))
    }

    async fn try_mark_event_key_seen(&self, dedupe_key: &str) -> bool {
        let now = Instant::now();
        if self.recent_event_keys.read().await.contains_key(dedupe_key) {
            return false;
        }

        let should_cleanup = {
            let last_cleanup = self.recent_event_cleanup_at.read().await;
            now.duration_since(*last_cleanup) >= LARK_EVENT_DEDUP_CLEANUP_INTERVAL
        };

        let mut seen = self.recent_event_keys.write().await;
        if seen.contains_key(dedupe_key) {
            return false;
        }

        if should_cleanup {
            seen.retain(|_, t| now.duration_since(*t) < LARK_EVENT_DEDUP_TTL);
            let mut last_cleanup = self.recent_event_cleanup_at.write().await;
            *last_cleanup = now;
        }

        seen.insert(dedupe_key.to_string(), now);
        true
    }

    async fn fetch_image_marker(
        &self,
        message_id: &str,
        image_key: &str,
    ) -> anyhow::Result<String> {
        if message_id.trim().is_empty() {
            anyhow::bail!("empty message_id");
        }
        if image_key.trim().is_empty() {
            anyhow::bail!("empty image_key");
        }

        let mut token = self.get_tenant_access_token().await?;
        let mut retried = false;
        let url = self.image_resource_url(message_id, image_key);

        loop {
            let response = self
                .http_client()
                .get(&url)
                .query(&[("type", "image")])
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await?;

            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let body = response.bytes().await?;

            if status.is_success() {
                if body.is_empty() {
                    anyhow::bail!("image payload is empty");
                }
                let media_type = content_type
                    .as_deref()
                    .and_then(|value| value.split(';').next())
                    .map(str::trim)
                    .filter(|value| value.starts_with("image/"))
                    .unwrap_or("image/png");
                let encoded = base64::engine::general_purpose::STANDARD.encode(body);
                return Ok(format!("[IMAGE:data:{media_type};base64,{encoded}]"));
            }

            let parsed = serde_json::from_slice::<serde_json::Value>(&body)
                .unwrap_or(serde_json::Value::Null);
            if !retried && should_refresh_lark_tenant_token(status, &parsed) {
                self.invalidate_token().await;
                token = self.get_tenant_access_token().await?;
                retried = true;
                continue;
            }

            anyhow::bail!(
                "Lark image download failed: status={status}, body={}",
                crate::providers::sanitize_api_error(&String::from_utf8_lossy(&body))
            );
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

    /// Best-effort "received" signal for incoming messages.
    /// Failures are logged and never block normal message handling.
    async fn try_add_ack_reaction(&self, message_id: &str, emoji_type: &str) {
        if message_id.is_empty() {
            return;
        }

        let mut token = match self.get_tenant_access_token().await {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!("Lark: failed to fetch token for reaction: {err}");
                return;
            }
        };

        let mut retried = false;
        loop {
            let response = match self
                .post_message_reaction_with_token(message_id, &token, emoji_type)
                .await
            {
                Ok(resp) => resp,
                Err(err) => {
                    tracing::warn!("Lark: failed to add reaction for {message_id}: {err}");
                    return;
                }
            };

            if response.status().as_u16() == 401 && !retried {
                self.invalidate_token().await;
                token = match self.get_tenant_access_token().await {
                    Ok(new_token) => new_token,
                    Err(err) => {
                        tracing::warn!(
                            "Lark: failed to refresh token for reaction on {message_id}: {err}"
                        );
                        return;
                    }
                };
                retried = true;
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                let err_body = response.text().await.unwrap_or_default();
                let sanitized = crate::providers::sanitize_api_error(&err_body);
                tracing::warn!(
                    "Lark: add reaction failed for {message_id}: status={status}, body={sanitized}"
                );
                return;
            }

            let payload: serde_json::Value = match response.json().await {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!("Lark: add reaction decode failed for {message_id}: {err}");
                    return;
                }
            };

            let code = payload.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            if code != 0 {
                let msg = payload
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                tracing::warn!("Lark: add reaction returned code={code} for {message_id}: {msg}");
            }
            return;
        }
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
                "Lark WS endpoint failed: code={} msg={}",
                resp.code,
                resp.msg.as_deref().unwrap_or("(none)")
            );
        }
        let ep = resp
            .data
            .ok_or_else(|| anyhow::anyhow!("Lark WS endpoint: empty data"))?;
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
        tracing::info!("Lark: connecting to {wss_url}");

        let (ws_stream, _) = tokio_tungstenite::connect_async(&wss_url).await?;
        let (mut write, mut read) = ws_stream.split();
        tracing::info!("Lark: WS connected (service_id={service_id})");

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
            anyhow::bail!("Lark: initial ping failed");
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
                        tracing::warn!("Lark: ping failed, reconnecting");
                        break;
                    }
                    // GC stale fragments > 5 min
                    let cutoff = Instant::now().checked_sub(Duration::from_secs(300)).unwrap_or(Instant::now());
                    frag_cache.retain(|_, (_, ts)| *ts > cutoff);
                }

                _ = timeout_check.tick() => {
                    if last_recv.elapsed() > WS_HEARTBEAT_TIMEOUT {
                        tracing::warn!("Lark: heartbeat timeout, reconnecting");
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
                                WsMsg::Close(_) => { tracing::info!("Lark: WS closed — reconnecting"); break; }
                                _ => continue,
                            }
                        }
                        None => { tracing::info!("Lark: WS closed — reconnecting"); break; }
                        Some(Err(e)) => { tracing::error!("Lark: WS read error: {e}"); break; }
                    };

                    let frame = match PbFrame::decode(&raw[..]) {
                        Ok(f) => f,
                        Err(e) => { tracing::error!("Lark: proto decode: {e}"); continue; }
                    };

                    // CONTROL frame
                    if frame.method == 0 {
                        if frame.header_value("type") == "pong" {
                            if let Some(p) = &frame.payload {
                                if let Ok(cfg) = serde_json::from_slice::<WsClientConfig>(p) {
                                    if let Some(secs) = cfg.ping_interval {
                                        let secs = secs.max(10);
                                        if secs != ping_secs {
                                            ping_secs = secs;
                                            hb_interval = tokio::time::interval(Duration::from_secs(ping_secs));
                                            tracing::info!("Lark: ping_interval → {ping_secs}s");
                                        }
                                    }
                                }
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
                        Err(e) => { tracing::error!("Lark: event JSON: {e}"); continue; }
                    };
                    if event.header.event_type != "im.message.receive_v1" { continue; }

                    let event_payload = event.event;

                    let recv: MsgReceivePayload = match serde_json::from_value(event_payload.clone()) {
                        Ok(r) => r,
                        Err(e) => { tracing::error!("Lark: payload parse: {e}"); continue; }
                    };

                    if recv.sender.sender_type == "app" || recv.sender.sender_type == "bot" { continue; }

                    let sender_open_id = recv.sender.sender_id.open_id.as_deref().unwrap_or("");
                    if !self.is_user_allowed(sender_open_id) {
                        tracing::warn!("Lark WS: ignoring {sender_open_id} (not in allowed_users)");
                        continue;
                    }

                    let lark_msg = &recv.message;

                    if let Some(dedupe_key) = Self::dedupe_event_key(
                        Some(event.header.event_id.as_str()),
                        Some(lark_msg.message_id.as_str()),
                    ) {
                        if !self.try_mark_event_key_seen(&dedupe_key).await {
                            tracing::debug!("Lark WS: duplicate event dropped ({dedupe_key})");
                            continue;
                        }
                    }

                    // Decode content by type (mirrors clawdbot-feishu parsing)
                    let (text, post_mentioned_open_ids) = match lark_msg.message_type.as_str() {
                        "text" => match extract_text_message_content(&lark_msg.content) {
                            Some(text) => (text, Vec::new()),
                            None => continue,
                        },
                        "post" => match parse_post_content_details_value(&lark_msg.content) {
                            Some(details) => (details.text, details.mentioned_open_ids),
                            None => continue,
                        },
                        "image" => {
                            let text = if let Some(image_key) = parse_image_key_value(&lark_msg.content) {
                                match self
                                    .fetch_image_marker(&lark_msg.message_id, &image_key)
                                    .await
                                {
                                    Ok(marker) => marker,
                                    Err(error) => {
                                        tracing::warn!(
                                            "Lark WS: failed to download image {image_key}: {error}"
                                        );
                                        LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string()
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    "Lark WS: image content missing image_key; using fallback text"
                                );
                                LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string()
                            };
                            (text, Vec::new())
                        }
                        _ => { tracing::debug!("Lark WS: skipping unsupported type '{}'", lark_msg.message_type); continue; }
                    };

                    // Strip @_user_N placeholders
                    let text = strip_at_placeholders(&text);
                    let text = text.trim().to_string();
                    if text.is_empty() { continue; }

                    // Group-chat: only respond when explicitly @-mentioned
                    let bot_open_id = self.resolved_bot_open_id();
                    if lark_msg.chat_type == "group"
                        && !should_respond_in_group(
                            self.mention_only,
                            sender_open_id,
                            &self.group_reply_allowed_sender_ids,
                            bot_open_id.as_deref(),
                            &lark_msg.mentions,
                            &post_mentioned_open_ids,
                        )
                    {
                        continue;
                    }

                    let locale = detect_lark_ack_locale(Some(&event_payload), &text);
                    let ack_defaults = lark_ack_pool(locale);
                    let reaction_ctx = AckReactionContext {
                        text: &text,
                        sender_id: Some(sender_open_id),
                        chat_id: Some(&lark_msg.chat_id),
                        chat_type: if lark_msg.chat_type == "group" {
                            AckReactionContextChatType::Group
                        } else {
                            AckReactionContextChatType::Direct
                        },
                        locale_hint: Some(lark_locale_tag(locale)),
                    };
                    if let Some(ack_emoji) =
                        select_ack_reaction(self.ack_reaction.as_ref(), ack_defaults, &reaction_ctx)
                    {
                        let reaction_channel = self.clone();
                        let reaction_message_id = lark_msg.message_id.clone();
                        tokio::spawn(async move {
                            reaction_channel
                                .try_add_ack_reaction(&reaction_message_id, &ack_emoji)
                                .await;
                        });
                    }

                    let channel_msg = ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        sender: lark_msg.chat_id.clone(),
                        reply_target: lark_msg.chat_id.clone(),
                        content: text,
                        channel: self.channel_name().to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                    };

                    tracing::debug!("Lark WS: message in {}", lark_msg.chat_id);
                    if tx.send(channel_msg).await.is_err() { break; }
                }
            }
        }
        Ok(())
    }

    /// Check if a user open_id is allowed
    fn is_user_allowed(&self, open_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == open_id)
    }

    /// Get or refresh tenant access token
    async fn get_tenant_access_token(&self) -> anyhow::Result<String> {
        // Check cache first
        {
            let cached = self.tenant_token.read().await;
            if let Some(ref token) = *cached {
                if Instant::now() < token.refresh_after {
                    return Ok(token.value.clone());
                }
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
            let sanitized = sanitize_lark_body(&data);
            anyhow::bail!(
                "Lark tenant_access_token request failed: status={status}, body={sanitized}"
            );
        }

        let code = data.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = data
                .get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("Lark tenant_access_token failed: {msg}");
        }

        let token = data
            .get("tenant_access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing tenant_access_token in response"))?
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
                let sanitized = sanitize_lark_body(&retry_body);
                anyhow::bail!(
                    "Lark bot info request failed after token refresh: status={retry_status}, body={sanitized}"
                );
            }
            retry_body
        } else {
            if !status.is_success() {
                let sanitized = sanitize_lark_body(&body);
                anyhow::bail!("Lark bot info request failed: status={status}, body={sanitized}");
            }
            body
        };

        let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            let sanitized = sanitize_lark_body(&body);
            anyhow::bail!("Lark bot info failed: code={code}, body={sanitized}");
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
                tracing::info!("Lark: resolved bot open_id: {open_id}");
            }
            Ok(None) => {
                tracing::warn!(
                    "Lark: bot open_id missing from /bot/v3/info response; mention_only group messages will be ignored"
                );
            }
            Err(err) => {
                tracing::warn!(
                    "Lark: failed to resolve bot open_id: {err}; mention_only group messages will be ignored"
                );
            }
        }
    }

    fn image_upload_url(&self) -> String {
        format!("{}/im/v1/images", self.api_base())
    }

    async fn send_image_once(
        &self,
        url: &str,
        token: &str,
        recipient: &str,
        image_key: &str,
    ) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
        let content = serde_json::json!({ "image_key": image_key }).to_string();
        let body = serde_json::json!({
            "receive_id": recipient,
            "msg_type": "image",
            "content": content,
        });

        self.send_text_once(url, token, &body).await
    }

    async fn upload_image_once(
        &self,
        url: &str,
        token: &str,
        bytes: Vec<u8>,
        file_name: &str,
    ) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        let resp = self
            .http_client()
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let raw = resp.text().await.unwrap_or_default();
        let parsed = serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap_or_else(|_| serde_json::json!({ "raw": raw }));
        Ok((status, parsed))
    }

    async fn resolve_outgoing_image_target(
        &self,
        target: &str,
    ) -> anyhow::Result<(Vec<u8>, String, String)> {
        let trimmed = target.trim();

        if is_data_image_uri(trimmed) {
            let (bytes, mime) = decode_data_image_uri(trimmed)?;
            let ext = image_extension_from_mime(&mime);
            return Ok((bytes, format!("image.{ext}"), mime));
        }

        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            let resp = self.http_client().get(trimmed).send().await?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                let sanitized = crate::providers::sanitize_api_error(&body);
                anyhow::bail!(
                    "failed to fetch remote image {trimmed}: status={status}, body={sanitized}"
                );
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(';').next())
                .map(str::trim)
                .map(|value| value.to_ascii_lowercase());

            let path_like = trimmed
                .split('?')
                .next()
                .unwrap_or(trimmed)
                .split('#')
                .next()
                .unwrap_or(trimmed);
            let guessed_mime = mime_guess::from_path(path_like)
                .first_raw()
                .unwrap_or("image/png")
                .to_string();

            let mime = content_type.unwrap_or(guessed_mime);
            if !mime.starts_with("image/") {
                anyhow::bail!("remote target is not an image: {trimmed}");
            }

            let file_name = path_like
                .rsplit('/')
                .next()
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| format!("image.{}", image_extension_from_mime(&mime)));

            let bytes = resp.bytes().await?.to_vec();
            if bytes.is_empty() {
                anyhow::bail!("remote image payload is empty: {trimmed}");
            }

            return Ok((bytes, file_name, mime));
        }

        let local_path = trimmed.strip_prefix("file://").unwrap_or(trimmed);
        let path = Path::new(local_path);
        if !path.is_file() {
            anyhow::bail!("local image path not found: {local_path}");
        }

        let mime = mime_guess::from_path(path)
            .first_raw()
            .unwrap_or("image/png")
            .to_string();
        if !mime.starts_with("image/") {
            anyhow::bail!("local image path is not an image: {local_path}");
        }

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to read local image {local_path}: {e}"))?;
        if bytes.is_empty() {
            anyhow::bail!("local image payload is empty: {local_path}");
        }

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("image.{}", image_extension_from_mime(&mime)));

        Ok((bytes, file_name, mime))
    }

    async fn send_text_with_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let token = self.get_tenant_access_token().await?;
        let (status, response) = self.send_text_once(url, &token, body).await?;

        if should_refresh_lark_tenant_token(status, &response) {
            self.invalidate_token().await;
            let new_token = self.get_tenant_access_token().await?;
            let (retry_status, retry_response) = self.send_text_once(url, &new_token, body).await?;

            if should_refresh_lark_tenant_token(retry_status, &retry_response) {
                let sanitized = sanitize_lark_body(&retry_response);
                anyhow::bail!(
                    "Lark send failed after token refresh: status={retry_status}, body={sanitized}"
                );
            }

            ensure_lark_send_success(retry_status, &retry_response, "after token refresh")?;
            return Ok(());
        }

        ensure_lark_send_success(status, &response, "without token refresh")?;
        Ok(())
    }

    async fn send_image_target_with_retry(
        &self,
        message_url: &str,
        recipient: &str,
        image_target: &str,
    ) -> anyhow::Result<()> {
        let upload_url = self.image_upload_url();
        let (image_bytes, file_name, _mime) =
            self.resolve_outgoing_image_target(image_target).await?;

        let mut token = self.get_tenant_access_token().await?;
        let (status, mut upload_response) = self
            .upload_image_once(&upload_url, &token, image_bytes.clone(), &file_name)
            .await?;

        if should_refresh_lark_tenant_token(status, &upload_response) {
            self.invalidate_token().await;
            token = self.get_tenant_access_token().await?;
            let (retry_status, retry_response) = self
                .upload_image_once(&upload_url, &token, image_bytes, &file_name)
                .await?;
            upload_response = retry_response;

            if should_refresh_lark_tenant_token(retry_status, &upload_response) {
                let sanitized = sanitize_lark_body(&upload_response);
                anyhow::bail!(
                    "Lark image upload failed after token refresh: status={retry_status}, body={sanitized}"
                );
            }

            ensure_lark_send_success(
                retry_status,
                &upload_response,
                "image upload after token refresh",
            )?;
        } else {
            ensure_lark_send_success(
                status,
                &upload_response,
                "image upload without token refresh",
            )?;
        }

        let image_key = upload_response
            .pointer("/data/image_key")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Lark image upload response missing data.image_key"))?;

        let (send_status, send_response) = self
            .send_image_once(message_url, &token, recipient, image_key)
            .await?;
        if should_refresh_lark_tenant_token(send_status, &send_response) {
            self.invalidate_token().await;
            let new_token = self.get_tenant_access_token().await?;
            let (retry_status, retry_response) = self
                .send_image_once(message_url, &new_token, recipient, image_key)
                .await?;
            if should_refresh_lark_tenant_token(retry_status, &retry_response) {
                let sanitized = sanitize_lark_body(&retry_response);
                anyhow::bail!(
                    "Lark image send failed after token refresh: status={retry_status}, body={sanitized}"
                );
            }
            ensure_lark_send_success(
                retry_status,
                &retry_response,
                "image send after token refresh",
            )?;
            return Ok(());
        }

        ensure_lark_send_success(
            send_status,
            &send_response,
            "image send without token refresh",
        )?;
        Ok(())
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

    /// Parse an event callback payload and extract incoming messages.
    ///
    /// Synchronous parser uses a non-network fallback for image messages.
    pub fn parse_event_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
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
            tracing::warn!("Lark: ignoring message from unauthorized user: {open_id}");
            return messages;
        }

        // Extract message content (text/post/image supported)
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

        let content = event
            .pointer("/message/content")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let (text, post_mentioned_open_ids): (String, Vec<String>) = match msg_type {
            "text" => match extract_text_message_content(&content) {
                Some(text) => (text, Vec::new()),
                None => return messages,
            },
            "post" => match parse_post_content_details_value(&content) {
                Some(details) => (details.text, details.mentioned_open_ids),
                None => return messages,
            },
            "image" => (LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string(), Vec::new()),
            _ => {
                tracing::debug!("Lark: skipping unsupported message type: {msg_type}");
                return messages;
            }
        };

        let bot_open_id = self.resolved_bot_open_id();
        if chat_type == "group"
            && !should_respond_in_group(
                self.mention_only,
                open_id,
                &self.group_reply_allowed_sender_ids,
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
            id: Uuid::new_v4().to_string(),
            sender: chat_id.to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }

    /// Async variant used by webhook runtime path.
    /// Unlike `parse_event_payload`, this path attempts image download and
    /// converts image content to `[IMAGE:data:...;base64,...]` markers.
    pub async fn parse_event_payload_async(
        &self,
        payload: &serde_json::Value,
    ) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

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
        let event_id = payload
            .pointer("/header/event_id")
            .and_then(|id| id.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let message_id = event
            .pointer("/message/message_id")
            .and_then(|id| id.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if let Some(dedupe_key) = Self::dedupe_event_key(event_id, message_id) {
            if !self.try_mark_event_key_seen(&dedupe_key).await {
                tracing::debug!("Lark webhook: duplicate event dropped ({dedupe_key})");
                return messages;
            }
        }

        let open_id = event
            .pointer("/sender/sender_id/open_id")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if open_id.is_empty() {
            return messages;
        }
        if !self.is_user_allowed(open_id) {
            tracing::warn!("Lark: ignoring message from unauthorized user: {open_id}");
            return messages;
        }

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
        let content = event
            .pointer("/message/content")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let (text, post_mentioned_open_ids): (String, Vec<String>) = match msg_type {
            "text" => match extract_text_message_content(&content) {
                Some(text) => (text, Vec::new()),
                None => return messages,
            },
            "post" => match parse_post_content_details_value(&content) {
                Some(details) => (details.text, details.mentioned_open_ids),
                None => return messages,
            },
            "image" => {
                let text = if let Some(image_key) = parse_image_key_value(&content) {
                    match message_id {
                        Some(mid) => match self.fetch_image_marker(mid, &image_key).await {
                            Ok(marker) => marker,
                            Err(error) => {
                                tracing::warn!(
                                    "Lark webhook: failed to download image {image_key}: {error}"
                                );
                                LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string()
                            }
                        },
                        None => {
                            tracing::warn!(
                                "Lark webhook: image message missing message_id; using fallback text"
                            );
                            LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string()
                        }
                    }
                } else {
                    tracing::warn!("Lark webhook: image message missing image_key");
                    LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT.to_string()
                };
                (text, Vec::new())
            }
            _ => {
                tracing::debug!("Lark: skipping unsupported message type: {msg_type}");
                return messages;
            }
        };

        let bot_open_id = self.resolved_bot_open_id();
        if chat_type == "group"
            && !should_respond_in_group(
                self.mention_only,
                open_id,
                &self.group_reply_allowed_sender_ids,
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
            id: Uuid::new_v4().to_string(),
            sender: chat_id.to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }
}

#[async_trait]
impl Channel for LarkChannel {
    fn name(&self) -> &str {
        self.channel_name()
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let url = self.send_message_url();
        let (text_content, image_targets) = parse_outgoing_content(&message.content);

        if !text_content.is_empty() {
            let content = serde_json::json!({ "text": text_content }).to_string();
            let body = serde_json::json!({
                "receive_id": message.recipient,
                "msg_type": "text",
                "content": content,
            });
            self.send_text_with_retry(&url, &body).await?;
        }

        for image_target in image_targets {
            if let Err(err) = self
                .send_image_target_with_retry(&url, &message.recipient, &image_target)
                .await
            {
                tracing::warn!(
                    "Lark image send failed for target '{}': {err}",
                    display_image_target(&image_target)
                );
                let fallback = serde_json::json!({
                    "text": format!("Image: {}", display_image_target(&image_target))
                })
                .to_string();
                let body = serde_json::json!({
                    "receive_id": message.recipient,
                    "msg_type": "text",
                    "content": fallback,
                });
                let _ = self.send_text_with_retry(&url, &body).await;
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        use crate::config::schema::LarkReceiveMode;
        match self.receive_mode {
            LarkReceiveMode::Websocket => self.listen_ws(tx).await,
            LarkReceiveMode::Webhook => self.listen_http(tx).await,
        }
    }

    async fn health_check(&self) -> bool {
        self.get_tenant_access_token().await.is_ok()
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
        use axum::{extract::State, routing::post, Json, Router};

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
                    .map_or(true, |t| t == state.verification_token);

                if !token_ok {
                    return (StatusCode::FORBIDDEN, "invalid token").into_response();
                }

                let resp = serde_json::json!({ "challenge": challenge });
                return (StatusCode::OK, Json(resp)).into_response();
            }

            // Parse event messages
            let messages = state.channel.parse_event_payload_async(&payload).await;
            if !messages.is_empty() {
                if let Some(message_id) = payload
                    .pointer("/event/message/message_id")
                    .and_then(|m| m.as_str())
                {
                    let ack_text = messages.first().map_or("", |msg| msg.content.as_str());
                    let locale = detect_lark_ack_locale(payload.get("event"), ack_text);
                    let sender_id = payload
                        .pointer("/event/sender/sender_id/open_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let chat_id = payload
                        .pointer("/event/message/chat_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    let chat_type = payload
                        .pointer("/event/message/chat_type")
                        .and_then(|value| value.as_str())
                        .map(|kind| {
                            if kind == "group" {
                                AckReactionContextChatType::Group
                            } else {
                                AckReactionContextChatType::Direct
                            }
                        })
                        .unwrap_or(AckReactionContextChatType::Direct);
                    let ack_defaults = lark_ack_pool(locale);
                    let reaction_ctx = AckReactionContext {
                        text: ack_text,
                        sender_id: sender_id.as_deref(),
                        chat_id: chat_id.as_deref(),
                        chat_type,
                        locale_hint: Some(lark_locale_tag(locale)),
                    };
                    if let Some(ack_emoji) = select_ack_reaction(
                        state.channel.ack_reaction.as_ref(),
                        ack_defaults,
                        &reaction_ctx,
                    ) {
                        let reaction_channel = Arc::clone(&state.channel);
                        let reaction_message_id = message_id.to_string();
                        tokio::spawn(async move {
                            reaction_channel
                                .try_add_ack_reaction(&reaction_message_id, &ack_emoji)
                                .await;
                        });
                    }
                }
            }

            for msg in messages {
                if state.tx.send(msg).await.is_err() {
                    tracing::warn!("Lark: message channel closed");
                    break;
                }
            }

            (StatusCode::OK, "ok").into_response()
        }

        let port = self.port.ok_or_else(|| {
            anyhow::anyhow!("Lark webhook mode requires `port` to be set in [channels_config.lark]")
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
        tracing::info!("Lark event callback server listening on {addr}");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WS helper functions
// ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::cast_possible_truncation)]
fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            return (value % upper) as usize;
        }
    }
}

fn random_from_pool(pool: &'static [&'static str]) -> &'static str {
    pool[pick_uniform_index(pool.len())]
}

fn lark_ack_pool(locale: LarkAckLocale) -> &'static [&'static str] {
    match locale {
        LarkAckLocale::ZhCn => LARK_ACK_REACTIONS_ZH_CN,
        LarkAckLocale::ZhTw => LARK_ACK_REACTIONS_ZH_TW,
        LarkAckLocale::En => LARK_ACK_REACTIONS_EN,
        LarkAckLocale::Ja => LARK_ACK_REACTIONS_JA,
    }
}

fn lark_locale_tag(locale: LarkAckLocale) -> &'static str {
    match locale {
        LarkAckLocale::ZhCn => "zh_cn",
        LarkAckLocale::ZhTw => "zh_tw",
        LarkAckLocale::En => "en",
        LarkAckLocale::Ja => "ja",
    }
}

fn map_locale_tag(tag: &str) -> Option<LarkAckLocale> {
    let normalized = tag.trim().to_ascii_lowercase().replace('-', "_");
    if normalized.is_empty() {
        return None;
    }

    if normalized.starts_with("ja") {
        return Some(LarkAckLocale::Ja);
    }
    if normalized.starts_with("en") {
        return Some(LarkAckLocale::En);
    }
    if normalized.contains("hant")
        || normalized.starts_with("zh_tw")
        || normalized.starts_with("zh_hk")
        || normalized.starts_with("zh_mo")
    {
        return Some(LarkAckLocale::ZhTw);
    }
    if normalized.starts_with("zh") {
        return Some(LarkAckLocale::ZhCn);
    }
    None
}

fn find_locale_hint(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            for key in [
                "locale",
                "language",
                "lang",
                "i18n_locale",
                "user_locale",
                "locale_id",
            ] {
                if let Some(locale) = map.get(key).and_then(serde_json::Value::as_str) {
                    return Some(locale.to_string());
                }
            }

            for child in map.values() {
                if let Some(locale) = find_locale_hint(child) {
                    return Some(locale);
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            for child in items {
                if let Some(locale) = find_locale_hint(child) {
                    return Some(locale);
                }
            }
            None
        }
        _ => None,
    }
}

fn detect_locale_from_post_content(content: &str) -> Option<LarkAckLocale> {
    let parsed = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let obj = parsed.as_object()?;
    for key in obj.keys() {
        if let Some(locale) = map_locale_tag(key) {
            return Some(locale);
        }
    }
    None
}

fn is_japanese_kana(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0x31F0..=0x31FF // Katakana Phonetic Extensions
    )
}

fn is_cjk_han(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3400..=0x4DBF | // CJK Extension A
        0x4E00..=0x9FFF // CJK Unified Ideographs
    )
}

fn is_traditional_only_han(ch: char) -> bool {
    matches!(
        ch,
        '奮' | '鬥'
            | '強'
            | '體'
            | '國'
            | '臺'
            | '萬'
            | '與'
            | '為'
            | '這'
            | '學'
            | '機'
            | '開'
            | '裡'
    )
}

fn is_simplified_only_han(ch: char) -> bool {
    matches!(
        ch,
        '奋' | '斗'
            | '强'
            | '体'
            | '国'
            | '台'
            | '万'
            | '与'
            | '为'
            | '这'
            | '学'
            | '机'
            | '开'
            | '里'
    )
}

fn detect_locale_from_text(text: &str) -> Option<LarkAckLocale> {
    if text.chars().any(is_japanese_kana) {
        return Some(LarkAckLocale::Ja);
    }
    if text.chars().any(is_traditional_only_han) {
        return Some(LarkAckLocale::ZhTw);
    }
    if text.chars().any(is_simplified_only_han) {
        return Some(LarkAckLocale::ZhCn);
    }
    if text.chars().any(is_cjk_han) {
        return Some(LarkAckLocale::ZhCn);
    }
    None
}

fn detect_lark_ack_locale(
    payload: Option<&serde_json::Value>,
    fallback_text: &str,
) -> LarkAckLocale {
    if let Some(payload) = payload {
        if let Some(locale) = find_locale_hint(payload).and_then(|hint| map_locale_tag(&hint)) {
            return locale;
        }

        let message_content = payload
            .pointer("/message/content")
            .or_else(|| payload.pointer("/event/message/content"));
        let message_content_str = message_content.and_then(|value| match value {
            serde_json::Value::String(raw) => Some(raw.clone()),
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => Some(value.to_string()),
            _ => None,
        });

        if let Some(locale) = message_content_str
            .as_deref()
            .and_then(detect_locale_from_post_content)
        {
            return locale;
        }
    }

    detect_locale_from_text(fallback_text).unwrap_or(LarkAckLocale::En)
}

fn random_lark_ack_reaction(
    payload: Option<&serde_json::Value>,
    fallback_text: &str,
) -> &'static str {
    let locale = detect_lark_ack_locale(payload, fallback_text);
    random_from_pool(lark_ack_pool(locale))
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
                        _ => {}
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

fn parse_post_content_details_value(content: &serde_json::Value) -> Option<ParsedPostContent> {
    let normalized = normalize_message_content(content)?;
    match normalized {
        serde_json::Value::String(raw) => parse_post_content_details(&raw),
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            parse_post_content_details(&normalized.to_string())
        }
        _ => None,
    }
}

fn parse_post_content(content: &str) -> Option<String> {
    parse_post_content_details(content).map(|details| details.text)
}

/// Remove `@_user_N` placeholder tokens injected by Feishu in group chats.
fn strip_at_placeholders(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if ch == '@' {
            let rest: String = chars.clone().map(|(_, c)| c).collect();
            if let Some(after) = rest.strip_prefix("_user_") {
                let skip =
                    "_user_".len() + after.chars().take_while(|c| c.is_ascii_digit()).count();
                for _ in 0..=skip {
                    chars.next();
                }
                if chars.peek().map(|(_, c)| *c == ' ').unwrap_or(false) {
                    chars.next();
                }
                continue;
            }
        }
        result.push(ch);
    }
    result
}

fn mention_matches_bot_open_id(mention: &serde_json::Value, bot_open_id: &str) -> bool {
    mention
        .pointer("/id/open_id")
        .or_else(|| mention.pointer("/open_id"))
        .and_then(|v| v.as_str())
        .is_some_and(|value| value == bot_open_id)
}

fn normalize_group_reply_allowed_sender_ids(sender_ids: Vec<String>) -> Vec<String> {
    let mut normalized = sender_ids
        .into_iter()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn sender_has_group_reply_override(sender_open_id: &str, allowed_sender_ids: &[String]) -> bool {
    let sender_open_id = sender_open_id.trim();
    if sender_open_id.is_empty() {
        return false;
    }
    allowed_sender_ids
        .iter()
        .any(|entry| entry == "*" || entry == sender_open_id)
}

/// Group-chat response policy:
/// - sender override IDs always trigger
/// - otherwise, mention gating applies when enabled
fn should_respond_in_group(
    mention_only: bool,
    sender_open_id: &str,
    group_reply_allowed_sender_ids: &[String],
    bot_open_id: Option<&str>,
    mentions: &[serde_json::Value],
    post_mentioned_open_ids: &[String],
) -> bool {
    if sender_has_group_reply_override(sender_open_id, group_reply_allowed_sender_ids) {
        return true;
    }
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

    fn make_channel() -> LarkChannel {
        with_bot_open_id(
            LarkChannel::new(
                "cli_test_app_id".into(),
                "test_app_secret".into(),
                "test_verification_token".into(),
                None,
                vec!["ou_testuser123".into()],
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
    fn lark_parse_outgoing_content_extracts_image_markers_and_local_path_lines() {
        let temp = tempfile::tempdir().expect("temp dir");
        let image_path = temp.path().join("capture.png");
        std::fs::write(&image_path, b"png-bytes").expect("write image");

        let input = format!(
            "处理好了\n[IMAGE:https://cdn.example.com/a.png]\n{}\n/path/does/not/exist.png",
            image_path.display()
        );
        let (text, images) = parse_outgoing_content(&input);

        assert_eq!(text, "处理好了\n/path/does/not/exist.png");
        assert_eq!(
            images,
            vec![
                "https://cdn.example.com/a.png".to_string(),
                image_path.display().to_string()
            ]
        );
    }

    #[test]
    fn lark_parse_outgoing_content_extracts_data_uri_lines() {
        let data_uri = "data:image/png;base64,aGVsbG8=";
        let input = format!("这是一张图\n{data_uri}");
        let (text, images) = parse_outgoing_content(&input);

        assert_eq!(text, "这是一张图");
        assert_eq!(images, vec![data_uri.to_string()]);
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
            "ou_user",
            &[],
            Some("ou_bot"),
            &mentions,
            &[]
        ));

        let mentions = vec![serde_json::json!({
            "id": { "open_id": "ou_bot" }
        })];
        assert!(should_respond_in_group(
            true,
            "ou_user",
            &[],
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
        assert!(!should_respond_in_group(
            true,
            "ou_user",
            &[],
            None,
            &mentions,
            &[]
        ));
    }

    #[test]
    fn lark_group_response_allows_post_mentions_for_bot_open_id() {
        assert!(should_respond_in_group(
            true,
            "ou_user",
            &[],
            Some("ou_bot"),
            &[],
            &[String::from("ou_bot")]
        ));
    }

    #[test]
    fn lark_group_response_allows_sender_override_without_mention() {
        assert!(should_respond_in_group(
            true,
            "ou_priority_user",
            &[String::from("ou_priority_user")],
            Some("ou_bot"),
            &[],
            &[]
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
            vec!["*".into()],
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
            vec![],
            true,
        );
        assert!(!ch.is_user_allowed("ou_anyone"));
    }

    #[test]
    fn lark_parse_challenge() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "challenge": "abc123",
            "token": "test_verification_token",
            "type": "url_verification"
        });
        // Challenge payloads should not produce messages
        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_valid_text_message() {
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

        let msgs = ch.parse_event_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].sender, "oc_chat123");
        assert_eq!(msgs[0].channel, "lark");
        assert_eq!(msgs[0].timestamp, 1_699_999_999);
    }

    #[test]
    fn lark_parse_valid_text_message_with_object_content() {
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
                    "content": { "text": "Hello from object content" },
                    "chat_id": "oc_chat123",
                    "create_time": "1699999999000"
                }
            }
        });

        let msgs = ch.parse_event_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello from object content");
        assert_eq!(msgs[0].sender, "oc_chat123");
        assert_eq!(msgs[0].channel, "lark");
    }

    #[test]
    fn lark_ws_payload_deserializes_object_content() {
        let payload = serde_json::json!({
            "sender": {
                "sender_id": { "open_id": "ou_testuser123" },
                "sender_type": "user"
            },
            "message": {
                "message_id": "om_123",
                "chat_id": "oc_chat123",
                "chat_type": "p2p",
                "message_type": "text",
                "content": { "text": "Hello websocket" },
                "mentions": []
            }
        });

        let parsed: MsgReceivePayload = serde_json::from_value(payload).unwrap();
        assert_eq!(
            extract_text_message_content(&parsed.message.content).as_deref(),
            Some("Hello websocket")
        );
    }

    #[test]
    fn lark_parse_unauthorized_user() {
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

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_image_message_uses_fallback_text() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT);
    }

    #[tokio::test]
    async fn lark_parse_event_payload_async_image_missing_key_uses_fallback_text() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload_async(&payload).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, LARK_IMAGE_DOWNLOAD_FALLBACK_TEXT);
    }

    #[tokio::test]
    async fn lark_parse_event_payload_async_dedupes_repeated_event_id() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
            true,
        );
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1",
                "event_id": "evt_abc"
            },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_id": "om_first",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let first = ch.parse_event_payload_async(&payload).await;
        let second = ch.parse_event_payload_async(&payload).await;
        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn lark_parse_event_payload_async_dedupes_by_message_id_without_event_id() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
            true,
        );
        let payload = serde_json::json!({
            "header": {
                "event_type": "im.message.receive_v1"
            },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_user" } },
                "message": {
                    "message_id": "om_fallback",
                    "message_type": "text",
                    "content": "{\"text\":\"hello\"}",
                    "chat_id": "oc_chat"
                }
            }
        });

        let first = ch.parse_event_payload_async(&payload).await;
        let second = ch.parse_event_payload_async(&payload).await;
        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn try_mark_event_key_seen_cleans_up_expired_keys_periodically() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
            true,
        );

        {
            let mut seen = ch.recent_event_keys.write().await;
            seen.insert(
                "event:stale".to_string(),
                Instant::now() - LARK_EVENT_DEDUP_TTL - Duration::from_secs(5),
            );
        }

        {
            let mut cleanup_at = ch.recent_event_cleanup_at.write().await;
            *cleanup_at =
                Instant::now() - LARK_EVENT_DEDUP_CLEANUP_INTERVAL - Duration::from_secs(1);
        }

        assert!(ch.try_mark_event_key_seen("event:fresh").await);
        let seen = ch.recent_event_keys.read().await;
        assert!(!seen.contains_key("event:stale"));
        assert!(seen.contains_key("event:fresh"));
    }

    #[test]
    fn lark_parse_empty_text_skipped() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_wrong_event_type() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.chat.disbanded_v1" },
            "event": {}
        });

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_missing_sender() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_unicode_message() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Hello world 🌍");
    }

    #[test]
    fn lark_parse_missing_event() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "header": { "event_type": "im.message.receive_v1" }
        });

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_parse_invalid_content_json() {
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn lark_config_serde() {
        use crate::config::schema::{LarkConfig, LarkReceiveMode};
        let lc = LarkConfig {
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["ou_user1".into(), "ou_user2".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: false,
            receive_mode: LarkReceiveMode::default(),
            port: None,
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };
        let json = serde_json::to_string(&lc).unwrap();
        let parsed: LarkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.app_id, "cli_app123");
        assert_eq!(parsed.app_secret, "secret456");
        assert_eq!(parsed.verification_token.as_deref(), Some("vtoken789"));
        assert_eq!(parsed.allowed_users.len(), 2);
    }

    #[test]
    fn lark_config_toml_roundtrip() {
        use crate::config::schema::{LarkConfig, LarkReceiveMode};
        let lc = LarkConfig {
            app_id: "app".into(),
            app_secret: "secret".into(),
            encrypt_key: None,
            verification_token: Some("tok".into()),
            allowed_users: vec!["*".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };
        let toml_str = toml::to_string(&lc).unwrap();
        let parsed: LarkConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.app_id, "app");
        assert_eq!(parsed.verification_token.as_deref(), Some("tok"));
        assert_eq!(parsed.allowed_users, vec!["*"]);
    }

    #[test]
    fn lark_config_defaults_optional_fields() {
        use crate::config::schema::{LarkConfig, LarkReceiveMode};
        let json = r#"{"app_id":"a","app_secret":"s"}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.verification_token.is_none());
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.mention_only);
        assert_eq!(parsed.receive_mode, LarkReceiveMode::Websocket);
        assert!(parsed.port.is_none());
    }

    #[test]
    fn lark_from_config_preserves_mode_and_region() {
        use crate::config::schema::{LarkConfig, LarkReceiveMode};

        let cfg = LarkConfig {
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["*".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };

        let ch = LarkChannel::from_config(&cfg);

        assert_eq!(ch.api_base(), LARK_BASE_URL);
        assert_eq!(ch.ws_base(), LARK_WS_BASE_URL);
        assert_eq!(ch.receive_mode, LarkReceiveMode::Webhook);
        assert_eq!(ch.port, Some(9898));
    }

    #[test]
    fn lark_from_lark_config_ignores_legacy_feishu_flag() {
        use crate::config::schema::{LarkConfig, LarkReceiveMode};

        let cfg = LarkConfig {
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["*".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: true,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };

        let ch = LarkChannel::from_lark_config(&cfg);

        assert_eq!(ch.api_base(), LARK_BASE_URL);
        assert_eq!(ch.ws_base(), LARK_WS_BASE_URL);
        assert_eq!(ch.name(), "lark");
    }

    #[test]
    fn lark_from_feishu_config_sets_feishu_platform() {
        use crate::config::schema::{FeishuConfig, LarkReceiveMode};

        let cfg = FeishuConfig {
            app_id: "cli_feishu_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["*".into()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };

        let ch = LarkChannel::from_feishu_config(&cfg);

        assert_eq!(ch.api_base(), FEISHU_BASE_URL);
        assert_eq!(ch.ws_base(), FEISHU_WS_BASE_URL);
        assert_eq!(ch.name(), "feishu");
    }

    #[test]
    fn lark_parse_fallback_sender_to_open_id() {
        // When chat_id is missing, sender should fall back to open_id
        let ch = LarkChannel::new(
            "id".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "ou_user");
    }

    #[test]
    fn lark_parse_group_message_requires_bot_mention_when_enabled() {
        let ch = with_bot_open_id(
            LarkChannel::new(
                "cli_app123".into(),
                "secret".into(),
                "token".into(),
                None,
                vec!["*".into()],
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
        assert!(ch.parse_event_payload(&no_mention_payload).is_empty());

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
        assert!(ch.parse_event_payload(&wrong_mention_payload).is_empty());

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
        assert_eq!(ch.parse_event_payload(&bot_mention_payload).len(), 1);
    }

    #[test]
    fn lark_parse_group_post_message_accepts_at_when_top_level_mentions_empty() {
        let ch = with_bot_open_id(
            LarkChannel::new(
                "cli_app123".into(),
                "secret".into(),
                "token".into(),
                None,
                vec!["*".into()],
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

        assert_eq!(ch.parse_event_payload(&payload).len(), 1);
    }

    #[test]
    fn lark_parse_group_message_allows_without_mention_when_disabled() {
        let ch = LarkChannel::new(
            "cli_app123".into(),
            "secret".into(),
            "token".into(),
            None,
            vec!["*".into()],
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

        assert_eq!(ch.parse_event_payload(&payload).len(), 1);
    }

    #[test]
    fn lark_reaction_url_matches_region() {
        let ch_lark = make_channel();
        assert_eq!(
            ch_lark.message_reaction_url("om_test_message_id"),
            "https://open.larksuite.com/open-apis/im/v1/messages/om_test_message_id/reactions"
        );

        let feishu_cfg = crate::config::schema::FeishuConfig {
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["*".into()],
            group_reply: None,
            receive_mode: crate::config::schema::LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };
        let ch_feishu = LarkChannel::from_feishu_config(&feishu_cfg);
        assert_eq!(
            ch_feishu.message_reaction_url("om_test_message_id"),
            "https://open.feishu.cn/open-apis/im/v1/messages/om_test_message_id/reactions"
        );
    }

    #[test]
    fn lark_image_resource_url_matches_region() {
        let ch_lark = make_channel();
        assert_eq!(
            ch_lark.image_resource_url("om_test_message_id", "img_v3_test"),
            "https://open.larksuite.com/open-apis/im/v1/messages/om_test_message_id/resources/img_v3_test"
        );

        let feishu_cfg = crate::config::schema::FeishuConfig {
            app_id: "cli_app123".into(),
            app_secret: "secret456".into(),
            encrypt_key: None,
            verification_token: Some("vtoken789".into()),
            allowed_users: vec!["*".into()],
            group_reply: None,
            receive_mode: crate::config::schema::LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: 3_000,
            max_draft_edits: 20,
        };
        let ch_feishu = LarkChannel::from_feishu_config(&feishu_cfg);
        assert_eq!(
            ch_feishu.image_resource_url("om_test_message_id", "img_v3_test"),
            "https://open.feishu.cn/open-apis/im/v1/messages/om_test_message_id/resources/img_v3_test"
        );
    }

    #[test]
    fn lark_reaction_locale_explicit_language_tags() {
        assert_eq!(map_locale_tag("zh-CN"), Some(LarkAckLocale::ZhCn));
        assert_eq!(map_locale_tag("zh_TW"), Some(LarkAckLocale::ZhTw));
        assert_eq!(map_locale_tag("zh-Hant"), Some(LarkAckLocale::ZhTw));
        assert_eq!(map_locale_tag("en-US"), Some(LarkAckLocale::En));
        assert_eq!(map_locale_tag("ja-JP"), Some(LarkAckLocale::Ja));
        assert_eq!(map_locale_tag("fr-FR"), None);
    }

    #[test]
    fn lark_reaction_locale_prefers_explicit_payload_locale() {
        let payload = serde_json::json!({
            "sender": {
                "locale": "ja-JP"
            },
            "message": {
                "content": "{\"text\":\"hello\"}"
            }
        });
        assert_eq!(
            detect_lark_ack_locale(Some(&payload), "你好，世界"),
            LarkAckLocale::Ja
        );
    }

    #[test]
    fn lark_reaction_locale_unsupported_payload_falls_back_to_text_script() {
        let payload = serde_json::json!({
            "sender": {
                "locale": "fr-FR"
            },
            "message": {
                "content": "{\"text\":\"頑張れ\"}"
            }
        });
        assert_eq!(
            detect_lark_ack_locale(Some(&payload), "頑張ってください"),
            LarkAckLocale::Ja
        );
    }

    #[test]
    fn lark_reaction_locale_detects_simplified_and_traditional_text() {
        assert_eq!(
            detect_lark_ack_locale(None, "继续奋斗，今天很强"),
            LarkAckLocale::ZhCn
        );
        assert_eq!(
            detect_lark_ack_locale(None, "繼續奮鬥，今天很強"),
            LarkAckLocale::ZhTw
        );
    }

    #[test]
    fn lark_reaction_locale_defaults_to_english_for_unsupported_text() {
        assert_eq!(
            detect_lark_ack_locale(None, "Bonjour tout le monde"),
            LarkAckLocale::En
        );
    }

    #[test]
    fn random_lark_ack_reaction_respects_detected_locale_pool() {
        let payload = serde_json::json!({
            "sender": {
                "locale": "zh-CN"
            }
        });
        let selected = random_lark_ack_reaction(Some(&payload), "hello");
        assert!(LARK_ACK_REACTIONS_ZH_CN.contains(&selected));

        let payload = serde_json::json!({
            "sender": {
                "locale": "zh-TW"
            }
        });
        let selected = random_lark_ack_reaction(Some(&payload), "hello");
        assert!(LARK_ACK_REACTIONS_ZH_TW.contains(&selected));

        let payload = serde_json::json!({
            "sender": {
                "locale": "en-US"
            }
        });
        let selected = random_lark_ack_reaction(Some(&payload), "hello");
        assert!(LARK_ACK_REACTIONS_EN.contains(&selected));

        let payload = serde_json::json!({
            "sender": {
                "locale": "ja-JP"
            }
        });
        let selected = random_lark_ack_reaction(Some(&payload), "hello");
        assert!(LARK_ACK_REACTIONS_JA.contains(&selected));
    }
}
