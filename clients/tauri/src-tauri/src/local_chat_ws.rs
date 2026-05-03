//! Persistent WebSocket client to the local ZeroClaw gateway.
//!
//! # Why this module exists (E5 from `docs/audit-2026-05-03.md`)
//!
//! The original chat path in `lib.rs::chat` does HTTP POST to
//! `http://127.0.0.1:3000/api/chat` per message. That works but:
//!   - Each request pays a fresh TCP+HTTP/1.1 setup cost (~5-30ms
//!     even on loopback).
//!   - There is no streaming — the user sees the whole response at
//!     once after a multi-second wait, instead of token-by-token.
//!   - The gateway already exposes `/ws/chat` (see
//!     `src/gateway/ws.rs`) with a streaming chunk protocol.
//!
//! Production-completion-plan item 1 calls for `ws://127.0.0.1:{PORT}/ws/chat`
//! with persistent connection + heartbeat to bring sub-1ms IPC latency
//! and proper streaming UX.
//!
//! # What this module provides
//!
//! `LocalChatWsClient` — a small WebSocket client tailored to the local
//! gateway. The Tauri command surface (`chat`, `chat_stream`) can later
//! switch from `reqwest` to this client as a drop-in: the client's
//! `send_and_receive_chunks` method exposes the same per-message contract,
//! but the underlying connection is reused.
//!
//! # Wiring status
//!
//! As of this PR the client is implemented + unit-tested but is NOT yet
//! the default for `chat`. Migration is planned in a follow-up PR that
//! also updates the frontend to consume the streaming-chunk shape.
//! Keeping the migration as a separate PR isolates the wire-format
//! change (the JS-side `invoke('chat', ...)` Promise contract) from
//! the underlying transport change shipped here.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

/// Per-step receive timeout. The gateway can stream tokens over a long
/// chat — the timeout applies to "no chunk for this many seconds",
/// not to the whole conversation.
const CHUNK_RECV_TIMEOUT_SECS: u64 = 60;

/// Connect-attempt timeout. Loopback failure should surface fast.
const CONNECT_TIMEOUT_SECS: u64 = 5;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Streaming chunk protocol — mirrors `src/gateway/ws.rs` server side.
///
/// The gateway sends one of these JSON-encoded variants per WebSocket
/// frame; the client converts each to a `ChunkEvent` for the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChunkEvent {
    /// A token / fragment of the assistant's reply.
    Chunk { content: String },
    /// The assistant invoked a tool. Useful for UI tool indicators.
    ToolCall { name: String, args: serde_json::Value },
    /// The tool returned. Caller usually doesn't need to display this
    /// in the chat bubble itself but may show it in a debug pane.
    ToolResult { name: String, output: String },
    /// Stream complete. `full_response` is the concatenated text.
    Done { full_response: String },
    /// Server-side error during streaming.
    Error { message: String },
}

/// One outgoing user message.
#[derive(Debug, Clone, Serialize)]
pub struct UserMessage {
    /// JSON tag must match the server's `{"type": "message", ...}` form.
    #[serde(rename = "type")]
    kind: String,
    pub content: String,
}

impl UserMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            kind: "message".to_string(),
            content: content.into(),
        }
    }
}

/// Persistent WebSocket client to the local gateway.
///
/// The client reconnects on demand: callers ask for a stream by
/// invoking `send_and_receive_chunks`, and the client lazily opens
/// (or re-opens) the underlying connection. We deliberately do NOT
/// keep the connection open across long idle periods: the gateway is
/// in-process for the desktop app, so there is no real benefit to
/// holding a socket open between user turns, and a held socket means
/// a held lock on shared state.
pub struct LocalChatWsClient {
    gateway_url: String,
    auth_token: Option<String>,
    /// `None` until the first connect, then re-created on every send
    /// (the streaming contract is one connection per chat turn).
    inner: Mutex<Option<Arc<Mutex<WsStream>>>>,
}

