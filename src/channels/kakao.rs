use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Maximum text length per KakaoTalk message (platform limit).
const KAKAO_MAX_TEXT_LEN: usize = 1000;

/// KakaoTalk REST API base URL.
const KAKAO_API_BASE: &str = "https://kapi.kakao.com";

/// KakaoTalk Channel API base URL for sending messages.
const KAKAO_CHANNEL_API: &str = "https://kapi.kakao.com/v1/api/talk/channels/message";

/// Alimtalk API base URL (Kakao notification templates).
const KAKAO_ALIMTALK_API: &str = "https://kapi.kakao.com/v2/api/talk/memo/default/send";

/// KakaoTalk channel — connects via webhook HTTP receiver for incoming messages,
/// sends replies through Kakao REST API with support for rich message types.
///
/// ## Architecture
/// - **Incoming**: Axum HTTP server receives webhook callbacks from Kakao Channel API
/// - **Outgoing**: REST API calls with OAuth2 Bearer token authentication
/// - **Rich Messages**: Text, carousel, buttons, quick replies via Kakao template system
/// - **Alimtalk**: Template-based notification messages for business use cases
///
/// ## Setup
/// 1. Register an app on Kakao Developers (https://developers.kakao.com)
/// 2. Enable Kakao Talk Channel messaging
/// 3. Set webhook URL to `https://your-domain:{port}/kakao/webhook`
/// 4. Configure REST API key and admin key in ZeroClaw config
pub struct KakaoTalkChannel {
    /// REST API key from Kakao Developers console
    rest_api_key: String,
    /// Admin key for server-side API calls (Alimtalk, push)
    admin_key: String,
    /// Webhook secret for verifying incoming payloads
    webhook_secret: Option<String>,
    /// Allowed Kakao user IDs. Empty = deny all, "*" = allow all.
    allowed_users: Vec<String>,
    /// HTTP port for the webhook receiver server
    port: u16,
    /// HTTP client for outgoing API calls
    client: reqwest::Client,
    /// Cached OAuth2 access token + expiry (epoch seconds)
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    /// Per-user reply context (user_id -> last known channel context)
    reply_contexts: Arc<RwLock<HashMap<String, ReplyContext>>>,
    /// Shared pairing store for channel connect flow.
    pairing_store: Option<Arc<super::pairing::ChannelPairingStore>>,
    /// Gateway base URL for pairing web pages.
    gateway_url: Option<String>,
}

/// Cached reply context for a KakaoTalk user session.
#[derive(Clone, Debug)]
struct ReplyContext {
    /// Kakao user ID
    user_id: String,
    /// Bot user key for API calls
    bot_user_key: Option<String>,
}

/// Shared state for the Axum webhook server.
#[derive(Clone)]
struct WebhookState {
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    webhook_secret: Option<String>,
    allowed_users: Vec<String>,
    reply_contexts: Arc<RwLock<HashMap<String, ReplyContext>>>,
    pairing_store: Option<Arc<super::pairing::ChannelPairingStore>>,
    gateway_url: Option<String>,
}

impl KakaoTalkChannel {
    pub fn new(
        rest_api_key: String,
        admin_key: String,
        webhook_secret: Option<String>,
        allowed_users: Vec<String>,
        port: u16,
        pairing_store: Option<Arc<super::pairing::ChannelPairingStore>>,
        gateway_url: Option<String>,
    ) -> Self {
        Self {
            rest_api_key,
            admin_key,
            webhook_secret,
            allowed_users,
            port,
            client: reqwest::Client::new(),
            token_cache: Arc::new(RwLock::new(None)),
            reply_contexts: Arc::new(RwLock::new(HashMap::new())),
            pairing_store,
            gateway_url,
        }
    }

    /// Build from config struct (convenience for factory wiring).
    pub fn from_config(
        config: &crate::config::schema::KakaoTalkConfig,
        pairing_store: Option<Arc<super::pairing::ChannelPairingStore>>,
        gateway_url: Option<String>,
    ) -> Self {
        Self::new(
            config.rest_api_key.clone(),
            config.admin_key.clone(),
            config.webhook_secret.clone(),
            config.allowed_users.clone(),
            config.port,
            pairing_store,
            gateway_url,
        )
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    /// Get a valid access token, refreshing if expired.
    async fn get_access_token(&self) -> anyhow::Result<String> {
        // Check cache
        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache {
                let now = current_epoch_secs();
                if now < expiry {
                    return Ok(token.clone());
                }
            }
        }

        // Token expired or missing — use admin key directly for server-to-server calls
        // KakaoTalk business API uses admin key as Bearer token for server-side calls
        Ok(self.admin_key.clone())
    }

