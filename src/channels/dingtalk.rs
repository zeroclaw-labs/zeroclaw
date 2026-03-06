use super::traits::{Channel, ChannelMessage, SendMessage};
use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::CONTENT_TYPE;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const DINGTALK_BOT_CALLBACK_TOPIC: &str = "/v1.0/im/bot/messages/get";
const DINGTALK_API_BASE: &str = "https://api.dingtalk.com";
const DINGTALK_MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const DINGTALK_IMAGE_DEFAULT_PROMPT: &str = "请识别这张图片";

/// Cached access token with expiry time
#[derive(Clone)]
struct AccessToken {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ExtractedInboundMessage {
    text: Option<String>,
    download_codes: Vec<String>,
    picture_urls: Vec<String>,
    caption: Option<String>,
    msg_type: String,
}

impl ExtractedInboundMessage {
    fn has_image_hints(&self) -> bool {
        !self.download_codes.is_empty() || !self.picture_urls.is_empty()
    }
}

/// DingTalk channel — connects via Stream Mode WebSocket for real-time messages.
/// Replies are sent through DingTalk Open API (no session webhook required).
pub struct DingTalkChannel {
    client_id: String,
    client_secret: String,
    allowed_users: Vec<String>,
    /// Per-chat session webhooks for sending replies (chatID -> webhook URL).
    /// DingTalk provides a unique webhook URL with each incoming message.
    session_webhooks: Arc<RwLock<HashMap<String, String>>>,
    /// Cached access token for Open API calls
    access_token: Arc<RwLock<Option<AccessToken>>>,
    /// Workspace directory used to persist inbound images for multimodal markers.
    workspace_dir: Option<PathBuf>,
}

/// Response from DingTalk gateway connection registration.
#[derive(serde::Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

impl DingTalkChannel {
    pub fn new(client_id: String, client_secret: String, allowed_users: Vec<String>) -> Self {
        Self {
            client_id,
            client_secret,
            allowed_users,
            session_webhooks: Arc::new(RwLock::new(HashMap::new())),
            access_token: Arc::new(RwLock::new(None)),
            workspace_dir: None,
        }
    }

    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    fn api_url(path: &str) -> String {
        format!("{DINGTALK_API_BASE}{path}")
    }

