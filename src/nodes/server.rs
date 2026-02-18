//! Node Server - WebSocket server for managing remote nodes
//!
//! The node server accepts reverse WebSocket connections from remote nodes,
//! handles pairing via 6-digit codes, and routes commands to connected nodes.

use crate::config::schema::NodesConfig;
use crate::nodes::types::{
    NodeCommand, NodeInfo, NodeResponse, PairingRequest, PairingResponse,
};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        Message,
    },
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const HEARTBEAT_TIMEOUT_SECS: u64 = 180; // 3分钟无响应视为断开

/// Represents a connected node with its WebSocket connection
#[derive(Clone)]
struct NodeConnection {
    info: NodeInfo,
    sender: mpsc::UnboundedSender<Message>,
    last_seen: Arc<RwLock<Instant>>,
}

/// Node Server - manages WebSocket connections from remote nodes
pub struct NodeServer {
    config: NodesConfig,
    nodes: Arc<RwLock<HashMap<String, NodeConnection>>>,
    pending_requests: Arc<RwLock<HashMap<u64, oneshot::Sender<NodeResponse>>>>,
    next_request_id: Arc<std::sync::atomic::AtomicU64>,
    pairing_code: Arc<RwLock<Option<String>>>,
    pairing_expiry: Arc<RwLock<Option<Instant>>>,
}

impl NodeServer {
    /// Create a new node server with the given configuration
    pub fn new(config: NodesConfig) -> Self {
        Self {
            config,
            nodes: Arc::new(RwLock::new(HashMap::new())),
            pending_requests: Arc::new(RwLock::new(HashMap::new())),
            next_request_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            pairing_code: Arc::new(RwLock::new(None)),
            pairing_expiry: Arc::new(RwLock::new(None)),
        }
    }

    /// Start the WebSocket server
    pub async fn start(&self) -> Result<()> {
        let addr = format!("0.0.0.0:{}", self.config.listen_port);
        let listener = TcpListener::bind(&addr)
            .await
            .context(format!("Failed to bind to {addr}"))?;

        tracing::info!("Node server listening on {}", addr);

        // Start heartbeat checker task
        let server_for_heartbeat = Arc::new(self.clone_for_handler());
        tokio::spawn(async move {
            server_for_heartbeat.heartbeat_checker().await;
        });

        while let Ok((stream, peer_addr)) = listener.accept().await {
            tracing::info!("New connection from {}", peer_addr);

            let server = self.clone_for_handler();
            tokio::spawn(async move {
                if let Err(e) = server.handle_connection(stream).await {
                    tracing::error!("Connection error: {}", e);
                }
            });
        }

        Ok(())
    }

    /// Generate a new 6-digit pairing code
    pub fn generate_pairing_code(&self) -> String {
        use rand::Rng;

        let code: String = (0..6)
            .map(|_| rand::rng().random_range(0..10))
            .map(|d| char::from_digit(d, 10).unwrap())
            .collect();

        let expiry = Instant::now() + Duration::from_secs(self.config.pairing_timeout_secs);
        *self.pairing_code.write() = Some(code.clone());
        *self.pairing_expiry.write() = Some(expiry);

        tracing::info!("Generated pairing code: {} (expires in {}s)", code, self.config.pairing_timeout_secs);

        code
    }

    /// List all connected nodes
    pub fn list_nodes(&self) -> Vec<NodeInfo> {
        self.nodes
            .read()
            .values()
            .map(|node| node.info.clone())
            .collect()
    }

