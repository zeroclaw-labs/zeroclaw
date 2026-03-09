//! Multi-machine node system for cross-node agent coordination.
//!
//! Provides [`NodeRegistry`] for tracking registered nodes,
//! [`NodeClient`] for invoking actions on remote nodes, and
//! supporting types ([`NodeInfo`], [`NodeStatus`], [`NodeSystemConfig`]).

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::NodeSystemConfig;

// ── Node types ──────────────────────────────────────────────────

/// Status of a registered node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum NodeStatus {
    Online,
    Offline,
    Busy,
    Error { message: String },
}

impl std::fmt::Display for NodeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Online => write!(f, "online"),
            Self::Offline => write!(f, "offline"),
            Self::Busy => write!(f, "busy"),
            Self::Error { message } => write!(f, "error: {message}"),
        }
    }
}

/// Metadata about a single node in the cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub hostname: String,
    /// Reachable address in `ip:port` form.
    pub address: String,
    /// Advertised capabilities (e.g. `["shell", "gpu", "docker"]`).
    pub capabilities: Vec<String>,
    pub status: NodeStatus,
    pub last_heartbeat: DateTime<Utc>,
    pub version: String,
}

// ── NodeRegistry ────────────────────────────────────────────────

/// In-memory registry of known nodes.
///
/// Thread-safe via `Arc<RwLock<...>>` — designed to be shared across
/// axum handlers and background heartbeat loops.
#[derive(Clone)]
pub struct NodeRegistry {
    nodes: Arc<RwLock<HashMap<String, NodeInfo>>>,
    config: NodeSystemConfig,
}

impl NodeRegistry {
    pub fn new(config: &NodeSystemConfig) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(HashMap::new())),
            config: config.clone(),
        }
    }

    /// Register (or re-register) a node.
    pub async fn register(&self, info: NodeInfo) -> Result<()> {
        let mut nodes = self.nodes.write().await;
        if nodes.len() >= self.config.max_nodes && !nodes.contains_key(&info.node_id) {
            bail!(
                "Node registry is full (max_nodes={})",
                self.config.max_nodes
            );
        }
        if !self.is_node_allowed(&info.node_id) {
            bail!("Node ID '{}' is not in the allowed list", info.node_id);
        }
        tracing::info!(node_id = %info.node_id, address = %info.address, "node registered");
        nodes.insert(info.node_id.clone(), info);
        Ok(())
    }

    /// Remove a node from the registry.
    pub async fn unregister(&self, node_id: &str) -> Result<()> {
        let mut nodes = self.nodes.write().await;
        if nodes.remove(node_id).is_none() {
            bail!("Node '{node_id}' not found in registry");
        }
        tracing::info!(node_id = %node_id, "node unregistered");
        Ok(())
    }

    /// Get a snapshot of a single node.
    pub async fn get(&self, node_id: &str) -> Option<NodeInfo> {
        self.nodes.read().await.get(node_id).cloned()
    }

    /// List all registered nodes.
    pub async fn list(&self) -> Vec<NodeInfo> {
        self.nodes.read().await.values().cloned().collect()
    }

    /// Update the heartbeat timestamp for a node and mark it online.
    pub async fn update_heartbeat(&self, node_id: &str) -> Result<()> {
        let mut nodes = self.nodes.write().await;
        let node = nodes
            .get_mut(node_id)
            .context(format!("Node '{node_id}' not found in registry"))?;
        node.last_heartbeat = Utc::now();
        node.status = NodeStatus::Online;
        Ok(())
    }

    /// Mark nodes as offline if their last heartbeat exceeds `timeout_secs`.
    /// Returns the IDs of pruned nodes.
    pub async fn prune_stale(&self, timeout_secs: u64) -> Vec<String> {
        let cutoff = Utc::now() - chrono::Duration::seconds(timeout_secs as i64);
        let mut nodes = self.nodes.write().await;
        let mut pruned = Vec::new();
        for (id, node) in nodes.iter_mut() {
            if node.last_heartbeat < cutoff && node.status == NodeStatus::Online {
                node.status = NodeStatus::Offline;
                pruned.push(id.clone());
            }
        }
        if !pruned.is_empty() {
            tracing::info!(pruned = ?pruned, "stale nodes marked offline");
        }
        pruned
    }

    /// Check whether a node ID is permitted by the allowlist.
    fn is_node_allowed(&self, node_id: &str) -> bool {
        if self.config.allowed_node_ids.is_empty() {
            return true;
        }
        self.config
            .allowed_node_ids
            .iter()
            .any(|id| id == "*" || id == node_id)
    }

    /// Expose a read-only reference to the config for auth checks.
    pub fn config(&self) -> &NodeSystemConfig {
        &self.config
    }
}

