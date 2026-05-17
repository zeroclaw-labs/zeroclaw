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
use zeroclaw_api::memory_traits::Memory;

use crate::approval::{PendingApprovals, WkApprovalAction, build_approval_card};
use crate::config::WuKongIMConfig;
use crate::connection::{
    ClearUnreadRequest, ConnectParams, HEARTBEAT_TIMEOUT, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, PING_INTERVAL, RecvAckParams, RecvNotificationParams, SendParams,
    SyncRequest, SyncResponse, WUKONGIM_RPC_VERSION, WkChannelType,
    WkMessageType, WsSink,
};
use crate::filter::{is_mentioned, is_user_allowed, parse_recipient};
use crate::messaging::{
    download_file_to_workspace, download_image_as_base64, encode_text_payload,
    process_markdown_resources,
};

#[derive(serde::Serialize, serde::Deserialize, Default, Debug)]
struct SyncState {
    max_version: i64,
    channel_seqs: HashMap<String, u32>,
}

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
    pub(crate) dawn_url: String,
    pub(crate) dawn_token: String,
    pub(crate) ack_reactions: bool,
    pub(crate) ack_reactions_message: String,
    pub(crate) ack_reactions_delay_secs: u64,
    pub(crate) memory: Arc<dyn Memory>,
    pub(crate) pending_responses:
        Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    pub(crate) pending_approvals: Arc<PendingApprovals>,
    pub(crate) ws_sink: Arc<RwLock<Option<WsSink>>>,
    pub(crate) downloads_dir: PathBuf,
    pub(crate) last_message_time: Arc<RwLock<HashMap<String, Instant>>>,
    pub(crate) workspace_dir: PathBuf,
    pub(crate) progress_streaming: bool,
}

