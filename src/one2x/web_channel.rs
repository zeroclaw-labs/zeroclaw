use std::collections::HashMap;
use std::sync::OnceLock;

use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{RwLock, broadcast};

/// Global WebChannel instance shared between gateway and channel system.
/// Initialized once when web channel is enabled in config.
static WEB_CHANNEL_INSTANCE: OnceLock<Arc<WebChannel>> = OnceLock::new();

/// Channel message sender, set by `start_channels` when the web channel is active.
static WEB_CHANNEL_TX: OnceLock<
    tokio::sync::mpsc::Sender<crate::channels::traits::ChannelMessage>,
> = OnceLock::new();

/// Get or create the global WebChannel instance.
pub fn get_or_init_web_channel() -> Arc<WebChannel> {
    Arc::clone(WEB_CHANNEL_INSTANCE.get_or_init(|| Arc::new(WebChannel::new())))
}

/// Get the global WebChannel instance if it exists.
pub fn get_web_channel() -> Option<Arc<WebChannel>> {
    WEB_CHANNEL_INSTANCE.get().map(Arc::clone)
}

/// Set the channel message sender (called from start_channels).
pub fn set_web_channel_tx(tx: tokio::sync::mpsc::Sender<crate::channels::traits::ChannelMessage>) {
    let _ = WEB_CHANNEL_TX.set(tx);
}

/// Get the channel message sender.
pub fn get_web_channel_tx()
-> Option<tokio::sync::mpsc::Sender<crate::channels::traits::ChannelMessage>> {
    WEB_CHANNEL_TX.get().cloned()
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON message types for WebSocket protocol
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Session init — must be the first message after WS connect.
    /// If session_id is provided, resumes that session (conversation history restored).
    /// If omitted, a new session is created and the server returns the generated session_id.
    #[serde(rename = "init")]
    Init {
        #[serde(default)]
        session_id: Option<String>,
    },
    #[serde(rename = "message")]
    Message {
        content: String,
        #[serde(default)]
        image_url: Option<String>,
    },
    #[serde(rename = "command")]
    Command { content: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Returned after Init — contains the session_id for this connection.
    #[serde(rename = "session")]
    Session { session_id: String },
    #[serde(rename = "typing")]
    Typing { active: bool },
    #[serde(rename = "draft")]
    Draft { content: String, message_id: String },
    #[serde(rename = "draft_update")]
    DraftUpdate { content: String, message_id: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        name: String,
        result: serde_json::Value,
    },
    #[serde(rename = "message")]
    Message { content: String, message_id: String },
    #[serde(rename = "reaction")]
    Reaction { emoji: String, action: String },
    #[serde(rename = "error")]
    Error { content: String },
    #[serde(rename = "pong")]
    Pong,
}

// ─────────────────────────────────────────────────────────────────────────────
// WebChannel struct + Channel trait implementation
// ─────────────────────────────────────────────────────────────────────────────

pub struct WebChannel {
    /// Per-session broadcast channels. Key = session_id.
    /// Multiple WS connections with the same session_id share one broadcast.
    sessions: Arc<RwLock<HashMap<String, broadcast::Sender<String>>>>,
    connection_count: Arc<AtomicUsize>,
}

impl WebChannel {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            connection_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connection_count.load(Ordering::Relaxed)
    }

    /// Send a message to a specific session, dropping if not connected.
    async fn send_to_session(&self, session_id: &str, msg: &ServerMessage) {
        if let Ok(json) = serde_json::to_string(msg) {
            let sessions = self.sessions.read().await;
            if let Some(tx) = sessions.get(session_id) {
                let _ = tx.send(json);
            }
            // Target session not connected — drop the message.
            // Same behavior as feishu/discord when chat_id is offline.
        }
    }
}