// ── NodeClient ──────────────────────────────────────────────────

/// HTTP client for invoking actions on remote nodes.
pub struct NodeClient {
    http: reqwest::Client,
    registry: Arc<NodeRegistry>,
}

impl NodeClient {
    pub fn new(registry: Arc<NodeRegistry>) -> Self {
        Self {
            http: reqwest::Client::new(),
            registry,
        }
    }

    /// Access the underlying registry.
    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// Invoke an action on a remote node via its advertised address.
    pub async fn invoke(
        &self,
        node_id: &str,
        action: &str,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let node = self
            .registry
            .get(node_id)
            .await
            .context(format!("Node '{node_id}' not found"))?;

        if node.status == NodeStatus::Offline {
            bail!("Node '{node_id}' is offline");
        }

        let url = format!("http://{}/api/node-control", node.address);
        let body = serde_json::json!({
            "method": "node.invoke",
            "node_id": node_id,
            "capability": action,
            "arguments": payload,
        });

        let mut req = self.http.post(&url).json(&body);

        // Attach HMAC signature if shared secret is configured.
        let config = self.registry.config();
        if !config.shared_secret.is_empty() {
            let sig = compute_hmac(&config.shared_secret, &body.to_string());
            req = req.header("X-Node-HMAC-Signature", sig);
        }
        if let Some(ref token) = config.auth_token {
            req = req.header("X-Node-Control-Token", token.as_str());
        }

        let resp = req
            .send()
            .await
            .context(format!("Failed to reach node '{node_id}' at {url}"))?;

        let status = resp.status();
        let result: serde_json::Value = resp
            .json()
            .await
            .context("Failed to parse response from remote node")?;

        if !status.is_success() {
            bail!(
                "Remote node '{node_id}' returned {}: {}",
                status,
                result
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("unknown error")
            );
        }

        Ok(result)
    }

    /// Check if a node is reachable and healthy.
    pub async fn health_check(&self, node_id: &str) -> Result<bool> {
        let node = self
            .registry
            .get(node_id)
            .await
            .context(format!("Node '{node_id}' not found"))?;

        let url = format!("http://{}/health", node.address);
        let resp = self.http.get(&url).send().await;
        Ok(resp.is_ok_and(|r| r.status().is_success()))
    }
}

// ── HMAC helpers ────────────────────────────────────────────────

type HmacSha256 = Hmac<Sha256>;

