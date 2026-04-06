//! Mock WebSocket node client for E2E testing of the node protocol.

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use zeroclaw::gateway::nodes::NodeCapability;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A simulated node that connects to the gateway via WebSocket.
pub struct MockNode {
    pub node_id: String,
    write: SplitSink<WsStream, Message>,
    read: SplitStream<WsStream>,
}

#[derive(Serialize)]
struct RegisterMsg {
    r#type: &'static str,
    node_id: String,
    capabilities: Vec<NodeCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_type: Option<String>,
}

#[derive(Serialize)]
struct ResultMsg {
    r#type: &'static str,
    call_id: String,
    success: bool,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// An invocation request received from the gateway.
#[derive(Debug, Deserialize)]
pub struct InvokeMsg {
    pub call_id: String,
    pub capability: String,
    pub args: serde_json::Value,
}

impl MockNode {
    /// Connect to the gateway's `/ws/nodes` endpoint without auth.
    pub async fn connect(ws_url: &str) -> Self {
        let (ws, _) = connect_async(ws_url)
            .await
            .expect("MockNode: failed to connect");
        let (write, read) = ws.split();
        Self {
            node_id: String::new(),
            write,
            read,
        }
    }

    /// Connect with a bearer token via query parameter.
    pub async fn connect_with_token(ws_url: &str, token: &str) -> Self {
        let url = if ws_url.contains('?') {
            format!("{ws_url}&token={token}")
        } else {
            format!("{ws_url}?token={token}")
        };
        let (ws, _) = connect_async(&url)
            .await
            .expect("MockNode: failed to connect with token");
        let (write, read) = ws.split();
        Self {
            node_id: String::new(),
            write,
            read,
        }
    }

    /// Attempt to connect — returns Err if the connection is refused/rejected.
    pub async fn try_connect_with_token(
        ws_url: &str,
        token: &str,
    ) -> Result<Self, tokio_tungstenite::tungstenite::Error> {
        let url = if ws_url.contains('?') {
            format!("{ws_url}&token={token}")
        } else {
            format!("{ws_url}?token={token}")
        };
        let (ws, _) = connect_async(&url).await?;
        let (write, read) = ws.split();
        Ok(Self {
            node_id: String::new(),
            write,
            read,
        })
    }

    /// Send a register message to the gateway.
    pub async fn register(
        &mut self,
        node_id: &str,
        capabilities: Vec<NodeCapability>,
        device_type: Option<&str>,
    ) {
        self.node_id = node_id.to_string();
        let msg = RegisterMsg {
            r#type: "register",
            node_id: node_id.to_string(),
            capabilities,
            device_type: device_type.map(String::from),
        };
        let json = serde_json::to_string(&msg).unwrap();
        self.write
            .send(Message::Text(json.into()))
            .await
            .expect("MockNode: failed to send register");
    }

    /// Wait for an invoke message from the gateway, with a timeout.
    pub async fn wait_for_invoke(&mut self, timeout: Duration) -> Option<InvokeMsg> {
        let result = tokio::time::timeout(timeout, async {
            while let Some(Ok(msg)) = self.read.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(invoke) = serde_json::from_str::<InvokeMsg>(text.as_ref()) {
                        return Some(invoke);
                    }
                }
            }
            None
        })
        .await;
        result.unwrap_or(None)
    }

    /// Send an invocation result back to the gateway.
    pub async fn send_result(&mut self, call_id: &str, success: bool, output: &str) {
        let msg = ResultMsg {
            r#type: "result",
            call_id: call_id.to_string(),
            success,
            output: output.to_string(),
            error: if success {
                None
            } else {
                Some(output.to_string())
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        self.write
            .send(Message::Text(json.into()))
            .await
            .expect("MockNode: failed to send result");
    }

    /// Close the WebSocket connection.
    pub async fn disconnect(mut self) {
        let _ = self.write.close().await;
    }
}
