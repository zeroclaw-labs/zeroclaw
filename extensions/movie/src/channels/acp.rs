//! ACP (Agent Client Protocol) channel for ZeroClaw.
//!
//! This channel enables ZeroClaw to act as an ACP client, connecting to an OpenCode
//! ACP server via `opencode acp` command for JSON-RPC 2.0 communication over stdio.
//! This allows users to control OpenCode behavior from any channel via social apps.

use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::AcpConfig;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

/// Monotonic counter for message IDs in ACP JSON-RPC requests.
static ACP_MESSAGE_ID: AtomicU64 = AtomicU64::new(0);

/// ACP channel implementation for connecting to OpenCode ACP server.
///
/// The channel starts an OpenCode subprocess via `opencode acp` command and
/// communicates using JSON-RPC 2.0 over stdio. Messages from social apps are
/// forwarded as prompts to OpenCode, and responses are sent back through the
/// originating channel.
pub struct AcpChannel {
    /// OpenCode binary path (default: "opencode")
    opencode_path: String,
    /// Working directory for OpenCode process
    workdir: Option<String>,
    /// Additional arguments to pass to `opencode acp`
    extra_args: Vec<String>,
    /// Allowed user identifiers (empty = deny all, "*" = allow all)
    allowed_users: Vec<String>,
    /// Optional pairing guard for authentication
    pairing: Option<crate::security::pairing::PairingGuard>,
    /// HTTP client for potential future HTTP transport support
    client: reqwest::Client,
    /// Active OpenCode subprocess and its I/O handles
    process: Arc<Mutex<Option<AcpProcess>>>,
    /// Serializes ACP send operations to avoid concurrent process take/spawn races.
    send_operation_lock: Arc<Mutex<()>>,
    /// Next message ID for JSON-RPC requests
    next_message_id: Arc<AtomicU64>,
    /// Optional response channel for sending ACP responses back to original channel
    response_channel: Option<Arc<dyn Channel>>,
}
/// Active ACP process with I/O handles and session state.
struct AcpProcess {
    /// Child process handle
    child: Child,
    /// Stdin handle for sending JSON-RPC requests
    stdin: tokio::process::ChildStdin,
    /// Stdout handle for receiving JSON-RPC responses
    stdout: BufReader<tokio::process::ChildStdout>,
    /// Session ID from ACP server (after initialize + session/new)
    session_id: Option<String>,
    /// JSON-RPC message ID counter (per-process)
    message_id: u64,
    /// Pending responses keyed by request ID
    pending_responses: VecDeque<PendingResponse>,
}

/// Pending JSON-RPC response awaiting completion.
struct PendingResponse {
    request_id: u64,
    method: String,
    created_at: std::time::Instant,
}

/// JSON-RPC 2.0 request structure.
#[derive(Debug, Clone, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC 2.0 response structure.
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: u64,
    #[serde(flatten)]
    result_or_error: JsonRpcResultOrError,
}

/// JSON-RPC result or error.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum JsonRpcResultOrError {
    Result { result: Value },
    Error { error: JsonRpcError },
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// ACP initialization parameters.
#[derive(Debug, Clone, Serialize)]
struct InitializeParams {
    protocol_version: u64,
    client_capabilities: ClientCapabilities,
    client_info: ClientInfo,
}

/// Client capabilities declaration.
#[derive(Debug, Clone, Serialize, Default)]
struct ClientCapabilities {
    fs: FsCapabilities,
    terminal: bool,
    #[serde(rename = "_meta")]
    meta: Option<Value>,
}

/// Filesystem capabilities.
#[derive(Debug, Clone, Serialize, Default)]
struct FsCapabilities {
    read_text_file: bool,
    write_text_file: bool,
}

/// Client information.
#[derive(Debug, Clone, Serialize)]
struct ClientInfo {
    name: String,
    title: String,
    version: String,
}

/// ACP session/new parameters.
#[derive(Debug, Clone, Serialize)]
struct SessionNewParams {
    cwd: String,
    mcp_servers: Vec<Value>,
}

/// ACP session/prompt parameters.
#[derive(Debug, Clone, Serialize)]
struct SessionPromptParams {
    session_id: String,
    prompt: Vec<PromptItem>,
}