/// Compute hex-encoded HMAC-SHA256.
pub fn compute_hmac(secret: &str, message: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verify an HMAC-SHA256 signature (constant-time).
pub fn verify_hmac(secret: &str, message: &str, signature: &str) -> bool {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let expected = hex::decode(signature).unwrap_or_default();
    mac.verify_slice(&expected).is_ok()
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> NodeSystemConfig {
        NodeSystemConfig {
            enabled: true,
            node_id: "test-node".into(),
            advertise_address: "127.0.0.1:42617".into(),
            heartbeat_interval_secs: 30,
            stale_timeout_secs: 120,
            max_nodes: 32,
            allowed_node_ids: vec![],
            require_auth: true,
            shared_secret: "test-secret".into(),
            auth_token: None,
        }
    }

    fn test_node(id: &str) -> NodeInfo {
        NodeInfo {
            node_id: id.into(),
            hostname: "zeroclaw-node".into(),
            address: "127.0.0.1:42618".into(),
            capabilities: vec!["shell".into()],
            status: NodeStatus::Online,
            last_heartbeat: Utc::now(),
            version: "0.1.0".into(),
        }
    }

    #[tokio::test]
    async fn registry_register_and_list() {
        let registry = NodeRegistry::new(&test_config());
        registry.register(test_node("node-a")).await.unwrap();
        registry.register(test_node("node-b")).await.unwrap();

        let nodes = registry.list().await;
        assert_eq!(nodes.len(), 2);
    }

    #[tokio::test]
    async fn registry_unregister() {
        let registry = NodeRegistry::new(&test_config());
        registry.register(test_node("node-a")).await.unwrap();
        registry.unregister("node-a").await.unwrap();

        let nodes = registry.list().await;
        assert!(nodes.is_empty());
    }

    #[tokio::test]
    async fn registry_unregister_missing_fails() {
        let registry = NodeRegistry::new(&test_config());
        let result = registry.unregister("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn registry_get() {
        let registry = NodeRegistry::new(&test_config());
        registry.register(test_node("node-a")).await.unwrap();

        let node = registry.get("node-a").await;
        assert!(node.is_some());
        assert_eq!(node.unwrap().node_id, "node-a");

        let missing = registry.get("nonexistent").await;
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn registry_heartbeat() {
        let registry = NodeRegistry::new(&test_config());
        let mut node = test_node("node-a");
        node.status = NodeStatus::Offline;
        registry.register(node).await.unwrap();

        registry.update_heartbeat("node-a").await.unwrap();

        let updated = registry.get("node-a").await.unwrap();
        assert_eq!(updated.status, NodeStatus::Online);
    }

    #[tokio::test]
    async fn registry_prune_stale() {
        let registry = NodeRegistry::new(&test_config());
        let mut node = test_node("node-a");
        node.last_heartbeat = Utc::now() - chrono::Duration::seconds(300);
        registry.register(node).await.unwrap();

        let pruned = registry.prune_stale(120).await;
        assert_eq!(pruned, vec!["node-a"]);

        let updated = registry.get("node-a").await.unwrap();
        assert_eq!(updated.status, NodeStatus::Offline);
    }

    #[tokio::test]
    async fn registry_max_nodes_enforced() {
        let mut config = test_config();
        config.max_nodes = 1;
        let registry = NodeRegistry::new(&config);
        registry.register(test_node("node-a")).await.unwrap();

        let result = registry.register(test_node("node-b")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn registry_allowlist_enforced() {
        let mut config = test_config();
        config.allowed_node_ids = vec!["node-a".into()];
        let registry = NodeRegistry::new(&config);

        registry.register(test_node("node-a")).await.unwrap();
        let result = registry.register(test_node("node-b")).await;
        assert!(result.is_err());
    }

    #[test]
    fn node_info_serialization_roundtrip() {
        let node = test_node("node-a");
        let json = serde_json::to_string(&node).unwrap();
        let parsed: NodeInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_id, "node-a");
        assert_eq!(parsed.hostname, "zeroclaw-node");
        assert_eq!(parsed.capabilities, vec!["shell"]);
    }

    #[test]
    fn node_status_serialization() {
        let online: NodeStatus = serde_json::from_str(r#"{"status":"online"}"#).unwrap();
        assert_eq!(online, NodeStatus::Online);

        let err: NodeStatus =
            serde_json::from_str(r#"{"status":"error","message":"timeout"}"#).unwrap();
        assert_eq!(
            err,
            NodeStatus::Error {
                message: "timeout".into()
            }
        );
    }

    #[test]
    fn config_defaults() {
        let config = NodeSystemConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert_eq!(config.stale_timeout_secs, 120);
        assert_eq!(config.max_nodes, 32);
        assert!(config.allowed_node_ids.is_empty());
        assert!(config.require_auth);
        assert!(config.shared_secret.is_empty());
    }

    #[test]
    fn hmac_compute_and_verify() {
        let sig = compute_hmac("secret", "hello");
        assert!(verify_hmac("secret", "hello", &sig));
        assert!(!verify_hmac("wrong", "hello", &sig));
        assert!(!verify_hmac("secret", "world", &sig));
    }
}
