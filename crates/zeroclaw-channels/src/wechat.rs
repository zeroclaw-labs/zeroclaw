use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info, warn};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const LONG_POLL_TIMEOUT_MS: u64 = 35_000;
const API_TIMEOUT_MS: u64 = 15_000;
const SESSION_EXPIRED_ERRCODE: i32 = -14;
const ITEM_TEXT: i32 = 1;
const ITEM_VOICE: i32 = 3;

// ── Credential loading ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AccountData {
    token: Option<String>,
    base_url: Option<String>,
}

fn state_dir() -> PathBuf {
    // Mirrors weixin-agent-rs resolution order:
    // $OPENCLAW_STATE_DIR → $CLAWDBOT_STATE_DIR → ~/.openclaw
    if let Some(v) = std::env::var("OPENCLAW_STATE_DIR").ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var("CLAWDBOT_STATE_DIR").ok().filter(|s| !s.is_empty()) {
        return PathBuf::from(v);
    }
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".openclaw"))
        .unwrap_or_else(|| PathBuf::from(".openclaw"))
}

fn account_path(account_id: &str) -> PathBuf {
    state_dir()
        .join("openclaw-weixin")
        .join("accounts")
        .join(format!("{account_id}.json"))
}

fn sync_buf_path(account_id: &str) -> PathBuf {
    state_dir()
        .join("openclaw-weixin")
        .join("sync_buf")
        .join(account_id)
}

fn load_sync_buf(account_id: &str) -> String {
    std::fs::read_to_string(sync_buf_path(account_id)).unwrap_or_default()
}

fn save_sync_buf(account_id: &str, buf: &str) {
    let path = sync_buf_path(account_id);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, buf);
}

// ── API types ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct GetUpdatesResp {
    ret: Option<i32>,
    errcode: Option<i32>,
    errmsg: Option<String>,
    msgs: Option<Vec<WeixinMessage>>,
    get_updates_buf: Option<String>,
    longpolling_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
