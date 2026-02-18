//! Node Client - connects to a ZeroClaw gateway and executes commands
//!
//! The node client runs on remote hosts and maintains a persistent WebSocket
//! connection to the main gateway. It can receive and execute commands.

use crate::nodes::types::{
    NodeCommand, NodeInfo, NodeResponse, PairingRequest, PairingResponse,
};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio::sync::RwLock;

/// Node Client - connects to gateway and executes commands
pub struct NodeClient {
    server_url: String,
    node_name: String,
    hostname: Option<String>,
    platform: String,
    node_id: Arc<RwLock<Option<String>>>,
}

impl NodeClient {
    /// Create a new node client
    pub fn new(
        server_url: String,
        node_name: String,
        hostname: Option<String>,
        platform: String,
    ) -> Self {
        Self {
            server_url,
            node_name,
            hostname,
            platform,
            node_id: Arc::new(RwLock::new(None)),
        }
    }

    /// Connect to the server with a pairing code
    pub async fn connect_with_code(&self, pairing_code: String) -> Result<String> {
        let url = if self.server_url.starts_with("ws://") || self.server_url.starts_with("wss://") {
            self.server_url.clone()
        } else {
            format!("ws://{}", self.server_url)
        };

        tracing::info!("Connecting to {} with pairing code {}", url, &pairing_code[..3]); // Partial mask for privacy
        tracing::debug!("Full URL: {}", url);

        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to server")?;

        let (mut write, mut read) = ws_stream.split();

        // Send pairing request
        let request = PairingRequest {
            pairing_code,
            node_name: self.node_name.clone(),
            hostname: self.hostname.clone(),
            platform: self.platform.clone(),
        };

        let request_json = serde_json::to_string(&request)?;
        write.send(Message::Text(request_json)).await?;

        // Wait for pairing response
        if let Some(Ok(msg)) = read.next().await {
            if let Message::Text(text) = msg {
                let response: PairingResponse = serde_json::from_str(&text)?;

                if response.success {
                    let node_id = response.node_id.unwrap();
                    *self.node_id.write().await = Some(node_id.clone());
                    tracing::info!("Successfully paired as node: {}", node_id);
                    return Ok(node_id);
                } else {
                    anyhow::bail!("Pairing failed: {}", response.error.unwrap_or_else(|| "Unknown error".to_string()));
                }
            }
        }

        anyhow::bail!("No response from server")
    }

