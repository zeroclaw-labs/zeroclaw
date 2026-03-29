use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use prost::Message as ProstMessage;
use std::collections::HashMap;
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

const MAX_LARK_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

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

/// Max byte size for a single interactive card's markdown content.
/// Lark card payloads have a ~30 KB limit; leave margin for JSON envelope.
const LARK_CARD_MARKDOWN_MAX_BYTES: usize = 28_000;

/// Maximum image size we will download and inline (5 MiB).
const LARK_IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

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

/// Build the full message body for sending an interactive card message.
fn build_interactive_card_body(recipient: &str, markdown: &str) -> serde_json::Value {
    serde_json::json!({
        "receive_id": recipient,
        "msg_type": "interactive",
        "content": build_card_content(markdown),
    })
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
        anyhow::bail!("Lark send failed {context}: status={status}, body={body}");
    }

    let code = extract_lark_response_code(body).unwrap_or(0);
    if code != 0 {
        anyhow::bail!("Lark send failed {context}: code={code}, body={body}");
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
    /// Bot open_id resolved at runtime via `/bot/v3/info`.
    resolved_bot_open_id: Arc<StdRwLock<Option<String>>>,
    mention_only: bool,
    /// Platform variant: Lark (international) or Feishu (CN).
    platform: LarkPlatform,
    /// How to receive events: WebSocket long-connection or HTTP webhook.
    receive_mode: crate::config::schema::LarkReceiveMode,
    /// Cached tenant access token
    tenant_token: Arc<RwLock<Option<CachedTenantToken>>>,
    /// Dedup set: WS message_ids seen in last ~30 min to prevent double-dispatch
    ws_seen_ids: Arc<RwLock<HashMap<String, Instant>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    transcription: Option<crate::config::TranscriptionConfig>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
    #[cfg(test)]
    api_base_override: Option<String>,
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
            resolved_bot_open_id: Arc::new(StdRwLock::new(None)),
            mention_only,
            platform,
            receive_mode: crate::config::schema::LarkReceiveMode::default(),
            tenant_token: Arc::new(RwLock::new(None)),
            ws_seen_ids: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
            #[cfg(test)]
            api_base_override: None,
        }
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
            config.mention_only,
            platform,
        );
        ch.receive_mode = config.receive_mode.clone();
        ch.proxy_url = config.proxy_url.clone();
        ch
    }

    /// Build from `LarkConfig` forcing `LarkPlatform::Lark`, ignoring the
    /// legacy `use_feishu` flag.  Used by the channel factory when the config
    /// section is explicitly `[channels_config.lark]`.
    pub fn from_lark_config(config: &crate::config::schema::LarkConfig) -> Self {
        let mut ch = Self::new_with_platform(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.verification_token.clone().unwrap_or_default(),
            config.port,
            config.allowed_users.clone(),
            config.mention_only,
            LarkPlatform::Lark,
        );
        ch.receive_mode = config.receive_mode.clone();
        ch.proxy_url = config.proxy_url.clone();
        ch
    }

    /// Build from `FeishuConfig` with `LarkPlatform::Feishu`.
    pub fn from_feishu_config(config: &crate::config::schema::FeishuConfig) -> Self {
        let mut ch = Self::new_with_platform(
            config.app_id.clone(),
            config.app_secret.clone(),
            config.verification_token.clone().unwrap_or_default(),
            config.port,
            config.allowed_users.clone(),
            config.mention_only,
            LarkPlatform::Feishu,
        );
        ch.receive_mode = config.receive_mode.clone();
        ch.proxy_url = config.proxy_url.clone();
        ch
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, audio transcription disabled: {e}"
                );
            }
        }
        self.transcription = Some(config);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_channel_proxy_client(
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

    fn message_reaction_url(&self, message_id: &str) -> String {
        format!("{}/im/v1/messages/{message_id}/reactions", self.api_base())
    }

    fn image_download_url(&self, image_key: &str) -> String {
        format!("{}/im/v1/images/{image_key}", self.api_base())
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
                tracing::warn!(
                    "Lark: add reaction failed for {message_id}: status={status}, body={err_body}"
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

        let (ws_stream, _) = crate::config::ws_connect_with_proxy(
            &wss_url,
            "channel.lark",
            self.proxy_url.as_deref(),
        )
        .await?;
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

                    // Dedup
                    {
                        let now = Instant::now();
                        let mut seen = self.ws_seen_ids.write().await;
                        // GC
                        seen.retain(|_, t| now.duration_since(*t) < Duration::from_secs(30 * 60));
                        if seen.contains_key(&lark_msg.message_id) {
                            tracing::debug!("Lark WS: dup {}", lark_msg.message_id);
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
                                None => { tracing::debug!("Lark WS: image message missing image_key"); continue; }
                            };
                            match self.download_image_as_marker(&image_key).await {
                                Some(marker) => (marker, Vec::new()),
                                None => {
                                    tracing::warn!("Lark WS: failed to download image {image_key}");
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
                                None => { tracing::debug!("Lark WS: file message missing file_key"); continue; }
                            };
                            let file_name = v.get("file_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown_file")
                                .to_string();
                            match self.download_file_as_content(&lark_msg.message_id, &file_key, &file_name).await {
                                Some(content) => (content, Vec::new()),
                                None => {
                                    tracing::warn!("Lark WS: failed to download file {file_key}");
                                    (format!("[ATTACHMENT:{file_name} | download failed]"), Vec::new())
                                }
                            }
                        }
                        "audio" => {
                            let Some(manager) = self.transcription_manager.as_deref() else {
                                tracing::debug!("Lark WS: audio message in {} (transcription not configured)", lark_msg.chat_id);
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
                            None => { tracing::debug!("Lark WS: list message with no extractable text"); continue; }
                        },
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
                            bot_open_id.as_deref(),
                            &lark_msg.mentions,
                            &post_mentioned_open_ids,
                        )
                    {
                        continue;
                    }

                    let ack_emoji =
                        random_lark_ack_reaction(Some(&event_payload), &text).to_string();
                    let reaction_channel = self.clone();
                    let reaction_message_id = lark_msg.message_id.clone();
                    tokio::spawn(async move {
                        reaction_channel
                            .try_add_ack_reaction(&reaction_message_id, &ack_emoji)
                            .await;
                    });

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
                        interruption_scope_id: None,
                    attachments: vec![],
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
            anyhow::bail!("Lark tenant_access_token request failed: status={status}, body={data}");
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

    /// Download an image from the Lark API and return an `[IMAGE:data:...]` marker string.
    async fn download_image_as_marker(&self, image_key: &str) -> Option<String> {
        let token = match self.get_tenant_access_token().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("Lark: failed to get token for image download: {e}");
                return None;
            }
        };

        let url = self.image_download_url(image_key);
        let resp = match self
            .http_client()
            .get(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Lark: image download request failed for {image_key}: {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                "Lark: image download failed for {image_key}: status={}",
                resp.status()
            );
            return None;
        }

        if let Some(cl) = resp.content_length() {
            if cl > LARK_IMAGE_MAX_BYTES as u64 {
                tracing::warn!("Lark: image too large for {image_key}: {cl} bytes exceeds limit");
                return None;
            }
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Lark: image body read failed for {image_key}: {e}");
                return None;
            }
        };

        if bytes.is_empty() || bytes.len() > LARK_IMAGE_MAX_BYTES {
            tracing::warn!(
                "Lark: image body empty or too large for {image_key}: {} bytes",
                bytes.len()
            );
            return None;
        }

        let mime = lark_detect_image_mime(content_type.as_deref(), &bytes)?;
        if !LARK_SUPPORTED_IMAGE_MIMES.contains(&mime.as_str()) {
            tracing::warn!("Lark: unsupported image MIME for {image_key}: {mime}");
            return None;
        }

        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Some(format!("[IMAGE:data:{mime};base64,{encoded}]"))
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
                tracing::warn!("Lark: failed to get token for file download: {e}");
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
                tracing::warn!("Lark: file download request failed for {file_key}: {e}");
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!(
                "Lark: file download failed for {file_key}: status={}",
                resp.status()
            );
            return None;
        }

        if let Some(cl) = resp.content_length() {
            if cl > LARK_FILE_MAX_BYTES as u64 {
                tracing::warn!("Lark: file too large for {file_key}: {cl} bytes exceeds limit");
                return Some(format!(
                    "[ATTACHMENT:{file_name} | size={cl} bytes | too large to inline]"
                ));
            }
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
                tracing::warn!("Lark: file body read failed for {file_key}: {e}");
                return None;
            }
        };

        if bytes.is_empty() {
            tracing::warn!("Lark: file body is empty for {file_key}");
            return None;
        }

        // If the content is image-like, return as image marker
        if content_type.starts_with("image/") && bytes.len() <= LARK_IMAGE_MAX_BYTES {
            if let Some(mime) = lark_detect_image_mime(Some(&content_type), &bytes) {
                if LARK_SUPPORTED_IMAGE_MIMES.contains(&mime.as_str()) {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    return Some(format!("[IMAGE:data:{mime};base64,{encoded}]"));
                }
            }
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
            let truncated = if text.len() > 50_000 {
                format!("{}...\n[truncated]", &text[..50_000])
            } else {
                text.into_owned()
            };
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
                    "Lark bot info request failed after token refresh: status={retry_status}, body={retry_body}"
                );
            }
            retry_body
        } else {
            if !status.is_success() {
                anyhow::bail!("Lark bot info request failed: status={status}, body={body}");
            }
            body
        };

        let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            anyhow::bail!("Lark bot info failed: code={code}, body={body}");
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

    async fn stream_audio_bytes(mut resp: reqwest::Response) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > MAX_LARK_AUDIO_BYTES {
                anyhow::bail!(
                    "Lark audio download exceeds {} byte limit",
                    MAX_LARK_AUDIO_BYTES
                );
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
                        "Lark audio download failed after token refresh: {}",
                        resp.status()
                    );
                }
                let bytes = Self::stream_audio_bytes(resp).await?;
                return Ok((bytes, inferred_audio_filename(file_key)));
            }

            anyhow::bail!("Lark audio download failed: {}", status);
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
                tracing::warn!("Lark: audio download failed for {message_id}: {e}");
                return None;
            }
        };

        match manager.transcribe(&audio_data, &filename).await {
            Ok(transcript) => {
                tracing::debug!("Lark: audio transcribed for {message_id}");
                Some(transcript)
            }
            Err(e) => {
                tracing::warn!("Lark: transcription failed for {message_id}: {e}");
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
            tracing::debug!("Lark webhook: audio message (transcription not configured)");
            return vec![];
        };

        let open_id = payload
            .pointer("/event/sender/sender_id/open_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !self.is_user_allowed(open_id) {
            tracing::warn!("Lark: ignoring audio from unauthorized user: {open_id}");
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
            id: Uuid::new_v4().to_string(),
            sender: chat_id.to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
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
            tracing::warn!("Lark: ignoring message from unauthorized user: {open_id}");
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
                        let marker = match self.download_image_as_marker(&key).await {
                            Some(m) => m,
                            None => {
                                tracing::warn!("Lark: failed to download image {key}");
                                format!("[IMAGE:{key} | download failed]")
                            }
                        };
                        (marker, Vec::new())
                    }
                    None => {
                        tracing::debug!("Lark: image message missing image_key");
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
                                tracing::warn!("Lark: failed to download file {key}");
                                format!("[ATTACHMENT:{file_name} | download failed]")
                            }
                        };
                        (content, Vec::new())
                    }
                    None => {
                        tracing::debug!("Lark: file message missing file_key");
                        return messages;
                    }
                }
            }
            "list" => match parse_list_content(content_str) {
                Some(t) => (t, Vec::new()),
                None => {
                    tracing::debug!("Lark: list message with no extractable text");
                    return messages;
                }
            },
            _ => {
                tracing::debug!("Lark: skipping unsupported message type: {msg_type}");
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
            id: Uuid::new_v4().to_string(),
            sender: chat_id.to_string(),
            reply_target: chat_id.to_string(),
            content: text,
            channel: self.channel_name().to_string(),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
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
                        "Lark send failed after token refresh: status={retry_status}, body={retry_response}"
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
                    let ack_emoji =
                        random_lark_ack_reaction(payload.get("event"), ack_text).to_string();
                    let reaction_channel = Arc::clone(&state.channel);
                    let reaction_message_id = message_id.to_string();
                    tokio::spawn(async move {
                        reaction_channel
                            .try_add_ack_reaction(&reaction_message_id, &ack_emoji)
                            .await;
                    });
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

fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            #[allow(clippy::cast_possible_truncation)]
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
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                payload
                    .pointer("/event/message/content")
                    .and_then(serde_json::Value::as_str)
            });

        if let Some(locale) = message_content.and_then(detect_locale_from_post_content) {
            return locale;
        }
    }

    detect_locale_from_text(fallback_text).unwrap_or(LarkAckLocale::En)
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