struct WeixinMessage {
    message_id: Option<u64>,
    from_user_id: Option<String>,
    #[allow(dead_code)]
    to_user_id: Option<String>,
    create_time_ms: Option<u64>,
    item_list: Option<Vec<MessageItem>>,
    #[allow(dead_code)]
    context_token: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct MessageItem {
    #[serde(rename = "type")]
    item_type: Option<i32>,
    text_item: Option<TextItem>,
    voice_item: Option<VoiceItem>,
}

#[derive(Debug, Deserialize, Default)]
struct TextItem {
    text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct VoiceItem {
    text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct GetConfigResp {
    typing_ticket: Option<String>,
}

// ── Channel struct ────────────────────────────────────────────────

pub struct WeChatChannel {
    bot_token: String,
    base_url: String,
    account_id: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

impl WeChatChannel {
    pub fn new(account_id: String, allowed_users: Vec<String>) -> anyhow::Result<Self> {
        let path = account_path(&account_id);
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            anyhow::anyhow!(
                "WeChat credentials not found at {}: {}. Run `wechat-agent login` first.",
                path.display(),
                e
            )
        })?;
        let data: AccountData = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("Failed to parse WeChat credentials: {e}"))?;
        let bot_token = data
            .token
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("WeChat credentials missing token"))?;
        let base_url = data
            .base_url
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        Ok(Self {
            bot_token,
            base_url,
            account_id,
            allowed_users,
            // Plain client — iLink Bot API doesn't require proxy routing.
            client: reqwest::Client::new(),
        })
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/{endpoint}", self.base_url.trim_end_matches('/'))
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(
            "AuthorizationType",
            HeaderValue::from_static("ilink_bot_token"),
        );
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {}", self.bot_token)) {
            h.insert(AUTHORIZATION, v);
        }
        h
    }

    async fn get_updates(&self, sync_buf: &str, timeout_ms: u64) -> anyhow::Result<GetUpdatesResp> {
        let body = serde_json::json!({
            "get_updates_buf": sync_buf,
            "base_info": { "channel_version": env!("CARGO_PKG_VERSION") },
        });
        let resp = self
            .client
            .post(self.api_url("ilink/bot/getupdates"))
            .headers(self.auth_headers())
            .timeout(Duration::from_millis(timeout_ms))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("getupdates HTTP {status}: {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn send_text(&self, to_user_id: &str, context_token: Option<&str>, text: &str) -> anyhow::Result<()> {
        let msg = serde_json::json!({
            "to_user_id": to_user_id,
            "context_token": context_token,
            "message_type": 2,
            "item_list": [{
                "type": 1,
                "text_item": { "text": text },
            }],
        });
        let body = serde_json::json!({
            "msg": msg,
            "base_info": { "channel_version": env!("CARGO_PKG_VERSION") },
        });
        let resp = self
            .client
            .post(self.api_url("ilink/bot/sendmessage"))
            .headers(self.auth_headers())
            .timeout(Duration::from_millis(API_TIMEOUT_MS))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("sendmessage HTTP {status}: {err}");
        }
        Ok(())
    }

    async fn get_typing_ticket(&self, user_id: &str, context_token: Option<&str>) -> Option<String> {
        let body = serde_json::json!({
            "ilink_user_id": user_id,
            "context_token": context_token,
            "base_info": { "channel_version": env!("CARGO_PKG_VERSION") },
        });
        let resp = self
            .client
            .post(self.api_url("ilink/bot/getconfig"))
            .headers(self.auth_headers())
            .timeout(Duration::from_millis(API_TIMEOUT_MS))
            .json(&body)
            .send()
            .await
            .ok()?;
        let parsed: GetConfigResp = resp.json().await.ok()?;
        parsed.typing_ticket.filter(|t| !t.is_empty())
    }

    async fn send_typing(&self, user_id: &str, ticket: &str, status: i32) -> anyhow::Result<()> {
        let body = serde_json::json!({
            "ilink_user_id": user_id,
            "typing_ticket": ticket,
            "status": status,
            "base_info": { "channel_version": env!("CARGO_PKG_VERSION") },
        });
        let resp = self
            .client
            .post(self.api_url("ilink/bot/sendtyping"))
            .headers(self.auth_headers())
            .timeout(Duration::from_millis(API_TIMEOUT_MS))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("sendtyping HTTP {status}: {err}");
        }
        Ok(())
    }
}

