use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::QQEnvironment;
use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use ring::signature::Ed25519KeyPair;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";
const QQ_AUTH_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

fn ensure_https(url: &str) -> anyhow::Result<()> {
    let parsed =
        reqwest::Url::parse(url).map_err(|e| anyhow::anyhow!("Invalid URL '{url}': {e}"))?;
    if parsed.scheme() != "https" {
        anyhow::bail!(
            "Refusing to transmit sensitive data over non-HTTPS URL: URL scheme must be https"
        );
    }
    Ok(())
}

fn is_remote_media_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.starts_with("https://") || trimmed.starts_with("http://")
}

fn is_data_image_uri(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    lower.starts_with("data:image/") && lower.contains(";base64,")
}

fn is_image_filename(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".bmp")
        || lower.ends_with(".heic")
        || lower.ends_with(".heif")
        || lower.ends_with(".svg")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutgoingImageTarget {
    RemoteUrl(String),
    LocalPath(String),
    DataUri(String),
}

impl OutgoingImageTarget {
    fn display_target(&self) -> &str {
        match self {
            Self::RemoteUrl(url) | Self::LocalPath(url) | Self::DataUri(url) => url,
        }
    }

    fn is_inline_data(&self) -> bool {
        matches!(self, Self::DataUri(_))
    }
}

fn extract_image_marker_from_attachment(attachment: &serde_json::Value) -> Option<String> {
    let url = attachment.get("url").and_then(|u| u.as_str())?.trim();
    if url.is_empty() {
        return None;
    }

    let content_type = attachment
        .get("content_type")
        .and_then(|ct| ct.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let filename = attachment
        .get("filename")
        .and_then(|f| f.as_str())
        .unwrap_or("");
    let is_image = content_type.starts_with("image/") || is_image_filename(filename);

    if !is_image {
        return None;
    }

    Some(format!("[IMAGE:{url}]"))
}

fn parse_image_marker_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let marker = trimmed.strip_prefix("[IMAGE:")?.strip_suffix(']')?.trim();
    if marker.is_empty() {
        return None;
    }
    Some(marker)
}

fn parse_outgoing_image_target(
    candidate: &str,
    allow_extensionless_remote_url: bool,
) -> Option<OutgoingImageTarget> {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return None;
    }

    let normalized = trimmed.trim_matches(|c| matches!(c, '`' | '"' | '\''));
    let normalized = normalized.strip_prefix("file://").unwrap_or(normalized);
    if normalized.is_empty() {
        return None;
    }

    if is_data_image_uri(normalized) {
        return Some(OutgoingImageTarget::DataUri(normalized.to_string()));
    }

    if is_remote_media_url(normalized) {
        if allow_extensionless_remote_url || is_image_filename(normalized) {
            return Some(OutgoingImageTarget::RemoteUrl(normalized.to_string()));
        }
        return None;
    }

    if !is_image_filename(normalized) {
        return None;
    }

    let path = Path::new(normalized);
    if !path.is_file() {
        return None;
    }

    Some(OutgoingImageTarget::LocalPath(normalized.to_string()))
}

fn parse_outgoing_content(content: &str) -> (String, Vec<OutgoingImageTarget>) {
    let mut passthrough_lines = Vec::new();
    let mut image_targets = Vec::new();

    for line in content.lines() {
        if let Some(marker_target) = parse_image_marker_line(line) {
            if let Some(parsed) = parse_outgoing_image_target(marker_target, true) {
                image_targets.push(parsed);
                continue;
            }
        }

        if let Some(parsed) = parse_outgoing_image_target(line, false) {
            if matches!(
                parsed,
                OutgoingImageTarget::LocalPath(_) | OutgoingImageTarget::DataUri(_)
            ) {
                image_targets.push(parsed);
                continue;
            }
        }

        passthrough_lines.push(line);
    }

    (
        passthrough_lines.join("\n").trim().to_string(),
        image_targets,
    )
}

