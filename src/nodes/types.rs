//! Node types for ZeroClaw multi-node management
//!
//! This module defines the data structures used for node communication
//! between the main gateway and remote nodes.

use serde::{Deserialize, Serialize};

/// Node identification and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique node identifier (UUID)
    pub id: String,
    /// Human-readable node name
    pub name: String,
    /// System hostname (optional)
    pub hostname: Option<String>,
    /// Platform/architecture (e.g., "x86_64-linux", "aarch64-darwin")
    pub platform: String,
    /// Unix timestamp when the node connected
    pub connected_at: u64,
    /// Unix timestamp of last activity
    pub last_seen: u64,
}

/// Commands sent from server to node
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NodeCommand {
    /// Ping request for health check
    #[serde(rename = "ping")]
    Ping,

    /// Execute a shell command on the node
    #[serde(rename = "exec")]
    Exec {
        /// Command string to execute
        command: String,
        /// Optional timeout in seconds (default: 60)
        timeout_secs: Option<u32>,
        /// Request ID for response routing
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<u64>,
    },

    /// Request node status information
    #[serde(rename = "status")]
    Status,
}

/// Responses from node to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NodeResponse {
    /// Pong response
    #[serde(rename = "pong")]
    Pong,

    /// Result of command execution
    #[serde(rename = "exec_result")]
    ExecResult {
        /// Whether the command succeeded
        success: bool,
        /// Standard output
        stdout: String,
        /// Standard error
        stderr: String,
        /// Exit code
        exit_code: i32,
    },

    /// System status report
    #[serde(rename = "status")]
    StatusReport {
        /// CPU usage percentage (0-100)
        cpu_percent: f32,
        /// Memory usage percentage (0-100)
        memory_percent: f32,
        /// System uptime in seconds
        uptime_secs: u64,
    },
}

/// Pairing request from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingRequest {
    /// 6-digit pairing code
    pub pairing_code: String,
    /// Node name
    pub node_name: String,
    /// System hostname (optional)
    pub hostname: Option<String>,
    /// Platform/architecture
    pub platform: String,
}

/// Pairing response from server to client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingResponse {
    /// Whether pairing succeeded
    pub success: bool,
    /// Assigned node ID (if successful)
    pub node_id: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_info_serde() {
        let info = NodeInfo {
            id: "test-id".to_string(),
            name: "Test Node".to_string(),
            hostname: Some("test-host".to_string()),
            platform: "x86_64-linux".to_string(),
            connected_at: 1234567890,
            last_seen: 1234567895,
        };

        let json = serde_json::to_string(&info).unwrap();
        let parsed: NodeInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, info.id);
        assert_eq!(parsed.name, info.name);
        assert_eq!(parsed.hostname, info.hostname);
        assert_eq!(parsed.platform, info.platform);
    }

    #[test]
    fn test_node_command_exec() {
        let cmd = NodeCommand::Exec {
            command: "echo hello".to_string(),
            timeout_secs: Some(30),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        let parsed: NodeCommand = serde_json::from_str(&json).unwrap();

        match parsed {
            NodeCommand::Exec { command, timeout_secs } => {
                assert_eq!(command, "echo hello");
                assert_eq!(timeout_secs, Some(30));
            }
            _ => panic!("Wrong command type"),
        }
    }

    #[test]
    fn test_node_response_exec_result() {
        let response = NodeResponse::ExecResult {
            success: true,
            stdout: "hello".to_string(),
            stderr: String::new(),
            exit_code: 0,
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: NodeResponse = serde_json::from_str(&json).unwrap();

        match parsed {
            NodeResponse::ExecResult { success, stdout, exit_code, .. } => {
                assert!(success);
                assert_eq!(stdout, "hello");
                assert_eq!(exit_code, 0);
            }
            _ => panic!("Wrong response type"),
        }
    }

    #[test]
    fn test_pairing_request() {
        let req = PairingRequest {
            pairing_code: "123456".to_string(),
            node_name: "My Node".to_string(),
            hostname: Some("myhost".to_string()),
            platform: "aarch64-darwin".to_string(),
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: PairingRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.pairing_code, "123456");
        assert_eq!(parsed.node_name, "My Node");
    }

    #[test]
    fn test_pairing_response_success() {
        let resp = PairingResponse {
            success: true,
            node_id: Some("node-uuid".to_string()),
            error: None,
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: PairingResponse = serde_json::from_str(&json).unwrap();

        assert!(parsed.success);
        assert_eq!(parsed.node_id, Some("node-uuid".to_string()));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn test_pairing_response_failure() {
        let resp = PairingResponse {
            success: false,
            node_id: None,
            error: Some("Invalid pairing code".to_string()),
        };

        let json = serde_json::to_string(&resp).unwrap();
        let parsed: PairingResponse = serde_json::from_str(&json).unwrap();

        assert!(!parsed.success);
        assert!(parsed.node_id.is_none());
        assert_eq!(parsed.error, Some("Invalid pairing code".to_string()));
    }
}