fn extract_text(msg: &WeixinMessage) -> Option<String> {
    let items = msg.item_list.as_deref()?;
    for item in items {
        match item.item_type {
            Some(t) if t == ITEM_TEXT => {
                if let Some(text) = item.text_item.as_ref().and_then(|i| i.text.as_deref()) {
                    return Some(text.to_owned());
                }
            }
            Some(t) if t == ITEM_VOICE => {
                // voice messages carry ASR transcript in text field
                if let Some(text) = item.voice_item.as_ref().and_then(|i| i.text.as_deref()).filter(|t| !t.is_empty()) {
                    return Some(text.to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

#[async_trait]
impl Channel for WeChatChannel {
    fn name(&self) -> &str {
        "wechat"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.send_text(&message.recipient, None, &message.content).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        info!("WeChat iLink Bot channel starting (account={})", self.account_id);
        let mut sync_buf = load_sync_buf(&self.account_id);
        let mut next_timeout = LONG_POLL_TIMEOUT_MS;

        loop {
            let resp = match self.get_updates(&sync_buf, next_timeout).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("WeChat getupdates error (retrying): {e}");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            if let Some(t) = resp.longpolling_timeout_ms.filter(|&t| t > 0) {
                next_timeout = t;
            }

            let ret = resp.ret.unwrap_or(0);
            let errcode = resp.errcode.unwrap_or(0);
            if ret != 0 || errcode != 0 {
                let code = if errcode != 0 { errcode } else { ret };
                if code == SESSION_EXPIRED_ERRCODE {
                    anyhow::bail!(
                        "WeChat session expired (errcode -14). Run `wechat-agent login` again."
                    );
                }
                warn!(
                    "WeChat API error ret={ret} errcode={errcode} msg={:?} (retrying)",
                    resp.errmsg
                );
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            if let Some(new_buf) = resp.get_updates_buf.filter(|s| !s.is_empty()) {
                save_sync_buf(&self.account_id, &new_buf);
                sync_buf = new_buf;
            }

            for msg in resp.msgs.unwrap_or_default() {
                let from = match &msg.from_user_id {
                    Some(u) => u.clone(),
                    None => continue,
                };
                if !self.is_allowed(&from) {
                    debug!("WeChat: message from {} rejected (not in allowed_users)", from);
                    continue;
                }
                let text = match extract_text(&msg) {
                    Some(t) if !t.is_empty() => t,
                    _ => continue,
                };
                let id = msg
                    .message_id
                    .map(|id| id.to_string())
                    .unwrap_or_default();
                let timestamp = msg
                    .create_time_ms
                    .map(|ms| ms / 1000)
                    .unwrap_or(0);

                let channel_msg = ChannelMessage {
                    id,
                    sender: from.clone(),
                    reply_target: from,
                    content: text,
                    channel: "wechat".to_string(),
                    timestamp,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: vec![],
                };
                if tx.send(channel_msg).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        if let Some(ticket) = self.get_typing_ticket(recipient, None).await {
            self.send_typing(recipient, &ticket, 1).await?;
        }
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        if let Some(ticket) = self.get_typing_ticket(recipient, None).await {
            self.send_typing(recipient, &ticket, 2).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = WeChatChannel {
            bot_token: "t".into(),
            base_url: "u".into(),
            account_id: "a".into(),
            allowed_users: vec![],
            client: reqwest::Client::new(),
        };
        assert_eq!(ch.name(), "wechat");
    }

    #[test]
    fn test_extract_text_text_item() {
        let msg = WeixinMessage {
            item_list: Some(vec![MessageItem {
                item_type: Some(ITEM_TEXT),
                text_item: Some(TextItem { text: Some("hello".to_string()) }),
                voice_item: None,
            }]),
            ..Default::default()
        };
        assert_eq!(extract_text(&msg), Some("hello".to_string()));
    }

    #[test]
    fn test_extract_text_voice_item() {
        let msg = WeixinMessage {
            item_list: Some(vec![MessageItem {
                item_type: Some(ITEM_VOICE),
                text_item: None,
                voice_item: Some(VoiceItem { text: Some("voice text".to_string()) }),
            }]),
            ..Default::default()
        };
        assert_eq!(extract_text(&msg), Some("voice text".to_string()));
    }

    #[test]
    fn test_extract_text_empty_voice() {
        let msg = WeixinMessage {
            item_list: Some(vec![MessageItem {
                item_type: Some(ITEM_VOICE),
                text_item: None,
                voice_item: Some(VoiceItem { text: Some("".to_string()) }),
            }]),
            ..Default::default()
        };
        assert_eq!(extract_text(&msg), None);
    }

    #[test]
    fn test_extract_text_no_items() {
        let msg = WeixinMessage::default();
        assert_eq!(extract_text(&msg), None);
    }

    #[test]
    fn test_is_allowed_wildcard() {
        let ch = WeChatChannel {
            bot_token: "t".into(),
            base_url: "u".into(),
            account_id: "a".into(),
            allowed_users: vec!["*".into()],
            client: reqwest::Client::new(),
        };
        assert!(ch.is_allowed("anyone"));
    }

    #[test]
    fn test_is_allowed_specific() {
        let ch = WeChatChannel {
            bot_token: "t".into(),
            base_url: "u".into(),
            account_id: "a".into(),
            allowed_users: vec!["user1".into()],
            client: reqwest::Client::new(),
        };
        assert!(ch.is_allowed("user1"));
        assert!(!ch.is_allowed("user2"));
    }

    #[test]
    fn test_is_denied_empty() {
        let ch = WeChatChannel {
            bot_token: "t".into(),
            base_url: "u".into(),
            account_id: "a".into(),
            allowed_users: vec![],
            client: reqwest::Client::new(),
        };
        assert!(!ch.is_allowed("anyone"));
    }
}
