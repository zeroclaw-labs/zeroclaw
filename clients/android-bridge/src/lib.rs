#![forbid(unsafe_code)]

//! ZeroClaw Android Bridge
//!
//! This crate provides UniFFI bindings for ZeroClaw to be used from Kotlin/Android.
//! It exposes a simplified API for:
//! - Starting/stopping the local gateway process
//! - Sending messages to the agent via HTTP
//! - Receiving responses
//! - Managing configuration
//!
//! Architecture: MoA Android launches the ZeroClaw binary as a child process.
//! This bridge communicates with the local gateway via HTTP (127.0.0.1:3000).

use std::sync::{Arc, Mutex, OnceLock};
use tokio::runtime::Runtime;

uniffi::setup_scaffolding!();

/// Global runtime for async operations
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime")
    })
}

/// Default gateway URL for the local ZeroClaw instance.
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:3000";

/// Agent status enum exposed to Kotlin
#[derive(Debug, Clone, uniffi::Enum)]
pub enum AgentStatus {
    Stopped,
    Starting,
    Running,
    Thinking,
    Error { message: String },
}

/// Configuration for the ZeroClaw agent
#[derive(Debug, Clone, uniffi::Record)]
pub struct ZeroClawConfig {
    pub data_dir: String,
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub system_prompt: Option<String>,
}

impl Default for ZeroClawConfig {
    fn default() -> Self {
        Self {
            data_dir: String::new(),
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            api_key: String::new(),
            system_prompt: None,
        }
    }
}

/// A message in the conversation
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChatMessage {
    pub id: String,
    pub content: String,
    pub role: String, // "user" | "assistant" | "system"
    pub timestamp_ms: i64,
}

/// Response from sending a message
#[derive(Debug, Clone, uniffi::Record)]
pub struct SendResult {
    pub success: bool,
    pub message_id: Option<String>,
    pub error: Option<String>,
}

/// Main ZeroClaw controller exposed to Android
#[derive(uniffi::Object)]
pub struct ZeroClawController {
    config: Mutex<ZeroClawConfig>,
    status: Mutex<AgentStatus>,
    messages: Mutex<Vec<ChatMessage>>,
    gateway_url: Mutex<String>,
    /// Handle to the gateway child process (if launched by us).
    gateway_process: Mutex<Option<std::process::Child>>,
}

#[uniffi::export]
impl ZeroClawController {
    /// Create a new controller with the given config
    #[uniffi::constructor]
    pub fn new(config: ZeroClawConfig) -> Arc<Self> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("zeroclaw=info")
            .try_init();

