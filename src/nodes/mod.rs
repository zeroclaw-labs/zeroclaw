//! Multi-Node Management module for ZeroClaw
//!
//! This module provides functionality for managing remote nodes via WebSocket connections.
//! Remote nodes can connect to the main gateway using a 6-digit pairing code, and the
//! gateway can execute commands on these nodes.

mod client;
mod server;
mod types;

use std::sync::OnceLock;
use parking_lot::RwLock;

pub use server::NodeServer;
pub use client::{connect_to_server, NodeClient};
pub use types::{
    NodeInfo, NodeCommand, NodeResponse, PairingRequest, PairingResponse,
};

/// Global NodeServer instance storage
static NODE_SERVER: OnceLock<RwLock<Option<std::sync::Arc<NodeServer>>>> = OnceLock::new();

/// Initialize the global NodeServer instance
/// This should be called once when the daemon starts
pub fn init_node_server(config: crate::config::schema::NodesConfig) -> std::sync::Arc<NodeServer> {
    let server = std::sync::Arc::new(NodeServer::new(config));
    let _ = NODE_SERVER.get_or_init(|| RwLock::new(Some(server.clone())));
    server
}

/// Get the global NodeServer instance
/// Returns None if not initialized
pub fn get_node_server() -> Option<std::sync::Arc<NodeServer>> {
    NODE_SERVER.get()?.read().clone()
}