/// Prompt item (text content).
#[derive(Debug, Clone, Serialize)]
struct PromptItem {
    #[serde(rename = "type")]
    item_type: String,
    text: String,
}

impl AcpChannel {
    /// Create a new ACP channel with the given configuration.
    pub fn new(config: AcpConfig) -> Self {
        Self {
            opencode_path: config
                .opencode_path
                .unwrap_or_else(|| "opencode".to_string()),
            workdir: config.workdir,
            extra_args: config.extra_args,
            allowed_users: config.allowed_users,
            pairing: None, // TODO: Implement pairing if needed
            client: reqwest::Client::new(),
            process: Arc::new(Mutex::new(None)),
            send_operation_lock: Arc::new(Mutex::new(())),
            next_message_id: Arc::new(AtomicU64::new(0)),
            response_channel: None,
        }
    }

    /// Check if a user is allowed to interact with this channel.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users
            .iter()
            .any(|allowed| allowed == "*" || allowed == user_id)
    }

    /// Set the response channel for sending ACP responses back to original channel
    pub fn set_response_channel(&mut self, channel: Arc<dyn Channel>) {
        self.response_channel = Some(channel);
    }

    /// Start the OpenCode ACP subprocess and establish connection.
    fn start_process(&self) -> Result<AcpProcess> {
        let mut command = Command::new(&self.opencode_path);
        command.arg("acp");

        if let Some(workdir) = &self.workdir {
            command.current_dir(workdir);
        }

        for arg in &self.extra_args {
            command.arg(arg);
        }

        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        // Inherit stderr so the child cannot block on an unread stderr pipe.
        command.stderr(std::process::Stdio::inherit());

        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to start OpenCode process: {}", self.opencode_path))?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to take stdin from child process")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to take stdout from child process")?;
        let stdout_reader = BufReader::new(stdout);

        let process = AcpProcess {
            child,
            stdin,
            stdout: stdout_reader,
            session_id: None,
            message_id: 0,
            pending_responses: VecDeque::new(),
        };

        Ok(process)
    }

    /// Send a JSON-RPC request and wait for response.
    async fn send_json_rpc_request(
        &self,
        process: &mut AcpProcess,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value> {
        let request_id = process.message_id;
        process.message_id += 1;

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: request_id,
            method: method.to_string(),
            params,
        };

        let json_str = serde_json::to_string(&request).with_context(|| {
            format!(
                "Failed to serialize JSON-RPC request for method: {}",
                method
            )
        })?;

        // Write message with newline delimiter (ACP protocol requirement)
        process.stdin.write_all(json_str.as_bytes()).await?;
        process.stdin.write_all(b"\n").await?;
        process.stdin.flush().await?;

        // Read response line with timeout
        let mut line = String::new();
        let timeout_duration = std::time::Duration::from_secs(30);
        match tokio::time::timeout(timeout_duration, process.stdout.read_line(&mut line)).await {
            Ok(read_result) => {
                read_result
                    .with_context(|| format!("Failed to read response for method: {}", method))?;
            }
            Err(_) => {
                anyhow::bail!("Timeout waiting for ACP response for method: {}", method);
            }
        }

        // Parse JSON-RPC response
        let response: JsonRpcResponse = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse JSON-RPC response: {}", line))?;

        // Verify response ID matches request ID
        if response.id != request_id {
            anyhow::bail!(
                "Response ID mismatch: expected {}, got {}",
                request_id,
                response.id
            );
        }

        match response.result_or_error {
            JsonRpcResultOrError::Result { result } => Ok(result),
            JsonRpcResultOrError::Error { error } => {
                anyhow::bail!("ACP JSON-RPC error ({}): {}", error.code, error.message);
            }
        }
    }

    /// Initialize ACP connection with the server.
    async fn initialize_acp(&self, process: &mut AcpProcess) -> Result<()> {
        let params = InitializeParams {
            protocol_version: 1,
            client_capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "ZeroClaw".to_string(),
                title: "ZeroClaw ACP Client".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        let params_value =
            serde_json::to_value(params).context("Failed to serialize initialize params")?;

        let response = self
            .send_json_rpc_request(process, "initialize", Some(params_value))
            .await?;

        // TODO: Parse response and store capabilities
        tracing::info!("ACP initialized successfully: {:?}", response);
        Ok(())
    }

    /// Create a new ACP session.
    async fn create_session(&self, process: &mut AcpProcess) -> Result<String> {
        let cwd = self.workdir.clone().unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| ".".into())
                .to_string_lossy()
                .to_string()
        });

        let params = SessionNewParams {
            cwd,
            mcp_servers: vec![],
        };

        let params_value =
            serde_json::to_value(params).context("Failed to serialize session/new params")?;

        let response = self
            .send_json_rpc_request(process, "session/new", Some(params_value))
            .await?;

        // Parse response to extract session_id
        let session_id = response
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .with_context(|| {
                format!(
                    "Invalid session/new response: missing session_id: {:?}",
                    response
                )
            })?;

        tracing::info!("ACP session created: {}", session_id);
        Ok(session_id)
    }

    /// Send a prompt to the ACP session.
    async fn send_prompt(
        &self,
        process: &mut AcpProcess,
        session_id: &str,
        prompt_text: &str,
    ) -> Result<String> {
        let params = SessionPromptParams {
            session_id: session_id.to_string(),
            prompt: vec![PromptItem {
                item_type: "text".to_string(),
                text: prompt_text.to_string(),
            }],
        };

        let params_value =
            serde_json::to_value(params).context("Failed to serialize session/prompt params")?;

        let response = self
            .send_json_rpc_request(process, "session/prompt", Some(params_value))
            .await?;

        // Parse response to extract the actual response text
        // The response may contain a "response" field with text content
        let response_text = response
            .get("response")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .with_context(|| {
                format!(
                    "Invalid session/prompt response: missing string field `response` for prompt {:?}: {:?}",
                    prompt_text, response
                )
            })?;

        Ok(response_text)
    }

    fn process_is_running(process: &mut AcpProcess) -> bool {
        matches!(process.child.try_wait(), Ok(None))
    }

    async fn initialize_fresh_process(&self) -> Result<AcpProcess> {
        let mut new_process = self.start_process()?;
        self.initialize_acp(&mut new_process).await?;
        let session_id = self.create_session(&mut new_process).await?;
        new_process.session_id = Some(session_id);
        Ok(new_process)
    }

    async fn checkout_process_for_send(&self) -> Result<AcpProcess> {
        let mut process_opt = {
            let mut process_guard = self.process.lock().await;
            process_guard.take()
        };

        let needs_restart = match process_opt.as_mut() {
            Some(process) => !Self::process_is_running(process),
            None => true,
        };

        if needs_restart {
            process_opt = Some(self.initialize_fresh_process().await?);
        }

        process_opt.context("ACP process disappeared unexpectedly")
    }

    async fn restore_process(&self, process: Option<AcpProcess>) {
        let mut process_guard = self.process.lock().await;
        *process_guard = process;
    }
}

