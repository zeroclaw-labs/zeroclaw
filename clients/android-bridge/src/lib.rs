//! ZeroClaw Android Bridge
//!
//! This crate provides UniFFI bindings for ZeroClaw to be used from Kotlin/Android.
//! It exposes a simplified API for:
//! - Starting/stopping the gateway
//! - Sending messages to the agent
//! - Receiving responses
//! - Managing configuration

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
    // TODO: Add actual gateway handle
    // gateway: Mutex<Option<GatewayHandle>>,
}

#[uniffi::export]
impl ZeroClawController {
    /// Create a new controller with the given config
    #[uniffi::constructor]
    pub fn new(config: ZeroClawConfig) -> Arc<Self> {
        // Initialize logging
        let _ = tracing_subscriber::fmt()
            .with_env_filter("zeroclaw=info")
            .try_init();

        Arc::new(Self {
            config: Mutex::new(config),
            status: Mutex::new(AgentStatus::Stopped),
            messages: Mutex::new(Vec::new()),
        })
    }

    /// Create with default config
    #[uniffi::constructor]
    pub fn with_defaults(data_dir: String) -> Arc<Self> {
        let mut config = ZeroClawConfig::default();
        config.data_dir = data_dir;
        Self::new(config)
    }

    /// Start the ZeroClaw gateway
    pub fn start(&self) -> Result<(), ZeroClawError> {
        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
        
        if matches!(*status, AgentStatus::Running | AgentStatus::Starting) {
            return Ok(());
        }

        *status = AgentStatus::Starting;
        drop(status);

        // TODO: Actually start the gateway
        // runtime().spawn(async move {
        //     let config = zeroclaw::Config::load()?;
        //     let gateway = zeroclaw::Gateway::new(config).await?;
        //     gateway.run().await
        // });

        // For now, simulate successful start
        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
        *status = AgentStatus::Running;

        tracing::info!("ZeroClaw gateway started");
        Ok(())
    }

    /// Stop the gateway
    pub fn stop(&self) -> Result<(), ZeroClawError> {
        let mut status = self.status.lock().map_err(|_| ZeroClawError::LockError)?;
        
        // TODO: Actually stop the gateway
        // if let Some(gateway) = self.gateway.lock()?.take() {
        //     gateway.shutdown();
        // }

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

    /// Send a message to the agent
    pub fn send_message(&self, content: String) -> SendResult {
        let msg_id = uuid_v4();
        
        // Add user message
        if let Ok(mut messages) = self.messages.lock() {
            messages.push(ChatMessage {
                id: msg_id.clone(),
                content: content.clone(),
                role: "user".to_string(),
                timestamp_ms: current_timestamp_ms(),
            });
        }

        // TODO: Actually send to gateway and get response
        // For now, echo back
        if let Ok(mut messages) = self.messages.lock() {
            messages.push(ChatMessage {
                id: uuid_v4(),
                content: format!("Echo: {}", content),
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

    /// Get conversation history
    pub fn get_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default()
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

// Helper functions
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", now)
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
    fn test_start_stop() {
        let controller = ZeroClawController::with_defaults("/tmp/zeroclaw".to_string());
        controller.start().unwrap();
        assert!(matches!(controller.get_status(), AgentStatus::Running));
        controller.stop().unwrap();
        assert!(matches!(controller.get_status(), AgentStatus::Stopped));
    }

    #[test]
    fn test_send_message() {
        let controller = ZeroClawController::with_defaults("/tmp/zeroclaw".to_string());
        let result = controller.send_message("Hello".to_string());
        assert!(result.success);
        
        let messages = controller.get_messages();
        assert_eq!(messages.len(), 2); // User + assistant
    }
}