    fn api_url_with_base(base: &str, path: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), path)
    }

    fn normalize_text(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn is_group_recipient(recipient: &str) -> bool {
        // DingTalk group conversation IDs are typically prefixed with `cid`.
        recipient.starts_with("cid")
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.dingtalk")
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    fn extract_text_content(data: &serde_json::Value) -> Option<String> {
        fn normalize_text(raw: &str) -> Option<String> {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }

        fn text_content_from_value(value: &serde_json::Value) -> Option<String> {
            match value {
                serde_json::Value::String(s) => {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
                        // Some DingTalk events encode nested text payloads as JSON strings.
                        if let Some(content) = parsed
                            .get("content")
                            .and_then(|v| v.as_str())
                            .and_then(normalize_text)
                        {
                            return Some(content);
                        }
                    }
                    normalize_text(s)
                }
                serde_json::Value::Object(map) => map
                    .get("content")
                    .and_then(|v| v.as_str())
                    .and_then(normalize_text),
                _ => None,
            }
        }

        fn collect_rich_text_fragments(
            value: &serde_json::Value,
            out: &mut Vec<String>,
            depth: usize,
        ) {
            const MAX_RICH_TEXT_DEPTH: usize = 16;
            if depth >= MAX_RICH_TEXT_DEPTH {
                return;
            }

            match value {
                serde_json::Value::String(s) => {
                    if let Some(normalized) = normalize_text(s) {
                        out.push(normalized);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        collect_rich_text_fragments(item, out, depth + 1);
                    }
                }
                serde_json::Value::Object(map) => {
                    for key in ["text", "content"] {
                        if let Some(text_val) = map.get(key).and_then(|v| v.as_str()) {
                            if let Some(normalized) = normalize_text(text_val) {
                                out.push(normalized);
                            }
                        }
                    }
                    for key in ["children", "elements", "richText", "rich_text"] {
                        if let Some(child) = map.get(key) {
                            collect_rich_text_fragments(child, out, depth + 1);
                        }
                    }
                }
                _ => {}
            }
        }

        // Canonical text payload.
        if let Some(content) = data.get("text").and_then(text_content_from_value) {
            return Some(content);
        }

        // Some events include top-level content directly.
        if let Some(content) = data
            .get("content")
            .and_then(|v| v.as_str())
            .and_then(normalize_text)
        {
            return Some(content);
        }

        // Rich text payload fallback.
        if let Some(rich) = data
            .get("content")
            .and_then(|v| v.get("richText").or_else(|| v.get("rich_text")))
            .or_else(|| data.get("richText").or_else(|| data.get("rich_text")))
        {
            let mut fragments = Vec::new();
            collect_rich_text_fragments(rich, &mut fragments, 0);
            if !fragments.is_empty() {
                let merged = fragments.join(" ");
                if let Some(content) = normalize_text(&merged) {
                    return Some(content);
                }
            }
        }

        // Markdown payload fallback.
        data.get("markdown")
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .and_then(normalize_text)
    }

    fn extract_picture_url_from_value(value: &serde_json::Value) -> Option<String> {
        for key in ["pictureUrl", "picture_url", "url"] {
            if let Some(url) = value
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(Self::normalize_text)
            {
                return Some(url);
            }
        }
        None
    }

    fn extract_download_code_from_value(value: &serde_json::Value) -> Option<String> {
        for key in ["downloadCode", "download_code", "pictureDownloadCode"] {
            if let Some(code) = value
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(Self::normalize_text)
            {
                return Some(code);
            }
        }
        None
    }

    fn extract_inbound_payload(data: &serde_json::Value) -> ExtractedInboundMessage {
        let msg_type = data
            .get("msgtype")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();

        let mut payload = ExtractedInboundMessage {
            text: Self::extract_text_content(data),
            caption: data
                .get("content")
                .and_then(|v| v.get("caption"))
                .and_then(|v| v.as_str())
                .and_then(Self::normalize_text),
            msg_type,
            ..Default::default()
        };

        let mut picture_urls = HashSet::new();
        let mut download_codes = HashSet::new();

        match payload.msg_type.as_str() {
            "picture" | "image" => {
                if let Some(content) = data.get("content") {
                    if let Some(url) = Self::extract_picture_url_from_value(content) {
                        picture_urls.insert(url);
                    }
                    if let Some(code) = Self::extract_download_code_from_value(content) {
                        download_codes.insert(code);
                    }
                }
            }
            "richText" | "rich_text" => {
                if let Some(parts) = data
                    .get("content")
                    .and_then(|v| v.get("richText").or_else(|| v.get("rich_text")))
                    .or_else(|| data.get("richText").or_else(|| data.get("rich_text")))
                    .and_then(|v| v.as_array())
                {
                    for part in parts {
                        if let Some(url) = Self::extract_picture_url_from_value(part) {
                            picture_urls.insert(url);
                        }

                        if let Some(code) = Self::extract_download_code_from_value(part) {
                            download_codes.insert(code);
                        }

                        if part
                            .get("type")
                            .and_then(|v| v.as_str())
                            .is_some_and(|value| value.eq_ignore_ascii_case("picture"))
                        {
                            if let Some(code) = Self::extract_download_code_from_value(part) {
                                download_codes.insert(code);
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        payload.picture_urls = picture_urls.into_iter().collect();
        payload.picture_urls.sort();

        payload.download_codes = download_codes.into_iter().collect();
        payload.download_codes.sort();

        if payload.caption.is_none() {
            payload.caption = payload.text.clone();
        }

        payload
    }

    fn should_ack_stream_frame(frame_type: &str) -> bool {
        matches!(frame_type, "SYSTEM" | "EVENT" | "CALLBACK")
    }

    fn build_ack_frame(frame: &serde_json::Value) -> Message {
        let message_id = frame
            .get("headers")
            .and_then(|h| h.get("messageId"))
            .and_then(|m| m.as_str())
            .unwrap_or("");

        let ack = serde_json::json!({
            "code": 200,
            "headers": {
                "contentType": "application/json",
                "messageId": message_id,
            },
            "message": "OK",
            "data": "",
        });

        Message::Text(ack.to_string().into())
    }

    fn image_extension_from_content_type(content_type: &str) -> &'static str {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        match mime.as_str() {
            "image/png" => "png",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/bmp" => "bmp",
            _ => "jpg",
        }
    }

    async fn resolve_workspace_inbound_dir(&self) -> anyhow::Result<PathBuf> {
        let workspace = self.workspace_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "DingTalk workspace_dir is not configured; inbound image download is disabled"
            )
        })?;

        fs::create_dir_all(workspace).await.with_context(|| {
            format!(
                "Failed to create DingTalk workspace: {}",
                workspace.display()
            )
        })?;

        let workspace_root = fs::canonicalize(workspace)
            .await
            .unwrap_or_else(|_| workspace.to_path_buf());

        let inbound_dir = workspace.join("dingtalk_files").join("inbound");
        fs::create_dir_all(&inbound_dir).await.with_context(|| {
            format!(
                "Failed to create DingTalk inbound media directory: {}",
                inbound_dir.display()
            )
        })?;

        let meta = fs::symlink_metadata(&inbound_dir).await.with_context(|| {
            format!(
                "Failed to inspect DingTalk inbound media directory: {}",
                inbound_dir.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            anyhow::bail!(
                "Refusing to use symlinked DingTalk inbound directory: {}",
                inbound_dir.display()
            );
        }

        let resolved = fs::canonicalize(&inbound_dir).await.with_context(|| {
            format!(
                "Failed to resolve DingTalk inbound media directory: {}",
                inbound_dir.display()
            )
        })?;

        if !resolved.starts_with(&workspace_root) {
            anyhow::bail!(
                "DingTalk inbound media directory escapes workspace: {}",
                resolved.display()
            );
        }

        Ok(resolved)
    }

    async fn resolve_download_url(&self, download_code: &str) -> anyhow::Result<String> {
        self.resolve_download_url_with_base(download_code, DINGTALK_API_BASE)
            .await
    }

    async fn resolve_download_url_with_base(
        &self,
        download_code: &str,
        api_base: &str,
    ) -> anyhow::Result<String> {
        let token = self.get_access_token().await?;
        let endpoint = Self::api_url_with_base(api_base, "/v1.0/robot/messageFiles/download");
        let body = serde_json::json!({
            "downloadCode": download_code,
            "robotCode": self.client_id,
        });

        let resp = self
            .http_client()
            .post(&endpoint)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let payload_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&payload_text);
            anyhow::bail!(
                "DingTalk messageFiles/download failed ({status}) for downloadCode={download_code}: {sanitized}"
            );
        }

        let payload: serde_json::Value = serde_json::from_str(&payload_text)
            .with_context(|| "DingTalk messageFiles/download returned non-JSON payload")?;

        let download_url = payload
            .get("downloadUrl")
            .or_else(|| payload.get("download_url"))
            .and_then(|v| v.as_str())
            .and_then(Self::normalize_text)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "DingTalk messageFiles/download missing downloadUrl for downloadCode={download_code}"
                )
            })?;

        Ok(download_url)
    }

    async fn download_image_to_workspace(&self, url: &str) -> anyhow::Result<PathBuf> {
        let inbound_dir = self.resolve_workspace_inbound_dir().await?;

        let resp = self.http_client().get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("DingTalk image download failed ({status}) from {url}");
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !content_type.to_ascii_lowercase().starts_with("image/") {
            anyhow::bail!(
                "DingTalk image download rejected non-image content-type '{}' from {}",
                content_type,
                url
            );
        }

        let bytes = resp
            .bytes()
            .await
            .with_context(|| "Failed to read DingTalk image download body")?;

        if bytes.is_empty() {
            anyhow::bail!("DingTalk image download returned an empty payload from {url}");
        }

        if bytes.len() > DINGTALK_MAX_IMAGE_BYTES {
            anyhow::bail!(
                "DingTalk image exceeds size limit: {} bytes (max {} bytes)",
                bytes.len(),
                DINGTALK_MAX_IMAGE_BYTES
            );
        }

        let extension = Self::image_extension_from_content_type(&content_type);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let output_path =
            inbound_dir.join(format!("dt_{ts}_{}.{}", Uuid::new_v4().simple(), extension));

        match fs::symlink_metadata(&output_path).await {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    anyhow::bail!(
                        "Refusing to write DingTalk image through symlink: {}",
                        output_path.display()
                    );
                }
                if !meta.is_file() {
                    anyhow::bail!(
                        "DingTalk image output path exists and is not a file: {}",
                        output_path.display()
                    );
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        fs::write(&output_path, &bytes).await.with_context(|| {
            format!(
                "Failed to persist DingTalk inbound image to {}",
                output_path.display()
            )
        })?;

        let resolved_output = fs::canonicalize(&output_path).await.with_context(|| {
            format!(
                "Failed to resolve written DingTalk image path: {}",
                output_path.display()
            )
        })?;

        if !resolved_output.starts_with(&inbound_dir) {
            anyhow::bail!(
                "Resolved DingTalk image path escaped inbound dir: {}",
                resolved_output.display()
            );
        }

        Ok(resolved_output)
    }

    fn compose_channel_content(
        local_image_paths: &[PathBuf],
        text: Option<&str>,
        caption: Option<&str>,
    ) -> Option<String> {
        let prompt_text = text
            .and_then(Self::normalize_text)
            .or_else(|| caption.and_then(Self::normalize_text));

        if local_image_paths.is_empty() {
            return prompt_text;
        }

        let markers = local_image_paths
            .iter()
            .map(|path| format!("[IMAGE:{}]", path.display()))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = prompt_text.unwrap_or_else(|| DINGTALK_IMAGE_DEFAULT_PROMPT.to_string());
        Some(format!("{markers}\n\n{prompt}"))
    }

    fn resolve_chat_id(data: &serde_json::Value, sender_id: &str) -> String {
        let is_private_chat = data
            .get("conversationType")
            .and_then(|value| {
                value
                    .as_str()
                    .map(|v| v == "1")
                    .or_else(|| value.as_i64().map(|v| v == 1))
            })
            .unwrap_or(true);

        if is_private_chat {
            sender_id.to_string()
        } else {
            data.get("conversationId")
                .and_then(|c| c.as_str())
                .unwrap_or(sender_id)
                .to_string()
        }
    }

    /// Get or refresh access token using OAuth2
    async fn get_access_token(&self) -> anyhow::Result<String> {
        {
            let cached = self.access_token.read().await;
            if let Some(ref at) = *cached {
                if at.expires_at > Instant::now() {
                    return Ok(at.token.clone());
                }
            }
        }

        // Re-check under write lock to avoid duplicate token fetches under contention.
        let mut cached = self.access_token.write().await;
        if let Some(ref at) = *cached {
            if at.expires_at > Instant::now() {
                return Ok(at.token.clone());
            }
        }

        let url = Self::api_url("/v1.0/oauth2/accessToken");
        let body = serde_json::json!({
            "appKey": self.client_id,
            "appSecret": self.client_secret,
        });

        let resp = self.http_client().post(url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk access token request failed ({status}): {err}");
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct TokenResponse {
            access_token: String,
            expire_in: u64,
        }

        let token_resp: TokenResponse = resp.json().await?;
        let expires_in = Duration::from_secs(token_resp.expire_in.saturating_sub(60));
        let token = token_resp.access_token;

        *cached = Some(AccessToken {
            token: token.clone(),
            expires_at: Instant::now() + expires_in,
        });

        Ok(token)
    }

    /// Register a connection with DingTalk's gateway to get a WebSocket endpoint.
    async fn register_connection(&self) -> anyhow::Result<GatewayResponse> {
        let body = serde_json::json!({
            "clientId": self.client_id,
            "clientSecret": self.client_secret,
            "subscriptions": [
                {
                    "type": "CALLBACK",
                    "topic": DINGTALK_BOT_CALLBACK_TOPIC,
                }
            ],
        });

        let resp = self
            .http_client()
            .post(Self::api_url("/v1.0/gateway/connections/open"))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("DingTalk gateway registration failed ({status}): {sanitized}");
        }

        let gw: GatewayResponse = resp.json().await?;
        Ok(gw)
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        let title = message.subject.as_deref().unwrap_or("ZeroClaw");

        let msg_param = serde_json::json!({
            "text": message.content,
            "title": title,
        });

        let (url, body) = if Self::is_group_recipient(&message.recipient) {
            (
                Self::api_url("/v1.0/robot/groupMessages/send"),
                serde_json::json!({
                    "robotCode": self.client_id,
                    "openConversationId": message.recipient,
                    "msgKey": "sampleMarkdown",
                    "msgParam": msg_param.to_string(),
                }),
            )
        } else {
            (
                Self::api_url("/v1.0/robot/oToMessages/batchSend"),
                serde_json::json!({
                    "robotCode": self.client_id,
                    "userIds": [&message.recipient],
                    "msgKey": "sampleMarkdown",
                    "msgParam": msg_param.to_string(),
                }),
            )
        };

        let resp = self
            .http_client()
            .post(url)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let resp_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let sanitized = crate::providers::sanitize_api_error(&resp_text);
            anyhow::bail!("DingTalk API send failed ({status}): {sanitized}");
        }

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&resp_text) {
            let app_code = json
                .get("errcode")
                .and_then(|v| v.as_i64())
                .or_else(|| json.get("code").and_then(|v| v.as_i64()))
                .unwrap_or(0);
            if app_code != 0 {
                let app_msg = json
                    .get("errmsg")
                    .and_then(|v| v.as_str())
                    .or_else(|| json.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error");
                anyhow::bail!("DingTalk API send rejected (code={app_code}): {app_msg}");
            }
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("DingTalk: registering gateway connection...");

        let gw = self.register_connection().await?;
        let ws_url = format!("{}?ticket={}", gw.endpoint, gw.ticket);

        tracing::info!("DingTalk: connecting to stream WebSocket...");
        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        tracing::info!("DingTalk: connected and listening for messages...");

        while let Some(msg) = read.next().await {
            let msg = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                    tracing::warn!("DingTalk WebSocket error: {sanitized}");
                    break;
                }
                _ => continue,
            };

            let frame: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");

            if Self::should_ack_stream_frame(frame_type) {
                if let Err(e) = write.send(Self::build_ack_frame(&frame)).await {
                    tracing::warn!("DingTalk: failed to send frame ack: {e}");
                    break;
                }
            }

            match frame_type {
                "EVENT" | "CALLBACK" => {
                    let data = match Self::parse_stream_data(&frame) {
                        Some(v) => v,
                        None => {
                            tracing::debug!("DingTalk: frame has no parseable data payload");
                            continue;
                        }
                    };

                    let sender_id = data
                        .get("senderStaffId")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown");

                    if !self.is_user_allowed(sender_id) {
                        tracing::warn!(
                            "DingTalk: ignoring message from unauthorized user: {sender_id}"
                        );
                        continue;
                    }

                    // Private chat uses sender ID, group chat uses conversation ID.
                    let chat_id = Self::resolve_chat_id(&data, sender_id);

                    // Store session webhook for later replies.
                    if let Some(webhook) = data.get("sessionWebhook").and_then(|w| w.as_str()) {
                        let webhook = webhook.to_string();
                        let mut webhooks = self.session_webhooks.write().await;
                        // Use both keys so reply routing works for both group and private flows.
                        webhooks.insert(chat_id.clone(), webhook.clone());
                        webhooks.insert(sender_id.to_string(), webhook);
                    }

                    let extracted = Self::extract_inbound_payload(&data);

                    let mut local_images = Vec::new();
                    let mut image_failures = Vec::new();

                    for url in &extracted.picture_urls {
                        match self.download_image_to_workspace(url).await {
                            Ok(path) => local_images.push(path),
                            Err(err) => {
                                let sanitized =
                                    crate::providers::sanitize_api_error(&err.to_string());
                                image_failures.push(format!("url={url}: {sanitized}"));
                            }
                        }
                    }

                    for code in &extracted.download_codes {
                        match self.resolve_download_url(code).await {
                            Ok(download_url) => {
                                match self.download_image_to_workspace(&download_url).await {
                                    Ok(path) => local_images.push(path),
                                    Err(err) => {
                                        let sanitized =
                                            crate::providers::sanitize_api_error(&err.to_string());
                                        image_failures.push(format!(
                                            "downloadCode={code} downloadUrl={download_url}: {sanitized}"
                                        ));
                                    }
                                }
                            }
                            Err(err) => {
                                let sanitized =
                                    crate::providers::sanitize_api_error(&err.to_string());
                                image_failures.push(format!("downloadCode={code}: {sanitized}"));
                            }
                        }
                    }

                    let mut content = Self::compose_channel_content(
                        &local_images,
                        extracted.text.as_deref(),
                        extracted.caption.as_deref(),
                    );

                    if content.is_none() && extracted.has_image_hints() {
                        content = extracted
                            .text
                            .clone()
                            .or(extracted.caption.clone())
                            .or_else(|| Some("[DingTalk 图片已收到但下载失败]".to_string()));
                    }

                    if local_images.is_empty() && extracted.has_image_hints() {
                        let first_error = image_failures.first().cloned().unwrap_or_default();
                        tracing::warn!(
                            msg_type = %extracted.msg_type,
                            download_codes = ?extracted.download_codes,
                            picture_urls = ?extracted.picture_urls,
                            first_error = %first_error,
                            "DingTalk: inbound image hints detected but no image was persisted"
                        );
                    }

                    let Some(content) = content.and_then(|v| Self::normalize_text(&v)) else {
                        let keys = data
                            .as_object()
                            .map(|obj| obj.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default();
                        let msg_type = data.get("msgtype").and_then(|v| v.as_str()).unwrap_or("");
                        tracing::warn!(
                            msg_type = %msg_type,
                            keys = ?keys,
                            "DingTalk: dropped callback without extractable text or image content"
                        );
                        continue;
                    };

                    let channel_msg = ChannelMessage {
                        id: Uuid::new_v4().to_string(),
                        sender: sender_id.to_string(),
                        reply_target: chat_id,
                        content,
                        channel: "dingtalk".to_string(),
                        timestamp: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        thread_ts: None,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        tracing::warn!("DingTalk: message channel closed");
                        break;
                    }
                }
                _ => {}
            }
        }

        anyhow::bail!("DingTalk WebSocket stream ended")
    }

    async fn health_check(&self) -> bool {
        self.register_connection().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_name() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec![]);
        assert_eq!(ch.name(), "dingtalk");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec!["user123".into()]);
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = DingTalkChannel::new("id".into(), "secret".into(), vec![]);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
client_id = "app_id_123"
client_secret = "secret_456"
allowed_users = ["user1", "*"]
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.client_id, "app_id_123");
        assert_eq!(config.client_secret, "secret_456");
        assert_eq!(config.allowed_users, vec!["user1", "*"]);
    }

    #[test]
    fn test_config_serde_defaults() {
        let toml_str = r#"
client_id = "id"
client_secret = "secret"
"#;
        let config: crate::config::schema::DingTalkConfig = toml::from_str(toml_str).unwrap();
        assert!(config.allowed_users.is_empty());
    }

    #[test]
    fn parse_stream_data_supports_string_payload() {
        let frame = serde_json::json!({
            "data": "{\"text\":{\"content\":\"hello\"}}"
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn parse_stream_data_supports_object_payload() {
        let frame = serde_json::json!({
            "data": {"text": {"content": "hello"}}
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn resolve_chat_id_handles_numeric_group_conversation_type() {
        let data = serde_json::json!({
            "conversationType": 2,
            "conversationId": "cid-group",
        });
        let chat_id = DingTalkChannel::resolve_chat_id(&data, "staff-1");
        assert_eq!(chat_id, "cid-group");
    }

    #[test]
    fn extract_text_content_prefers_nested_text_content() {
        let data = serde_json::json!({
            "text": {"content": "  你好，世界  "},
            "content": "fallback",
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("你好，世界")
        );
    }

    #[test]
    fn extract_text_content_supports_json_encoded_text_string() {
        let data = serde_json::json!({
            "text": "{\"content\":\"中文消息\"}"
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("中文消息")
        );
    }

    #[test]
    fn extract_text_content_falls_back_to_content_and_markdown() {
        let direct = serde_json::json!({
            "content": "  direct payload  "
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&direct).as_deref(),
            Some("direct payload")
        );

        let markdown = serde_json::json!({
            "markdown": {"text": "  markdown body  "}
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&markdown).as_deref(),
            Some("markdown body")
        );
    }

    #[test]
    fn extract_text_content_supports_rich_text_payload() {
        let data = serde_json::json!({
            "richText": [
                {"text": "现在"},
                {"content": "呢？"}
            ]
        });
        assert_eq!(
            DingTalkChannel::extract_text_content(&data).as_deref(),
            Some("现在 呢？")
        );
    }

    #[test]
    fn extract_text_content_bounds_rich_text_recursion_depth() {
        let mut deep = serde_json::json!({"text": "deep-content"});
        for _ in 0..24 {
            deep = serde_json::json!({"children": [deep]});
        }
        let data = serde_json::json!({"richText": deep});

        assert_eq!(DingTalkChannel::extract_text_content(&data), None);
    }

    #[test]
    fn extract_inbound_payload_picture_download_code() {
        let data = serde_json::json!({
            "msgtype": "picture",
            "content": {
                "downloadCode": "dc_123",
                "pictureUrl": "https://img.example.com/a.png"
            }
        });

        let extracted = DingTalkChannel::extract_inbound_payload(&data);
        assert_eq!(extracted.msg_type, "picture");
        assert_eq!(extracted.download_codes, vec!["dc_123".to_string()]);
        assert_eq!(
            extracted.picture_urls,
            vec!["https://img.example.com/a.png".to_string()]
        );
    }

    #[test]
    fn extract_inbound_payload_richtext_picture_and_text() {
        let data = serde_json::json!({
            "msgtype": "richText",
            "content": {
                "richText": [
                    {"text": "请看这张图"},
                    {
                        "type": "picture",
                        "downloadCode": "dc_456",
                        "pictureUrl": "https://img.example.com/b.jpg"
                    }
                ]
            }
        });

        let extracted = DingTalkChannel::extract_inbound_payload(&data);
        assert_eq!(extracted.msg_type, "richText");
        assert_eq!(extracted.text.as_deref(), Some("请看这张图"));
        assert_eq!(extracted.download_codes, vec!["dc_456".to_string()]);
        assert_eq!(
            extracted.picture_urls,
            vec!["https://img.example.com/b.jpg".to_string()]
        );
    }

    #[test]
    fn compose_channel_content_images_only() {
        let paths = vec![PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.jpg")];

        let content = DingTalkChannel::compose_channel_content(&paths, None, None).unwrap();
        assert!(content.contains("[IMAGE:/tmp/a.png]"));
        assert!(content.contains("[IMAGE:/tmp/b.jpg]"));
        assert!(content.ends_with(DINGTALK_IMAGE_DEFAULT_PROMPT));
    }

    #[test]
    fn compose_channel_content_images_with_caption() {
        let paths = vec![PathBuf::from("/tmp/a.png")];

        let content =
            DingTalkChannel::compose_channel_content(&paths, None, Some("请描述细节")).unwrap();
        assert!(content.contains("[IMAGE:/tmp/a.png]"));
        assert!(content.ends_with("请描述细节"));
    }

    #[test]
    fn download_image_extension_from_mime() {
        assert_eq!(
            DingTalkChannel::image_extension_from_content_type("image/png"),
            "png"
        );
        assert_eq!(
            DingTalkChannel::image_extension_from_content_type("image/jpeg; charset=utf-8"),
            "jpg"
        );
        assert_eq!(
            DingTalkChannel::image_extension_from_content_type("image/webp"),
            "webp"
        );
        assert_eq!(
            DingTalkChannel::image_extension_from_content_type("application/octet-stream"),
            "jpg"
        );
    }

    #[test]
    fn should_ack_stream_frame_for_event_callback_and_system() {
        assert!(DingTalkChannel::should_ack_stream_frame("SYSTEM"));
        assert!(DingTalkChannel::should_ack_stream_frame("EVENT"));
        assert!(DingTalkChannel::should_ack_stream_frame("CALLBACK"));
        assert!(!DingTalkChannel::should_ack_stream_frame("UNKNOWN"));
    }

    #[test]
    fn build_ack_frame_includes_message_id() {
        let frame = serde_json::json!({
            "headers": {
                "messageId": "msg-123"
            }
        });

        let Message::Text(raw) = DingTalkChannel::build_ack_frame(&frame) else {
            panic!("ack frame should be text");
        };

        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid ack json");
        assert_eq!(parsed["headers"]["messageId"], "msg-123");
        assert_eq!(parsed["code"], 200);
    }

    #[tokio::test]
    async fn resolve_download_url_calls_message_files_download() {
        let mock_server = MockServer::start().await;
        let expected_url = format!("{}/files/image.png", mock_server.uri());

        Mock::given(method("POST"))
            .and(path("/v1.0/robot/messageFiles/download"))
            .and(body_json(serde_json::json!({
                "downloadCode": "dc_test",
                "robotCode": "app_key"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "downloadUrl": expected_url
            })))
            .mount(&mock_server)
            .await;

        let channel = DingTalkChannel::new("app_key".to_string(), "secret".to_string(), vec![]);
        {
            let mut token = channel.access_token.write().await;
            *token = Some(AccessToken {
                token: "cached-token".to_string(),
                expires_at: Instant::now() + Duration::from_secs(3600),
            });
        }

        let resolved = channel
            .resolve_download_url_with_base("dc_test", &mock_server.uri())
            .await
            .expect("download url should resolve");

        assert_eq!(resolved, format!("{}/files/image.png", mock_server.uri()));
    }

    #[tokio::test]
    async fn download_image_to_workspace_persists_image_under_workspace() {
        let workspace = tempdir().expect("temp workspace");
        let mock_server = MockServer::start().await;

        let body = vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        Mock::given(method("GET"))
            .and(path("/img/cat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(body.clone()),
            )
            .mount(&mock_server)
            .await;

        let channel = DingTalkChannel::new("id".to_string(), "secret".to_string(), vec![])
            .with_workspace_dir(workspace.path().to_path_buf());

        let saved_path = channel
            .download_image_to_workspace(&format!("{}/img/cat", mock_server.uri()))
            .await
            .expect("image should download");

        let workspace_root =
            std::fs::canonicalize(workspace.path()).expect("workspace canonical path");
        assert!(saved_path.starts_with(&workspace_root));
        assert!(
            saved_path
                .to_string_lossy()
                .contains("dingtalk_files/inbound/dt_"),
            "saved path should live under dingtalk_files/inbound"
        );

        let saved_body = fs::read(&saved_path)
            .await
            .expect("saved image should be readable");
        assert_eq!(saved_body, body);
    }

    #[tokio::test]
    async fn download_image_to_workspace_rejects_non_image_content_type() {
        let workspace = tempdir().expect("temp workspace");
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/img/not-image"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("{}"),
            )
            .mount(&mock_server)
            .await;

        let channel = DingTalkChannel::new("id".to_string(), "secret".to_string(), vec![])
            .with_workspace_dir(workspace.path().to_path_buf());

        let result = channel
            .download_image_to_workspace(&format!("{}/img/not-image", mock_server.uri()))
            .await;

        assert!(result.is_err());
    }
}