#[async_trait]
impl Channel for WebChannel {
    fn name(&self) -> &str {
        "Web"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.send_to_session(
            &message.recipient,
            &ServerMessage::Message {
                content: message.content.clone(),
                message_id: uuid::Uuid::new_v4().to_string(),
            },
        )
        .await;
        Ok(())
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Web channel receives messages via WS handler, not by actively listening.
        // Block until shutdown.
        tokio::signal::ctrl_c().await?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.send_to_session(recipient, &ServerMessage::Typing { active: true })
            .await;
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.send_to_session(recipient, &ServerMessage::Typing { active: false })
            .await;
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.send_to_session(
            &message.recipient,
            &ServerMessage::Draft {
                content: message.content.clone(),
                message_id: message_id.clone(),
            },
        )
        .await;
        Ok(Some(message_id))
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.send_to_session(
            recipient,
            &ServerMessage::DraftUpdate {
                content: text.to_string(),
                message_id: message_id.to_string(),
            },
        )
        .await;
        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.send_to_session(
            recipient,
            &ServerMessage::Message {
                content: text.to_string(),
                message_id: message_id.to_string(),
            },
        )
        .await;
        Ok(())
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        // Reactions are broadcast to all sessions (no recipient context in trait signature)
        let msg = ServerMessage::Reaction {
            emoji: emoji.to_string(),
            action: "add".to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let sessions = self.sessions.read().await;
            for tx in sessions.values() {
                let _ = tx.send(json.clone());
            }
        }
        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let msg = ServerMessage::Reaction {
            emoji: emoji.to_string(),
            action: "remove".to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let sessions = self.sessions.read().await;
            for tx in sessions.values() {
                let _ = tx.send(json.clone());
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WebSocket connection handler
// ─────────────────────────────────────────────────────────────────────────────

pub async fn handle_ws_connection(
    socket: WebSocket,
    channel: Arc<WebChannel>,
    message_tx: tokio::sync::mpsc::Sender<ChannelMessage>,
) {
    let (mut ws_sender, mut ws_receiver) = socket.split();
    channel.connection_count.fetch_add(1, Ordering::Relaxed);

    // ── Phase 1: Wait for Init message ──────────────────────────────────
    let session_id = loop {
        match ws_receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Init { session_id }) => {
                        let sid = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                        break sid;
                    }
                    Ok(_) => {
                        // Not an init message — tell client to init first
                        let err = serde_json::to_string(&ServerMessage::Error {
                            content: "First message must be {\"type\":\"init\"} or {\"type\":\"init\",\"session_id\":\"...\"}".to_string(),
                        })
                        .unwrap_or_default();
                        let _ = ws_sender.send(Message::Text(err.into())).await;
                    }
                    Err(_) => {
                        let err = serde_json::to_string(&ServerMessage::Error {
                            content: "Invalid JSON".to_string(),
                        })
                        .unwrap_or_default();
                        let _ = ws_sender.send(Message::Text(err.into())).await;
                    }
                }
            }
            Some(Ok(Message::Close(_)) | Err(_)) | None => {
                channel.connection_count.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            _ => {}
        }
    };

    // ── Phase 2: Register session and subscribe ─────────────────────────
    let broadcast_rx = {
        let mut sessions = channel.sessions.write().await;
        let tx = sessions
            .entry(session_id.clone())
            .or_insert_with(|| broadcast::channel(256).0);
        tx.subscribe()
    };
    let mut broadcast_rx = broadcast_rx;

    // Send session_id back to client
    let session_msg = serde_json::to_string(&ServerMessage::Session {
        session_id: session_id.clone(),
    })
    .unwrap_or_default();
    if ws_sender
        .send(Message::Text(session_msg.into()))
        .await
        .is_err()
    {
        channel.connection_count.fetch_sub(1, Ordering::Relaxed);
        return;
    }

    // ── Phase 3: Message loop ───────────────────────────────────────────
    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(ClientMessage::Message { content, .. }) => {
                                if content.trim().is_empty() {
                                    continue;
                                }
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let _ = message_tx.send(ChannelMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    sender: session_id.clone(),
                                    reply_target: session_id.clone(),
                                    content,
                                    channel: "Web".to_string(),
                                    timestamp: ts,
                                    thread_ts: None,
                                    interruption_scope_id: None,
                                    attachments: vec![],
                                }).await;
                            }
                            Ok(ClientMessage::Command { content }) => {
                                let ts = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let _ = message_tx.send(ChannelMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    sender: session_id.clone(),
                                    reply_target: session_id.clone(),
                                    content,
                                    channel: "Web".to_string(),
                                    timestamp: ts,
                                    thread_ts: None,
                                    interruption_scope_id: None,
                                    attachments: vec![],
                                }).await;
                            }
                            Ok(ClientMessage::Ping) => {
                                let pong = serde_json::to_string(&ServerMessage::Pong)
                                    .unwrap_or_default();
                                let _ = ws_sender.send(Message::Text(pong.into())).await;
                            }
                            Ok(ClientMessage::Init { .. }) => {
                                // Already initialized — ignore duplicate init
                            }
                            Err(_) => {
                                let err = serde_json::to_string(&ServerMessage::Error {
                                    content: "Invalid JSON".to_string(),
                                }).unwrap_or_default();
                                let _ = ws_sender.send(Message::Text(err.into())).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    _ => {},
                }
            }
            msg = broadcast_rx.recv() => {
                if let Ok(json_str) = msg {
                    if ws_sender.send(Message::Text(json_str.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // ── Cleanup ─────────────────────────────────────────────────────────
    channel.connection_count.fetch_sub(1, Ordering::Relaxed);

    // Remove session if no more subscribers
    let sessions = channel.sessions.read().await;
    if let Some(tx) = sessions.get(&session_id) {
        if tx.receiver_count() == 0 {
            drop(sessions);
            channel.sessions.write().await.remove(&session_id);
        }
    }
}