fn parse_post_content(content: &str) -> Option<String> {
    parse_post_content_details(content).map(|details| details.text)
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
            vec!["*".into()],
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
            vec!["*".into()],
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
            vec!["*".into()],
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

        let msgs = ch.parse_event_payload(&payload).await;
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
            use_feishu: false,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::default(),
            port: None,
            proxy_url: None,
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
            use_feishu: false,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
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
            use_feishu: false,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
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
            use_feishu: true,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
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
            mention_only: false,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
        };

        let ch = LarkChannel::from_feishu_config(&cfg);

        assert_eq!(ch.api_base(), FEISHU_BASE_URL);
        assert_eq!(ch.ws_base(), FEISHU_WS_BASE_URL);
        assert_eq!(ch.name(), "feishu");
    }

    #[test]
    fn feishu_config_defaults_optional_fields() {
        use crate::config::schema::FeishuConfig;
        let json = r#"{"app_id":"a","app_secret":"s"}"#;
        let parsed: FeishuConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.verification_token.is_none());
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.mention_only);
        assert!(!parsed.interrupt_on_new_message);
    }

    #[test]
    fn feishu_from_config_respects_mention_only() {
        use crate::config::schema::{FeishuConfig, LarkReceiveMode};
        let cfg = FeishuConfig {
            app_id: "cli_app".into(),
            app_secret: "secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec!["*".into()],
            mention_only: true,
            interrupt_on_new_message: false,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        };
        let ch = LarkChannel::from_feishu_config(&cfg);
        assert!(ch.mention_only);
    }

    #[test]
    fn lark_config_defaults_interrupt_on_new_message() {
        use crate::config::schema::LarkConfig;
        let json = r#"{"app_id":"a","app_secret":"s"}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert!(!parsed.interrupt_on_new_message);
    }

    #[tokio::test]
    async fn lark_parse_fallback_sender_to_open_id() {
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

        assert_eq!(ch.parse_event_payload(&payload).await.len(), 1);
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
            mention_only: false,
            interrupt_on_new_message: false,
            receive_mode: crate::config::schema::LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
        };
        let ch_feishu = LarkChannel::from_feishu_config(&feishu_cfg);
        assert_eq!(
            ch_feishu.message_reaction_url("om_test_message_id"),
            "https://open.feishu.cn/open-apis/im/v1/messages/om_test_message_id/reactions"
        );
    }

    #[test]
    fn lark_image_download_url_matches_region() {
        let ch = make_channel();
        assert_eq!(
            ch.image_download_url("img_abc123"),
            "https://open.larksuite.com/open-apis/im/v1/images/img_abc123"
        );
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
        let tc = crate::config::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        };
        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn lark_manager_none_and_warn_on_init_failure() {
        let tc = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".to_string(),
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
            vec![],
            false,
            LarkPlatform::Feishu,
        );
        assert_eq!(ch.api_base(), FEISHU_BASE_URL);
    }

    #[tokio::test]
    async fn lark_audio_file_key_missing_returns_none() {
        let ch = make_channel();
        let mut tc = crate::config::TranscriptionConfig::default();
        tc.enabled = true;
        tc.default_provider = "local_whisper".to_string();
        tc.local_whisper = Some(crate::config::LocalWhisperConfig {
            url: "http://localhost:0/v1/transcribe".to_string(),
            bearer_token: Some("unused".to_string()),
            max_audio_bytes: 10 * 1024 * 1024,
            timeout_secs: 30,
        });
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

        let mut config = crate::config::TranscriptionConfig::default();
        config.enabled = true;
        config.local_whisper = Some(crate::config::LocalWhisperConfig {
            url: format!("{}/v1/transcribe", whisper_server.uri()),
            bearer_token: Some("test-token".to_string()),
            max_audio_bytes: 10 * 1024 * 1024,
            timeout_secs: 30,
        });
        config.default_provider = "local_whisper".to_string();

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
}