fn decode_data_image_payload(data_uri: &str) -> anyhow::Result<String> {
    let trimmed = data_uri.trim();
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

    let decoded = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| anyhow::anyhow!("invalid data URI base64 payload: {e}"))?;
    if decoded.is_empty() {
        anyhow::bail!("image payload is empty");
    }

    Ok(base64::engine::general_purpose::STANDARD.encode(decoded))
}

fn compose_message_content(payload: &serde_json::Value) -> Option<String> {
    let text = payload
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .trim();

    let image_markers: Vec<String> = payload
        .get("attachments")
        .and_then(|a| a.as_array())
        .map(|attachments| {
            attachments
                .iter()
                .filter_map(extract_image_marker_from_attachment)
                .collect()
        })
        .unwrap_or_default();

    if text.is_empty() && image_markers.is_empty() {
        return None;
    }

    if text.is_empty() {
        return Some(image_markers.join("\n"));
    }

    if image_markers.is_empty() {
        return Some(text.to_string());
    }

    Some(format!("{text}\n\n{}", image_markers.join("\n")))
}

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn build_channel_message(
    sender: &str,
    reply_target: String,
    content: String,
    msg_id: &str,
) -> ChannelMessage {
    ChannelMessage {
        id: Uuid::new_v4().to_string(),
        sender: sender.to_string(),
        reply_target,
        content,
        channel: "qq".to_string(),
        timestamp: current_unix_timestamp_secs(),
        thread_ts: (!msg_id.is_empty()).then(|| msg_id.to_string()),
    }
}

fn extract_message_id(payload: &serde_json::Value) -> &str {
    payload
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| payload.get("msg_id").and_then(Value::as_str))
        .unwrap_or("")
}

fn qq_seed_from_secret(secret: &str) -> Option<[u8; 32]> {
    let bytes = secret.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut seed = [0_u8; 32];
    for (idx, slot) in seed.iter_mut().enumerate() {
        *slot = bytes[idx % bytes.len()];
    }
    Some(seed)
}

fn qq_webhook_validation_signature(
    app_secret: &str,
    event_ts: &str,
    plain_token: &str,
) -> Option<String> {
    let seed = qq_seed_from_secret(app_secret)?;
    let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).ok()?;
    let mut payload = String::with_capacity(event_ts.len() + plain_token.len());
    payload.push_str(event_ts);
    payload.push_str(plain_token);
    Some(hex::encode(key_pair.sign(payload.as_bytes()).as_ref()))
}

fn apply_passive_reply_fields(body: &mut Map<String, Value>, msg_id: Option<&str>, msg_seq: u64) {
    if let Some(msg_id) = msg_id {
        body.insert("msg_id".to_string(), Value::String(msg_id.to_string()));
        body.insert("msg_seq".to_string(), Value::from(msg_seq));
    }
}

fn build_text_message_body(content: &str, msg_id: Option<&str>, msg_seq: u64) -> Option<Value> {
    let text = content.trim();
    if text.is_empty() {
        return None;
    }

    let mut body = Map::new();
    body.insert("content".to_string(), Value::String(text.to_string()));
    body.insert("msg_type".to_string(), Value::from(0));
    apply_passive_reply_fields(&mut body, msg_id, msg_seq);

    Some(Value::Object(body))
}

fn build_media_message_body(file_info: &str, msg_id: Option<&str>, msg_seq: u64) -> Value {
    let mut body = Map::new();
    body.insert("content".to_string(), Value::String(" ".to_string()));
    body.insert("msg_type".to_string(), Value::from(7));
    body.insert("media".to_string(), json!({ "file_info": file_info }));
    apply_passive_reply_fields(&mut body, msg_id, msg_seq);
    Value::Object(body)
}

fn resolve_send_endpoints(api_base: &str, recipient: &str) -> (String, String) {
    if let Some(group_id) = recipient.strip_prefix("group:") {
        (
            format!("{api_base}/v2/groups/{group_id}/messages"),
            format!("{api_base}/v2/groups/{group_id}/files"),
        )
    } else {
        let raw_uid = recipient.strip_prefix("user:").unwrap_or(recipient);
        let user_id: String = raw_uid
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (
            format!("{api_base}/v2/users/{user_id}/messages"),
            format!("{api_base}/v2/users/{user_id}/files"),
        )
    }
}