    /// Run the node client loop (process incoming commands)
    pub async fn run(&self) -> Result<()> {
        let url = if self.server_url.starts_with("ws://") || self.server_url.starts_with("wss://") {
            self.server_url.clone()
        } else {
            format!("ws://{}", self.server_url)
        };

        tracing::info!("Starting node client loop, connecting to {}", url);

        loop {
            match self.run_session().await {
                Ok(_) => {
                    tracing::warn!("Connection closed, reconnecting in 5 seconds...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
                Err(e) => {
                    tracing::error!("Connection error: {}, reconnecting in 5 seconds...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Run a single connection session
    async fn run_session(&self) -> Result<()> {
        let url = if self.server_url.starts_with("ws://") || self.server_url.starts_with("wss://") {
            self.server_url.clone()
        } else {
            format!("ws://{}", self.server_url)
        };

        let (ws_stream, _) = connect_async(&url)
            .await
            .context("Failed to connect to server")?;

        let (mut write, mut read) = ws_stream.split();

        // Start heartbeat task
        let heartbeat_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    // Send ping
                    if let Err(e) = write.send(Message::Ping(vec![])).await {
                        anyhow::bail!("Failed to send ping: {}", e);
                    }
                }
                msg_result = read.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            if let Err(e) = self.handle_message(msg, &mut write).await {
                                tracing::error!("Failed to handle message: {}", e);
                            }
                        }
                        Some(Err(e)) => {
                            anyhow::bail!("WebSocket error: {}", e);
                        }
                        None => {
                            anyhow::bail!("Server closed connection");
                        }
                    }
                }
            }
        }
    }

    /// Handle an incoming message from the server
    async fn handle_message(
        &self,
        msg: Message,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<()> {
        match msg {
            Message::Text(text) => {
                let json: serde_json::Value = serde_json::from_str(&text)?;

                let msg_type = json["type"].as_str().unwrap_or("");

                match msg_type {
                    "exec" => {
                        // Execute command
                        let command = json["command"]
                            .as_str()
                            .context("Command missing 'command' field")?;

                        let timeout_secs = json["timeout_secs"].and_then(|v| v.as_u64());

                        let result = self.execute_command(command, timeout_secs).await?;

                        let response = NodeResponse::ExecResult {
                            success: result.success,
                            stdout: result.stdout,
                            stderr: result.stderr,
                            exit_code: result.exit_code,
                        };

                        let response_json = serde_json::to_string(&response)?;
                        write.send(Message::Text(response_json)).await?;
                    }
                    "status" => {
                        // Return status report
                        let response = NodeResponse::StatusReport {
                            cpu_percent: self.get_cpu_usage()?,
                            memory_percent: self.get_memory_usage()?,
                            uptime_secs: self.get_uptime()?,
                        };

                        let response_json = serde_json::to_string(&response)?;
                        write.send(Message::Text(response_json)).await?;
                    }
                    "ping" => {
                        let response = NodeResponse::Pong;
                        let response_json = serde_json::to_string(&response)?;
                        write.send(Message::Text(response_json)).await?;
                    }
                    _ => {
                        tracing::warn!("Unknown message type: {}", msg_type);
                    }
                }
            }
            Message::Ping(data) => {
                write.send(Message::Pong(data)).await?;
            }
            Message::Close(_) => {
                anyhow::bail!("Server requested close");
            }
            _ => {}
        }

        Ok(())
    }

    /// Execute a shell command
    async fn execute_command(&self, command: &str, timeout_secs: Option<u64>) -> Result<ExecResult> {
        use tokio::process::Command;
        use tokio::time::timeout;

        let duration = std::time::Duration::from_secs(timeout_secs.unwrap_or(60));

        let output = tokio::task::spawn_blocking(move || {
            let mut cmd = if cfg!(target_os = "windows") {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", command]);
                c
            } else {
                let mut c = std::process::Command::new("sh");
                c.args(["-c", command]);
                c
            };
            cmd.output()
        });

        let output = timeout(duration, output)
            .await
            .context("Command timed out")?
            .context("Failed to execute command")?;

        let output = output?;

        Ok(ExecResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    /// Get CPU usage percentage
    fn get_cpu_usage(&self) -> Result<f32> {
        let mut sys = sysinfo::System::new_all();
        sys.refresh_cpu();
        let cpu_usage = sys.global_cpu_usage();
        Ok(cpu_usage)
    }

    /// Get memory usage percentage
    fn get_memory_usage(&self) -> Result<f32> {
        let mut sys = sysinfo::System::new_all();
        sys.refresh_memory();
        let total = sys.total_memory() as f32;
        let used = sys.used_memory() as f32;
        if total > 0.0 {
            Ok((used / total) * 100.0)
        } else {
            Ok(0.0)
        }
    }

    /// Get system uptime in seconds
    fn get_uptime(&self) -> Result<u64> {
        #[cfg(target_os = "linux")]
        {
            let contents = std::fs::read_to_string("/proc/uptime")?;
            let uptime: f64 = contents
                .split_whitespace()
                .next()
                .ok_or_else(|| anyhow::anyhow!("Invalid uptime format"))?
                .parse()?;
            Ok(uptime as u64)
        }
        #[cfg(target_os = "windows")]
        {
            use std::time::SystemTime;
            // Windows uptime - simplified implementation
            // In production, use GetTickCount64 from winapi
            let boot_time = sysinfo::System::new_all()
                .boot_time();
            let now = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            Ok(now.saturating_sub(boot_time))
        }
        #[cfg(target_os = "macos")]
        {
            use std::time::SystemTime;
            // macOS uptime - simplified implementation
            let boot_time = sysinfo::System::new_all()
                .boot_time();
            let now = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            Ok(now.saturating_sub(boot_time))
        }
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            Ok(0)
        }
    }
}

/// Result of command execution
#[derive(Debug)]
struct ExecResult {
    success: bool,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Connect to a server and return a node client
pub fn connect_to_server(
    server_url: String,
    node_name: String,
    hostname: Option<String>,
    platform: String,
) -> NodeClient {
    NodeClient::new(server_url, node_name, hostname, platform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_client() {
        let client = NodeClient::new(
            "ws://localhost:8765".to_string(),
            "test-node".to_string(),
            Some("localhost".to_string()),
            "linux".to_string(),
        );

        assert_eq!(client.node_name, "test-node");
        assert_eq!(client.platform, "linux");
    }
}