impl WuKongIMChannel {
    pub fn from_config(config: &WuKongIMConfig, workspace_dir: &Path, memory: Arc<dyn Memory>) -> Self {
        let downloads_dir = if config.downloads_dir.starts_with('/') {
            PathBuf::from(&config.downloads_dir)
        } else {
            workspace_dir.join(&config.downloads_dir)
        };
        Self {
            ws_url: config.ws_url.clone(),
            uid: config.uid.clone(),
            token: config.token.clone(),
            device_id: config.device_id.clone(),
            device_flag: config.device_flag,
            allowed_users: config.allowed_users.clone(),
            approval_timeout_secs: config.approval_timeout_secs,
            mention_only: config.mention_only,
            dawn_url: config.dawn_url.clone(),
            dawn_token: config.dawn_token.clone(),
            ack_reactions: config.ack_reactions,
            ack_reactions_message: if config.ack_reactions_message.is_empty() { "👋 收到".to_string() } else { config.ack_reactions_message.clone() },
            ack_reactions_delay_secs: config.ack_reactions_delay,
            memory,
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            ws_sink: Arc::new(RwLock::new(None)),
            downloads_dir,
            last_message_time: Arc::new(RwLock::new(HashMap::new())),
            workspace_dir: workspace_dir.to_path_buf(),
            progress_streaming: config.progress_streaming,
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

    fn get_sync_state_path(&self) -> PathBuf {
        self.workspace_dir.join("wukongim_sync.json")
    }

    async fn load_sync_state(&self) -> SyncState {
        let path = self.get_sync_state_path();
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            SyncState::default()
        }
    }

    async fn save_sync_state(&self, state: &SyncState) -> anyhow::Result<()> {
        let path = self.get_sync_state_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(state)?;
        tokio::fs::write(&path, content).await?;
        Ok(())
    }

    async fn update_sync_state(&self, channel_id: &str, channel_type: u8, seq: u32, timestamp_ns: i64) -> anyhow::Result<()> {
        let mut state = self.load_sync_state().await;
        let mut changed = false;

        // 1. Update channel sequence
        let seq_key = format!("{}:{}", channel_id, channel_type);
        let current_seq = *state.channel_seqs.get(&seq_key).unwrap_or(&0);
        if seq > current_seq {
            tracing::info!("WuKongIM: updating sequence for {} from {} to {}", seq_key, current_seq, seq);
            state.channel_seqs.insert(seq_key, seq);
            changed = true;
        }

        // 2. Update max version
        if timestamp_ns > state.max_version {
            tracing::info!("WuKongIM: updating max_version from {} to {}", state.max_version, timestamp_ns);
            state.max_version = timestamp_ns;
            changed = true;
        }

        if changed {
            self.save_sync_state(&state).await?;
        }

        Ok(())
    }

    async fn clear_unread(&self, channel_id: &str, channel_type: u8, message_seq: u32) -> anyhow::Result<()> {
        let url = format!("{}/v1/conversations/clear_unread", self.dawn_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let req = ClearUnreadRequest {
            uid: self.uid.clone(),
            channel_id: channel_id.to_string(),
            channel_type,
            message_seq,
        };

        let resp = client.put(&url)
            .header("X-Assistant-Token", &self.dawn_token)
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::warn!("WuKongIM: failed to clear unread for {}:{}: status={} url={}", channel_id, channel_type, resp.status(), url);
        }
        Ok(())
    }

    async fn sync_history(&self) -> anyhow::Result<Vec<RecvNotificationParams>> {
        // 1. Load sync state from file
        let state = self.load_sync_state().await;
        let version = state.max_version;
        let last_msg_seqs = state.channel_seqs.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<_>>()
            .join("|");

        tracing::info!(version = version, last_msg_seqs = %last_msg_seqs, "WuKongIM: starting history sync");

        // 3. HTTP Sync
        let url = format!("{}/v1/conversations/sync", self.dawn_url.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let req = SyncRequest {
            uid: self.uid.clone(),
            version,
            last_msg_seqs,
            msg_count: 50,
        };

        let resp = client.post(&url)
            .header("X-Assistant-Token", &self.dawn_token)
            .json(&req)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("WuKongIM sync failed: status={}", resp.status());
        }

        let body_text = resp.text().await?;
        let sync_resp: SyncResponse = match serde_json::from_str(&body_text) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("WuKongIM: failed to decode sync response: {}. Raw body: {}", e, body_text);
                anyhow::bail!("WuKongIM: sync decode error: {}", e);
            }
        };
        let mut all_history = Vec::new();
        let mut total_messages = 0;
        let num_conversations = sync_resp.conversations.len();
        for conv in sync_resp.conversations {
            tracing::info!("WuKongIM: syncing conversation {}:{} last_seq={} version={}", conv.channel_id, conv.channel_type, conv.last_msg_seq, conv.version);
            if let Some(messages) = conv.recents {
                total_messages += messages.len();
                for m in messages {
                    let msg_id = if m.message_id.is_string() {
                        m.message_id.as_str().unwrap_or_default().to_string()
                    } else {
                        m.message_id.to_string()
                    };
                    
                    all_history.push(RecvNotificationParams {
                        message_id: msg_id.clone(),
                        message_seq: m.message_seq,
                        from_uid: m.from_uid.clone(),
                        channel_id: conv.channel_id.clone(),
                        channel_type: conv.channel_type,
                        payload: m.payload.clone(),
                        timestamp: m.timestamp,
                    });
                    
                    // Log the message content summary by decoding Base64 payload if string
                    let payload_json: Option<serde_json::Value> = if m.payload.is_string() {
                        base64::engine::general_purpose::STANDARD.decode(m.payload.as_str().unwrap_or_default())
                            .ok()
                            .and_then(|d| serde_json::from_slice(&d).ok())
                    } else {
                        Some(m.payload.clone())
                    };

                    let summary = if let Some(pj) = payload_json {
                        if let Some(text) = pj.get("content").and_then(|v| v.as_str()) {
                            text.chars().take(50).collect::<String>()
                        } else {
                            format!("type={}", pj.get("type").and_then(|v| v.as_i64()).unwrap_or(0))
                        }
                    } else {
                        "unparseable_payload".to_string()
                    };
                    tracing::info!("  - [History] from {}: {}", m.from_uid, summary);
                }
            }
            // Clear unread after fetch
            let _ = self.clear_unread(&conv.channel_id, conv.channel_type, conv.last_msg_seq).await;
            // Update version based on conversation
            self.update_sync_state(&conv.channel_id, conv.channel_type, conv.last_msg_seq, conv.version).await?;
        }