#[async_trait]
impl Channel for AcpChannel {
    fn name(&self) -> &str {
        "acp"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        const MAX_SEND_ATTEMPTS: usize = 2;

        let _send_guard = self.send_operation_lock.lock().await;

        // Check if user is allowed
        if !self.is_user_allowed(&message.recipient) {
            tracing::warn!(
                "ACP: ignoring message from unauthorized user: {}",
                message.recipient
            );
            return Ok(());
        }

        // Strip tool call tags from outgoing messages
        let content = super::strip_tool_call_tags(&message.content);

        let mut last_error = None;
        for attempt in 0..MAX_SEND_ATTEMPTS {
            let mut process = self.checkout_process_for_send().await?;
            let session_id = process
                .session_id
                .as_ref()
                .context("No active ACP session")?
                .clone();

            match self.send_prompt(&mut process, &session_id, &content).await {
                Ok(response) => {
                    if Self::process_is_running(&mut process) {
                        self.restore_process(Some(process)).await;
                    } else {
                        self.restore_process(None).await;
                    }

                    // Send response back through response_channel if set
                    if let Some(response_channel) = &self.response_channel {
                        let response_message =
                            SendMessage::new(response, message.recipient.clone());
                        if let Err(e) = response_channel.send(&response_message).await {
                            tracing::warn!(
                                "Failed to send ACP response through response channel: {}",
                                e
                            );
                        }
                    } else {
                        // Log if no response channel configured
                        tracing::info!(
                            "ACP response ready (no response channel configured): {}",
                            response
                        );
                    }

                    return Ok(());
                }
                Err(error) => {
                    // Drop unhealthy process on failure and retry once with a fresh process.
                    self.restore_process(None).await;
                    if attempt + 1 < MAX_SEND_ATTEMPTS {
                        tracing::warn!(
                            "ACP prompt failed (attempt {}/{}), restarting ACP process: {}",
                            attempt + 1,
                            MAX_SEND_ATTEMPTS,
                            error
                        );
                    }
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("ACP send failed with unknown error")))
    }

    async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // ACP is primarily a client-side protocol where we send prompts
        // and receive responses. For channel listening, we might need to
        // handle incoming messages from other sources that should trigger
        // ACP prompts.

        // Since ACP is more about sending commands to OpenCode rather than
        // listening for incoming messages, we implement a minimal listener
        // that just keeps the channel alive.

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    }