    /// Send a text message to a specific user via KakaoTalk Channel API.
    async fn send_text_message(&self, user_id: &str, text: &str) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        let template = serde_json::json!({
            "object_type": "text",
            "text": text,
            "link": {
                "web_url": "",
                "mobile_web_url": ""
            }
        });

        let resp = self
            .client
            .post(KAKAO_CHANNEL_API)
            .header("Authorization", format!("KakaoAK {token}"))
            .form(&[
                ("receiver_uuids", serde_json::json!([user_id]).to_string()),
                ("template_object", template.to_string()),
            ])
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("KakaoTalk send failed ({status}): {err}");
        }

        Ok(())
    }

    /// Send an Alimtalk template message (for business notifications).
    async fn send_alimtalk(
        &self,
        user_id: &str,
        template_id: &str,
        template_args: &HashMap<String, String>,
    ) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        let args: Vec<serde_json::Value> = template_args
            .iter()
            .map(|(k, v)| {
                serde_json::json!({
                    "key": k,
                    "value": v
                })
            })
            .collect();

        let body = serde_json::json!({
            "template_id": template_id,
            "receiver_uuids": [user_id],
            "template_args": args
        });

        let resp = self
            .client
            .post(KAKAO_ALIMTALK_API)
            .header("Authorization", format!("KakaoAK {token}"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("KakaoTalk Alimtalk send failed ({status}): {err}");
        }

        Ok(())
    }

    /// Split a long message into KakaoTalk-sized chunks (1000 characters max).
    /// Uses character count (not byte length) since KakaoTalk counts characters,
    /// and Korean characters are 3 bytes in UTF-8 but count as 1 character.
    fn split_message(text: &str) -> Vec<String> {
        if text.chars().count() <= KAKAO_MAX_TEXT_LEN {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut current = String::new();
        let mut current_char_count = 0usize;

        for line in text.lines() {
            let line_char_count = line.chars().count();
            let needed = if current.is_empty() {
                line_char_count
            } else {
                current_char_count + 1 + line_char_count // +1 for newline
            };

            if needed > KAKAO_MAX_TEXT_LEN {
                if !current.is_empty() {
                    chunks.push(current.clone());
                    current.clear();
                    current_char_count = 0;
                }
                // Handle single lines that exceed the limit
                if line_char_count > KAKAO_MAX_TEXT_LEN {
                    let mut chars = line.chars().peekable();
                    while chars.peek().is_some() {
                        let chunk: String = chars.by_ref().take(KAKAO_MAX_TEXT_LEN).collect();
                        chunks.push(chunk);
                    }
                } else {
                    current.push_str(line);
                    current_char_count = line_char_count;
                }
            } else {
                if !current.is_empty() {
                    current.push('\n');
                    current_char_count += 1;
                }
                current.push_str(line);
                current_char_count += line_char_count;
            }
        }

        if !current.is_empty() {
            chunks.push(current);
        }

        chunks
    }

    /// Build a carousel (list) template for rich message display.
    fn build_carousel_template(items: &[CarouselItem]) -> serde_json::Value {
        let contents: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                let mut content = serde_json::json!({
                    "title": item.title,
                    "description": item.description,
                    "link": {
                        "web_url": item.link_url,
                        "mobile_web_url": item.link_url
                    }
                });
                if let Some(ref img) = item.image_url {
                    content["image_url"] = serde_json::json!(img);
                    content["image_width"] = serde_json::json!(640);
                    content["image_height"] = serde_json::json!(640);
                }
                content
            })
            .collect();

        serde_json::json!({
            "object_type": "list",
            "header_title": "ZeroClaw",
            "header_link": {
                "web_url": "",
                "mobile_web_url": ""
            },
            "contents": contents
        })
    }

    /// Build a button template for interactive messages.
    fn build_button_template(text: &str, buttons: &[MessageButton]) -> serde_json::Value {
        let button_list: Vec<serde_json::Value> = buttons
            .iter()
            .map(|btn| {
                serde_json::json!({
                    "title": btn.label,
                    "link": {
                        "web_url": btn.url.as_deref().unwrap_or(""),
                        "mobile_web_url": btn.url.as_deref().unwrap_or("")
                    }
                })
            })
            .collect();

        serde_json::json!({
            "object_type": "text",
            "text": text,
            "link": {
                "web_url": "",
                "mobile_web_url": ""
            },
            "buttons": button_list
        })
    }

    /// Parse a remote command from an incoming message.
    /// Commands start with `/` prefix for ZeroClaw control via KakaoTalk.
    fn parse_remote_command(text: &str) -> Option<RemoteCommand> {
        let trimmed = text.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let parts: Vec<&str> = trimmed[1..].splitn(2, ' ').collect();
        let command = parts.first()?.to_lowercase();
        let args = (*parts.get(1).unwrap_or(&"")).to_string();

        match command.as_str() {
            "status" => Some(RemoteCommand::Status),
            "memory" => Some(RemoteCommand::MemoryQuery(args)),
            "remember" => Some(RemoteCommand::MemoryStore(args)),
            "forget" => Some(RemoteCommand::MemoryForget(args)),
            "cron" => Some(RemoteCommand::CronList),
            "help" => Some(RemoteCommand::Help),
            "shell" => Some(RemoteCommand::Shell(args)),
            _ => None,
        }
    }
}

