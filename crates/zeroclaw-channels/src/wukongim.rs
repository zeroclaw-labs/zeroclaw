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

const WUKONGIM_RPC_VERSION: &str = "2.0";
const PING_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

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
}

impl WuKongIMChannel {
    pub fn from_config(config: &zeroclaw_config::schema::WuKongIMConfig) -> Self {
        Self {
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: format!("zeroclaw-{}", &Uuid::new_v4().to_string()[..8]),
            allowed_users: config.allowed_users.clone(),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            approval_timeout_secs: config.approval_timeout_secs,
            ws_sink: Arc::new(RwLock::new(None)),
        }
    }

    fn is_user_allowed(&self, uid: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == uid)
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
    ) -> serde_json::Value {
        serde_json::json!({
            "type": 20,
            "approval_id": approval_id,
            "timeout_secs": timeout_secs,
            "header": {
                "title": "需要审批"
            },
            "body": {
                "elements": [
                    {
                        "tag": "markdown",
                        "content": format!("🔧 Agent wants to execute: **{}**\n\n{}", request.tool_name, request.arguments_summary)
                    },
                    {
                        "tag": "action",
                        "actions": [
                            {
                                "tag": "button",
                                "text": { "tag": "plain_text", "content": "Approve" },
                                "type": "primary",
                                "value": "approve"
                            },
                            {
                                "tag": "button",
                                "text": { "tag": "plain_text", "content": "Deny" },
                                "type": "danger",
                                "value": "deny"
                            },
                            {
                                "tag": "button",
                                "text": { "tag": "plain_text", "content": "Always" },
                                "type": "default",
                                "value": "always"
                            }
                        ]
                    }
                ]
            }
        })
    }
}

#[async_trait]
impl Channel for WuKongIMChannel {
    fn name(&self) -> &str {
        "wukongim"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        tracing::debug!(
            "WuKongIM: sending message to {}: {}",
            message.recipient,
            message.content
        );
        let payload_obj = serde_json::json!({
            "type": 1,
            "content": message.content,
        });

        let payload_json = serde_json::to_string(&payload_obj)?;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload_json);

        // Parse channel_type from recipient if formatted as "type:id"
        let (channel_id, channel_type) = if let Some(pos) = message.recipient.find(':') {
            let (t_str, id_str) = message.recipient.split_at(pos);
            let id_str = &id_str[1..]; // skip the colon
            let t = t_str.parse::<u8>().unwrap_or(1);
            (id_str.to_string(), t)
        } else {
            (message.recipient.clone(), 1)
        };

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
                    device_flag: 1, // Web/Sys
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

                            if !self.is_user_allowed(&params.from_uid) {
                                tracing::warn!("WuKongIM: ignoring message from {} (unauthorized)", params.from_uid);
                                continue;
                            }

                            // 1. Decode Base64 payload
                            let decoded_payload = base64::engine::general_purpose::STANDARD.decode(&params.payload)?;
                            let payload_json: serde_json::Value = serde_json::from_slice(&decoded_payload)?;

                            // 2. Filter out system commands (type 99 or presence of 'cmd')
                            let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
                            if msg_type == 99 || payload_json.get("cmd").is_some() {
                                tracing::trace!("WuKongIM: skipping system message or internal command");
                                // Still Ack to stop server retries
                                let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                                continue;
                            }

                            // Handle type 20 interactive response
                            if let (Some(20), Some(approval_id), Some(action)) = (
                                payload_json.get("type").and_then(|t| t.as_u64()),
                                payload_json.get("approval_id").and_then(|id| id.as_str()),
                                payload_json.get("action").and_then(|a| a.as_str()),
                            ) {
                                let response = match action {
                                    "approve" => {
                                        Some(zeroclaw_api::channel::ChannelApprovalResponse::Approve)
                                    }
                                    "deny" => {
                                        Some(zeroclaw_api::channel::ChannelApprovalResponse::Deny)
                                    }
                                    "always" => Some(
                                        zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
                                    ),
                                    _ => None,
                                };
                                if let Some(resp) = response {
                                    let mut pending = self.pending_approvals.write().await;
                                    if let Some(tx) = pending.remove(approval_id) {
                                        let _ = tx.send(resp);
                                        continue;
                                    }
                                }
                            }

                            // Ack receipt
                            let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;

                            // Extract text content
                            let content = payload_json.get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or(&params.payload)
                                .to_string();

                            // Determine target: for type 1 (personal), reply to from_uid. For others, reply to channel_id.
                            let target_id = if params.channel_type == 1 {
                                &params.from_uid
                            } else {
                                &params.channel_id
                            };

                            let channel_msg = ChannelMessage {
                                id: params.message_id,
                                sender: params.from_uid.clone(),
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

        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: recipient.to_string(),
            channel_type: 1, // Person
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