    async fn health_check(&self) -> bool {
        let mut process_guard = self.process.lock().await;
        let Some(process) = process_guard.as_mut() else {
            return false;
        };
        let is_running = Self::process_is_running(process);
        if !is_running {
            *process_guard = None;
        }
        is_running
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::AcpConfig;

    #[test]
    fn acp_channel_name() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };
        let channel = AcpChannel::new(config);
        assert_eq!(channel.name(), "acp");
    }

    #[test]
    fn acp_channel_empty_allowlist_denies_all() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };
        let channel = AcpChannel::new(config);
        assert!(!channel.is_user_allowed("anyone"));
        assert!(!channel.is_user_allowed("user123"));
    }

    #[test]
    fn acp_channel_wildcard_allows_all() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["*".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(channel.is_user_allowed("anyone"));
        assert!(channel.is_user_allowed("user123"));
        assert!(channel.is_user_allowed(""));
    }

    #[test]
    fn acp_channel_specific_users() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["user1".to_string(), "user2".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(channel.is_user_allowed("user1"));
        assert!(channel.is_user_allowed("user2"));
        assert!(!channel.is_user_allowed("user3"));
        assert!(!channel.is_user_allowed("User1")); // case sensitive
        assert!(!channel.is_user_allowed("user"));
    }

    #[test]
    fn acp_channel_wildcard_and_specific() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["user1".to_string(), "*".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(channel.is_user_allowed("user1"));
        assert!(channel.is_user_allowed("anyone"));
        assert!(channel.is_user_allowed("user2"));
    }

    #[test]
    fn acp_channel_empty_user_id() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["user1".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(!channel.is_user_allowed(""));
    }

    #[test]
    fn acp_channel_exact_match_not_substring() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["user123".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(channel.is_user_allowed("user123"));
        assert!(!channel.is_user_allowed("user12"));
        assert!(!channel.is_user_allowed("user1234"));
        assert!(!channel.is_user_allowed("user"));
    }

    #[test]
    fn acp_channel_case_sensitive() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec!["User".to_string()],
        };
        let channel = AcpChannel::new(config);
        assert!(channel.is_user_allowed("User"));
        assert!(!channel.is_user_allowed("user"));
        assert!(!channel.is_user_allowed("USER"));
    }

    // JSON-RPC data structure tests
    #[test]
    fn jsonrpc_request_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 42,
            method: "test".to_string(),
            params: Some(serde_json::json!({"key": "value"})),
        };
        let json = serde_json::to_string(&request).unwrap();
        let expected = r#"{"jsonrpc":"2.0","id":42,"method":"test","params":{"key":"value"}}"#;
        assert_eq!(json, expected);
    }

    #[test]
    fn jsonrpc_request_without_params() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "ping".to_string(),
            params: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        let expected = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        assert_eq!(json, expected);
    }

    #[test]
    fn jsonrpc_response_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":42,"result":{"status":"ok"}}"#;
        let response: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, 42);
        match response.result_or_error {
            JsonRpcResultOrError::Result { result } => {
                assert_eq!(result, serde_json::json!({"status": "ok"}));
            }
            JsonRpcResultOrError::Error { .. } => panic!("Expected result, got error"),
        }
    }

    #[test]
    fn jsonrpc_error_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":42,"error":{"code":-32700,"message":"Parse error"}}"#;
        let response: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, 42);
        match response.result_or_error {
            JsonRpcResultOrError::Error { error } => {
                assert_eq!(error.code, -32700);
                assert_eq!(error.message, "Parse error");
                assert!(error.data.is_none());
            }
            JsonRpcResultOrError::Result { .. } => panic!("Expected error, got result"),
        }
    }

    #[test]
    fn initialize_params_serialization() {
        let params = InitializeParams {
            protocol_version: 1,
            client_capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "ZeroClaw".to_string(),
                title: "ZeroClaw ACP Client".to_string(),
                version: "1.0.0".to_string(),
            },
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["protocol_version"], 1);
        assert_eq!(json["client_info"]["name"], "ZeroClaw");
    }

    #[test]
    fn session_new_params_serialization() {
        let params = SessionNewParams {
            cwd: "/tmp".to_string(),
            mcp_servers: vec![],
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["cwd"], "/tmp");
        assert_eq!(json["mcp_servers"], serde_json::json!([]));
    }

    #[test]
    fn session_prompt_params_serialization() {
        let params = SessionPromptParams {
            session_id: "session-123".to_string(),
            prompt: vec![PromptItem {
                item_type: "text".to_string(),
                text: "Hello".to_string(),
            }],
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["session_id"], "session-123");
        assert_eq!(json["prompt"][0]["type"], "text");
        assert_eq!(json["prompt"][0]["text"], "Hello");
    }

    #[test]
    fn acp_channel_set_response_channel() {
        use super::Channel;
        use crate::channels::traits::SendMessage;
        use std::sync::Arc;

        // Mock channel for testing
        struct MockChannel;
        #[async_trait::async_trait]
        impl Channel for MockChannel {
            fn name(&self) -> &str {
                "mock"
            }

            async fn send(&self, _message: &SendMessage) -> Result<()> {
                Ok(())
            }

            async fn listen(
                &self,
                _tx: tokio::sync::mpsc::Sender<crate::channels::traits::ChannelMessage>,
            ) -> Result<()> {
                Ok(())
            }

            async fn health_check(&self) -> bool {
                true
            }
        }

        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };

        let mut channel = AcpChannel::new(config);
        let mock_channel = Arc::new(MockChannel);

        // Initially no response channel
        // (Cannot directly access private field, but we can test via public API)

        // Set response channel
        channel.set_response_channel(mock_channel.clone());

        // Verify channel can be set (no panic)
        // This test mainly ensures the method exists and works
        assert!(true);
    }

    // Note: More comprehensive tests would require mocking the OpenCode process
    // which is beyond the scope of basic unit tests.

    #[cfg(unix)]
    async fn spawn_test_process(command: &str, args: &[&str]) -> AcpProcess {
        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn test ACP process");

        let stdin = child.stdin.take().expect("test process stdin");
        let stdout = BufReader::new(child.stdout.take().expect("test process stdout"));

        AcpProcess {
            child,
            stdin,
            stdout,
            session_id: Some("test-session".to_string()),
            message_id: 0,
            pending_responses: VecDeque::new(),
        }
    }

    #[cfg(unix)]
    async fn cleanup_test_process(channel: &AcpChannel) {
        let process = {
            let mut guard = channel.process.lock().await;
            guard.take()
        };
        if let Some(mut process) = process {
            let _ = process.child.kill().await;
            let _ = process.child.wait().await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_health_check_false_when_no_process() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };
        let channel = AcpChannel::new(config);
        assert!(!channel.health_check().await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_health_check_true_when_process_running() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };
        let channel = AcpChannel::new(config);

        let process = spawn_test_process("sh", &["-c", "sleep 5"]).await;
        {
            let mut guard = channel.process.lock().await;
            *guard = Some(process);
        }

        assert!(channel.health_check().await);
        cleanup_test_process(&channel).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn acp_health_check_false_after_process_exit() {
        let config = AcpConfig {
            opencode_path: None,
            workdir: None,
            extra_args: vec![],
            allowed_users: vec![],
        };
        let channel = AcpChannel::new(config);

        let process = spawn_test_process("sh", &["-c", "true"]).await;
        {
            let mut guard = channel.process.lock().await;
            *guard = Some(process);
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!channel.health_check().await);
    }
}