/// Carousel item for rich list messages.
#[derive(Debug, Clone)]
pub struct CarouselItem {
    pub title: String,
    pub description: String,
    pub link_url: String,
    pub image_url: Option<String>,
}

/// Button for interactive messages.
#[derive(Debug, Clone)]
pub struct MessageButton {
    pub label: String,
    pub url: Option<String>,
}

/// Remote commands available via KakaoTalk `/command` syntax.
#[derive(Debug, Clone)]
pub enum RemoteCommand {
    /// Check ZeroClaw agent status
    Status,
    /// Query long-term memory
    MemoryQuery(String),
    /// Store to long-term memory
    MemoryStore(String),
    /// Forget a memory entry
    MemoryForget(String),
    /// List scheduled cron tasks
    CronList,
    /// Show help for available commands
    Help,
    /// Execute a shell command (requires Full autonomy level)
    Shell(String),
}

/// Get the current epoch time in seconds.
fn current_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Verify HMAC-SHA256 webhook signature from Kakao.
fn verify_webhook_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);

    // Kakao sends base64-encoded HMAC
    use base64::Engine;
    let Ok(expected_bytes) = base64::engine::general_purpose::STANDARD.decode(signature) else {
        return false;
    };

    mac.verify_slice(&expected_bytes).is_ok()
}

/// Build a Kakao Chatbot Skill JSON response with a simple text message.
fn kakao_skill_response(text: &str) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "version": "2.0",
        "template": {
            "outputs": [
                {
                    "simpleText": {
                        "text": text
                    }
                }
            ]
        }
    }))
}