        Arc::new(Self {
            config: Mutex::new(config),
            status: Mutex::new(AgentStatus::Stopped),
            messages: Mutex::new(Vec::new()),
            gateway_url: Mutex::new(DEFAULT_GATEWAY_URL.to_string()),
            gateway_process: Mutex::new(None::<std::process::Child>),
        })
    }

    /// Create with default config
    #[uniffi::constructor]
    pub fn with_defaults(data_dir: String) -> Arc<Self> {
        let mut config = ZeroClawConfig::default();
        config.data_dir = data_dir;
        Self::new(config)
    }

    /// Start the ZeroClaw gateway.
    ///
    /// Tries to launch the zeroclaw binary from the app's native libs directory.
    /// If a gateway is already running (health check passes), reuses it.
    pub fn start(&self) -> Result<(), ZeroClawError> {
        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;

        if matches!(*status, AgentStatus::Running | AgentStatus::Starting) {
            return Ok(());
        }

        *status = AgentStatus::Starting;
        drop(status);

        let gateway_url = self
            .gateway_url
            .lock()
            .map_err(|_| ZeroClawError::LockError)?
            .clone();
        let data_dir = self
            .config
            .lock()
            .map_err(|_| ZeroClawError::LockError)?
            .data_dir
            .clone();

        // Check if gateway is already running
        let is_running = runtime().block_on(async { check_gateway_health(&gateway_url).await });

        if is_running {
            let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
            *status = AgentStatus::Running;
            tracing::info!("ZeroClaw gateway already running at {gateway_url}");
            return Ok(());
        }

        // Try to launch zeroclaw binary
        let zeroclaw_bin = find_zeroclaw_binary(&data_dir);
        if let Some(bin_path) = zeroclaw_bin {
            match std::process::Command::new(&bin_path)
                .arg("daemon")
                .arg("--host")
                .arg("127.0.0.1")
                .arg("--port")
                .arg("3000")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(child) => {
                    let pid = child.id();
                    if let Ok(mut proc) = self.gateway_process.lock() {
                        *proc = Some(child);
                    }
                    tracing::info!(pid, bin = %bin_path, "Launched ZeroClaw gateway");
                }
                Err(e) => {
                    tracing::warn!("Failed to launch ZeroClaw binary: {e}");
                }
            }
        }

        // Wait for gateway to become ready (up to 10 seconds)
        let ready = runtime().block_on(async {
            for _ in 0..20 {
                if check_gateway_health(&gateway_url).await {
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            false
        });

        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
        if ready {
            *status = AgentStatus::Running;
            tracing::info!("ZeroClaw gateway is ready");
        } else {
            *status = AgentStatus::Error {
                message: "Gateway failed to start within 10s".to_string(),
            };
            return Err(ZeroClawError::GatewayError {
                message: "Gateway not ready after 10s".to_string(),
            });
        }

        Ok(())
    }

    /// Stop the gateway
    pub fn stop(&self) -> Result<(), ZeroClawError> {
        // Kill the child process if we launched it
        if let Ok(mut proc) = self.gateway_process.lock() {
            if let Some(mut child) = proc.take() {
                let pid = child.id();
                if let Err(e) = child.kill() {
                    tracing::warn!(pid, "Failed to kill gateway process: {e}");
                } else {
                    // Reap the child to avoid zombie process
                    let _ = child.wait();
                    tracing::info!(pid, "Stopped ZeroClaw gateway process");
                }
            }
        }

        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
        *status = AgentStatus::Stopped;
        tracing::info!("ZeroClaw gateway stopped");
        Ok(())
    }

    /// Get current agent status
    pub fn get_status(&self) -> AgentStatus {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(AgentStatus::Error {
                message: "Failed to get status".to_string(),
            })
    }

    /// Send a message to the agent via the local gateway HTTP API.
    pub fn send_message(&self, content: String) -> SendResult {
        let msg_id = uuid_v4();

        // Add user message to local history
        if let Ok(mut messages) = self.messages.lock() {
            messages.push(ChatMessage {
                id: msg_id.clone(),
                content: content.clone(),
                role: "user".to_string(),
                timestamp_ms: current_timestamp_ms(),
            });
        }

        // Set status to thinking
        if let Ok(mut status) = self.status.lock() {
            *status = AgentStatus::Thinking;
        }

        let gateway_url = self
            .gateway_url
            .lock()
            .map(|u| u.clone())
            .unwrap_or_else(|_| DEFAULT_GATEWAY_URL.to_string());

        // Send to local gateway via HTTP
        let result = runtime().block_on(async { send_to_gateway(&gateway_url, &content).await });

        // Restore status
        if let Ok(mut status) = self.status.lock() {
            *status = AgentStatus::Running;
        }

        match result {
            Ok(response) => {
                // Add assistant response to local history
                if let Ok(mut messages) = self.messages.lock() {
                    messages.push(ChatMessage {
                        id: uuid_v4(),
                        content: response,
                        role: "assistant".to_string(),
                        timestamp_ms: current_timestamp_ms(),
                    });
                }

                SendResult {
                    success: true,
                    message_id: Some(msg_id),
                    error: None,
                }
            }
            Err(e) => SendResult {
                success: false,
                message_id: Some(msg_id),
                error: Some(e),
            },
        }
    }

    /// Get conversation history
    pub fn get_messages(&self) -> Vec<ChatMessage> {
        self.messages.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Clear conversation history
    pub fn clear_messages(&self) {
        if let Ok(mut messages) = self.messages.lock() {
            messages.clear();
        }
    }

    /// Update configuration
    pub fn update_config(&self, config: ZeroClawConfig) -> Result<(), ZeroClawError> {
        let mut current = self.config.lock().map_err(|_| ZeroClawError::LockError)?;
        *current = config;
        Ok(())
    }

    /// Get current configuration
    pub fn get_config(&self) -> Result<ZeroClawConfig, ZeroClawError> {
        self.config
            .lock()
            .map(|c| c.clone())
            .map_err(|_| ZeroClawError::LockError)
    }

    /// Check if API key is configured
    pub fn is_configured(&self) -> bool {
        self.config
            .lock()
            .map(|c| !c.api_key.is_empty())
            .unwrap_or(false)
    }

    /// Check if the gateway is reachable
    pub fn is_gateway_running(&self) -> bool {
        let url = self
            .gateway_url
            .lock()
            .map(|u| u.clone())
            .unwrap_or_else(|_| DEFAULT_GATEWAY_URL.to_string());
        runtime().block_on(async { check_gateway_health(&url).await })
    }
}

/// Errors that can occur in the bridge
#[derive(Debug, Clone, uniffi::Error)]
pub enum ZeroClawError {
    NotInitialized,
    AlreadyRunning,
    ConfigError { message: String },
    GatewayError { message: String },
    LockError,
}

impl std::fmt::Display for ZeroClawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "ZeroClaw not initialized"),
            Self::AlreadyRunning => write!(f, "Gateway already running"),
            Self::ConfigError { message } => write!(f, "Config error: {}", message),
            Self::GatewayError { message } => write!(f, "Gateway error: {}", message),
            Self::LockError => write!(f, "Failed to acquire lock"),
        }
    }
}