/// Deduplication set capacity — evict half of entries when full.
const DEDUP_CAPACITY: usize = 10_000;

/// QQ Official Bot channel — uses Tencent's official QQ Bot API with
/// OAuth2 authentication and a Discord-like WebSocket gateway protocol.
pub struct QQChannel {
    app_id: String,
    app_secret: String,
    environment: QQEnvironment,
    allowed_users: Vec<String>,
    /// Cached access token + expiry timestamp.
    token_cache: Arc<RwLock<Option<(String, u64)>>>,
    /// Message deduplication set.
    dedup: Arc<RwLock<HashSet<String>>>,
}

impl QQChannel {
    pub fn new(app_id: String, app_secret: String, allowed_users: Vec<String>) -> Self {
        Self::new_with_environment(app_id, app_secret, allowed_users, QQEnvironment::Production)
    }

    pub fn new_with_environment(
        app_id: String,
        app_secret: String,
        allowed_users: Vec<String>,
        environment: QQEnvironment,
    ) -> Self {
        Self {
            app_id,
            app_secret,
            environment,
            allowed_users,
            token_cache: Arc::new(RwLock::new(None)),
            dedup: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    fn http_client(&self) -> reqwest::Client {
        crate::config::build_runtime_proxy_client("channel.qq")
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    fn api_base(&self) -> &'static str {
        match self.environment {
            QQEnvironment::Production => QQ_API_BASE,
            QQEnvironment::Sandbox => QQ_SANDBOX_API_BASE,
        }
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    async fn parse_dispatch_message_event(
        &self,
        event_type: &str,
        payload: &serde_json::Value,
    ) -> Option<ChannelMessage> {
        match event_type {
            "C2C_MESSAGE_CREATE" => {
                let msg_id = extract_message_id(payload);
                if self.is_duplicate(msg_id).await {
                    return None;
                }

                let content = compose_message_content(payload)?;
                let author_id = payload
                    .get("author")
                    .and_then(|a| a.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let user_openid = payload
                    .get("author")
                    .and_then(|a| a.get("user_openid"))
                    .and_then(Value::as_str)
                    .unwrap_or(author_id);

                if !self.is_user_allowed(user_openid) {
                    tracing::warn!(
                        "QQ: ignoring C2C message from unauthorized user: {user_openid}"
                    );
                    return None;
                }

                let chat_id = format!("user:{user_openid}");
                Some(build_channel_message(user_openid, chat_id, content, msg_id))
            }
            "GROUP_AT_MESSAGE_CREATE" => {
                let msg_id = extract_message_id(payload);
                if self.is_duplicate(msg_id).await {
                    return None;
                }

                let content = compose_message_content(payload)?;
                let author_id = payload
                    .get("author")
                    .and_then(|a| a.get("member_openid"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                if !self.is_user_allowed(author_id) {
                    tracing::warn!(
                        "QQ: ignoring group message from unauthorized user: {author_id}"
                    );
                    return None;
                }

                let group_openid = payload
                    .get("group_openid")
                    .and_then(Value::as_str)
                    .or_else(|| payload.get("group_id").and_then(Value::as_str))
                    .unwrap_or("unknown");
                let chat_id = format!("group:{group_openid}");
                Some(build_channel_message(author_id, chat_id, content, msg_id))
            }
            _ => None,
        }
    }

    pub fn build_webhook_validation_response(
        &self,
        payload: &serde_json::Value,
    ) -> Option<serde_json::Value> {
        let op = payload
            .get("op")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if op != 13 {
            return None;
        }

        let validation = payload.get("d")?;
        let plain_token = validation
            .get("plain_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())?;
        let event_ts = validation
            .get("event_ts")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())?;

        let signature = qq_webhook_validation_signature(&self.app_secret, event_ts, plain_token)?;
        Some(json!({
            "plain_token": plain_token,
            "signature": signature
        }))
    }

    pub async fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let op = payload
            .get("op")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if op != 0 {
            return Vec::new();
        }

        let event_type = payload
            .get("t")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if event_type.is_empty() {
            return Vec::new();
        }

        let Some(dispatch_payload) = payload.get("d") else {
            return Vec::new();
        };

        self.parse_dispatch_message_event(event_type, dispatch_payload)
            .await
            .into_iter()
            .collect()
    }

    async fn post_json(
        &self,
        token: &str,
        url: &str,
        body: &Value,
        op: &str,
    ) -> anyhow::Result<()> {
        ensure_https(url)?;
        let parsed_url = reqwest::Url::parse(url)
            .map_err(|e| anyhow::anyhow!("Invalid URL '{url}' for QQ {op}: {e}"))?;

        let resp = self
            .http_client()
            .post(parsed_url)
            .header("Authorization", format!("QQBot {token}"))
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("QQ {op} failed ({status}): {sanitized}");
        }

        Ok(())
    }

    async fn upload_media_file_info(
        &self,
        token: &str,
        files_url: &str,
        media_url: &str,
    ) -> anyhow::Result<String> {
        ensure_https(files_url)?;
        ensure_https(media_url)?;
        let parsed_files_url = reqwest::Url::parse(files_url)
            .map_err(|e| anyhow::anyhow!("Invalid QQ files endpoint URL '{files_url}': {e}"))?;

        let upload_body = json!({
            "file_type": 1,
            "url": media_url,
            "srv_send_msg": false
        });

        let resp = self
            .http_client()
            .post(parsed_files_url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&upload_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("QQ upload media failed ({status}): {sanitized}");
        }

        let payload: Value = resp.json().await?;
        let file_info = payload
            .get("file_info")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("QQ upload media response missing file_info"))?;

        Ok(file_info.to_string())
    }

    async fn upload_media_file_data(
        &self,
        token: &str,
        files_url: &str,
        file_data_base64: &str,
    ) -> anyhow::Result<String> {
        ensure_https(files_url)?;
        let parsed_files_url = reqwest::Url::parse(files_url)
            .map_err(|e| anyhow::anyhow!("Invalid QQ files endpoint URL '{files_url}': {e}"))?;

        let upload_body = json!({
            "file_type": 1,
            "file_data": file_data_base64,
            "srv_send_msg": false
        });

        let resp = self
            .http_client()
            .post(parsed_files_url)
            .header("Authorization", format!("QQBot {token}"))
            .json(&upload_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("QQ upload media(file_data) failed ({status}): {sanitized}");
        }

        let payload: Value = resp.json().await?;
        let file_info = payload
            .get("file_info")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("QQ upload media(file_data) response missing file_info")
            })?;

        Ok(file_info.to_string())
    }

    /// Fetch an access token from QQ's OAuth2 endpoint.
    async fn fetch_access_token(&self) -> anyhow::Result<(String, u64)> {
        let body = json!({
            "appId": self.app_id,
            "clientSecret": self.app_secret,
        });

        let resp = self
            .http_client()
            .post(QQ_AUTH_URL)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("QQ token request failed ({status}): {sanitized}");
        }

        let data: serde_json::Value = resp.json().await?;
        let token = data
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing access_token in QQ response"))?
            .to_string();

        let expires_in = data
            .get("expires_in")
            .and_then(|e| e.as_str())
            .and_then(|e| e.parse::<u64>().ok())
            .unwrap_or(7200);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Expire 60 seconds early to avoid edge cases
        let expiry = now + expires_in.saturating_sub(60);

        Ok((token, expiry))
    }