/// Webhook handler: POST /kakao/webhook
/// Returns Kakao Chatbot Skill JSON response format for Skill API requests,
/// and plain StatusCode for direct callback format.
async fn handle_webhook(
    State(state): State<WebhookState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Verify webhook signature if secret is configured
    if let Some(ref secret) = state.webhook_secret {
        let signature = headers
            .get("X-Kakao-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_webhook_signature(secret, &body, signature) {
            tracing::warn!("KakaoTalk: webhook signature verification failed");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("KakaoTalk: invalid webhook payload: {e}");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    // Extract user event type
    let user_request = payload.get("userRequest");
    // Kakao Chatbot Skill format — must return JSON response
    if let Some(user_req) = user_request {
        let user_id = user_req
            .get("user")
            .and_then(|u| u.get("id"))
            .and_then(|id| id.as_str())
            .unwrap_or("unknown");

        // Check user allowlist
        if !state.allowed_users.iter().any(|u| u == "*" || u == user_id) {
            // Check if user was paired via web flow (one-click auto-pair)
            if let Some(ref store) = state.pairing_store {
                if store.is_paired("kakao", user_id) {
                    let uid = user_id.to_string();
                    tokio::spawn(async move {
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            super::pairing::persist_channel_allowlist("kakao", &uid)
                        })
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
                        {
                            tracing::error!("KakaoTalk: failed to persist pairing: {e}");
                        }
                    });
                    // Fall through to process message normally
                } else {
                    // Create token and show one-click connect button
                    if let Some(ref gw_url) = state.gateway_url {
                        let token = store.create_token("kakao", user_id);
                        let auto_url =
                            super::pairing::ChannelPairingStore::auto_pair_url(gw_url, &token);
                        return Json(serde_json::json!({
                            "version": "2.0",
                            "template": {
                                "outputs": [{
                                    "simpleText": {
                                        "text": "MoA에 연결하려면 아래 버튼을 눌러주세요.\n\nTap the button below to connect to MoA."
                                    }
                                }],
                                "quickReplies": [{
                                    "messageText": "연결하기",
                                    "action": "webLink",
                                    "label": "🔗 연결하기 / Connect",
                                    "webLinkUrl": auto_url
                                }]
                            }
                        })).into_response();
                    }

                    tracing::warn!("KakaoTalk: ignoring message from unauthorized user: {user_id}");
                    return kakao_skill_response("접근이 허용되지 않은 사용자입니다.\n\nAccess denied. Please contact the operator.").into_response();
                }
            } else {
                tracing::warn!("KakaoTalk: ignoring message from unauthorized user: {user_id}");
                return kakao_skill_response("접근이 허용되지 않은 사용자입니다.\n\nAccess denied. Please contact the operator.").into_response();
            }
        }

        let utterance = user_req
            .get("utterance")
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .trim();

        if utterance.is_empty() {
            return kakao_skill_response("메시지를 입력해주세요.").into_response();
        }

        // Check for remote commands (e.g., /status, /help, /memory)
        if let Some(cmd) = KakaoTalkChannel::parse_remote_command(utterance) {
            let cmd_content = format!(
                "[remote_command] {}",
                match &cmd {
                    RemoteCommand::Status => "/status".to_string(),
                    RemoteCommand::Help => "/help".to_string(),
                    RemoteCommand::CronList => "/cron".to_string(),
                    RemoteCommand::MemoryQuery(q) => format!("/memory {q}"),
                    RemoteCommand::MemoryStore(v) => format!("/remember {v}"),
                    RemoteCommand::MemoryForget(k) => format!("/forget {k}"),
                    RemoteCommand::Shell(c) => format!("/shell {c}"),
                }
            );

            let channel_msg = ChannelMessage {
                id: Uuid::new_v4().to_string(),
                sender: user_id.to_string(),
                reply_target: user_id.to_string(),
                content: cmd_content,
                channel: "kakao".to_string(),
                timestamp: current_epoch_secs(),
                thread_ts: None,
                silent: false,
            };

            if state.tx.send(channel_msg).await.is_err() {
                tracing::warn!("KakaoTalk: message channel closed");
                return kakao_skill_response(
                    "시스템 오류가 발생했습니다. 잠시 후 다시 시도해주세요.",
                )
                .into_response();
            }
            return kakao_skill_response("명령을 처리 중입니다...").into_response();
        }

        // Extract bot_user_key for replies
        let bot_user_key = user_req
            .get("user")
            .and_then(|u| u.get("properties"))
            .and_then(|p| p.get("bot_user_key"))
            .and_then(|k| k.as_str())
            .map(String::from);

        // Cache reply context
        {
            let mut contexts = state.reply_contexts.write().await;
            contexts.insert(
                user_id.to_string(),
                ReplyContext {
                    user_id: user_id.to_string(),
                    bot_user_key,
                },
            );
        }

        let channel_msg = ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: user_id.to_string(),
            reply_target: user_id.to_string(),
            content: utterance.to_string(),
            channel: "kakao".to_string(),
            timestamp: current_epoch_secs(),
            thread_ts: None,
            silent: false,
        };

        if state.tx.send(channel_msg).await.is_err() {
            tracing::warn!("KakaoTalk: message channel closed");
            return kakao_skill_response("시스템 오류가 발생했습니다.").into_response();
        }

        return kakao_skill_response("요청을 처리 중입니다. 잠시만 기다려주세요...")
            .into_response();
    }

    // Also handle direct message callback format (plain StatusCode response)
    if let Some(content) = payload.get("content") {
        let user_id = payload
            .get("user_id")
            .and_then(|u| u.as_str())
            .unwrap_or("unknown");

        if !state.allowed_users.iter().any(|u| u == "*" || u == user_id) {
            // Check if user was paired via one-click web flow
            if let Some(ref store) = state.pairing_store {
                if store.is_paired("kakao", user_id) {
                    let uid = user_id.to_string();
                    tokio::spawn(async move {
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            super::pairing::persist_channel_allowlist("kakao", &uid)
                        })
                        .await
                        .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
                        {
                            tracing::error!("KakaoTalk: failed to persist pairing: {e}");
                        }
                    });
                    // Fall through to process message
                } else {
                    return StatusCode::FORBIDDEN.into_response();
                }
            } else {
                return StatusCode::FORBIDDEN.into_response();
            }
        }

        let text = content.as_str().unwrap_or("").trim();
        if text.is_empty() {
            return StatusCode::OK.into_response();
        }

        let channel_msg = ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: user_id.to_string(),
            reply_target: user_id.to_string(),
            content: text.to_string(),
            channel: "kakao".to_string(),
            timestamp: current_epoch_secs(),
            thread_ts: None,
            silent: false,
        };

        if state.tx.send(channel_msg).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }

        return StatusCode::OK.into_response();
    }

    StatusCode::OK.into_response()
}

