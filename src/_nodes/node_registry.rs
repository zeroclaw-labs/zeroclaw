//! Connected-node registry: holds WebSocket node sessions and pending request/response.

use crate::tools::{NodeCommandResult, NodeDescription, NodeInfo, NodeRegistry};
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Default timeout for invoke/run when waiting for node response.
pub const DEFAULT_NODE_INVOKE_TIMEOUT_SECS: u64 = 60;

/// Message sent from registry to the connection handler to be forwarded to the node.
#[derive(Debug)]
pub enum OutgoingMessage {
    Invoke {
        request_id: String,
        capability: String,
        arguments: Value,
    },
    Run {
        request_id: String,
        command: String,
    },
}

/// One connected node session: channel to send invoke/run to the handler.
struct NodeSession {
    node_id: String,
    capabilities: Vec<String>,
    meta: Option<Value>,
    tx: mpsc::Sender<OutgoingMessage>,
}

/// In-memory registry of connected nodes; implements [`NodeRegistry`].
pub struct ConnectedNodeRegistry {
    sessions: RwLock<HashMap<String, NodeSession>>,
    pending: Mutex<HashMap<String, oneshot::Sender<NodeCommandResult>>>,
    invoke_timeout: Duration,
}

static CONNECTED_NODE_REGISTRY: OnceLock<Arc<ConnectedNodeRegistry>> = OnceLock::new();

impl ConnectedNodeRegistry {
    /// Get global singleton instance of connected-node registry.
    pub fn global() -> Arc<ConnectedNodeRegistry> {
        CONNECTED_NODE_REGISTRY
            .get_or_init(|| Arc::new(ConnectedNodeRegistry::new()))
            .clone()
    }

    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_NODE_INVOKE_TIMEOUT_SECS))
    }

    pub fn with_timeout(invoke_timeout: Duration) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            invoke_timeout,
        }
    }

    /// Register a node (called from WebSocket handler after receiving register message).
    /// Returns a receiver for outgoing messages the handler must forward to the node.
    pub fn register(
        &self,
        node_id: String,
        capabilities: Vec<String>,
        meta: Option<Value>,
    ) -> mpsc::Receiver<OutgoingMessage> {
        let (tx, rx) = mpsc::channel(32);
        let session = NodeSession {
            node_id: node_id.clone(),
            capabilities: capabilities.clone(),
            meta: meta.clone(),
            tx,
        };
        self.sessions.write().insert(node_id, session);
        rx
    }

    /// Remove a node (called when WebSocket disconnects).
    /// In-flight requests for this node will time out in the caller.
    pub fn unregister(&self, node_id: &str) {
        self.sessions.write().remove(node_id);
    }

    /// Complete a pending request (called from WebSocket handler on invoke_result/run_result).
    pub fn complete_pending(&self, request_id: &str, result: NodeCommandResult) {
        if let Some(tx) = self.pending.lock().remove(request_id) {
            let _ = tx.send(result);
        }
    }

    /// Filter list by allowlist when non-empty; pass empty slice to allow all.
    fn filter_by_allowlist(&self, allowed_node_ids: &[String]) -> Vec<NodeInfo> {
        let list = self.list_inner();
        if allowed_node_ids.is_empty() {
            return list;
        }
        let allowed: std::collections::HashSet<_> = allowed_node_ids
            .iter()
            .filter(|s| *s != "*")
            .collect();
        if allowed.is_empty() {
            return list;
        }
        list.into_iter()
            .filter(|n| allowed.contains(&n.node_id))
            .collect()
    }
}

impl Default for ConnectedNodeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectedNodeRegistry {
    fn list_inner(&self) -> Vec<NodeInfo> {
        self.sessions
            .read()
            .values()
            .map(|s| NodeInfo {
                node_id: s.node_id.clone(),
                status: "connected".to_string(),
                capabilities: s.capabilities.clone(),
                meta: s.meta.clone(),
            })
            .collect()
    }
}

#[async_trait]
impl NodeRegistry for ConnectedNodeRegistry {
    fn list(&self) -> Vec<NodeInfo> {
        self.list_inner()
    }

    fn describe(&self, node_id: &str) -> Option<NodeDescription> {
        self.sessions.read().get(node_id).map(|s| NodeDescription {
            node_id: s.node_id.clone(),
            status: "connected".to_string(),
            capabilities: s.capabilities.clone(),
            meta: s.meta.clone(),
        })
    }

    async fn invoke(
        &self,
        node_id: &str,
        capability: &str,
        arguments: Value,
    ) -> anyhow::Result<NodeCommandResult> {
        let tx = self
            .sessions
            .read()
            .get(node_id)
            .map(|s| s.tx.clone())
            .ok_or_else(|| anyhow::anyhow!("node '{node_id}' not connected"))?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending.lock().insert(request_id.clone(), resp_tx);

        let msg = OutgoingMessage::Invoke {
            request_id: request_id.clone(),
            capability: capability.to_string(),
            arguments,
        };
        tx.send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("node '{node_id}' channel closed"))?;

        match tokio::time::timeout(self.invoke_timeout, resp_rx).await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(_)) => {
                self.pending.lock().remove(&request_id);
                Ok(NodeCommandResult {
                    success: false,
                    output: String::new(),
                    error: Some("request cancelled".into()),
                })
            }
            Err(_) => {
                self.pending.lock().remove(&request_id);
                Ok(NodeCommandResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "timeout after {}s waiting for node response",
                        self.invoke_timeout.as_secs()
                    )),
                })
            }
        }
    }

    async fn run(&self, node_id: &str, raw_command: &str) -> anyhow::Result<NodeCommandResult> {
        let tx = self
            .sessions
            .read()
            .get(node_id)
            .map(|s| s.tx.clone())
            .ok_or_else(|| anyhow::anyhow!("node '{node_id}' not connected"))?;

        let request_id = uuid::Uuid::new_v4().to_string();
        let (resp_tx, resp_rx) = oneshot::channel();
        self.pending.lock().insert(request_id.clone(), resp_tx);

        let msg = OutgoingMessage::Run {
            request_id: request_id.clone(),
            command: raw_command.to_string(),
        };
        tx.send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("node '{node_id}' channel closed"))?;

        match tokio::time::timeout(self.invoke_timeout, resp_rx).await {
            Ok(Ok(res)) => Ok(res),
            Ok(Err(_)) => {
                self.pending.lock().remove(&request_id);
                Ok(NodeCommandResult {
                    success: false,
                    output: String::new(),
                    error: Some("request cancelled".into()),
                })
            }
            Err(_) => {
                self.pending.lock().remove(&request_id);
                Ok(NodeCommandResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "timeout after {}s waiting for node response",
                        self.invoke_timeout.as_secs()
                    )),
                })
            }
        }
    }
}

/// Extension: list filtered by allowlist (for HTTP API).
impl ConnectedNodeRegistry {
    pub fn list_filtered(&self, allowed_node_ids: &[String]) -> Vec<NodeInfo> {
        self.filter_by_allowlist(allowed_node_ids)
    }

    pub fn describe_filtered(
        &self,
        node_id: &str,
        allowed_node_ids: &[String],
    ) -> Option<NodeDescription> {
        if !allowed_node_ids.is_empty()
            && !allowed_node_ids.iter().any(|a| a == "*" || a == node_id)
        {
            return None;
        }
        self.describe(node_id)
    }
}
