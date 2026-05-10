use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

use crate::file::download_image_as_base64;

const WUKONGIM_RPC_VERSION: &str = "2.0";
const PING_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

pub struct WkMessageType;
impl WkMessageType {
    pub const TEXT: u32 = 1;
    pub const IMAGE: u32 = 2;
    pub const MARKDOWN: u32 = 14;
    pub const INTERACTIVE_CARD: u32 = 20;
    pub const INTERACTIVE_RESPONSE: u32 = 21;
    pub const CMD: u32 = 99;
}

pub struct WkChannelType;
impl WkChannelType {
    pub const PERSONAL: u8 = 1;
    pub const GROUP: u8 = 2;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: String,
    pub method: String,
    pub id: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "R: DeserializeOwned"))]
pub struct JsonRpcResponse<R> {
    pub jsonrpc: String,
    pub id: Option<String>,
    pub result: Option<R>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "P: DeserializeOwned"))]
pub struct JsonRpcNotification<P> {
    pub jsonrpc: String,
    pub method: String,
    pub params: P,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Header {
    #[serde(rename = "noPersist", skip_serializing_if = "Option::is_none")]
    pub no_persist: Option<bool>,
    #[serde(rename = "redDot", skip_serializing_if = "Option::is_none")]
    pub red_dot: Option<bool>,
    #[serde(rename = "syncOnce", skip_serializing_if = "Option::is_none")]
    pub sync_once: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dup: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectParams {
    pub uid: String,
    pub token: String,
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "deviceFlag")]
    pub device_flag: i32, // 0: App, 1: Web, 2: Sys
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendParams {
    #[serde(rename = "fromUid", skip_serializing_if = "Option::is_none")]
    pub from_uid: Option<String>,
    #[serde(rename = "clientMsgNo")]
    pub client_msg_no: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: String, // Base64 encoded payload
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<Header>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<u32>,
    #[serde(rename = "msgKey", skip_serializing_if = "Option::is_none")]
    pub msg_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire: Option<u32>,
    #[serde(rename = "streamNo", skip_serializing_if = "Option::is_none")]
    pub stream_no: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecvNotificationParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
    #[serde(rename = "fromUid")]
    pub from_uid: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelType")]
    pub channel_type: u8,
    pub payload: String, // Received as Base64 string
    pub timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecvAckParams {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "messageSeq")]
    pub message_seq: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub timeout_secs: u64,
    pub title: String,
    pub body: WkApprovalBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkAction {
    pub text: String,
    pub value: String,
    pub style: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalBody {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalAction {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub action: String,
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMsg,
>;

#[derive(Clone)]
pub struct WuKongIMChannel {
    ws_url: String,
    uid: String,
    token: String,
    device_id: String,
    device_flag: i32,
    allowed_users: Vec<String>,
    /// Pending responses: id -> tx
    pending_responses:
        Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    /// Pending approvals: id -> tx
    pending_approvals: Arc<
        RwLock<
            HashMap<
                String,
                tokio::sync::oneshot::Sender<zeroclaw_api::channel::ChannelApprovalResponse>,
            >,
        >,
    >,
    approval_timeout_secs: u64,
    /// Outbound WS sink
    ws_sink: Arc<RwLock<Option<WsSink>>>,
    /// When true, only respond to messages that @-mention the bot in groups.
    mention_only: bool,
}

impl WuKongIMChannel {
    pub fn from_config(config: &zeroclaw_config::schema::WuKongIMConfig) -> Self {
        Self {
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: config.device_id.clone(),
            device_flag: config.device_flag,
            allowed_users: config.allowed_users.clone(),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            approval_timeout_secs: config.approval_timeout_secs,
            ws_sink: Arc::new(RwLock::new(None)),
            mention_only: config.mention_only,
        }
    }

    fn is_user_allowed(&self, uid: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == uid)
    }

    fn parse_recipient(&self, recipient: &str) -> (String, u8) {
        if let Some(pos) = recipient.find(':') {
            let (t_str, id_str) = recipient.split_at(pos);
            let id_str = &id_str[1..]; // skip the colon
            let t = t_str.parse::<u8>().unwrap_or(WkChannelType::PERSONAL);
            (id_str.to_string(), t)
        } else {
            (recipient.to_string(), WkChannelType::PERSONAL)
        }
    }

    async fn send_rpc<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> anyhow::Result<R> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = Uuid::new_v4().to_string();

        let req = JsonRpcRequest {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: method.to_string(),
            id: id.clone(),
            params,
        };

        {
            let mut pending = self.pending_responses.write().await;
            pending.insert(id.clone(), tx);
        }

        let msg = serde_json::to_string(&req)?;
        {
            let mut sink_guard = self.ws_sink.write().await;
            if let Some(ref mut sink) = *sink_guard {
                tracing::info!("WuKongIM: sending RPC {} (id {}): {}", method, id, msg);
                sink.send(WsMsg::Text(msg.into())).await?;
            } else {
                anyhow::bail!("WuKongIM: WebSocket not connected");
            }
        }

        let resp_val = match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(result) => result?,
            Err(e) => {
                let mut pending = self.pending_responses.write().await;
                pending.remove(&id);
                anyhow::bail!("WuKongIM RPC timeout ({}): {}", method, e);
            }
        };
        let resp: JsonRpcResponse<R> = serde_json::from_value(resp_val)?;

        if let Some(err) = resp.error {
            tracing::error!(
                "WuKongIM RPC error ({}): {} (code {})",
                method,
                err.message,
                err.code
            );
            anyhow::bail!("WuKongIM RPC error: {} (code {})", err.message, err.code);
        }

        resp.result
            .ok_or_else(|| anyhow::anyhow!("WuKongIM RPC: missing result"))
    }

    async fn send_ack(&self, message_id: String, message_seq: u32) -> anyhow::Result<()> {
        let req = JsonRpcNotification {
            jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
            method: "recvack".to_string(),
            params: RecvAckParams {
                message_id,
                message_seq,
            },
        };

        let msg = serde_json::to_string(&req)?;
        let mut sink_guard = self.ws_sink.write().await;
        if let Some(ref mut sink) = *sink_guard {
            sink.send(WsMsg::Text(msg.into())).await?;
        }
        Ok(())
    }

    fn build_approval_card(
        &self,
        approval_id: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
        timeout_secs: u64,
    ) -> WkApprovalCard {
        // Human-readable summarization for cron_add or other tools
        let (title, content) = if request.tool_name == "cron_add" {
            let mut summary = request.arguments_summary.clone();
            // Basic cleanup of common patterns in cron_add summary
            summary = summary
                .replace("job_type: agent, ", "任务类型: 智能体, ")
                .replace("job_type: shell, ", "任务类型: 脚本, ")
                .replace("name: ", "任务名称: ")
                .replace("prompt: ", "提示词: ")
                .replace("command: ", "执行命令: ")
                .replace("schedule: ", "\n执行计划: ");

            // Try to extract a prettier time from the "at" JSON if possible
            let mut time_info = summary
                .split("\n执行计划: ")
                .last()
                .unwrap_or("按计划执行")
                .to_string();
            if time_info.contains("\"at\":") {
                // Simple heuristic to pull out the timestamp: "at":"2026-04-30T14:39:46Z"
                if let Some(start) = time_info.find("\"at\":\"") {
                    let rest = &time_info[start + 6..];
                    if let Some(end) = rest.find('"') {
                        time_info = rest[..end]
                            .to_string()
                            .replace('T', " ")
                            .replace('Z', " (UTC)");
                    }
                }
            }

            (
                "📋 任务执行审批",
                format!(
                    "1. **执行的是什么**\n添加定时任务: **{}**\n\n2. **执行的时间相关信息**\n{}\n\n3. **执行内容的总结**\n{}",
                    request.tool_name, time_info, summary
                ),
            )
        } else {
            (
                "📋 任务执行审批",
                format!(
                    "🔧 智能体请求执行: **{}**\n\n**执行内容总结**:\n{}",
                    request.tool_name, request.arguments_summary
                ),
            )
        };

        WkApprovalCard {
            msg_type: WkMessageType::INTERACTIVE_CARD,
            approval_id: approval_id.to_string(),
            timeout_secs,
            title: title.to_string(),
            body: WkApprovalBody {
                content: content.to_string(),
            },
            actions: Some(vec![
                WkAction {
                    text: "同意".to_string(),
                    value: "approve".to_string(),
                    style: "primary".to_string(),
                },
                WkAction {
                    text: "拒绝".to_string(),
                    value: "deny".to_string(),
                    style: "danger".to_string(),
                },
            ]),
        }
    }
}

#[async_trait]
impl Channel for WuKongIMChannel {
    fn name(&self) -> &str {
        "wukongim"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let content = match message.content.as_str() {
            "ERR:context_window_exceeded" => "⚠️ 模型服务暂时遇到问题，请稍后重试。",
            other => other,
        };
        tracing::debug!(
            "WuKongIM: sending message to {}: {}",
            message.recipient,
            content
        );
        let payload_obj = serde_json::json!({
            "type": 1,
            "content": content,
        });

        let payload_json = serde_json::to_string(&payload_obj)?;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload_json);

