// src/channel.rs
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message as WsMsg;
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

use crate::approval::{PendingApprovals, WkApprovalAction, build_approval_card};
use crate::config::WuKongIMConfig;
use crate::connection::{
    ConnectParams, HEARTBEAT_TIMEOUT, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    PING_INTERVAL, RecvAckParams, RecvNotificationParams, SendParams, WUKONGIM_RPC_VERSION,
    WkChannelType, WkMessageType, WsSink,
};
use crate::filter::{is_mentioned, is_user_allowed, parse_recipient};
use crate::messaging::{
    download_image_as_base64, encode_text_payload, process_markdown_resources,
};

#[derive(Clone)]
pub struct WuKongIMChannel {
    pub(crate) ws_url: String,
    pub(crate) uid: String,
    pub(crate) token: String,
    pub(crate) device_id: String,
    pub(crate) device_flag: i32,
    pub(crate) allowed_users: Vec<String>,
    pub(crate) approval_timeout_secs: u64,
    pub(crate) mention_only: bool,
    pub(crate) pending_responses:
        Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    pub(crate) pending_approvals: Arc<PendingApprovals>,
    pub(crate) ws_sink: Arc<RwLock<Option<WsSink>>>,
    pub(crate) workspace_dir: PathBuf,
}

impl WuKongIMChannel {
    pub fn from_config(config: &WuKongIMConfig, workspace_dir: &Path) -> Self {
        Self {
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: config.device_id.clone(),
            device_flag: config.device_flag,
            allowed_users: config.allowed_users.clone(),
            approval_timeout_secs: config.approval_timeout_secs,
            mention_only: config.mention_only,
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            ws_sink: Arc::new(RwLock::new(None)),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    async fn send_rpc<P: serde::Serialize, R: serde::de::DeserializeOwned>(
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
        self.pending_responses.write().await.insert(id.clone(), tx);
        let send_result: anyhow::Result<()> = async {
            let msg = serde_json::to_string(&req)?;
            let mut g = self.ws_sink.write().await;
            match g.as_mut() {
                Some(s) => {
                    tracing::info!("WuKongIM: RPC {} id={}", method, id);
                    s.send(WsMsg::Text(msg.into())).await?;
                    Ok(())
                }
                None => anyhow::bail!("WuKongIM: WebSocket not connected"),
            }
        }
        .await;
        if let Err(e) = send_result {
            self.pending_responses.write().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(val)) => {
                let resp: JsonRpcResponse<R> = serde_json::from_value(val)?;
                if let Some(err) = resp.error {
                    anyhow::bail!("WuKongIM RPC error: {} (code {})", err.message, err.code);
                }
                resp.result
                    .ok_or_else(|| anyhow::anyhow!("WuKongIM RPC: missing result"))
            }
            Ok(Err(_)) => {
                // Sender dropped — remove stale entry (already removed by listen loop, but be safe)
                self.pending_responses.write().await.remove(&id);
                anyhow::bail!("WuKongIM RPC: response channel closed for {}", method);
            }
            Err(_) => {
                self.pending_responses.write().await.remove(&id);
                anyhow::bail!("WuKongIM RPC timeout: {}", method);
            }
        }
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
        let mut g = self.ws_sink.write().await;
        if let Some(s) = g.as_mut() {
            s.send(WsMsg::Text(msg.into())).await?;
        }
        Ok(())
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
        let payload_b64 = encode_text_payload(content)?;
        let (channel_id, channel_type) = parse_recipient(&message.recipient);
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
        tracing::info!("WuKongIM: connecting to {}", self.ws_url);
        let (ws_stream, _) = tokio_tungstenite::connect_async(&self.ws_url).await?;
        let (write, mut read) = ws_stream.split();
        *self.ws_sink.write().await = Some(write);

        // Handshake
        {
            let connect_id = Uuid::new_v4().to_string();
            let req = JsonRpcRequest {
                jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                method: "connect".to_string(),
                id: connect_id,
                params: ConnectParams {
                    uid: self.uid.clone(),
                    token: self.token.clone(),
                    device_id: self.device_id.clone(),
                    device_flag: self.device_flag,
                    version: Some(2),
                },
            };
            let msg = serde_json::to_string(&req)?;
            if let Some(s) = self.ws_sink.write().await.as_mut() {
                s.send(WsMsg::Text(msg.into())).await?;
            }
            let connack = tokio::time::timeout(Duration::from_secs(15), read.next())
                .await
                .map_err(|_| anyhow::anyhow!("WuKongIM: connect timeout"))?
                .ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed during connect"))??;
            if let WsMsg::Text(text) = connack {
                let val: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(err) = val.get("error").filter(|e| !e.is_null()) {
                    anyhow::bail!("WuKongIM: connect rejected: {}", err);
                }
            }
        }
        tracing::info!("WuKongIM: connected as {}", self.uid);

        let mut hb = tokio::time::interval(PING_INTERVAL);
        let mut last_activity = Instant::now();

        loop {
            tokio::select! {
                _ = hb.tick() => {
                    if last_activity.elapsed() > HEARTBEAT_TIMEOUT {
                        anyhow::bail!("WuKongIM: heartbeat timeout");
                    }
                    let ping = JsonRpcRequest {
                        jsonrpc: WUKONGIM_RPC_VERSION.to_string(),
                        method: "ping".to_string(),
                        id: Uuid::new_v4().to_string(),
                        params: serde_json::json!({}),
                    };
                    if let Ok(msg) = serde_json::to_string(&ping)
                        && let Some(s) = self.ws_sink.write().await.as_mut()
                    {
                        let _ = s.send(WsMsg::Text(msg.into())).await;
                    }
                }
                frame = read.next() => {
                    let frame = frame.ok_or_else(|| anyhow::anyhow!("WuKongIM: stream closed"))??;
                    last_activity = Instant::now();
                    let WsMsg::Text(text) = frame else { continue; };
                    let val: serde_json::Value = serde_json::from_str(&text)?;

                    // pong
                    if val.get("method").and_then(|m| m.as_str()) == Some("pong") { continue; }

                    // RPC response (matched by id)
                    let msg_id = val.get("id").and_then(|i| {
                        if i.is_string() { i.as_str().map(str::to_string) }
                        else if i.is_number() { Some(i.to_string()) }
                        else { None }
                    });
                    if let Some(id) = msg_id
                        && let Some(resp_tx) = self.pending_responses.write().await.remove(&id)
                    {
                        let _ = resp_tx.send(val);
                        continue;
                    }

                    // Inbound message notification
                    if val.get("method").and_then(|m| m.as_str()) != Some("recv") { continue; }
                    let notif: JsonRpcNotification<RecvNotificationParams> = serde_json::from_value(val)?;
                    let params = notif.params;

                    if params.from_uid == self.uid { continue; }
                    if !is_user_allowed(&self.allowed_users, &params.from_uid) {
                        tracing::warn!("WuKongIM: unauthorized sender {}", params.from_uid);
                        continue;
                    }

                    let decoded = base64::engine::general_purpose::STANDARD.decode(&params.payload)?;
                    let payload_json: serde_json::Value = serde_json::from_slice(&decoded)?;
                    let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);

                    // System command — ack and skip
                    if msg_type == WkMessageType::CMD as u64 || payload_json.get("cmd").is_some() {
                        let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                        continue;
                    }

                    // Interactive response (approval answer)
                    if msg_type == WkMessageType::INTERACTIVE_RESPONSE as u64 {
                        let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
                        if let Ok(action) = serde_json::from_value::<WkApprovalAction>(payload_json) {
                            let resp = match action.action.as_str() {
                                "approve" => Some(ChannelApprovalResponse::Approve),
                                "deny"    => Some(ChannelApprovalResponse::Deny),
                                "always"  => Some(ChannelApprovalResponse::AlwaysApprove),
                                _         => None,
                            };
                            if let Some(r) = resp
                                && let Some(ptx) = self.pending_approvals.write().await.remove(&action.approval_id)
                            {
                                let _ = ptx.send(r);
                            }
                        }
                        continue;
                    }

                    // mention_only filter for group messages
                    if self.mention_only && params.channel_type == WkChannelType::GROUP {
                        let content_str = payload_json.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        if !is_mentioned(&self.uid, &payload_json, content_str) {
                            continue;
                        }
                    }

                    let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;

                    // Decode content by message type
                    let content = match msg_type as u32 {
                        WkMessageType::IMAGE => {
                            let url = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            download_image_as_base64(url).await
                                .unwrap_or_else(|| format!("[图片下载失败]{}\n请直接描述图片内容", url))
                        }
                        WkMessageType::FILE => {
                            let url  = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                            let name = payload_json.get("name").and_then(|n| n.as_str()).unwrap_or("文件");
                            format!("[文件]{}: {}", name, url)
                        }
                        WkMessageType::MARKDOWN => {
                            let text = payload_json
                                .get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str())
                                .unwrap_or("");
                            process_markdown_resources(text, std::path::Path::new(".")).await
                        }
                        _ => payload_json.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    };

                    let target_id = if params.channel_type == WkChannelType::PERSONAL {
                        &params.from_uid
                    } else {
                        &params.channel_id
                    };

                    let ch_msg = ChannelMessage {
                        id: params.message_id,
                        sender: target_id.clone(),
                        reply_target: format!("{}:{}", params.channel_type, target_id),
                        content,
                        channel: "wukongim".to_string(),
                        timestamp: params.timestamp.max(0) as u64,
                        thread_ts: None,
                        interruption_scope_id: None,
                        attachments: vec![],
                    };
                    if tx.send(ch_msg).await.is_err() { break; }
                }
            }
        }
        Ok(())
    }

    async fn health_check(&self) -> bool {
        let Ok(parsed) = url::Url::parse(&self.ws_url) else {
            return false;
        };
        let Some(host) = parsed.host_str() else {
            return false;
        };
        let port = parsed.port_or_known_default().unwrap_or(80);
        tokio::net::TcpStream::connect((host, port)).await.is_ok()
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let approval_id = Uuid::new_v4().to_string();
        let card = build_approval_card(&approval_id, request, self.approval_timeout_secs);
        let payload_b64 =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?);
        let (channel_id, channel_type) = parse_recipient(recipient);
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
        let (otx, orx) = tokio::sync::oneshot::channel();
        self.pending_approvals
            .write()
            .await
            .insert(approval_id.clone(), otx);
        self.send_rpc::<_, serde_json::Value>("send", params)
            .await?;
        match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), orx).await {
            Ok(Ok(resp)) => Ok(Some(resp)),
            _ => {
                self.pending_approvals.write().await.remove(&approval_id);
                Ok(Some(ChannelApprovalResponse::Deny))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WuKongIMConfig;

    fn make_config(allowed: Vec<String>, mention_only: bool) -> WuKongIMConfig {
        WuKongIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".to_string(),
            uid: "bot001".to_string(),
            token: "tok".to_string(),
            device_id: "web-001".to_string(),
            device_flag: 2,
            allowed_users: allowed,
            mention_only,
            approval_timeout_secs: 300,
        }
    }

    #[test]
    fn from_config_maps_fields() {
        let workspace = std::path::PathBuf::from("/tmp/test");
        let ch = WuKongIMChannel::from_config(&make_config(vec!["*".to_string()], true), &workspace);
        assert_eq!(ch.ws_url, "ws://localhost:5200");
        assert_eq!(ch.uid, "bot001");
        assert_eq!(ch.device_id, "web-001");
        assert_eq!(ch.device_flag, 2);
        assert!(ch.mention_only);
        assert_eq!(ch.approval_timeout_secs, 300);
        assert_eq!(ch.workspace_dir, workspace);
    }

    #[test]
    fn channel_name_is_wukongim() {
        use zeroclaw_api::channel::Channel;
        let workspace = std::path::PathBuf::from("/tmp/test");
        let ch = WuKongIMChannel::from_config(&make_config(vec![], false), &workspace);
        assert_eq!(ch.name(), "wukongim");
    }
}
