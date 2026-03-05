use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::BridgeConfig;
use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct BridgeInboundFrame {
    #[serde(default)]
    r#type: String,
    id: Option<String>,
    sender: Option<String>,
    reply_target: Option<String>,
    content: Option<String>,
    timestamp: Option<u64>,
    thread_ts: Option<String>,
}

#[derive(Debug, Serialize)]
struct BridgeOutboundFrame<'a> {
    #[serde(rename = "type")]
    type_name: &'static str,
    recipient: &'a str,
    content: &'a str,
    subject: Option<&'a str>,
    thread_ts: Option<&'a str>,
}

/// Bridge websocket channel.
///
/// Protocol (JSON text frames):
/// - Client -> Server (message):
///   `{ "type":"message", "content":"...", "sender":"alice", "reply_target":"chat-1" }`
/// - Server -> Client (assistant reply):
///   `{ "type":"message", "recipient":"chat-1", "content":"..." }`
#[derive(Debug, Clone)]
pub struct BridgeChannel {
    config: BridgeConfig,
    peers: Arc<RwLock<HashMap<String, mpsc::UnboundedSender<String>>>>,
    routes: Arc<RwLock<HashMap<String, String>>>,
    connection_seq: Arc<AtomicU64>,
}

impl BridgeChannel {
    pub fn new(config: BridgeConfig) -> Self {
        Self {
            config,
            peers: Arc::new(RwLock::new(HashMap::new())),
            routes: Arc::new(RwLock::new(HashMap::new())),
            connection_seq: Arc::new(AtomicU64::new(1)),
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

    fn next_connection_id(&self) -> String {
        let seq = self.connection_seq.fetch_add(1, Ordering::Relaxed);
        format!("bridge-conn-{seq}")
    }

    fn now_unix_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn parse_inbound_text(
        payload: &str,
        connection_id: &str,
    ) -> anyhow::Result<Option<ChannelMessage>> {
        let frame: BridgeInboundFrame =
            serde_json::from_str(payload).context("invalid bridge inbound JSON")?;
        let frame_type = if frame.r#type.trim().is_empty() {
            "message"
        } else {
            frame.r#type.as_str()
        };

        if frame_type != "message" {
            return Ok(None);
        }

        let content = frame.content.unwrap_or_default();
        if content.trim().is_empty() {
            return Ok(None);
        }

        let sender = frame
            .sender
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| connection_id.to_string());
        let reply_target = frame
            .reply_target
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| sender.clone());