    /// Get a valid access token, refreshing if expired.
    async fn get_token(&self) -> anyhow::Result<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        {
            let cache = self.token_cache.read().await;
            if let Some((ref token, expiry)) = *cache {
                if now < expiry {
                    return Ok(token.clone());
                }
            }
        }

        let (token, expiry) = self.fetch_access_token().await?;
        {
            let mut cache = self.token_cache.write().await;
            *cache = Some((token.clone(), expiry));
        }
        Ok(token)
    }

    /// Get the WebSocket gateway URL.
    async fn get_gateway_url(&self, token: &str) -> anyhow::Result<String> {
        let resp = self
            .http_client()
            .get(format!("{}/gateway", self.api_base()))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            let sanitized = crate::providers::sanitize_api_error(&err);
            anyhow::bail!("QQ gateway request failed ({status}): {sanitized}");
        }

        let data: serde_json::Value = resp.json().await?;
        let url = data
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing gateway URL in QQ response"))?
            .to_string();

        Ok(url)
    }

    /// Check and insert message ID for deduplication.
    async fn is_duplicate(&self, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }

        let mut dedup = self.dedup.write().await;

        if dedup.contains(msg_id) {
            return true;
        }

        // Evict oldest half when at capacity
        if dedup.len() >= DEDUP_CAPACITY {
            let to_remove: Vec<String> = dedup.iter().take(DEDUP_CAPACITY / 2).cloned().collect();
            for key in to_remove {
                dedup.remove(&key);
            }
        }

        dedup.insert(msg_id.to_string());
        false
    }
}

