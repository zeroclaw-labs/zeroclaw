use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::BridgeConfig;
use crate::security::pairing::{constant_time_eq, is_public_bind};
use anyhow::Context;
use async_trait::async_trait;
use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, RwLock, Semaphore},
};
use uuid::Uuid;

const AUTH_TIMEOUT_SECS: u64 = 15;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

type ConnectionId = Uuid;
type OutboundTx = mpsc::UnboundedSender<BridgeOutboundEvent>;

#[derive(Debug)]
struct BridgeRuntimeState {
    peers: RwLock<HashMap<String, HashMap<ConnectionId, OutboundTx>>>,
}

impl BridgeRuntimeState {
    fn new() -> Self {
        Self {
            peers: RwLock::new(HashMap::new()),
        }
    }

    async fn register_peer(&self, sender_id: &str, connection_id: ConnectionId, tx: OutboundTx) {
        let mut guard = self.peers.write().await;
        guard
            .entry(sender_id.to_string())
            .or_default()
            .insert(connection_id, tx);
    }

    async fn unregister_peer(&self, sender_id: &str, connection_id: ConnectionId) {
        let mut guard = self.peers.write().await;
        if let Some(connections) = guard.get_mut(sender_id) {
            connections.remove(&connection_id);
            if connections.is_empty() {
                guard.remove(sender_id);
            }
        }
    }

    async fn dispatch_to_sender(&self, sender_id: &str, event: BridgeOutboundEvent) -> usize {
        let mut guard = self.peers.write().await;
        let Some(connections) = guard.get_mut(sender_id) else {
            return 0;
        };

        let mut delivered = 0usize;
        let mut stale_ids = Vec::new();
        for (connection_id, tx) in connections.iter() {
            if tx.send(event.clone()).is_ok() {
                delivered += 1;
            } else {
                stale_ids.push(*connection_id);
            }
        }

        for connection_id in stale_ids {
            connections.remove(&connection_id);
        }

        if connections.is_empty() {
            guard.remove(sender_id);
        }

        delivered
    }
}