        // Sort globally by timestamp
        all_history.sort_by_key(|m| m.timestamp);
        if num_conversations == 0 {
            tracing::info!("WuKongIM: history sync completed, no new updates from server");
        } else {
            tracing::info!("WuKongIM: history sync completed, {} messages found across {} conversations", total_messages, num_conversations);
        }
        Ok(all_history)
    }

    async fn process_inbound_message(
        &self,
        params: RecvNotificationParams,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        if params.from_uid == self.uid { return Ok(()); }

        // Idempotency check: skip if already processed
        let seq_key = format!("wukongim:channel_seq:{}:{}", params.channel_id, params.channel_type);
        let current_seq = self.memory.get(&seq_key).await?.and_then(|e| e.content.parse::<u32>().ok()).unwrap_or(0);
        if params.message_seq <= current_seq {
            return Ok(());
        }

        if !is_user_allowed(&self.allowed_users, &params.from_uid) {
            tracing::warn!("WuKongIM: unauthorized sender {}", params.from_uid);
            return Ok(());
        }

        // Decode payload if it's a string (Base64), otherwise use as is if it's an object
        let payload_json: serde_json::Value = if params.payload.is_string() {
            let decoded = base64::engine::general_purpose::STANDARD.decode(params.payload.as_str().unwrap_or_default())?;
            serde_json::from_slice(&decoded)?
        } else {
            params.payload.clone()
        };

        let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);

        // System command — handle la_init_helloworld CMD
        if msg_type == WkMessageType::CMD as u64 || payload_json.get("cmd").is_some() {
            let _ = self.send_ack(params.message_id.clone(), params.message_seq).await;
            if payload_json.get("cmd").and_then(|c| c.as_str()) == Some("la_init_helloworld")
                && let Some(content) = payload_json.get("content").and_then(|c| c.as_str())
                && !(params.channel_type == WkChannelType::GROUP && !is_mentioned(&self.uid, &payload_json, content))
            {
                let target_id = if params.channel_type == WkChannelType::GROUP { &params.channel_id } else { &params.from_uid };
                let ch_msg = ChannelMessage {
                    id: params.message_id.clone(),
                    sender: target_id.clone(),
                    reply_target: format!("{}:{}", params.channel_type, target_id),
                    content: content.to_string(),
                    channel: "wukongim".to_string(),
                    timestamp: params.timestamp.max(0) as u64,
                    thread_ts: None,
                    interruption_scope_id: None,
                    attachments: vec![],
                };
                if tx.send(ch_msg).await.is_ok() {
                    self.update_sync_state(&params.channel_id, params.channel_type, params.message_seq, params.timestamp * 1_000_000_000).await?;
                }
            }
            return Ok(());
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
            return Ok(());
        }

        // mention_only filter for group messages
        let mut silent = false;
        let mut final_content_str = if msg_type == WkMessageType::MARKDOWN as u64 {
            payload_json.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()).unwrap_or("").to_string()
        } else {
            payload_json.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string()
        };

        if self.mention_only && params.channel_type == WkChannelType::GROUP {
            let mentioned = is_mentioned(&self.uid, &payload_json, &final_content_str);
            tracing::info!(
                "WuKongIM: Group message mention check: mentioned={}, uid={}, content_len={}",
                mentioned, self.uid, final_content_str.len()
            );

            if !mentioned {
                silent = true;
            } else if !final_content_str.contains(&format!("@{}", self.uid)) {
                tracing::info!("WuKongIM: Metadata mention detected, prepending bot UID to force orchestrator reply");
                final_content_str = format!("@{} {}", self.uid, final_content_str);
            }
        }

        // Send quick acknowledgment message only if:
        // 1. ack_reactions is enabled AND
        // 2. More than 60 seconds have passed since the last message from this sender
        if self.ack_reactions {
            let sender_key = format!("{}:{}", params.channel_id, params.from_uid);
            let now = Instant::now();
            let should_send = {
                let last_time = self.last_message_time.read().await;
                match last_time.get(&sender_key) {
                    Some(last) if now.duration_since(*last) < Duration::from_secs(self.ack_reactions_delay_secs) => false,
                    _ => true,
                }
            };
            if should_send {
                let ack_msg = self.ack_reactions_message.clone();
                let target_id = if params.channel_type == WkChannelType::PERSONAL { &params.from_uid } else { &params.channel_id };
                let _ = self.send_text_message(target_id, params.channel_type, &ack_msg).await;
            }
            // Update last message time
            {
                let mut last_time = self.last_message_time.write().await;
                last_time.insert(sender_key, now);
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
                let raw_url = payload_json.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let url = raw_url.split_whitespace().next().unwrap_or(raw_url);
                let name = payload_json.get("name").and_then(|n| n.as_str()).unwrap_or("文件");
                match download_file_to_workspace(url, &self.downloads_dir, Some(name)).await {
                    Ok(local_path) => format!("[文件]{}: {}", name, local_path),
                    Err(err_msg) => format!("[文件]{}: {} [下载失败: {}]", name, url, err_msg),
                }
            }
            WkMessageType::MARKDOWN => {
                process_markdown_resources(&final_content_str, &self.downloads_dir).await
            }
            _ => final_content_str,
        };

        let target_id = if params.channel_type == WkChannelType::PERSONAL { &params.from_uid } else { &params.channel_id };

        let ch_msg = ChannelMessage {
            id: params.message_id,
            sender: target_id.clone(),
            reply_target: format!("{}:{}", params.channel_type, target_id),
            content: if silent { format!("<!-- zeroclaw:silent -->{}", content) } else { content },
            channel: "wukongim".to_string(),
            timestamp: params.timestamp.max(0) as u64,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        if tx.send(ch_msg).await.is_ok() {
            tracing::info!("WuKongIM: message sent to orchestrator, updating sync state: seq={}, ts={}", params.message_seq, params.timestamp);
            self.update_sync_state(&params.channel_id, params.channel_type, params.message_seq, params.timestamp * 1_000_000_000).await?;
        }
        Ok(())
    }

    async fn send_text_message(
        &self,
        channel_id: &str,
        channel_type: u8,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload_b64 = encode_text_payload(text)?;
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: channel_id.to_string(),
            channel_type,
            payload: serde_json::Value::String(payload_b64),
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

    /// Send a structured "application command" message.
    ///
    /// WuKongIM's Go server expects `SendParams.payload` as `[]uint8`, which
    /// JSON-encodes as a base64 string — same contract as `send_text_message`.
    async fn send_cmd_message(
        &self,
        channel_id: &str,
        channel_type: u8,
        cmd_payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        let json = serde_json::to_string(&cmd_payload)?;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(json);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: channel_id.to_string(),
            channel_type,
            payload: serde_json::Value::String(payload_b64),
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
}

fn phase_to_content(phase: &zeroclaw_api::channel::StatusPhase) -> &'static str {
    use zeroclaw_api::channel::StatusPhase;
    match phase {
        StatusPhase::AgentStart => "Agent 启动",
        StatusPhase::LlmThinking => "正在思考",
        StatusPhase::ToolStart => "工具启动",
        StatusPhase::ToolDone { success: true, .. } => "工具完成",
        StatusPhase::ToolDone { success: false, .. } => "工具失败",
        StatusPhase::Error => "错误",
        StatusPhase::AgentEnd => "处理完成",
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
            payload: serde_json::Value::String(payload_b64),
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

    async fn send_status_update(
        &self,
        recipient: &str,
        _thread_ts: Option<&str>,
        update: zeroclaw_api::channel::StatusUpdate,
    ) -> anyhow::Result<()> {
        if !self.progress_streaming {
            return Ok(());
        }
        let (channel_id, channel_type) = parse_recipient(recipient);
        let payload = serde_json::json!({
            "cmd": "la_status_update",
            "content": phase_to_content(&update.phase),
            "param": {
                "mid": update.execution_id,
                "name": update.name,
                "desc": update.desc,
            }
        });
        self.send_cmd_message(&channel_id, channel_type, payload).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // 1. Fetch history from HTTP (before WS connect to avoid version race)
        let history = self.sync_history().await.unwrap_or_else(|e| {
            tracing::error!("WuKongIM: history sync failed: {}", e);
            vec![]
        });

        // 2. Connect WebSocket
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

        // 3. Process History (now that WS is connected, Agent can reply)
        for msg in history {
            let _ = self.process_inbound_message(msg, &tx).await;
        }

        // 4. Start Live Listening
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

                    if val.get("method").and_then(|m| m.as_str()) == Some("pong") { continue; }

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

                    if val.get("method").and_then(|m| m.as_str()) != Some("recv") { continue; }
                    let notif: JsonRpcNotification<RecvNotificationParams> = serde_json::from_value(val)?;
                    let _ = self.process_inbound_message(notif.params, &tx).await;
                }
            }
        }
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
            payload: serde_json::Value::String(payload_b64),
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
    use zeroclaw_api::memory_traits::{Memory, MemoryCategory, MemoryEntry};

    struct MockMemory;
    #[async_trait::async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str { "mock" }
        async fn store(&self, _: &str, _: &str, _: MemoryCategory, _: Option<&str>) -> anyhow::Result<()> { Ok(()) }
        async fn recall(&self, _: &str, _: usize, _: Option<&str>, _: Option<&str>, _: Option<&str>) -> anyhow::Result<Vec<MemoryEntry>> { Ok(vec![]) }
        async fn get(&self, _: &str) -> anyhow::Result<Option<MemoryEntry>> { Ok(None) }
        async fn list(&self, _: Option<&MemoryCategory>, _: Option<&str>) -> anyhow::Result<Vec<MemoryEntry>> { Ok(vec![]) }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> { Ok(true) }
        async fn count(&self) -> anyhow::Result<usize> { Ok(0) }
        async fn health_check(&self) -> bool { true }
    }

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
            downloads_dir: "downloads".to_string(),
            dawn_url: "".to_string(),
            dawn_token: "".to_string(),
            ack_reactions: true,
            ack_reactions_message: String::new(),
            ack_reactions_delay: 60,
            progress_streaming: false,
        }
    }

    #[test]
    fn from_config_maps_fields() {
        let workspace = std::path::PathBuf::from("/tmp/test");
        let memory = Arc::new(MockMemory);
        let ch = WuKongIMChannel::from_config(&make_config(vec!["*".to_string()], true), &workspace, memory);
        assert_eq!(ch.ws_url, "ws://localhost:5200");
        assert_eq!(ch.uid, "bot001");
        assert_eq!(ch.device_id, "web-001");
        assert_eq!(ch.device_flag, 2);
        assert!(ch.mention_only);
        assert_eq!(ch.approval_timeout_secs, 300);
        assert_eq!(ch.downloads_dir, workspace.join("downloads"));
        assert!(!ch.progress_streaming, "progress_streaming must map from config");
    }

    #[test]
    fn channel_name_is_wukongim() {
        use zeroclaw_api::channel::Channel;
        let workspace = std::path::PathBuf::from("/tmp/test");
        let memory = Arc::new(MockMemory);
        let ch = WuKongIMChannel::from_config(&make_config(vec![], false), &workspace, memory);
        assert_eq!(ch.name(), "wukongim");
    }

    fn make_test_channel(progress_streaming: bool) -> WuKongIMChannel {
        let workspace = std::path::PathBuf::from("/tmp/test");
        let memory = Arc::new(MockMemory);
        let mut config = make_config(vec!["*".to_string()], false);
        config.progress_streaming = progress_streaming;
        WuKongIMChannel::from_config(&config, &workspace, memory)
    }

    #[test]
    fn cmd_payload_serializes_with_known_shape() {
        let payload = serde_json::json!({
            "cmd": "la_status_update",
            "content": "执行状态",
            "param": {
                "mid": "exec-9c4f",
                "name": "shell",
                "desc": "执行命令：ls",
            }
        });

        let s = serde_json::to_string(&payload).unwrap();
        assert!(s.contains("\"cmd\":\"la_status_update\""));
        assert!(s.contains("\"mid\":\"exec-9c4f\""));
        assert!(s.contains("\"desc\":\"执行命令：ls\""));
    }

    use zeroclaw_api::channel::StatusPhase;

    #[test]
    fn phase_to_content_covers_all_variants() {
        assert_eq!(phase_to_content(&StatusPhase::AgentStart), "Agent 启动");
        assert_eq!(phase_to_content(&StatusPhase::LlmThinking), "正在思考");
        assert_eq!(phase_to_content(&StatusPhase::ToolStart), "工具启动");
        assert_eq!(
            phase_to_content(&StatusPhase::ToolDone { success: true, elapsed_ms: 0 }),
            "工具完成",
        );
        assert_eq!(
            phase_to_content(&StatusPhase::ToolDone { success: false, elapsed_ms: 0 }),
            "工具失败",
        );
        assert_eq!(phase_to_content(&StatusPhase::Error), "错误");
        assert_eq!(phase_to_content(&StatusPhase::AgentEnd), "处理完成");
    }

    #[tokio::test]
    async fn send_status_update_returns_ok_when_opt_in_disabled() {
        use zeroclaw_api::channel::{Channel, StatusUpdate};
        let ch = make_test_channel(false);
        let update = StatusUpdate {
            execution_id: "e".into(),
            phase: StatusPhase::AgentStart,
            name: "agent".into(),
            desc: "x".into(),
        };
        assert!(ch.send_status_update("P:u1", None, update).await.is_ok());
    }
}