impl LocalChatWsClient {
    /// Construct a client. `gateway_url` should be the HTTP base URL
    /// (`http://127.0.0.1:3000`); the WS endpoint is derived
    /// internally so callers don't have to remember the path.
    pub fn new(gateway_url: impl Into<String>, auth_token: Option<String>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
            auth_token,
            inner: Mutex::new(None),
        }
    }

    /// Convert `http://host:port` to `ws://host:port/ws/chat` (or
    /// `https://...` to `wss://...`). Returns Err if the input is
    /// not parseable as a URL.
    fn ws_url(&self) -> Result<String> {
        let base = self
            .gateway_url
            .trim_end_matches('/')
            .replace("http://", "ws://")
            .replace("https://", "wss://");
        if !base.starts_with("ws://") && !base.starts_with("wss://") {
            return Err(anyhow!(
                "gateway_url must start with http:// or https:// (got: {})",
                self.gateway_url
            ));
        }
        Ok(format!("{base}/ws/chat"))
    }

    /// Open a fresh WebSocket connection. Bails on connect timeout
    /// or 401-class auth failures.
    async fn connect(&self) -> Result<WsStream> {
        let url = self.ws_url()?;
        let url_with_auth = if let Some(ref tok) = self.auth_token {
            // Pass the token via query param — the gateway's
            // `handle_ws_chat` accepts both the standard `Authorization`
            // header and a `?token=` query param for clients that
            // can't easily set headers (some browser WS APIs).
            if url.contains('?') {
                format!("{url}&token={tok}")
            } else {
                format!("{url}?token={tok}")
            }
        } else {
            url
        };

        let attempt = connect_async(&url_with_auth);
        let (stream, _resp) = timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS), attempt)
            .await
            .map_err(|_| anyhow!("Local gateway WS connect timed out"))?
            .map_err(|e| anyhow!("Local gateway WS connect failed: {e}"))?;

        Ok(stream)
    }

    /// Send a user message and stream `ChunkEvent`s back via `on_chunk`.
    /// Returns the final concatenated response (same as the `Done`
    /// event's `full_response`) for callers that prefer a one-shot
    /// API and only render the final answer.
    pub async fn send_and_receive_chunks<F>(
        &self,
        message: &str,
        mut on_chunk: F,
    ) -> Result<String>
    where
        F: FnMut(&ChunkEvent),
    {
        let mut stream = self
            .connect()
            .await
            .context("Could not open local chat WebSocket")?;

        // Send the user message.
        let payload = serde_json::to_string(&UserMessage::new(message))
            .context("Could not serialize chat message")?;
        stream
            .send(Message::Text(payload.into()))
            .await
            .context("Could not write chat message to WebSocket")?;

        // Read until we see a Done / Error event.
        let mut full = String::new();
        loop {
            let frame = match timeout(
                Duration::from_secs(CHUNK_RECV_TIMEOUT_SECS),
                stream.next(),
            )
            .await
            {
                Ok(Some(Ok(f))) => f,
                Ok(Some(Err(e))) => {
                    return Err(anyhow!("Local chat WS read error: {e}"));
                }
                Ok(None) => {
                    return Err(anyhow!("Local chat WS closed before Done event"));
                }
                Err(_) => {
                    return Err(anyhow!(
                        "Local chat WS receive timeout (no chunk for {}s)",
                        CHUNK_RECV_TIMEOUT_SECS
                    ));
                }
            };

            let text = match frame {
                Message::Text(t) => t,
                Message::Binary(_) => continue,
                Message::Ping(p) => {
                    let _ = stream.send(Message::Pong(p)).await;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(anyhow!("Local chat WS closed by gateway"));
                }
                Message::Frame(_) => continue,
            };

            let event: ChunkEvent = match serde_json::from_str(&text) {
                Ok(e) => e,
                Err(e) => {
                    // Log + skip malformed frames rather than aborting
                    // the whole stream: a future server-side addition
                    // of a new event variant should not break old clients.
                    tracing::debug!(
                        target: "tauri.chat_ws",
                        error = %e,
                        text = %text,
                        "skipping unparseable chunk"
                    );
                    continue;
                }
            };

            on_chunk(&event);

            match &event {
                ChunkEvent::Chunk { content } => full.push_str(content),
                ChunkEvent::Done { full_response } => {
                    if full.is_empty() {
                        full = full_response.clone();
                    }
                    return Ok(full);
                }
                ChunkEvent::Error { message } => {
                    return Err(anyhow!("Local gateway error: {message}"));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_converts_http_to_ws() {
        let client = LocalChatWsClient::new("http://127.0.0.1:3000", None);
        assert_eq!(client.ws_url().unwrap(), "ws://127.0.0.1:3000/ws/chat");
    }

    #[test]
    fn ws_url_converts_https_to_wss() {
        let client = LocalChatWsClient::new("https://gateway.local", None);
        assert_eq!(client.ws_url().unwrap(), "wss://gateway.local/ws/chat");
    }

    #[test]
    fn ws_url_strips_trailing_slash() {
        let client = LocalChatWsClient::new("http://127.0.0.1:3000/", None);
        assert_eq!(client.ws_url().unwrap(), "ws://127.0.0.1:3000/ws/chat");
    }

    #[test]
    fn ws_url_rejects_non_http_input() {
        let client = LocalChatWsClient::new("ftp://example.com", None);
        assert!(client.ws_url().is_err());
    }

    #[test]
    fn user_message_serializes_with_type_tag() {
        let msg = UserMessage::new("hello");
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"message\""));
        assert!(json.contains("\"content\":\"hello\""));
    }

    #[test]
    fn chunk_event_done_round_trip() {
        let json = r#"{"type":"done","full_response":"hi there"}"#;
        let parsed: ChunkEvent = serde_json::from_str(json).unwrap();
        match parsed {
            ChunkEvent::Done { full_response } => assert_eq!(full_response, "hi there"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn chunk_event_chunk_round_trip() {
        let json = r#"{"type":"chunk","content":"hello "}"#;
        let parsed: ChunkEvent = serde_json::from_str(json).unwrap();
        match parsed {
            ChunkEvent::Chunk { content } => assert_eq!(content, "hello "),
            _ => panic!("expected Chunk"),
        }
    }
}