#[derive(Clone)]
struct BridgeAppState {
    runtime: Arc<BridgeRuntimeState>,
    inbound_tx: mpsc::Sender<ChannelMessage>,
    auth_token: String,
    allowed_senders: Vec<String>,
    endpoint_url: String,
    connection_permits: Arc<Semaphore>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeInboundEvent {
    Auth {
        token: String,
        sender_id: String,
    },
    Message {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        sender_id: Option<String>,
        content: String,
        #[serde(default)]
        thread_ts: Option<String>,
    },
    Ping {
        #[serde(default)]
        nonce: Option<String>,
    },
    Pong {
        #[serde(default)]
        nonce: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeOutboundEvent {
    Ready {
        sender_id: String,
        endpoint: String,
    },
    Error {
        code: String,
        message: String,
    },
    Message {
        id: String,
        recipient: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        subject: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_ts: Option<String>,
    },
    Typing {
        recipient: String,
        active: bool,
    },
    Draft {
        recipient: String,
        message_id: String,
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    ApprovalPrompt {
        recipient: String,
        request_id: String,
        tool_name: String,
        arguments: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        thread_ts: Option<String>,
    },
    Reaction {
        action: String,
        channel_id: String,
        message_id: String,
        emoji: String,
    },
    Ack {
        id: String,
    },
    Pong {
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
    },
}

/// Generic websocket bridge channel for third-party integrations.
#[derive(Debug, Clone)]
pub struct BridgeChannel {
    config: BridgeConfig,
    runtime: Arc<BridgeRuntimeState>,
}

impl BridgeChannel {
    pub fn new(mut config: BridgeConfig) -> Self {
        config.path = normalize_path(&config.path);
        Self {
            config,
            runtime: Arc::new(BridgeRuntimeState::new()),
        }
    }

    #[must_use]
    pub fn config(&self) -> &BridgeConfig {
        &self.config
    }

    #[must_use]
    pub fn endpoint_url(&self) -> String {
        format!(
            "ws://{}:{}{}",
            self.config.bind_host, self.config.bind_port, self.config.path
        )
    }

    fn validate_config(&self) -> anyhow::Result<()> {
        if self.config.bind_host.trim().is_empty() {
            anyhow::bail!("Bridge bind_host must not be empty");
        }
        if self.config.bind_port == 0 {
            anyhow::bail!("Bridge bind_port must be greater than 0");
        }
        if self.config.max_connections == 0 {
            anyhow::bail!("Bridge max_connections must be greater than 0");
        }
        if self.config.auth_token.trim().is_empty() {
            anyhow::bail!(
                "Bridge auth_token is required. Set [channels_config.bridge].auth_token to enable authenticated bridge clients."
            );
        }

        if is_public_bind(self.config.bind_host.trim()) && !self.config.allow_public_bind {
            anyhow::bail!(
                "Bridge bind_host '{}' is public; set allow_public_bind = true to opt in.",
                self.config.bind_host
            );
        }

        if !self.config.path.starts_with('/') {
            anyhow::bail!("Bridge path must start with '/'");
        }

        Ok(())
    }

    async fn dispatch_event(
        &self,
        recipient: &str,
        event: BridgeOutboundEvent,
        require_delivery: bool,
    ) -> anyhow::Result<()> {
        let recipient = recipient.trim();
        if recipient.is_empty() {
            anyhow::bail!("Bridge recipient is empty");
        }

        let delivered = self.runtime.dispatch_to_sender(recipient, event).await;
        if require_delivery && delivered == 0 {
            anyhow::bail!("No active bridge connection for recipient '{recipient}'");
        }

        Ok(())
    }
}

#[async_trait]
impl Channel for BridgeChannel {
    fn name(&self) -> &str {
        "bridge"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        self.dispatch_event(
            &message.recipient,
            BridgeOutboundEvent::Message {
                id: Uuid::new_v4().to_string(),
                recipient: message.recipient.clone(),
                content: message.content.clone(),
                subject: message.subject.clone(),
                thread_ts: message.thread_ts.clone(),
            },
            true,
        )
        .await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        self.validate_config()?;

        let bind_addr = format!("{}:{}", self.config.bind_host, self.config.bind_port);
        let listener = TcpListener::bind(&bind_addr)
            .await
            .with_context(|| format!("Failed to bind bridge listener on {bind_addr}"))?;

        let app_state = Arc::new(BridgeAppState {
            runtime: Arc::clone(&self.runtime),
            inbound_tx: tx.clone(),
            auth_token: self.config.auth_token.clone(),
            allowed_senders: normalize_allowed_senders(&self.config.allowed_senders),
            endpoint_url: self.endpoint_url(),
            connection_permits: Arc::new(Semaphore::new(self.config.max_connections)),
        });

        let route_path = self.config.path.clone();
        let app = Router::new()
            .route(route_path.as_str(), get(bridge_ws_upgrade))
            .with_state(app_state);

        tracing::info!(
            endpoint = %self.endpoint_url(),
            max_connections = self.config.max_connections,
            "Bridge websocket listener started"
        );

        let serve_result = axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                tx.closed().await;
            })
            .await;

        serve_result.context("Bridge websocket listener exited unexpectedly")?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.validate_config().is_ok()
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::Typing {
                recipient: recipient.to_string(),
                active: true,
            },
            false,
        )
        .await
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::Typing {
                recipient: recipient.to_string(),
                active: false,
            },
            false,
        )
        .await
    }

    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        let message_id = Uuid::new_v4().to_string();
        self.dispatch_event(
            &message.recipient,
            BridgeOutboundEvent::Draft {
                recipient: message.recipient.clone(),
                message_id: message_id.clone(),
                event: "start".to_string(),
                text: Some(message.content.clone()),
            },
            false,
        )
        .await?;
        Ok(Some(message_id))
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<Option<String>> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::Draft {
                recipient: recipient.to_string(),
                message_id: message_id.to_string(),
                event: "update".to_string(),
                text: Some(text.to_string()),
            },
            false,
        )
        .await?;
        Ok(None)
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::Draft {
                recipient: recipient.to_string(),
                message_id: message_id.to_string(),
                event: "finalize".to_string(),
                text: Some(text.to_string()),
            },
            false,
        )
        .await
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::Draft {
                recipient: recipient.to_string(),
                message_id: message_id.to_string(),
                event: "cancel".to_string(),
                text: None,
            },
            false,
        )
        .await
    }

    async fn send_approval_prompt(
        &self,
        recipient: &str,
        request_id: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
        thread_ts: Option<String>,
    ) -> anyhow::Result<()> {
        self.dispatch_event(
            recipient,
            BridgeOutboundEvent::ApprovalPrompt {
                recipient: recipient.to_string(),
                request_id: request_id.to_string(),
                tool_name: tool_name.to_string(),
                arguments: arguments.clone(),
                thread_ts,
            },
            false,
        )
        .await
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.dispatch_event(
            channel_id,
            BridgeOutboundEvent::Reaction {
                action: "add".to_string(),
                channel_id: channel_id.to_string(),
                message_id: message_id.to_string(),
                emoji: emoji.to_string(),
            },
            false,
        )
        .await
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        self.dispatch_event(
            channel_id,
            BridgeOutboundEvent::Reaction {
                action: "remove".to_string(),
                channel_id: channel_id.to_string(),
                message_id: message_id.to_string(),
                emoji: emoji.to_string(),
            },
            false,
        )
        .await
    }
}