    /// Run a command on a specific node
    pub async fn run_command(
        &self,
        node_id: &str,
        command: &str,
        timeout_secs: Option<u32>,
    ) -> Result<NodeResponse> {
        // Get sender from node - release lock before await
        let sender = {
            let nodes = self.nodes.read();
            let node = nodes
                .get(node_id)
                .context(format!("Node '{}' not found", node_id))?;
            node.sender.clone()
        };

        // Generate unique request ID
        let request_id = self.next_request_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();

        // Register the request
        self.pending_requests.write().insert(request_id, tx);

        // Send command with request ID
        let cmd = NodeCommand::Exec {
            command: command.to_string(),
            timeout_secs,
            request_id: Some(request_id),
        };

        let cmd_json = serde_json::to_string(&cmd).context("Failed to serialize command")?;
        let message = Message::Text(cmd_json);

        // Send command
        sender
            .send(message)
            .context("Failed to send command to node")?;

        // Wait for response with timeout
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(60) as u64);
        tokio::select! {
            result = rx => {
                Ok(result.context("Failed to receive response from node")?)
            }
            _ = tokio::time::sleep(timeout) => {
                // Clean up pending request on timeout
                self.pending_requests.write().remove(&request_id);
                anyhow::bail!("Command timed out after {} seconds", timeout_secs.unwrap_or(60))
            }
        }
    }

    /// Get node status information
    pub fn get_node_status(&self, node_id: &str) -> Option<NodeInfo> {
        self.nodes.read().get(node_id).map(|node| node.info.clone())
    }

    /// Clone for use in async handlers
    fn clone_for_handler(&self) -> Self {
        Self {
            config: self.config.clone(),
            nodes: Arc::clone(&self.nodes),
            pending_requests: Arc::clone(&self.pending_requests),
            next_request_id: Arc::clone(&self.next_request_id),
            pairing_code: Arc::clone(&self.pairing_code),
            pairing_expiry: Arc::clone(&self.pairing_expiry),
        }
    }

    /// Handle an incoming WebSocket connection
    async fn handle_connection(&self, stream: tokio::net::TcpStream) -> Result<()> {
        // Check connection limit
        if self.nodes.read().len() >= self.config.max_connections {
            tracing::warn!("Max connections reached ({}), rejecting new connection", self.config.max_connections);
            anyhow::bail!("Max connections reached");
        }

        let callback = |req: &Request, response: Response| {
            tracing::info!("WebSocket handshake from {}", req.uri());
            Ok(response)
        };

        let ws_stream = accept_hdr_async(stream, callback)
            .await
            .context("Failed to accept WebSocket connection")?;

        let (mut write, mut read) = ws_stream.split();
        let (sender, mut receiver) = mpsc::unbounded_channel::<Message>();

        // Spawn task to handle outgoing messages
        let send_task = tokio::spawn(async move {
            while let Some(msg) = receiver.recv().await {
                if let Err(e) = write.send(msg).await {
                    tracing::error!("Failed to send message: {}", e);
                    break;
                }
            }
        });

        // Handle incoming messages
        let mut node_id: Option<String> = None;
        let server = self.clone_for_handler();

        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    if let Err(e) = server.handle_message(&text, &sender, &mut node_id).await {
                        tracing::error!("Failed to handle message: {}", e);
                        break;
                    }
                }
                Ok(Message::Ping(data)) => {
                    let _ = sender.send(Message::Pong(data));
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("Client requested close");
                    break;
                }
                Err(e) => {
                    tracing::error!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }

        // Clean up node on disconnect
        if let Some(id) = node_id {
            server.nodes.write().remove(&id);
            tracing::info!("Node '{}' disconnected", id);
        }

        send_task.abort();
        Ok(())
    }

    /// Handle an incoming message from a node
    async fn handle_message(
        &self,
        text: &str,
        sender: &mpsc::UnboundedSender<Message>,
        node_id: &mut Option<String>,
    ) -> Result<()> {
        let json: Value = serde_json::from_str(text).context("Failed to parse JSON")?;

        let msg_type = json["type"]
            .as_str()
            .context("Message missing 'type' field")?;

        match msg_type {
            "pair" => {
                // Handle pairing request
                let request: PairingRequest =
                    serde_json::from_value(json).context("Failed to parse pairing request")?;

                let response = self.handle_pairing_request(&request, sender).await?;

                let response_json = serde_json::to_string(&response)?;
                sender.send(Message::Text(response_json))?;

                if response.success {
                    *node_id = Some(response.node_id.clone().unwrap());
                }
            }
            "pong" => {
                // Update last seen timestamp
                if let Some(id) = node_id {
                    if let Some(node) = self.nodes.read().get(id) {
                        *node.last_seen.write() = Instant::now();
                    }
                }
            }
            "exec_result" => {
                // Handle command execution result and route back to waiting command
                let response: NodeResponse =
                    serde_json::from_value(json.clone()).context("Failed to parse exec result")?;

                if let NodeResponse::ExecResult { .. } = &response {
                    // Extract request_id from the message if present
                    if let Some(request_id) = json.get("request_id").and_then(|v| v.as_u64()) {
                        if let Some(sender) = self.pending_requests.write().remove(&request_id) {
                            // Send response to the waiting command
                            if sender.send(response).is_err() {
                                tracing::warn!("Failed to send response for request {}", request_id);
                            }
                        } else {
                            tracing::warn!("No pending request found for request_id {}", request_id);
                        }
                    } else {
                        tracing::warn!("Exec result missing request_id");
                    }
                }
            }
            "status_report" => {
                // Handle status report
                let response: NodeResponse =
                    serde_json::from_value(json).context("Failed to parse status report")?;

                if let NodeResponse::StatusReport { .. } = response {
                    // Update node status info
                    tracing::info!("Received status report from node");
                }
            }
            _ => {
                tracing::warn!("Unknown message type: {}", msg_type);
            }
        }

        Ok(())
    }

    /// Handle a pairing request from a node
    async fn handle_pairing_request(
        &self,
        request: &PairingRequest,
        sender: &mpsc::UnboundedSender<Message>,
    ) -> Result<PairingResponse> {
        // Validate pairing code
        let current_code = self.pairing_code.read();
        let current_expiry = self.pairing_expiry.read();

        let is_valid = if let (Some(code), Some(expiry)) = (&*current_code, &*current_expiry) {
            *code == request.pairing_code && Instant::now() < *expiry
        } else {
            false
        };

        if !is_valid {
            return Ok(PairingResponse {
                success: false,
                node_id: None,
                error: Some("Invalid or expired pairing code".to_string()),
            });
        }

        // Generate unique node ID
        let node_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp() as u64;

        let info = NodeInfo {
            id: node_id.clone(),
            name: request.node_name.clone(),
            hostname: request.hostname.clone(),
            platform: request.platform.clone(),
            connected_at: now,
            last_seen: now,
        };

        let connection = NodeConnection {
            info: info.clone(),
            sender: sender.clone(),
            last_seen: Arc::new(RwLock::new(Instant::now())),
        };

        // Register node
        self.nodes.write().insert(node_id.clone(), connection);
        tracing::info!("Node '{}' paired successfully", request.node_name);

        // Clear pairing code after successful pairing
        *self.pairing_code.write() = None;
        *self.pairing_expiry.write() = None;

        Ok(PairingResponse {
            success: true,
            node_id: Some(node_id),
            error: None,
        })
    }

    /// Heartbeat checker - removes nodes that haven't been seen recently
    async fn heartbeat_checker(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        loop {
            interval.tick().await;

            let timeout_secs = HEARTBEAT_TIMEOUT_SECS;
            let mut nodes = self.nodes.write();
            let mut timed_out = Vec::new();

            nodes.retain(|id, node| {
                let last_seen = node.last_seen.read().elapsed().as_secs();
                if last_seen > timeout_secs {
                    timed_out.push((id.clone(), last_seen));
                    false
                } else {
                    true
                }
            });

            if !timed_out.is_empty() {
                for (id, last_seen) in &timed_out {
                    tracing::warn!("Node {} timeout (last seen {}s ago), removing", id, last_seen);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_pairing_code() {
        let config = NodesConfig::default();
        let server = NodeServer::new(config);

        let code = server.generate_pairing_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn test_list_nodes_empty() {
        let config = NodesConfig::default();
        let server = NodeServer::new(config);

        let nodes = server.list_nodes();
        assert!(nodes.is_empty());
    }
}