#[async_trait]
impl Channel for QQChannel {
    fn name(&self) -> &str {
        "qq"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let token = self.get_token().await?;
        let (message_url, files_url) = resolve_send_endpoints(self.api_base(), &message.recipient);

        let passive_msg_id = message
            .thread_ts
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut msg_seq: u64 = 1;

        let (text_content, image_urls) = parse_outgoing_content(&message.content);

        if let Some(body) = build_text_message_body(&text_content, passive_msg_id, msg_seq) {
            self.post_json(&token, &message_url, &body, "send message")
                .await?;
            if passive_msg_id.is_some() {
                msg_seq += 1;
            }
        }

        for image_target in image_urls {
            let file_info = match &image_target {
                OutgoingImageTarget::RemoteUrl(image_url) => {
                    self.upload_media_file_info(&token, &files_url, image_url)
                        .await
                }
                OutgoingImageTarget::LocalPath(path) => match tokio::fs::read(path).await {
                    Ok(bytes) => {
                        if bytes.is_empty() {
                            Err(anyhow::anyhow!("QQ local image payload is empty: {path}"))
                        } else {
                            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                            self.upload_media_file_data(&token, &files_url, &encoded)
                                .await
                        }
                    }
                    Err(e) => Err(anyhow::anyhow!("QQ local image read failed ({path}): {e}")),
                },
                OutgoingImageTarget::DataUri(data_uri) => {
                    match decode_data_image_payload(data_uri) {
                        Ok(encoded) => {
                            self.upload_media_file_data(&token, &files_url, &encoded)
                                .await
                        }
                        Err(err) => Err(err),
                    }
                }
            };

            match file_info {
                Ok(file_info) => {
                    let media_body = build_media_message_body(&file_info, passive_msg_id, msg_seq);
                    self.post_json(&token, &message_url, &media_body, "send message")
                        .await?;
                    if passive_msg_id.is_some() {
                        msg_seq += 1;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "QQ: failed to upload image target '{}': {err}",
                        if image_target.is_inline_data() {
                            "[inline image data]"
                        } else {
                            image_target.display_target()
                        }
                    );
                    let fallback_text = if image_target.is_inline_data() {
                        "Image attachment upload failed".to_string()
                    } else {
                        format!("Image: {}", image_target.display_target())
                    };
                    if let Some(body) =
                        build_text_message_body(&fallback_text, passive_msg_id, msg_seq)
                    {
                        self.post_json(&token, &message_url, &body, "send message")
                            .await?;
                        if passive_msg_id.is_some() {
                            msg_seq += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!("QQ: authenticating...");
        let token = self.get_token().await?;

        tracing::info!("QQ: fetching gateway URL...");
        let gw_url = self.get_gateway_url(&token).await?;

        tracing::info!("QQ: connecting to gateway WebSocket...");
        let (ws_stream, _) = tokio_tungstenite::connect_async(&gw_url).await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read
            .next()
            .await
            .ok_or(anyhow::anyhow!("QQ: no hello frame"))??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Send Identify (opcode 2)
        // Intents: PUBLIC_GUILD_MESSAGES (1<<30) | C2C_MESSAGE_CREATE & GROUP_AT_MESSAGE_CREATE (1<<25)
        let intents: u64 = (1 << 25) | (1 << 30);
        let identify = json!({
            "op": 2,
            "d": {
                "token": format!("QQBot {token}"),
                "intents": intents,
                "properties": {
                    "os": "linux",
                    "browser": "zeroclaw",
                    "device": "zeroclaw",
                }
            }
        });
        write
            .send(Message::Text(identify.to_string().into()))
            .await?;

        tracing::info!("QQ: connected and identified");

        let mut sequence: i64 = -1;

        // Spawn heartbeat timer
        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        let hb_interval = heartbeat_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(hb_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        loop {
            tokio::select! {
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write
                        .send(Message::Text(hb.to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Track sequence number
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);

                    match op {
                        // Server requests immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write
                                .send(Message::Text(hb.to_string().into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        // Reconnect
                        7 => {
                            tracing::warn!("QQ: received Reconnect (op 7)");
                            break;
                        }
                        // Invalid Session
                        9 => {
                            tracing::warn!("QQ: received Invalid Session (op 9)");
                            break;
                        }
                        _ => {}
                    }

                    // Only process dispatch events (op 0)
                    if op != 0 {
                        continue;
                    }

                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    let d = match event.get("d") {
                        Some(d) => d,
                        None => continue,
                    };

                    if let Some(channel_msg) =
                        self.parse_dispatch_message_event(event_type, d).await
                    {
                        if tx.send(channel_msg).await.is_err() {
                            tracing::warn!("QQ: message channel closed");
                            break;
                        }
                    }
                }
            }
        }

        anyhow::bail!("QQ WebSocket connection closed")
    }

    async fn health_check(&self) -> bool {
        self.fetch_access_token().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_name() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec![]);
        assert_eq!(ch.name(), "qq");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["*".into()]);
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["user123".into()]);
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec![]);
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[tokio::test]
    async fn test_dedup() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec![]);
        assert!(!ch.is_duplicate("msg1").await);
        assert!(ch.is_duplicate("msg1").await);
        assert!(!ch.is_duplicate("msg2").await);
    }

    #[tokio::test]
    async fn test_dedup_empty_id() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec![]);
        // Empty IDs should never be considered duplicates
        assert!(!ch.is_duplicate("").await);
        assert!(!ch.is_duplicate("").await);
    }

    #[test]
    fn test_config_serde() {
        let toml_str = r#"
app_id = "12345"
app_secret = "secret_abc"
allowed_users = ["user1"]
"#;
        let config: crate::config::schema::QQConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.app_id, "12345");
        assert_eq!(config.app_secret, "secret_abc");
        assert_eq!(config.allowed_users, vec!["user1"]);
        assert_eq!(
            config.receive_mode,
            crate::config::schema::QQReceiveMode::Webhook
        );
        assert_eq!(
            config.environment,
            crate::config::schema::QQEnvironment::Production
        );
    }