async fn bridge_ws_upgrade(
    State(state): State<Arc<BridgeAppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Err(error) = handle_bridge_socket(socket, state).await {
            tracing::warn!("Bridge websocket session error: {error}");
        }
    })
}

async fn handle_bridge_socket(
    mut socket: WebSocket,
    state: Arc<BridgeAppState>,
) -> anyhow::Result<()> {
    let permit = match Arc::clone(&state.connection_permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            let _ = send_direct_event(
                &mut socket,
                BridgeOutboundEvent::Error {
                    code: "connection_limit".to_string(),
                    message: "Bridge connection limit reached".to_string(),
                },
            )
            .await;
            let _ = socket.close().await;
            return Ok(());
        }
    };

    let (mut ws_sink, mut ws_stream) = socket.split();

    let auth_msg = tokio::time::timeout(Duration::from_secs(AUTH_TIMEOUT_SECS), ws_stream.next())
        .await
        .context("Timed out waiting for bridge auth message")?;

    let Some(first_frame) = auth_msg else {
        return Ok(());
    };

    let first_frame = first_frame.context("Bridge websocket read failed before auth")?;
    let auth = match first_frame {
        WsMessage::Text(text) => parse_inbound_event(&text).ok(),
        WsMessage::Close(_) => return Ok(()),
        _ => None,
    };

    let Some(BridgeInboundEvent::Auth { token, sender_id }) = auth else {
        let _ = send_via_sink(
            &mut ws_sink,
            BridgeOutboundEvent::Error {
                code: "auth_required".to_string(),
                message: "First bridge message must be an auth event".to_string(),
            },
        )
        .await;
        let _ = ws_sink.send(WsMessage::Close(None)).await;
        return Ok(());
    };

    let sender_id = sender_id.trim().to_string();
    if sender_id.is_empty() {
        let _ = send_via_sink(
            &mut ws_sink,
            BridgeOutboundEvent::Error {
                code: "invalid_sender".to_string(),
                message: "sender_id must not be empty".to_string(),
            },
        )
        .await;
        let _ = ws_sink.send(WsMessage::Close(None)).await;
        return Ok(());
    }

    if !constant_time_eq(token.trim(), state.auth_token.trim()) {
        let _ = send_via_sink(
            &mut ws_sink,
            BridgeOutboundEvent::Error {
                code: "unauthorized".to_string(),
                message: "Invalid bridge auth token".to_string(),
            },
        )
        .await;
        let _ = ws_sink.send(WsMessage::Close(None)).await;
        return Ok(());
    }

    if !sender_is_allowed(&state.allowed_senders, &sender_id) {
        let _ = send_via_sink(
            &mut ws_sink,
            BridgeOutboundEvent::Error {
                code: "forbidden_sender".to_string(),
                message: "sender_id is not allowlisted".to_string(),
            },
        )
        .await;
        let _ = ws_sink.send(WsMessage::Close(None)).await;
        return Ok(());
    }

    let connection_id = Uuid::new_v4();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    state
        .runtime
        .register_peer(&sender_id, connection_id, out_tx)
        .await;

    send_via_sink(
        &mut ws_sink,
        BridgeOutboundEvent::Ready {
            sender_id: sender_id.clone(),
            endpoint: state.endpoint_url.clone(),
        },
    )
    .await
    .context("Failed sending bridge ready event")?;

    tracing::info!(
        sender_id = %sender_id,
        connection_id = %connection_id,
        "Bridge websocket client authenticated"
    );

    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));

    loop {
        tokio::select! {
            maybe_outbound = out_rx.recv() => {
                let Some(event) = maybe_outbound else {
                    break;
                };
                if send_via_sink(&mut ws_sink, event).await.is_err() {
                    break;
                }
            }
            _ = heartbeat.tick() => {
                if ws_sink.send(WsMessage::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            maybe_inbound = ws_stream.next() => {
                let Some(inbound_result) = maybe_inbound else {
                    break;
                };

                let inbound = match inbound_result {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::warn!(sender_id = %sender_id, "Bridge websocket read failed: {error}");
                        break;
                    }
                };

                match inbound {
                    WsMessage::Text(text) => {
                        let Ok(event) = parse_inbound_event(&text) else {
                            let _ = send_via_sink(
                                &mut ws_sink,
                                BridgeOutboundEvent::Error {
                                    code: "invalid_payload".to_string(),
                                    message: "Bridge inbound payload must be valid JSON".to_string(),
                                },
                            ).await;
                            continue;
                        };

                        match event {
                            BridgeInboundEvent::Auth { .. } => {
                                let _ = send_via_sink(
                                    &mut ws_sink,
                                    BridgeOutboundEvent::Error {
                                        code: "already_authenticated".to_string(),
                                        message: "Auth event is only valid as the first frame".to_string(),
                                    },
                                ).await;
                            }
                            BridgeInboundEvent::Message {
                                id,
                                sender_id: claimed_sender,
                                content,
                                thread_ts,
                            } => {
                                if let Some(claimed_sender) = claimed_sender
                                    .map(|value| value.trim().to_string())
                                    .filter(|value| !value.is_empty())
                                {
                                    if !claimed_sender.eq_ignore_ascii_case(&sender_id) {
                                        let _ = send_via_sink(
                                            &mut ws_sink,
                                            BridgeOutboundEvent::Error {
                                                code: "sender_mismatch".to_string(),
                                                message: "sender_id must match authenticated sender".to_string(),
                                            },
                                        ).await;
                                        continue;
                                    }
                                }

                                if content.trim().is_empty() {
                                    continue;
                                }

                                let message_id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
                                let channel_message = ChannelMessage {
                                    id: message_id.clone(),
                                    sender: sender_id.clone(),
                                    reply_target: sender_id.clone(),
                                    content,
                                    channel: "bridge".to_string(),
                                    timestamp: unix_timestamp_secs(),
                                    thread_ts,
                                };

                                if state.inbound_tx.send(channel_message).await.is_err() {
                                    break;
                                }

                                let _ = send_via_sink(
                                    &mut ws_sink,
                                    BridgeOutboundEvent::Ack { id: message_id },
                                ).await;
                            }
                            BridgeInboundEvent::Ping { nonce } => {
                                let _ = send_via_sink(&mut ws_sink, BridgeOutboundEvent::Pong { nonce }).await;
                            }
                            BridgeInboundEvent::Pong { nonce: _ } => {
                                // Heartbeat acknowledgement from client.
                            }
                        }
                    }
                    WsMessage::Ping(payload) => {
                        if ws_sink.send(WsMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    WsMessage::Pong(_) => {
                        // Native websocket heartbeat acknowledgement.
                    }
                    WsMessage::Close(_) => {
                        break;
                    }
                    WsMessage::Binary(_) => {
                        let _ = send_via_sink(
                            &mut ws_sink,
                            BridgeOutboundEvent::Error {
                                code: "unsupported_binary".to_string(),
                                message: "Binary websocket messages are not supported by bridge".to_string(),
                            },
                        ).await;
                    }
                }
            }
        }
    }

    state
        .runtime
        .unregister_peer(&sender_id, connection_id)
        .await;
    drop(permit);

    tracing::info!(
        sender_id = %sender_id,
        connection_id = %connection_id,
        "Bridge websocket client disconnected"
    );

    Ok(())
}

async fn send_direct_event(
    socket: &mut WebSocket,
    event: BridgeOutboundEvent,
) -> anyhow::Result<()> {
    let payload = serde_json::to_string(&event)?;
    socket.send(WsMessage::Text(payload.into())).await?;
    Ok(())
}

async fn send_via_sink<S>(sink: &mut S, event: BridgeOutboundEvent) -> anyhow::Result<()>
where
    S: futures_util::Sink<WsMessage, Error = axum::Error> + Unpin,
{
    let payload = serde_json::to_string(&event)?;
    sink.send(WsMessage::Text(payload.into())).await?;
    Ok(())
}

fn parse_inbound_event(text: &str) -> anyhow::Result<BridgeInboundEvent> {
    serde_json::from_str::<BridgeInboundEvent>(text)
        .with_context(|| "Failed to parse bridge inbound JSON event")
}

fn normalize_path(raw_path: &str) -> String {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return "/ws".to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn normalize_allowed_senders(entries: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            normalized.push(trimmed.to_string());
        }
    }

    normalized
}

fn sender_is_allowed(allowlist: &[String], sender_id: &str) -> bool {
    if allowlist.is_empty() {
        return false;
    }

    allowlist
        .iter()
        .any(|entry| entry == "*" || entry.eq_ignore_ascii_case(sender_id))
}

fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_channel_name_and_endpoint_from_config() {
        let mut cfg = BridgeConfig::default();
        cfg.auth_token = "token".to_string();
        let channel = BridgeChannel::new(cfg);

        assert_eq!(channel.name(), "bridge");
        assert_eq!(channel.endpoint_url(), "ws://127.0.0.1:8765/ws");
        assert_eq!(channel.config().bind_host, "127.0.0.1");
    }

    #[test]
    fn bridge_path_is_normalized_to_leading_slash() {
        let mut cfg = BridgeConfig::default();
        cfg.auth_token = "token".to_string();
        cfg.path = "bridge/ws".to_string();

        let channel = BridgeChannel::new(cfg);
        assert_eq!(channel.endpoint_url(), "ws://127.0.0.1:8765/bridge/ws");
    }

    #[tokio::test]
    async fn bridge_health_check_rejects_public_bind_without_opt_in() {
        let mut cfg = BridgeConfig::default();
        cfg.auth_token = "token".to_string();
        cfg.bind_host = "0.0.0.0".to_string();
        cfg.allow_public_bind = false;

        let channel = BridgeChannel::new(cfg);
        assert!(!channel.health_check().await);
    }

    #[test]
    fn sender_allowlist_is_deny_by_default_and_supports_wildcard() {
        assert!(!sender_is_allowed(&[], "alice"));
        assert!(!sender_is_allowed(&["bob".to_string()], "alice"));
        assert!(sender_is_allowed(&["*".to_string()], "alice"));
        assert!(sender_is_allowed(&["Alice".to_string()], "alice"));
    }
}