        Ok(Some(ChannelMessage {
            id: frame
                .id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| format!("bridge-{}", Uuid::new_v4())),
            sender,
            reply_target,
            content,
            channel: "bridge".to_string(),
            timestamp: frame.timestamp.unwrap_or_else(Self::now_unix_secs),
            thread_ts: frame.thread_ts,
        }))
    }

    fn build_outbound_text(message: &SendMessage) -> anyhow::Result<String> {
        let frame = BridgeOutboundFrame {
            type_name: "message",
            recipient: &message.recipient,
            content: &message.content,
            subject: message.subject.as_deref(),
            thread_ts: message.thread_ts.as_deref(),
        };
        serde_json::to_string(&frame).context("serialize bridge outbound message")
    }

    async fn sender_for_recipient(&self, recipient: &str) -> Option<mpsc::UnboundedSender<String>> {
        let route_id = {
            let routes = self.routes.read().await;
            routes.get(recipient).cloned()
        };
        if let Some(connection_id) = route_id {
            let peers = self.peers.read().await;
            if let Some(sender) = peers.get(&connection_id) {
                return Some(sender.clone());
            }
        }

        // Fallback to any active client so bridge can still be exercised with
        // a single connection even before explicit routing is established.
        let peers = self.peers.read().await;
        peers.values().next().cloned()
    }

    async fn remove_connection_state(&self, connection_id: &str) {
        {
            let mut peers = self.peers.write().await;
            peers.remove(connection_id);
        }
        {
            let mut routes = self.routes.write().await;
            routes.retain(|_, mapped| mapped != connection_id);
        }
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
        tx: mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        let ws_stream = accept_async(stream)
            .await
            .context("bridge websocket handshake failed")?;
        let connection_id = self.next_connection_id();
        let (mut ws_write, mut ws_read) = ws_stream.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

        {
            let mut peers = self.peers.write().await;
            peers.insert(connection_id.clone(), out_tx.clone());
        }

        tracing::info!(
            connection_id = %connection_id,
            endpoint = %self.endpoint_url(),
            "Bridge client connected"
        );

        let writer = tokio::spawn(async move {
            while let Some(payload) = out_rx.recv().await {
                if ws_write
                    .send(WsMessage::Text(payload.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        while let Some(frame) = ws_read.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => {
                    match Self::parse_inbound_text(&text, &connection_id) {
                        Ok(Some(message)) => {
                            {
                                let mut routes = self.routes.write().await;
                                routes.insert(message.reply_target.clone(), connection_id.clone());
                            }
                            if tx.send(message).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::warn!(
                                connection_id = %connection_id,
                                error = %err,
                                "Bridge dropped invalid inbound payload"
                            );
                        }
                    }
                }
                Ok(WsMessage::Close(_)) => break,
                Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) => {}
                Ok(WsMessage::Binary(_)) => {
                    tracing::warn!(
                        connection_id = %connection_id,
                        "Bridge ignores binary websocket frames"
                    );
                }
                Ok(WsMessage::Frame(_)) => {}
                Err(err) => {
                    tracing::warn!(
                        connection_id = %connection_id,
                        error = %err,
                        "Bridge websocket receive error"
                    );
                    break;
                }
            }
        }

        self.remove_connection_state(&connection_id).await;
        writer.abort();
        tracing::info!(connection_id = %connection_id, "Bridge client disconnected");
        Ok(())
    }
}

#[async_trait]
impl Channel for BridgeChannel {
    fn name(&self) -> &str {
        "bridge"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let payload = Self::build_outbound_text(message)?;
        match self.sender_for_recipient(&message.recipient).await {
            Some(sender) => sender
                .send(payload)
                .map_err(|_| anyhow::anyhow!("bridge client is disconnected")),
            None => anyhow::bail!(
                "no active bridge client for recipient `{}`",
                message.recipient
            ),
        }
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bind_addr = format!("{}:{}", self.config.bind_host, self.config.bind_port);
        let listener = TcpListener::bind(&bind_addr)
            .await
            .with_context(|| format!("bridge failed to bind listener at {bind_addr}"))?;

        tracing::info!(
            endpoint = %self.endpoint_url(),
            "Bridge websocket listener started"
        );

        loop {
            tokio::select! {
                _ = tx.closed() => {
                    tracing::info!("Bridge listener shutting down: channel dispatcher closed");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer_addr)) => {
                            tracing::debug!(peer = %peer_addr, "Bridge accepted websocket TCP connection");
                            let channel = self.clone();
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                if let Err(err) = channel.handle_connection(stream, tx).await {
                                    tracing::warn!(error = %err, "Bridge connection handler failed");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "Bridge listener accept failed");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        !self.config.bind_host.trim().is_empty()
            && self.config.bind_port > 0
            && self.config.path.starts_with('/')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_channel_name_and_endpoint_from_config() {
        let channel = BridgeChannel::new(BridgeConfig::default());

        assert_eq!(channel.name(), "bridge");
        assert_eq!(channel.endpoint_url(), "ws://127.0.0.1:8765/ws");
        assert_eq!(channel.config().bind_host, "127.0.0.1");
    }

    #[test]
    fn parse_inbound_message_defaults_sender_and_target() {
        let parsed = BridgeChannel::parse_inbound_text(
            r#"{"type":"message","content":"hello"}"#,
            "bridge-conn-1",
        )
        .expect("parse should succeed")
        .expect("message should be produced");

        assert_eq!(parsed.channel, "bridge");
        assert_eq!(parsed.sender, "bridge-conn-1");
        assert_eq!(parsed.reply_target, "bridge-conn-1");
        assert_eq!(parsed.content, "hello");
    }

    #[tokio::test]
    async fn send_routes_to_registered_recipient() {
        let channel = BridgeChannel::new(BridgeConfig::default());
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        channel
            .peers
            .write()
            .await
            .insert("bridge-conn-9".to_string(), tx);
        channel
            .routes
            .write()
            .await
            .insert("chat-1".to_string(), "bridge-conn-9".to_string());

        channel
            .send(&SendMessage::new("pong", "chat-1"))
            .await
            .expect("send should route to known recipient");

        let payload = rx.recv().await.expect("outbound payload");
        let json: serde_json::Value = serde_json::from_str(&payload).expect("valid json");
        assert_eq!(json["type"], "message");
        assert_eq!(json["recipient"], "chat-1");
        assert_eq!(json["content"], "pong");
    }
}