    #[test]
    fn test_resolve_send_endpoints_respects_selected_api_base() {
        let (group_messages, group_files) =
            resolve_send_endpoints(QQ_SANDBOX_API_BASE, "group:12345");
        assert_eq!(
            group_messages,
            "https://sandbox.api.sgroup.qq.com/v2/groups/12345/messages"
        );
        assert_eq!(
            group_files,
            "https://sandbox.api.sgroup.qq.com/v2/groups/12345/files"
        );

        let (user_messages, user_files) = resolve_send_endpoints(QQ_API_BASE, "user:abc_123");
        assert_eq!(
            user_messages,
            "https://api.sgroup.qq.com/v2/users/abc_123/messages"
        );
        assert_eq!(
            user_files,
            "https://api.sgroup.qq.com/v2/users/abc_123/files"
        );
    }

    #[test]
    fn test_build_webhook_validation_response() {
        let ch = QQChannel::new(
            "11111111".into(),
            "DG5g3B4j9X2KOErG".into(),
            vec!["*".into()],
        );
        let payload = json!({
            "op": 13,
            "d": {
                "plain_token": "Arq0D5A61EgUu4OxUvOp",
                "event_ts": "1725442341"
            }
        });

        let response = ch
            .build_webhook_validation_response(&payload)
            .expect("validation response expected");

        assert_eq!(response["plain_token"], "Arq0D5A61EgUu4OxUvOp");
        assert_eq!(
            response["signature"],
            "87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706"
        );
    }