/// Health check endpoint: GET /kakao/health
async fn handle_health() -> StatusCode {
    StatusCode::OK
}

#[async_trait]
impl Channel for KakaoTalkChannel {
    fn name(&self) -> &str {
        "kakao"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let chunks = Self::split_message(&message.content);

        for chunk in chunks {
            self.send_text_message(&message.recipient, &chunk).await?;
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let state = WebhookState {
            tx,
            webhook_secret: self.webhook_secret.clone(),
            allowed_users: self.allowed_users.clone(),
            reply_contexts: Arc::clone(&self.reply_contexts),
            pairing_store: self.pairing_store.clone(),
            gateway_url: self.gateway_url.clone(),
        };

        let app = Router::new()
            .route("/kakao/webhook", post(handle_webhook))
            .route("/kakao/health", get(handle_health))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.port));
        tracing::info!("KakaoTalk: webhook server listening on {addr}");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("KakaoTalk webhook server error: {e}"))?;

        anyhow::bail!("KakaoTalk webhook server stopped unexpectedly")
    }

    async fn health_check(&self) -> bool {
        // Verify API connectivity by checking if admin key is set
        // A more thorough check could call /v1/api/talk/profile endpoint
        let result = self
            .client
            .get(format!("{KAKAO_API_BASE}/v1/api/talk/profile"))
            .header("Authorization", format!("KakaoAK {}", self.admin_key))
            .send()
            .await;

        match result {
            Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 401,
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch =
            KakaoTalkChannel::new("key".into(), "admin".into(), None, vec![], 8080, None, None);
        assert_eq!(ch.name(), "kakao");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = KakaoTalkChannel::new(
            "key".into(),
            "admin".into(),
            None,
            vec!["*".into()],
            8080,
            None,
            None,
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = KakaoTalkChannel::new(
            "key".into(),
            "admin".into(),
            None,
            vec!["user_123".into()],
            8080,
            None,
            None,
        );
        assert!(ch.is_user_allowed("user_123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch =
            KakaoTalkChannel::new("key".into(), "admin".into(), None, vec![], 8080, None, None);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_split_message_short() {
        let text = "Hello, world!";
        let chunks = KakaoTalkChannel::split_message(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "Hello, world!");
    }

    #[test]
    fn test_split_message_long() {
        let text = "a".repeat(2500);
        let chunks = KakaoTalkChannel::split_message(&text);
        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= KAKAO_MAX_TEXT_LEN);
        }
        let rejoined: String = chunks.join("");
        assert_eq!(rejoined, text);
    }

    #[test]
    fn test_split_message_multiline() {
        let lines: Vec<String> = (0..20)
            .map(|i| format!("Line {i}: {}", "x".repeat(80)))
            .collect();
        let text = lines.join("\n");
        let chunks = KakaoTalkChannel::split_message(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= KAKAO_MAX_TEXT_LEN);
        }
    }

    #[test]
    fn test_split_message_exact_boundary() {
        let text = "a".repeat(KAKAO_MAX_TEXT_LEN);
        let chunks = KakaoTalkChannel::split_message(&text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), KAKAO_MAX_TEXT_LEN);
    }

    #[test]
    fn test_split_message_utf8_safe() {
        // Korean text (3 bytes per char in UTF-8)
        // 1500 Korean chars should split into 2 chunks by character count
        let korean = "가".repeat(1500);
        let chunks = KakaoTalkChannel::split_message(&korean);
        assert_eq!(chunks.len(), 2);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= KAKAO_MAX_TEXT_LEN);
        }
        // Total characters should be preserved
        let total_chars: usize = chunks.iter().map(|c| c.chars().count()).sum();
        assert_eq!(total_chars, 1500);
    }

    #[test]
    fn test_parse_remote_command_status() {
        let cmd = KakaoTalkChannel::parse_remote_command("/status");
        assert!(matches!(cmd, Some(RemoteCommand::Status)));
    }

    #[test]
    fn test_parse_remote_command_memory_query() {
        let cmd = KakaoTalkChannel::parse_remote_command("/memory what is my name");
        assert!(matches!(cmd, Some(RemoteCommand::MemoryQuery(ref s)) if s == "what is my name"));
    }

    #[test]
    fn test_parse_remote_command_help() {
        let cmd = KakaoTalkChannel::parse_remote_command("/help");
        assert!(matches!(cmd, Some(RemoteCommand::Help)));
    }

    #[test]
    fn test_parse_remote_command_shell() {
        let cmd = KakaoTalkChannel::parse_remote_command("/shell ls -la");
        assert!(matches!(cmd, Some(RemoteCommand::Shell(ref s)) if s == "ls -la"));
    }

    #[test]
    fn test_parse_remote_command_unknown() {
        let cmd = KakaoTalkChannel::parse_remote_command("/unknown_cmd");
        assert!(cmd.is_none());
    }

    #[test]
    fn test_parse_remote_command_no_prefix() {
        let cmd = KakaoTalkChannel::parse_remote_command("just a message");
        assert!(cmd.is_none());
    }

    #[test]
    fn test_parse_remote_command_remember() {
        let cmd = KakaoTalkChannel::parse_remote_command("/remember my favorite color is blue");
        assert!(
            matches!(cmd, Some(RemoteCommand::MemoryStore(ref s)) if s == "my favorite color is blue")
        );
    }

    #[test]
    fn test_parse_remote_command_forget() {
        let cmd = KakaoTalkChannel::parse_remote_command("/forget my_key");
        assert!(matches!(cmd, Some(RemoteCommand::MemoryForget(ref s)) if s == "my_key"));
    }

    #[test]
    fn test_parse_remote_command_cron() {
        let cmd = KakaoTalkChannel::parse_remote_command("/cron");
        assert!(matches!(cmd, Some(RemoteCommand::CronList)));
    }

    #[test]
    fn test_carousel_template_structure() {
        let items = vec![
            CarouselItem {
                title: "Item 1".into(),
                description: "Description 1".into(),
                link_url: "https://example.com/1".into(),
                image_url: Some("https://example.com/img1.png".into()),
            },
            CarouselItem {
                title: "Item 2".into(),
                description: "Description 2".into(),
                link_url: "https://example.com/2".into(),
                image_url: None,
            },
        ];

        let template = KakaoTalkChannel::build_carousel_template(&items);
        assert_eq!(template["object_type"], "list");
        assert_eq!(template["header_title"], "ZeroClaw");
        let contents = template["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["title"], "Item 1");
        assert!(contents[0].get("image_url").is_some());
        assert!(contents[1].get("image_url").is_none());
    }

    #[test]
    fn test_button_template_structure() {
        let buttons = vec![
            MessageButton {
                label: "Visit".into(),
                url: Some("https://example.com".into()),
            },
            MessageButton {
                label: "Cancel".into(),
                url: None,
            },
        ];

        let template = KakaoTalkChannel::build_button_template("Choose an option:", &buttons);
        assert_eq!(template["object_type"], "text");
        assert_eq!(template["text"], "Choose an option:");
        let btns = template["buttons"].as_array().unwrap();
        assert_eq!(btns.len(), 2);
        assert_eq!(btns[0]["title"], "Visit");
        assert_eq!(btns[1]["title"], "Cancel");
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
rest_api_key = "test_key_123"
admin_key = "admin_key_456"
webhook_secret = "secret_789"
allowed_users = ["user_a", "*"]
port = 9090
"#;
        let config: crate::config::schema::KakaoTalkConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.rest_api_key, "test_key_123");
        assert_eq!(config.admin_key, "admin_key_456");
        assert_eq!(config.webhook_secret, Some("secret_789".to_string()));
        assert_eq!(config.allowed_users, vec!["user_a", "*"]);
        assert_eq!(config.port, 9090);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
rest_api_key = "key"
admin_key = "admin"
"#;
        let config: crate::config::schema::KakaoTalkConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
        assert!(config.webhook_secret.is_none());
        assert_eq!(config.port, 8787);
    }
}
