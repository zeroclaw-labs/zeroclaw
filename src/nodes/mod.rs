//! Multi-Node Management module for ZeroClaw
//!
//! This module provides functionality for managing remote nodes via WebSocket connections.
//! Remote nodes can connect to the main gateway using a 6-digit pairing code, and the
//! gateway can execute commands on these nodes.

mod client;
mod server;
mod types;

pub use server::NodeServer;
pub use client::{connect_to_server, NodeClient};
pub use types::{
    NodeInfo, NodeCommand, NodeResponse, PairingRequest, PairingResponse,
};