        // Parse channel_type from recipient if formatted as "type:id"
        let (channel_id, channel_type) = self.parse_recipient(&message.recipient);

        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: payload_b64,
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };

        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let ws_url = &self.ws_url;
        tracing::info!("WuKongIM: connecting to {}", ws_url);

        let (ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await?;
        let (write, mut read) = ws_stream.split();

        {
            let mut sink_guard = self.ws_sink.write().await;
            *sink_guard = Some(write);
        }

        // 1. Connect — send the request directly and read the response from the stream.
        // We CANNOT use send_rpc() here because send_rpc() blocks waiting for a response
        // on a oneshot channel, but the read loop below hasn't started yet, so nobody
        // would consume incoming frames → the server's connack sits unread → 10s timeout.
        {
            let connect_id = Uuid::new_v4().to_string();
            let connect_req = JsonRpcRequest {
                jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                method: "connect".to_string(),
                id: connect_id.clone(),
                params: ConnectParams {
                    uid: self.uid.clone(),
                    token: self.token.clone(),
                    device_id: self.device_id.clone(),
                    device_flag: self.device_flag, // 0: App, 1: Web, 2: Sys
                    version: Some(2),
                },
            };
            let msg = serde_json::to_string(&connect_req)?;
            {
                let mut sink_guard = self.ws_sink.write().await;
                if let Some(ref mut sink) = *sink_guard {
                    sink.send(WsMsg::Text(msg.into())).await?;
                }
            }
            // Read the connack directly from the stream (before the main loop starts).
            let connack = tokio::time::timeout(Duration::from_secs(15), read.next())
                .await
                .map_err(|_| anyhow::anyhow!("WuKongIM: connect timeout (no connack within 15s)"))?
                .ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed during connect"))??;
            if let WsMsg::Text(text) = connack {
                let val: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(err) = val.get("error").filter(|e| !e.is_null()) {
                    anyhow::bail!("WuKongIM: connect rejected: {}", err);
                }
                tracing::debug!("WuKongIM: connack received: {}", text);
            }
        }
        tracing::info!("WuKongIM: connected as {}", self.uid);

        let mut hb_interval = tokio::time::interval(PING_INTERVAL);
        let mut last_activity = Instant::now();

        loop {
            tokio::select! {
                _ = hb_interval.tick() => {
                    if last_activity.elapsed() > HEARTBEAT_TIMEOUT {
                        anyhow::bail!("WuKongIM: heartbeat timeout");
                    }
                    // Send ping
                    let req = JsonRpcRequest {
                        jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                        method: "ping".to_string(),
                        id: Uuid::new_v4().to_string(),
                        params: serde_json::json!({}),
                    };
                    if let Ok(msg) = serde_json::to_string(&req) {
                        let mut sink_guard = self.ws_sink.write().await;
                        if let Some(ref mut sink) = *sink_guard {
                            let _ = sink.send(WsMsg::Text(msg.into())).await;
                        }
                    }
                }
                msg = read.next() => {
                    let msg = msg.ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed"))??;
                    last_activity = Instant::now();

                    if let WsMsg::Text(text) = msg {
                        tracing::trace!("WuKongIM: incoming raw text: {}", text);
                        let val: serde_json::Value = serde_json::from_str(&text)?;

                        // Handle pong (server responds with {"method": "pong"}, no id)
                        if val.get("method").and_then(|m| m.as_str()) == Some("pong") {
                            tracing::debug!("WuKongIM: pong received");
                            continue;
                        }

                        // Handle Response (matched by id)
                        let msg_id = val.get("id").and_then(|i| {
                            if i.is_string() {
                                i.as_str().map(|s| s.to_string())
                            } else if i.is_number() {
                                Some(i.to_string())
                            } else {
                                None
                            }
                        });

                        if let Some(id) = msg_id {
                            let mut pending = self.pending_responses.write().await;
                            if let Some(resp_tx) = pending.remove(&id) {
                                tracing::debug!("WuKongIM: matched RPC response for id {}", id);
                                let _ = resp_tx.send(val);
                                continue;
                            } else {
                                tracing::debug!("WuKongIM: received response with id {} but no pending request found", id);
                            }
                        }

                        // Handle Notification
                        if let Some("recv") = val.get("method").and_then(|m| m.as_str()) {
                            let notification: JsonRpcNotification<RecvNotificationParams> =
                                serde_json::from_value(val)?;
                            let params = notification.params;

                            if params.from_uid == self.uid {
                                tracing::trace!("WuKongIM: ignoring message from self");
                                continue;
                            }

                            if !self.is_user_allowed(&params.from_uid) {
                                tracing::warn!("WuKongIM: ignoring message from {} (unauthorized)", params.from_uid);
                                continue;
                            }

                            // 1. Decode Base64 payload
                            let decoded_payload = base64::engine::general_purpose::STANDARD.decode(&params.payload)?;
                            let payload_json: serde_json::Value = serde_json::from_slice(&decoded_payload)?;
                            tracing::debug!("WuKongIM: decoded payload: {:?}", payload_json);

                            // 2. Filter out system commands (type 99 or presence of 'cmd')
                            let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
                            if msg_type == WkMessageType::CMD as u64 || payload_json.get("cmd").is_some() {
                                tracing::trace!("WuKongIM: skipping system message or internal command");
                                // Still Ack to stop server retries
                                let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                                continue;
                            }

                            // Handle type 21 interactive response (Strict 21)
                            if msg_type == WkMessageType::INTERACTIVE_RESPONSE as u64 {
                                // Always Ack to stop server retries
                                let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;

                                if let Ok(action_msg) = serde_json::from_value::<WkApprovalAction>(payload_json.clone()) {
                                    let response = match action_msg.action.as_str() {
                                        "approve" => Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve),
                                        "deny" => Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny),
                                        "always" => Some(zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove),
                                        _ => None,
                                    };
                                    if let Some(resp) = response {
                                        let mut pending = self.pending_approvals.write().await;
                                        if let Some(tx) = pending.remove(&action_msg.approval_id) {
                                            tracing::info!("WuKongIM: handled approval response for {}", action_msg.approval_id);
                                            let _ = tx.send(resp);
                                        } else {
                                            tracing::warn!("WuKongIM: received approval response for {} but not found in pending", action_msg.approval_id);
                                        }
                                    }
                                }
                                continue;
                            }

                            // 3. Handle mention_only logic for group chats (MUST check before sending ACK)
                            // If we send ACK first and then skip, the message is permanently lost.
                            let is_group = params.channel_type == WkChannelType::GROUP;
                            if self.mention_only && is_group {
                                let mut mentioned = false;

                                // Check formal mention object
                                if let Some(mention) = payload_json.get("mention") {
                                    // Check 'all'
                                    if let Some(all) = mention.get("all")
                                        && (all.as_u64() == Some(1)
                                            || all.as_str() == Some("1")
                                            || all.as_str() == Some("true")
                                            || all.as_bool() == Some(true))
                                    {
                                        mentioned = true;
                                    }
                                    // Check 'uids'
                                    if !mentioned
                                        && let Some(uids) =
                                            mention.get("uids").and_then(|v| v.as_array())
                                        && uids.iter().any(|u| {
                                            u.as_str() == Some(&self.uid)
                                                || u.as_u64()
                                                    .map(|n| n.to_string())
                                                    .as_deref()
                                                    == Some(&self.uid)
                                        })
                                    {
                                        mentioned = true;
                                    }
                                }

                                // Also check text content for @mentions
                                let content_for_check = if let Some(c) =
                                    payload_json.get("content").and_then(|c| c.as_str())
                                {
                                    c.to_string()
                                } else if let Some(s) = payload_json.as_str() {
                                    s.to_string()
                                } else {
                                    String::new()
                                };

                                if !mentioned
                                    && (content_for_check.contains(&format!("@{}", self.uid))
                                        || content_for_check.contains("@all"))
                                {
                                    mentioned = true;
                                }

                                if !mentioned {
                                    tracing::debug!(
                                        "WuKongIM: ignoring non-mention message in group (uid: {}), NOT sending ACK",
                                        self.uid
                                    );
                                    continue;
                                }
                            }

                            // Only send ACK after all checks pass (message will be processed)
                            let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;

                            // Extract content based on message type
                            // type 1 = text, type 2 = image, type 5 = file
                            let content = match msg_type {
                                2 => {
                                    // Image: extract URL and download
                                    let url = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                                    tracing::info!(
                                        "WuKongIM: received IMAGE from {} (channel: {}:{}): url={}",
                                        params.from_uid,
                                        params.channel_type,
                                        params.channel_id,
                                        url
                                    );
                                    // Try to download the image and return as [IMAGE:...] marker
                                    if let Some(marker) = self.download_image_as_marker(url).await {
                                        marker
                                    } else {
                                        tracing::warn!(
                                            "WuKongIM: failed to download image from {}, falling back to text",
                                            url
                                        );
                                        format!("[图片下载失败]{}\n请直接描述图片内容", url)
                                    }
                                }
                                5 => {
                                    // File: extract URL and name
                                    let url = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                                    let name = payload_json.get("name").and_then(|n| n.as_str()).unwrap_or("文件");
                                    tracing::info!(
                                        "WuKongIM: received FILE from {} (channel: {}:{}): name={}, url={}",
                                        params.from_uid,
                                        params.channel_type,
                                        params.channel_id,
                                        name,
                                        url
                                    );
                                    format!("[文件]{}: {}", name, url)
                                }
                                14 => {
                                    // Markdown: extract text and process inline images
                                    let inner = payload_json.get("content");
                                    let text = inner.and_then(|c| c.get("text")).and_then(|t| t.as_str()).unwrap_or("");
                                    let inner_type = inner.and_then(|c| c.get("type")).and_then(|t| t.as_str()).unwrap_or("");
                                    tracing::info!(
                                        "WuKongIM: received MARKDOWN from {} (channel: {}:{}): inner_type={}, text_len={}",
                                        params.from_uid,
                                        params.channel_type,
                                        params.channel_id,
                                        inner_type,
                                        text.len()
                                    );
                                    // Process markdown: download inline images and replace with base64
                                    self.process_markdown_with_images(text).await
                                }
                                _ => {
                                    // Text or other: extract from content field
                                    if let Some(c) = payload_json.get("content").and_then(|c| c.as_str()) {
                                        c.to_string()
                                    } else if let Some(s) = payload_json.as_str() {
                                        s.to_string()
                                    } else {
                                        tracing::warn!(
                                            "WuKongIM: received UNKNOWN type {} from {} (channel: {}:{}): {:?}",
                                            msg_type,
                                            params.from_uid,
                                            params.channel_type,
                                            params.channel_id,
                                            payload_json
                                        );
                                        String::new()
                                    }
                                }
                            };
                            tracing::info!(
                                "WuKongIM: received message from {} (channel: {}:{}): {}",
                                params.from_uid,
                                params.channel_type,
                                params.channel_id,
                                content
                            );

                            // Determine target: for type 1 (personal), reply to from_uid. For others, reply to channel_id.
                            let target_id = if params.channel_type == WkChannelType::PERSONAL {
                                &params.from_uid
                            } else {
                                &params.channel_id
                            };

                            let channel_msg = ChannelMessage {
                                id: params.message_id,
                                sender: target_id.clone(),
                                reply_target: format!("{}:{}", params.channel_type, target_id),
                                content,
                                channel: "wukongim".to_string(),
                                timestamp: params.timestamp as u64,
                                thread_ts: None,
                                interruption_scope_id: None,
                                attachments: vec![],
                            };

                            if tx.send(channel_msg).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        // Basic connectivity check: attempt a TCP connection to the WS endpoint.
        let url = self
            .ws_url
            .trim_start_matches("ws://")
            .trim_start_matches("wss://");
        tokio::net::TcpStream::connect(url).await.is_ok()
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> anyhow::Result<Option<zeroclaw_api::channel::ChannelApprovalResponse>> {
        let approval_id = Uuid::new_v4().to_string();
        let timeout_secs = self.approval_timeout_secs;

        let card = self.build_approval_card(&approval_id, request, timeout_secs);

        let payload_json = serde_json::to_string(&card)?;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload_json);

        let (channel_id, channel_type) = self.parse_recipient(recipient);

        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: payload_b64,
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending_approvals.write().await;
            pending.insert(approval_id.clone(), tx);
        }

        // Send card
        self.send_rpc::<_, serde_json::Value>("send", params)
            .await?;

        // Wait for response with timeout
        match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
            Ok(Ok(resp)) => Ok(Some(resp)),
            _ => {
                let mut pending = self.pending_approvals.write().await;
                pending.remove(&approval_id);
                // Return Deny on timeout or sender drop
                Ok(Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_approval_structs() {
        let card = WkApprovalCard {
            msg_type: WkMessageType::INTERACTIVE_CARD,
            approval_id: "id123".to_string(),
            timeout_secs: 300,
            title: "Title".to_string(),
            body: WkApprovalBody {
                content: "Body".to_string(),
            },
            actions: Some(vec![
                WkAction {
                    text: "同意".to_string(),
                    value: "approve".to_string(),
                    style: "primary".to_string(),
                },
                WkAction {
                    text: "拒绝".to_string(),
                    value: "deny".to_string(),
                    style: "danger".to_string(),
                },
            ]),
        };
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"type\":20"));
        assert!(json.contains("\"approval_id\":\"id123\""));

        let action_json = r#"{"type": 21, "approval_id": "id123", "action": "approve"}"#;
        let action: WkApprovalAction = serde_json::from_str(action_json).unwrap();
        assert_eq!(action.msg_type, WkMessageType::INTERACTIVE_RESPONSE);
        assert_eq!(action.approval_id, "id123");
        assert_eq!(action.action, "approve");
    }
}

// Helper methods for WuKongIMChannel
impl WuKongIMChannel {
    /// Download image from URL and return as `[IMAGE:...]` marker.
    async fn download_image_as_marker(&self, url: &str) -> Option<String> {
        download_image_as_base64(url).await
    }

    /// Process markdown text, downloading inline images and replacing them with base64 markers.
    /// Markdown image syntax: ![alt](url)
    async fn process_markdown_with_images(&self, text: &str) -> String {
        let mut result = text.to_string();

        // Find all markdown image patterns: ![...](...)
        // We need to handle multiple images in the text
        for (alt_text, url) in Self::extract_markdown_images(text) {
            if let Some(base64_marker) = download_image_as_base64(&url).await {
                // Replace the markdown image with base64 version
                // Format: ![alt](url) -> ![alt](data:mime;base64,...)
                let pattern = format!("![{}]({})", alt_text, url);
                // For LLM processing, we keep the markdown format but use base64 data URL
                let replacement = format!("![{}]({})", alt_text, base64_marker);
                result = result.replace(&pattern, &replacement);
            }
        }

        result
    }

    /// Extract all markdown image URLs from text.
    /// Returns iterator of (alt_text, url) pairs.
    fn extract_markdown_images(text: &str) -> Vec<(String, String)> {
        let mut images = Vec::new();
        let mut rest = text;

        while let Some(start) = rest.find("![") {
            let after_bracket = &rest[start + 2..];
            if let Some(close_bracket) = after_bracket.find(']') {
                let alt = after_bracket[..close_bracket].to_string();
                let after_close = &after_bracket[close_bracket + 1..];
                if let Some(paren_start) = after_close.strip_prefix('(') {
                    if let Some(paren_end) = paren_start.find(')') {
                        let url = paren_start[..paren_end].to_string();
                        images.push((alt, url));
                        rest = &after_close[paren_end + 1..];
                        continue;
                    }
                }
            }
            // No more images, break
            break;
        }

        images
    }
}