impl std::error::Error for ZeroClawError {}

// ── HTTP communication with local gateway ────────────────────────

/// Check if the gateway is responding to health checks.
async fn check_gateway_health(gateway_url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    client
        .get(format!("{gateway_url}/health"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Send a chat message to the local gateway and return the response text.
async fn send_to_gateway(gateway_url: &str, message: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let body = serde_json::json!({ "message": message });

    let res = client
        .post(format!("{gateway_url}/webhook"))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("Gateway error ({status}): {text}"));
    }

    let json: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("Response parse error: {e}"))?;

    Ok(json
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("No response")
        .to_string())
}

/// Find the zeroclaw binary in common Android locations.
fn find_zeroclaw_binary(data_dir: &str) -> Option<String> {
    let candidates = [
        format!("{data_dir}/zeroclaw"),
        format!("{data_dir}/bin/zeroclaw"),
        format!("{data_dir}/../lib/libzeroclaw.so"), // Packed as native lib
        "/data/local/tmp/zeroclaw".to_string(),
    ];

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    // Also check PATH
    if let Ok(output) = std::process::Command::new("which").arg("zeroclaw").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    None
}

// ── Helpers ────────────────────────────────────────────────────────

fn uuid_v4() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Mix timestamp with a hash to fill 128 bits
    let mut hasher = DefaultHasher::new();
    nanos.hash(&mut hasher);
    let hash_hi = hasher.finish();
    let mut hasher2 = DefaultHasher::new();
    (nanos.wrapping_mul(6_364_136_223_846_793_005)).hash(&mut hasher2);
    let hash_lo = hasher2.finish();

    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&hash_hi.to_be_bytes());
    bytes[8..].copy_from_slice(&hash_lo.to_be_bytes());

    // Set version 4 (bits 48-51)
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    // Set variant 1 (bits 64-65)
    bytes[8] = (bytes[8] & 0x3F) | 0x80;

    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u16::from_be_bytes([bytes[4], bytes[5]]),
        u16::from_be_bytes([bytes[6], bytes[7]]),
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u64::from_be_bytes([0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]]),
    )
}

fn current_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_controller_creation() {
        let controller = ZeroClawController::with_defaults("/tmp/zeroclaw".to_string());
        assert!(matches!(controller.get_status(), AgentStatus::Stopped));
    }

    #[test]
    fn test_send_message_without_gateway() {
        let controller = ZeroClawController::with_defaults("/tmp/zeroclaw".to_string());
        let result = controller.send_message("Hello".to_string());
        // Without a gateway running, this will fail
        // But the user message should still be in history
        let messages = controller.get_messages();
        assert!(!messages.is_empty());
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hello");
    }

    #[test]
    fn test_config_update() {
        let controller = ZeroClawController::with_defaults("/tmp/zeroclaw".to_string());
        let new_config = ZeroClawConfig {
            data_dir: "/tmp/new".to_string(),
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            api_key: "test-key".to_string(),
            system_prompt: None,
        };
        controller.update_config(new_config).unwrap();
        assert!(controller.is_configured());
    }
}