    #[tokio::test]
    async fn test_parse_webhook_payload_c2c_event() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["user_open_1".into()]);
        let payload = json!({
            "op": 0,
            "t": "C2C_MESSAGE_CREATE",
            "d": {
                "id": "msg-1",
                "content": "hello webhook",
                "author": {
                    "id": "author-1",
                    "user_openid": "user_open_1"
                }
            }
        });

        let messages = ch.parse_webhook_payload(&payload).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender, "user_open_1");
        assert_eq!(messages[0].reply_target, "user:user_open_1");
        assert_eq!(messages[0].thread_ts.as_deref(), Some("msg-1"));
    }

    #[tokio::test]
    async fn test_parse_webhook_payload_deduplicates_by_message_id() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["user_open_1".into()]);
        let payload = json!({
            "op": 0,
            "t": "C2C_MESSAGE_CREATE",
            "d": {
                "id": "msg-dup",
                "content": "hello webhook",
                "author": {
                    "id": "author-1",
                    "user_openid": "user_open_1"
                }
            }
        });

        let first = ch.parse_webhook_payload(&payload).await;
        let second = ch.parse_webhook_payload(&payload).await;
        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn test_parse_webhook_payload_supports_msg_id_fallback_for_passive_reply() {
        let ch = QQChannel::new("id".into(), "secret".into(), vec!["user_open_1".into()]);
        let payload = json!({
            "op": 0,
            "t": "C2C_MESSAGE_CREATE",
            "d": {
                "msg_id": "msg-fallback-1",
                "content": "hello webhook",
                "author": {
                    "id": "author-1",
                    "user_openid": "user_open_1"
                }
            }
        });

        let messages = ch.parse_webhook_payload(&payload).await;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].thread_ts.as_deref(), Some("msg-fallback-1"));
    }

    #[test]
    fn test_compose_message_content_text_only() {
        let payload = json!({
            "content": "  hello world  "
        });

        assert_eq!(
            compose_message_content(&payload),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_compose_message_content_attachment_only_image() {
        let payload = json!({
            "content": "   ",
            "attachments": [
                {
                    "content_type": "image/jpg",
                    "url": "https://cdn.example.com/a.jpg"
                }
            ]
        });

        assert_eq!(
            compose_message_content(&payload),
            Some("[IMAGE:https://cdn.example.com/a.jpg]".to_string())
        );
    }

    #[test]
    fn test_compose_message_content_text_and_image_attachments() {
        let payload = json!({
            "content": "Here is an image",
            "attachments": [
                {
                    "content_type": "image/png",
                    "url": "https://cdn.example.com/a.png"
                },
                {
                    "filename": "b.jpeg",
                    "url": "https://cdn.example.com/b.jpeg"
                }
            ]
        });

        assert_eq!(
            compose_message_content(&payload),
            Some(
                "Here is an image\n\n[IMAGE:https://cdn.example.com/a.png]\n[IMAGE:https://cdn.example.com/b.jpeg]"
                    .to_string()
            )
        );
    }

    #[test]
    fn test_compose_message_content_ignores_non_image_attachments() {
        let payload = json!({
            "content": "text",
            "attachments": [
                {
                    "content_type": "application/pdf",
                    "url": "https://cdn.example.com/a.pdf"
                }
            ]
        });

        assert_eq!(compose_message_content(&payload), Some("text".to_string()));
    }

    #[test]
    fn test_compose_message_content_drops_empty_without_valid_attachments() {
        let payload = json!({
            "content": "   ",
            "attachments": [
                {
                    "content_type": "application/pdf",
                    "url": "https://cdn.example.com/a.pdf"
                },
                {
                    "content_type": "image/png",
                    "url": "   "
                }
            ]
        });

        assert_eq!(compose_message_content(&payload), None);
    }

    #[test]
    fn test_parse_outgoing_content_extracts_remote_image_markers() {
        let input = "hello\n[IMAGE:https://cdn.example.com/a.png]\n[IMAGE:http://cdn.example.com/b.jpg]\nbye";
        let (text, images) = parse_outgoing_content(input);

        assert_eq!(text, "hello\nbye");
        assert_eq!(
            images,
            vec![
                OutgoingImageTarget::RemoteUrl("https://cdn.example.com/a.png".to_string()),
                OutgoingImageTarget::RemoteUrl("http://cdn.example.com/b.jpg".to_string())
            ]
        );
    }

    #[test]
    fn test_parse_outgoing_content_accepts_marker_remote_url_without_extension() {
        let input = "hello\n[IMAGE:https://multimedia.nt.qq.com.cn/download?appid=1406]\nbye";
        let (text, images) = parse_outgoing_content(input);

        assert_eq!(text, "hello\nbye");
        assert_eq!(
            images,
            vec![OutgoingImageTarget::RemoteUrl(
                "https://multimedia.nt.qq.com.cn/download?appid=1406".to_string()
            )]
        );
    }

    #[test]
    fn test_parse_outgoing_content_keeps_non_remote_image_marker_as_text() {
        let input = "[IMAGE:/tmp/a.png]\nhello";
        let (text, images) = parse_outgoing_content(input);

        assert_eq!(text, "[IMAGE:/tmp/a.png]\nhello");
        assert!(images.is_empty());
    }

    #[test]
    fn test_parse_outgoing_content_extracts_existing_local_path_lines() {
        let temp = tempfile::tempdir().expect("temp dir");
        let local_path = temp.path().join("capture.png");
        std::fs::write(&local_path, b"png-bytes").expect("write local image");

        let input = format!("done\n{}\nnext", local_path.display());
        let (text, images) = parse_outgoing_content(&input);

        assert_eq!(text, "done\nnext");
        assert_eq!(
            images,
            vec![OutgoingImageTarget::LocalPath(
                local_path.display().to_string()
            )]
        );
    }

    #[test]
    fn test_parse_outgoing_content_extracts_data_uri_markers() {
        let input = "hello\n[IMAGE:data:image/png;base64,aGVsbG8=]\nbye";
        let (text, images) = parse_outgoing_content(input);

        assert_eq!(text, "hello\nbye");
        assert_eq!(
            images,
            vec![OutgoingImageTarget::DataUri(
                "data:image/png;base64,aGVsbG8=".to_string()
            )]
        );
    }

    #[test]
    fn test_build_text_message_body_with_passive_fields() {
        let body = build_text_message_body("hello", Some("msg-123"), 2).expect("text body");
        assert_eq!(
            body,
            json!({
                "content": "hello",
                "msg_type": 0,
                "msg_id": "msg-123",
                "msg_seq": 2
            })
        );
    }

    #[test]
    fn test_build_media_message_body_with_passive_fields() {
        let body = build_media_message_body("file-info-abc", Some("msg-123"), 3);
        assert_eq!(
            body,
            json!({
                "content": " ",
                "msg_type": 7,
                "media": {
                    "file_info": "file-info-abc"
                },
                "msg_id": "msg-123",
                "msg_seq": 3
            })
        );
    }
}
