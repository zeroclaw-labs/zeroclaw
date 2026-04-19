//! Host function registry: manages host-defined functions exposed to WASM plugins.
//!
//! The `HostFunctionRegistry` holds references to ZeroClaw subsystems (memory,
//! tools, audit) so that host functions can call back into the agent runtime on
//! behalf of a plugin.
//!
//! # Host Function ABI
//!
//! All host functions follow a consistent encoding convention:
//!
//! - **Input**: JSON-encoded UTF-8 bytes passed via Extism's shared memory
//!   (`serde_json::to_vec`). Any valid JSON value is accepted (objects, arrays,
//!   primitives).
//! - **Output**: JSON-encoded UTF-8 bytes returned via Extism's shared memory,
//!   parsed back with `serde_json::from_slice`.
//! - **Errors**: Execution errors are **never** propagated as `anyhow::Error`.
//!   Instead they are returned as `ToolResult { success: false, error: Some(..) }`.
//!   Error messages use the format `[plugin:<name>/<export>] <classification>`
//!   so the caller can identify which plugin and export function failed.
//! - **Error JSON shape**: When a host function returns an error to the plugin
//!   side, it is encoded as `{ "error": "<message>" }`.

use crate::channels::{Channel, SendMessage};
use crate::memory::traits::{Memory, MemoryCategory};
use crate::plugins::PluginManifest;
use crate::security::audit::{AuditLogger, CliAuditEntry};
use crate::security::{
    validate_arguments, validate_arguments_strict, validate_command_allowlist,
    validate_path_traversal, warn_broad_cli_patterns,
};
use crate::tools::Tool;
use extism::Function;
use extism::UserData;
use extism::ValType;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Maximum allowed nesting depth for `zeroclaw_tool_call` host function
/// invocations.  This prevents infinite recursion when plugin A delegates to
/// plugin B which delegates back to A, etc.
pub const MAX_TOOL_CALL_DEPTH: u32 = 5;

thread_local! {
    /// Tracks the current nesting depth of `zeroclaw_tool_call` invocations on
    /// this thread.  Incremented on entry, decremented on exit (via a guard).
    static TOOL_CALL_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// RAII guard that decrements `TOOL_CALL_DEPTH` when dropped, ensuring the
/// counter stays balanced even if the tool execution panics.
struct DepthGuard;

impl DepthGuard {
    fn enter() -> Self {
        TOOL_CALL_DEPTH.with(|d| d.set(d.get() + 1));
        DepthGuard
    }

    fn current() -> u32 {
        TOOL_CALL_DEPTH.with(|d| d.get())
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        TOOL_CALL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Sliding-window rate limiter keyed by (plugin_name, channel_name).
///
/// Each plugin gets an independent send budget per channel within a
/// configurable time window (default: 1 hour). Thread-safe via `parking_lot::Mutex`.
pub struct ChannelRateLimiter {
    /// Maximum sends allowed per (plugin, channel) within the window.
    max_per_window: u32,
    /// Window duration.
    window: Duration,
    /// Recorded timestamps keyed by (plugin_name, channel_name).
    state: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

impl ChannelRateLimiter {
    /// Create a new rate limiter with the given budget and window.
    pub fn new(max_per_window: u32, window_secs: u64) -> Self {
        Self {
            max_per_window,
            window: Duration::from_secs(window_secs),
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Record a send attempt. Returns `Ok(())` if within budget,
    /// `Err(message)` if rate-limited.
    pub fn record_send(&self, plugin_name: &str, channel: &str) -> Result<(), String> {
        let mut state = self.state.lock();
        let key = (plugin_name.to_string(), channel.to_string());
        let timestamps = state.entry(key).or_default();

        // Prune expired entries
        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        timestamps.retain(|t| *t > cutoff);

        if u32::try_from(timestamps.len()).unwrap_or(u32::MAX) >= self.max_per_window {
            return Err(format!(
                "Rate limit exceeded: plugin '{}' has exhausted its messaging budget for channel '{}'",
                plugin_name, channel
            ));
        }

        timestamps.push(Instant::now());
        Ok(())
    }
}

/// Sliding-window rate limiter for CLI executions, keyed by plugin name.
///
/// Each plugin gets an independent execution budget within a 1-minute window.
/// The rate limit is configured per-plugin via `rate_limit_per_minute` in the
/// plugin manifest. Thread-safe via `parking_lot::Mutex`.
pub struct CliRateLimiter {
    /// Window duration (1 minute).
    window: Duration,
    /// Recorded timestamps keyed by plugin_name.
    state: Mutex<HashMap<String, Vec<Instant>>>,
}

impl CliRateLimiter {
    /// Create a new CLI rate limiter with a 1-minute sliding window.
    pub fn new() -> Self {
        Self {
            window: Duration::from_secs(60),
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Record a CLI execution attempt. Returns `Ok(())` if within budget,
    /// `Err(retry_after_secs)` if rate-limited.
    ///
    /// # Arguments
    /// * `plugin_name` - Name of the plugin attempting execution
    /// * `limit_per_minute` - Maximum executions allowed per minute (0 = unlimited)
    pub fn record_execution(&self, plugin_name: &str, limit_per_minute: u32) -> Result<(), u64> {
        if limit_per_minute == 0 {
            return Ok(()); // No limit configured
        }

        let mut state = self.state.lock();
        let timestamps = state.entry(plugin_name.to_string()).or_default();

        let now = Instant::now();
        let cutoff = now.checked_sub(self.window).unwrap_or(now);
        timestamps.retain(|t| *t > cutoff);

        if u32::try_from(timestamps.len()).unwrap_or(u32::MAX) >= limit_per_minute {
            // Calculate time until oldest request expires
            if let Some(oldest) = timestamps.first() {
                if let Some(expires_at) = oldest.checked_add(self.window) {
                    if let Some(wait) = expires_at.checked_duration_since(now) {
                        return Err(wait.as_secs().saturating_add(1));
                    }
                }
            }
            return Err(60); // Fallback: wait full window
        }

        timestamps.push(now);
        Ok(())
    }
}

impl Default for CliRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned by host functions to WASM plugins.
///
/// Serialized as `{ "error": "<message>" }` in the host-function ABI.
/// On the Rust side this type is used to construct error responses before
/// encoding them back through Extism's shared memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostFunctionError {
    /// Human-readable error message.
    pub error: String,
}

impl HostFunctionError {
    /// Create a new host function error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            error: message.into(),
        }
    }

    /// Serialize this error to JSON bytes for the Extism ABI.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("HostFunctionError serialization is infallible")
    }
}

impl std::fmt::Display for HostFunctionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for HostFunctionError {}

/// JSON input for the `zeroclaw_memory_store` host function.
///
/// Plugins pass this structure (JSON-encoded) through Extism shared memory
/// when calling the host function.
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryStoreRequest {
    /// The memory key to store under (will be prefixed with the plugin author tag).
    pub key: String,
    /// The value to persist in memory.
    pub value: String,
}

/// JSON output for successful `zeroclaw_memory_store` calls.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryStoreResponse {
    pub success: bool,
}

/// JSON input for the `zeroclaw_memory_recall` host function.
///
/// Plugins pass this structure (JSON-encoded) through Extism shared memory
/// when calling the host function.
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryRecallRequest {
    /// The query string to search for in memory.
    pub query: String,
}

/// JSON output for successful `zeroclaw_memory_recall` calls.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryRecallResponse {
    /// The matching memory entries serialized as a JSON string.
    pub results: String,
}

/// JSON input for the `zeroclaw_memory_forget` host function.
///
/// Plugins pass this structure (JSON-encoded) through Extism shared memory
/// when calling the host function.
#[derive(Debug, Clone, Deserialize)]
pub struct MemoryForgetRequest {
    /// The memory key to remove.
    pub key: String,
}

/// JSON output for successful `zeroclaw_memory_forget` calls.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryForgetResponse {
    pub success: bool,
}

/// JSON input for the `zeroclaw_tool_call` host function.
///
/// Plugins pass this structure (JSON-encoded) through Extism shared memory
/// when calling the host function.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallRequest {
    /// The name of the tool to invoke.
    pub tool_name: String,
    /// The arguments to pass to the tool (JSON object).
    pub arguments: Value,
}

/// JSON output for `zeroclaw_tool_call` calls.
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallResponse {
    /// Whether the tool executed successfully.
    pub success: bool,
    /// The tool's output text.
    pub output: String,
    /// Error message, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// JSON input for the `zeroclaw_send_message` host function.
///
/// Plugins pass this structure (JSON-encoded) through Extism shared memory
/// when calling the host function.
#[derive(Debug, Clone, Deserialize)]
pub struct ChannelSendRequest {
    /// The channel name to send through (must be in allowed_channels).
    pub channel: String,
    /// The recipient identifier (channel-specific).
    pub recipient: String,
    /// The message content to send.
    pub message: String,
}

/// JSON output for successful `zeroclaw_send_message` calls.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelSendResponse {
    pub success: bool,
}

/// JSON output for the `context_session` host function.
///
/// Returns a read-only snapshot of the current session context: which channel
/// the plugin was invoked from, the conversation ID, and a timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContextResponse {
    /// The channel name the current request originated from (e.g. "telegram", "slack").
    pub channel_name: String,
    /// An opaque conversation/session identifier.
    pub conversation_id: String,
    /// ISO-8601 timestamp of the current request.
    pub timestamp: String,
}

impl Default for SessionContextResponse {
    fn default() -> Self {
        Self {
            channel_name: String::new(),
            conversation_id: String::new(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// JSON output for the `context_user_identity` host function.
///
/// Returns information about the user who triggered the current plugin
/// invocation: their username, display name, and channel-specific ID.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserIdentityResponse {
    /// The user's username (e.g. "jdoe").
    pub username: String,
    /// The user's display name (e.g. "Jane Doe").
    pub display_name: String,
    /// A channel-specific identifier for the user (e.g. Telegram user ID, Slack member ID).
    pub channel_user_id: String,
}

/// JSON output for the `zeroclaw_get_channels` host function.
///
/// Returns only channel names — no credentials, tokens, or connection details.
#[derive(Debug, Clone, Serialize)]
pub struct GetChannelsResponse {
    /// Available channel names (filtered by the plugin's allowed_channels list).
    pub channels: Vec<String>,
}

/// Shared state passed into the `channel_send` host function callback
/// via Extism's `UserData`.
struct ChannelSendData {
    channels: HashMap<String, Arc<dyn Channel>>,
    allowed_channels: Vec<String>,
    plugin_name: String,
    rate_limiter: Arc<ChannelRateLimiter>,
}

/// Shared state passed into the `zeroclaw_get_channels` host function callback
/// via Extism's `UserData`.
struct ChannelGetData {
    channels: HashMap<String, Arc<dyn Channel>>,
    allowed_channels: Vec<String>,
}

/// Shared state passed into the `zeroclaw_tool_call` host function callback
/// via Extism's `UserData`.
struct ToolCallData {
    tools: Vec<Arc<dyn Tool>>,
    allowed_tools: Vec<String>,
    plugin_name: String,
}

/// Shared state passed into the `zeroclaw_memory_store` host function callback
/// via Extism's `UserData`.
struct MemoryStoreData {
    memory: Arc<dyn Memory>,
    plugin_name: String,
}

/// Shared state passed into the `context_session` host function callback
/// via Extism's `UserData`.
struct SessionContextData {
    session_context: Arc<Mutex<SessionContextResponse>>,
}

/// Shared state passed into the `context_user_identity` host function callback
/// via Extism's `UserData`.
struct UserIdentityData {
    user_identity: Arc<Mutex<UserIdentityResponse>>,
}

/// JSON output for the `context_agent_config` host function.
///
/// Returns agent personality/identity configuration: the agent's name,
/// personality traits, and any additional identity fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfigResponse {
    /// The agent's configured display name.
    pub name: String,
    /// Personality traits (e.g. "friendly", "concise", "technical").
    pub personality_traits: Vec<String>,
    /// Arbitrary identity fields from the agent configuration.
    pub identity: HashMap<String, String>,
}

/// JSON input for the `zeroclaw_cli_exec` host function.
///
/// Requests execution of a CLI command with specified arguments,
/// working directory, and environment variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliExecRequest {
    /// The command to execute (e.g., "git", "npm").
    pub command: String,
    /// Arguments to pass to the command.
    pub args: Vec<String>,
    /// Working directory for command execution.
    /// Must be within the plugin's allowed_paths.
    pub working_dir: Option<String>,
    /// Environment variables to set for the command.
    /// Only variables in the plugin's allowed_env list will be applied.
    pub env: Option<HashMap<String, String>>,
}

/// JSON output for the `zeroclaw_cli_exec` host function.
///
/// Contains the captured output and status of the executed command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliExecResponse {
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Exit code returned by the command.
    pub exit_code: i32,
    /// Whether the output was truncated due to size limits.
    pub truncated: bool,
    /// Whether the command was terminated due to timeout.
    pub timed_out: bool,
}

/// Shared state passed into the `context_agent_config` host function callback
/// via Extism's `UserData`.
struct AgentConfigData {
    agent_config: Arc<Mutex<AgentConfigResponse>>,
}

/// Shared state passed into the `zeroclaw_memory_recall` host function callback
/// via Extism's `UserData`.
struct MemoryRecallData {
    memory: Arc<dyn Memory>,
}

/// Shared state passed into the `zeroclaw_memory_forget` host function callback
/// via Extism's `UserData`.
struct MemoryForgetData {
    memory: Arc<dyn Memory>,
    plugin_name: String,
}

/// Shared state passed into the `zeroclaw_cli_exec` host function callback
/// via Extism's `UserData`.
///
/// Made public to enable integration testing of CLI execution resource limits.
#[derive(Clone)]
pub struct CliExecData {
    /// Plugin name for error messages and audit logging.
    pub plugin_name: String,
    /// Allowed commands the plugin can execute.
    pub allowed_commands: Vec<String>,
    /// Argument patterns for command validation.
    pub allowed_args: Vec<super::ArgPattern>,
    /// Environment variables the plugin is allowed to pass through.
    pub allowed_env: Vec<String>,
    /// Plugin's allowed filesystem paths (logical name -> physical path).
    pub allowed_paths: std::collections::HashMap<String, String>,
    /// Command timeout in milliseconds.
    pub timeout_ms: u64,
    /// Maximum output size in bytes before truncation.
    pub max_output_bytes: usize,
    /// Maximum concurrent CLI executions allowed for this plugin.
    pub max_concurrent: usize,
    /// Current count of active CLI executions (shared across all calls).
    pub concurrent_count: Arc<AtomicUsize>,
    /// Audit logger for recording CLI executions.
    pub audit: Arc<AuditLogger>,
    /// Shared CLI rate limiter for tracking executions per plugin.
    pub cli_rate_limiter: Arc<CliRateLimiter>,
    /// Maximum CLI executions allowed per minute for this plugin.
    pub rate_limit_per_minute: u32,
    /// Network security level for validation strictness.
    pub security_level: super::loader::NetworkSecurityLevel,
}

/// RAII guard that decrements the concurrent execution counter when dropped.
struct ConcurrencyGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Registry of host-defined functions that WASM plugins can call.
///
/// Holds shared references to ZeroClaw subsystems so host functions can
/// interact with memory, tools, and audit logging on behalf of plugins.
pub struct HostFunctionRegistry {
    /// Shared reference to the agent memory backend.
    pub memory: Arc<dyn Memory>,
    /// Tool implementations available for host-function delegation.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Audit logger for recording plugin host-function invocations.
    pub audit: Arc<AuditLogger>,
    /// Channel implementations available for messaging host functions.
    pub channels: HashMap<String, Arc<dyn Channel>>,
    /// Per-plugin per-channel rate limiter for messaging.
    pub channel_rate_limiter: Arc<ChannelRateLimiter>,
    /// Per-plugin rate limiter for CLI executions.
    pub cli_rate_limiter: Arc<CliRateLimiter>,
    /// Current session context snapshot exposed to plugins via `context_session`.
    pub session_context: Arc<Mutex<SessionContextResponse>>,
    /// Current user identity snapshot exposed to plugins via `context_user_identity`.
    pub user_identity: Arc<Mutex<UserIdentityResponse>>,
    /// Current agent config snapshot exposed to plugins via `context_agent_config`.
    pub agent_config: Arc<Mutex<AgentConfigResponse>>,
}

impl HostFunctionRegistry {
    /// Create a new registry with references to ZeroClaw subsystems.
    pub fn new(
        memory: Arc<dyn Memory>,
        tools: Vec<Arc<dyn Tool>>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        // Default rate limiter: 60 messages per plugin per channel per hour.
        let channel_rate_limiter = Arc::new(ChannelRateLimiter::new(60, 3600));
        // CLI rate limiter: per-plugin limits are configured in manifests.
        let cli_rate_limiter = Arc::new(CliRateLimiter::new());
        Self {
            memory,
            tools,
            audit,
            channels: HashMap::new(),
            channel_rate_limiter,
            cli_rate_limiter,
            session_context: Arc::new(Mutex::new(SessionContextResponse::default())),
            user_identity: Arc::new(Mutex::new(UserIdentityResponse::default())),
            agent_config: Arc::new(Mutex::new(AgentConfigResponse::default())),
        }
    }

    /// Add channel implementations for messaging host functions.
    pub fn with_channels(mut self, channels: HashMap<String, Arc<dyn Channel>>) -> Self {
        self.channels = channels;
        self
    }

    /// Set the current session context snapshot.
    ///
    /// This should be called before building host functions for a plugin
    /// invocation so that `context_session` returns up-to-date information.
    pub fn set_session_context(&self, ctx: SessionContextResponse) {
        *self.session_context.lock() = ctx;
    }

    /// Set the current user identity snapshot.
    ///
    /// This should be called before building host functions for a plugin
    /// invocation so that `context_user_identity` returns up-to-date information.
    pub fn set_user_identity(&self, identity: UserIdentityResponse) {
        *self.user_identity.lock() = identity;
    }

    /// Set the current agent config snapshot.
    ///
    /// This should be called before building host functions for a plugin
    /// invocation so that `context_agent_config` returns up-to-date information.
    pub fn set_agent_config(&self, config: AgentConfigResponse) {
        *self.agent_config.lock() = config;
    }

    /// Build the set of host functions a plugin is allowed to import based on
    /// its declared `[plugin.host_capabilities]`.
    ///
    /// A plugin that declares no host capabilities receives an empty vector —
    /// meaning no host-function imports are available to it at all.
    pub fn build_functions(&self, manifest: &PluginManifest) -> Vec<Function> {
        self.build_functions_for_level(
            manifest,
            crate::plugins::loader::NetworkSecurityLevel::Default,
        )
    }

    /// Build host functions with security-level enforcement.
    ///
    /// Behaves like [`build_functions`] but additionally enforces security-level
    /// restrictions. In `Paranoid` mode, **all context host functions are denied**
    /// regardless of the manifest's declared context capabilities.
    pub fn build_functions_for_level(
        &self,
        manifest: &PluginManifest,
        security_level: crate::plugins::loader::NetworkSecurityLevel,
    ) -> Vec<Function> {
        let caps = &manifest.host_capabilities;
        let mut fns: Vec<Function> = Vec::new();

        // Memory capability — up to 2 functions (read / write).
        if let Some(ref mem) = caps.memory {
            if mem.read {
                fns.push(self.make_zeroclaw_memory_recall_fn());
            }
            if mem.write {
                fns.push(self.make_zeroclaw_memory_store_fn(&manifest.name));
                fns.push(self.make_zeroclaw_memory_forget_fn(&manifest.name));
            }
        }

        // Tool delegation — 1 function.
        if let Some(ref td) = caps.tool_delegation {
            // The calling plugin's maximum risk level (from its [[tools]] entries)
            // acts as the ceiling for all delegated calls.
            fns.push(self.make_zeroclaw_tool_call_fn(&manifest.name, td.allowed_tools.clone()));
        }

        // Messaging — 2 functions.
        if let Some(ref msg) = caps.messaging {
            // Use the per-plugin rate limit from the manifest, or fall back to
            // the registry's default limiter.
            let limiter = Arc::new(ChannelRateLimiter::new(msg.rate_limit_per_hour, 3600));
            fns.push(self.make_zeroclaw_send_message_fn(
                &manifest.name,
                msg.allowed_channels.clone(),
                limiter,
            ));
            fns.push(self.make_zeroclaw_get_channels_fn(msg.allowed_channels.clone()));
        }

        // CLI execution — 1 function.
        // Paranoid mode denies ALL CLI execution regardless of manifest flags.
        if let Some(ref cli) = caps.cli {
            if security_level == crate::plugins::loader::NetworkSecurityLevel::Paranoid {
                tracing::warn!(
                    plugin = %manifest.name,
                    "plugin declares CLI capability but security level is Paranoid; denying CLI access"
                );
            } else {
                fns.push(self.make_zeroclaw_cli_exec_fn(
                    &manifest.name,
                    cli.clone(),
                    manifest.allowed_paths.clone(),
                    security_level,
                ));
            }
        }

        // Context — up to 3 functions (session / user_identity / agent_config).
        // Paranoid mode denies ALL context access regardless of manifest flags.
        if security_level != crate::plugins::loader::NetworkSecurityLevel::Paranoid {
            if let Some(ref ctx) = caps.context {
                if ctx.session {
                    fns.push(self.make_context_session_fn());
                }
                if ctx.user_identity {
                    fns.push(self.make_context_user_identity_fn());
                }
                if ctx.agent_config {
                    fns.push(self.make_context_agent_config_fn());
                }
            }
        }

        fns
    }

    /// Build a memory key tagged with the plugin name as author.
    ///
    /// All memory writes performed on behalf of a plugin must use this method
    /// to prefix the key so that stored entries can be attributed to the
    /// originating plugin.  Format: `plugin:<name>:<original_key>`.
    pub fn tagged_key(plugin_name: &str, key: &str) -> String {
        format!("plugin:{plugin_name}:{key}")
    }

    /// Build the `zeroclaw_memory_store` host function for a specific plugin.
    ///
    /// The function reads a JSON `{ "key": "..", "value": ".." }` payload from
    /// Extism shared memory, tags the key with the plugin name as author, and
    /// delegates to the configured `Memory` backend's `store` method.
    ///
    /// Returns `{ "success": true }` on success, or `{ "error": ".." }` on failure.
    fn make_zeroclaw_memory_store_fn(&self, plugin_name: &str) -> Function {
        let data = MemoryStoreData {
            memory: self.memory.clone(),
            plugin_name: plugin_name.to_string(),
        };
        Function::new(
            "zeroclaw_memory_store",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: MemoryStoreRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_memory_store input: {e}"))
                    })?;

                // Tag the key with the plugin name as author
                let tagged_key = HostFunctionRegistry::tagged_key(&data.plugin_name, &request.key);

                // Bridge async Memory::store to sync host function callback.
                // Spawn a blocking thread to avoid nested-runtime panics.
                let memory = data.memory.clone();
                let result = std::thread::scope(|s| {
                    s.spawn(|| {
                        tokio::runtime::Handle::current().block_on(memory.store(
                            &tagged_key,
                            &request.value,
                            MemoryCategory::Custom("plugin".into()),
                            None,
                        ))
                    })
                    .join()
                    .expect("memory store thread panicked")
                });

                // Encode response as JSON and write to shared memory
                let response_bytes = match result {
                    Ok(()) => serde_json::to_vec(&MemoryStoreResponse { success: true })
                        .expect("MemoryStoreResponse serialization is infallible"),
                    Err(e) => HostFunctionError::new(e.to_string()).to_json_bytes(),
                };

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_memory_recall` host function.
    ///
    /// The function reads a JSON `{ "query": ".." }` payload from Extism shared
    /// memory and delegates to the configured `Memory` backend's `recall` method.
    ///
    /// Returns `{ "results": "<json>" }` on success, or `{ "error": ".." }` on failure.
    fn make_zeroclaw_memory_recall_fn(&self) -> Function {
        let data = MemoryRecallData {
            memory: self.memory.clone(),
        };
        Function::new(
            "zeroclaw_memory_recall",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: MemoryRecallRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_memory_recall input: {e}"))
                    })?;

                // Bridge async Memory::recall to sync host function callback.
                let memory = data.memory.clone();
                let query = request.query;
                let result = std::thread::scope(|s| {
                    s.spawn(|| {
                        tokio::runtime::Handle::current()
                            .block_on(memory.recall(&query, 10, None, None, None))
                    })
                    .join()
                    .expect("memory recall thread panicked")
                });

                // Encode response as JSON and write to shared memory
                let response_bytes = match result {
                    Ok(entries) => {
                        let results_json = serde_json::to_string(&entries)
                            .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
                        serde_json::to_vec(&MemoryRecallResponse {
                            results: results_json,
                        })
                        .expect("MemoryRecallResponse serialization is infallible")
                    }
                    Err(e) => HostFunctionError::new(e.to_string()).to_json_bytes(),
                };

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_memory_forget` host function for a specific plugin.
    ///
    /// The function reads a JSON `{ "key": ".." }` payload from Extism shared
    /// memory, tags the key with the plugin name as author, and delegates to the
    /// configured `Memory` backend's `forget` method.
    ///
    /// Returns `{ "success": true }` if the key was removed, `{ "success": false }`
    /// if the key did not exist, or `{ "error": ".." }` on failure.
    fn make_zeroclaw_memory_forget_fn(&self, plugin_name: &str) -> Function {
        let data = MemoryForgetData {
            memory: self.memory.clone(),
            plugin_name: plugin_name.to_string(),
        };
        Function::new(
            "zeroclaw_memory_forget",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: MemoryForgetRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_memory_forget input: {e}"))
                    })?;

                // Tag the key with the plugin name as author
                let tagged_key = HostFunctionRegistry::tagged_key(&data.plugin_name, &request.key);

                // Bridge async Memory::forget to sync host function callback.
                let memory = data.memory.clone();
                let result = std::thread::scope(|s| {
                    s.spawn(|| {
                        tokio::runtime::Handle::current().block_on(memory.forget(&tagged_key))
                    })
                    .join()
                    .expect("memory forget thread panicked")
                });

                // Encode response as JSON and write to shared memory
                let response_bytes = match result {
                    Ok(removed) => serde_json::to_vec(&MemoryForgetResponse { success: removed })
                        .expect("MemoryForgetResponse serialization is infallible"),
                    Err(e) => HostFunctionError::new(e.to_string()).to_json_bytes(),
                };

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_tool_call` host function for a specific plugin.
    ///
    /// The function reads a JSON `{ "tool_name": "..", "arguments": {..} }` payload
    /// from Extism shared memory, validates that the requested tool is in the
    /// plugin's `allowed_tools` list, looks up the tool in the registry, and
    /// delegates to its `execute` method.
    ///
    /// Returns `{ "success": .., "output": "..", "error": ".." }` on completion,
    /// or `{ "error": ".." }` on failure.
    fn make_zeroclaw_tool_call_fn(
        &self,
        plugin_name: &str,
        allowed_tools: Vec<String>,
    ) -> Function {
        let data = ToolCallData {
            tools: self.tools.clone(),
            allowed_tools,
            plugin_name: plugin_name.to_string(),
        };
        Function::new(
            "zeroclaw_tool_call",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: ToolCallRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_tool_call input: {e}"))
                    })?;

                // Enforce call-depth limit to prevent infinite recursion
                // (e.g. plugin A → tool_call → plugin B → tool_call → plugin A …)
                let _depth_guard = DepthGuard::enter();
                let current_depth = DepthGuard::current();
                if current_depth > MAX_TOOL_CALL_DEPTH {
                    let err = HostFunctionError::new(format!(
                        "Maximum delegation depth exceeded ({current_depth}/{MAX_TOOL_CALL_DEPTH})"
                    ));
                    let handle = plugin.memory_new(err.to_json_bytes())?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

                // Validate that the tool is in the allowed list
                if !data
                    .allowed_tools
                    .iter()
                    .any(|t| t == "*" || t == &request.tool_name)
                {
                    let err = HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_tool_call] tool '{}' is not in allowed_tools",
                        data.plugin_name, request.tool_name
                    ));
                    let handle = plugin.memory_new(err.to_json_bytes())?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

                // Look up the tool by name
                let tool = data.tools.iter().find(|t| t.name() == request.tool_name);
                let tool = match tool {
                    Some(t) => Arc::clone(t),
                    None => {
                        let err = HostFunctionError::new(format!(
                            "[plugin:{}/zeroclaw_tool_call] tool '{}' not found in registry",
                            data.plugin_name, request.tool_name
                        ));
                        let handle = plugin.memory_new(err.to_json_bytes())?;
                        outputs[0] = plugin.memory_to_val(handle);
                        return Ok(());
                    }
                };

                // Risk level enforcement is handled at the tool level.
                // The Tool trait's execute() method applies its own security checks.

                // Bridge async Tool::execute to sync host function callback.
                let args = request.arguments;
                let result = std::thread::scope(|s| {
                    s.spawn(|| tokio::runtime::Handle::current().block_on(tool.execute(args)))
                        .join()
                        .expect("tool call thread panicked")
                });

                // Encode response as JSON and write to shared memory
                let response_bytes = match result {
                    Ok(tool_result) => serde_json::to_vec(&ToolCallResponse {
                        success: tool_result.success,
                        output: tool_result.output,
                        error: tool_result.error,
                    })
                    .expect("ToolCallResponse serialization is infallible"),
                    Err(e) => HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_tool_call] execution error: {e}",
                        data.plugin_name
                    ))
                    .to_json_bytes(),
                };

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_send_message` host function for a specific plugin.
    ///
    /// The function reads a JSON `{ "channel": "..", "recipient": "..", "message": ".." }`
    /// payload from Extism shared memory, validates the channel is in the plugin's
    /// `allowed_channels` list, looks up the channel by name, and delegates to its
    /// `send` method.
    ///
    /// Returns `{ "success": true }` on success, or `{ "error": ".." }` on failure.
    fn make_zeroclaw_send_message_fn(
        &self,
        plugin_name: &str,
        allowed_channels: Vec<String>,
        rate_limiter: Arc<ChannelRateLimiter>,
    ) -> Function {
        let data = ChannelSendData {
            channels: self.channels.clone(),
            allowed_channels,
            plugin_name: plugin_name.to_string(),
            rate_limiter,
        };
        Function::new(
            "zeroclaw_send_message",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: ChannelSendRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_send_message input: {e}"))
                    })?;

                // Validate that the channel is in the allowed list
                if !data
                    .allowed_channels
                    .iter()
                    .any(|c| c == "*" || c == &request.channel)
                {
                    let err = HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_send_message] channel '{}' is not in allowed_channels",
                        data.plugin_name, request.channel
                    ));
                    let handle = plugin.memory_new(err.to_json_bytes())?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

                // Enforce per-plugin per-channel rate limit
                if let Err(rate_err) = data
                    .rate_limiter
                    .record_send(&data.plugin_name, &request.channel)
                {
                    let err = HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_send_message] {}",
                        data.plugin_name, rate_err
                    ));
                    let handle = plugin.memory_new(err.to_json_bytes())?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

                // Look up the channel by name
                let channel = match data.channels.get(&request.channel) {
                    Some(ch) => Arc::clone(ch),
                    None => {
                        let err = HostFunctionError::new(format!(
                            "[plugin:{}/zeroclaw_send_message] channel '{}' not found",
                            data.plugin_name, request.channel
                        ));
                        let handle = plugin.memory_new(err.to_json_bytes())?;
                        outputs[0] = plugin.memory_to_val(handle);
                        return Ok(());
                    }
                };

                // Bridge async Channel::send to sync host function callback.
                let msg = SendMessage::new(request.message, request.recipient);
                let result = std::thread::scope(|s| {
                    s.spawn(|| tokio::runtime::Handle::current().block_on(channel.send(&msg)))
                        .join()
                        .expect("channel send thread panicked")
                });

                // Encode response as JSON and write to shared memory
                let response_bytes = match result {
                    Ok(()) => serde_json::to_vec(&ChannelSendResponse { success: true })
                        .expect("ChannelSendResponse serialization is infallible"),
                    Err(e) => HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_send_message] send error: {e}",
                        data.plugin_name
                    ))
                    .to_json_bytes(),
                };

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_get_channels` host function.
    ///
    /// Takes no meaningful input (empty JSON `{}`), returns
    /// `{ "channels": ["slack", "telegram", ...] }` listing only the channel names
    /// that are both registered in the runtime and permitted by the plugin's
    /// `allowed_channels` list. No credentials, tokens, or connection details are
    /// exposed.
    fn make_zeroclaw_get_channels_fn(&self, allowed_channels: Vec<String>) -> Function {
        let data = ChannelGetData {
            channels: self.channels.clone(),
            allowed_channels,
        };
        Function::new(
            "zeroclaw_get_channels",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, _inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Filter registered channels by allowed_channels list
                let mut channels: Vec<String> = data
                    .channels
                    .keys()
                    .filter(|name| {
                        data.allowed_channels
                            .iter()
                            .any(|a| a == "*" || a == name.as_str())
                    })
                    .cloned()
                    .collect();
                channels.sort();

                let response_bytes = serde_json::to_vec(&GetChannelsResponse { channels })
                    .expect("GetChannelsResponse serialization is infallible");

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `context_session` host function.
    ///
    /// Returns a JSON `SessionContextResponse` with the current channel name,
    /// conversation ID, and timestamp. This is a read-only getter — no input
    /// is expected from the plugin.
    fn make_context_session_fn(&self) -> Function {
        let data = SessionContextData {
            session_context: self.session_context.clone(),
        };
        Function::new(
            "context_session",
            [],             // no input — read-only getter
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, _inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                let ctx = data.session_context.lock();
                let response_bytes = serde_json::to_vec(&*ctx)
                    .expect("SessionContextResponse serialization is infallible");

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `context_user_identity` host function.
    ///
    /// Returns a JSON `UserIdentityResponse` with the requesting user's
    /// username, display name, and channel-specific ID. This is a read-only
    /// getter — no input is expected from the plugin.
    fn make_context_user_identity_fn(&self) -> Function {
        let data = UserIdentityData {
            user_identity: self.user_identity.clone(),
        };
        Function::new(
            "context_user_identity",
            [],             // no input — read-only getter
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, _inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                let identity = data.user_identity.lock();
                let response_bytes = serde_json::to_vec(&*identity)
                    .expect("UserIdentityResponse serialization is infallible");

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `context_agent_config` host function.
    ///
    /// Returns a JSON `AgentConfigResponse` with the agent's name, personality
    /// traits, and identity fields. This is a read-only getter — no input
    /// is expected from the plugin.
    fn make_context_agent_config_fn(&self) -> Function {
        let data = AgentConfigData {
            agent_config: self.agent_config.clone(),
        };
        Function::new(
            "context_agent_config",
            [],             // no input — read-only getter
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, _inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                let config = data.agent_config.lock();
                let response_bytes = serde_json::to_vec(&*config)
                    .expect("AgentConfigResponse serialization is infallible");

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Build the `zeroclaw_cli_exec` host function for a specific plugin.
    ///
    /// The function reads a JSON `CliExecRequest` payload from Extism shared memory,
    /// validates the command and arguments against the plugin's CLI capability config,
    /// spawns the process via `std::process::Command` (no shell), captures output,
    /// and returns a `CliExecResponse`.
    ///
    /// # Security
    ///
    /// - Commands are validated against the plugin's `allowed_commands` list
    /// - Arguments are validated against `allowed_args` patterns and checked for
    ///   shell metacharacters and path traversal
    /// - Working directory must be within the plugin's `allowed_paths`
    /// - Environment variables are filtered to only allow those in `allowed_env`
    /// - Output is truncated if it exceeds `max_output_bytes`
    /// - Commands are killed if they exceed `timeout_ms`
    fn make_zeroclaw_cli_exec_fn(
        &self,
        plugin_name: &str,
        cli_cap: super::CliCapability,
        allowed_paths: std::collections::HashMap<String, String>,
        security_level: super::loader::NetworkSecurityLevel,
    ) -> Function {
        let data = CliExecData {
            plugin_name: plugin_name.to_string(),
            allowed_commands: cli_cap.allowed_commands,
            allowed_args: cli_cap.allowed_args,
            allowed_env: cli_cap.allowed_env,
            allowed_paths,
            timeout_ms: cli_cap.timeout_ms,
            max_output_bytes: cli_cap.max_output_bytes,
            max_concurrent: cli_cap.max_concurrent,
            concurrent_count: Arc::new(AtomicUsize::new(0)),
            audit: Arc::clone(&self.audit),
            cli_rate_limiter: Arc::clone(&self.cli_rate_limiter),
            rate_limit_per_minute: cli_cap.rate_limit_per_minute,
            security_level,
        };
        Function::new(
            "zeroclaw_cli_exec",
            [ValType::I64], // input: memory handle with JSON payload
            [ValType::I64], // output: memory handle with JSON response
            UserData::new(data),
            |plugin, inputs, outputs, user_data| {
                let data_lock = user_data.get()?;
                let data = data_lock
                    .lock()
                    .map_err(|e| extism::Error::msg(format!("failed to lock user data: {e}")))?;

                // Read JSON input from shared memory
                let input_bytes: Vec<u8> = plugin.memory_get_val(&inputs[0])?;
                let request: CliExecRequest =
                    serde_json::from_slice(&input_bytes).map_err(|e| {
                        extism::Error::msg(format!("invalid zeroclaw_cli_exec input: {e}"))
                    })?;

                // Check rate limit before proceeding
                if let Err(retry_after) = data
                    .cli_rate_limiter
                    .record_execution(&data.plugin_name, data.rate_limit_per_minute)
                {
                    let response = CliExecResponse {
                        stdout: String::new(),
                        stderr: format!(
                            "[plugin:{}] rate limit exceeded ({} executions/minute). Retry after {} seconds.",
                            data.plugin_name, data.rate_limit_per_minute, retry_after
                        ),
                        exit_code: -1,
                        truncated: false,
                        timed_out: false,
                    };
                    let response_bytes = serde_json::to_vec(&response)
                        .expect("CliExecResponse serialization is infallible");
                    let handle = plugin.memory_new(&response_bytes)?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

                // Check concurrent execution limit before proceeding
                loop {
                    let current = data.concurrent_count.load(Ordering::SeqCst);
                    if current >= data.max_concurrent {
                        // Limit reached, return error response
                        let response = CliExecResponse {
                            stdout: String::new(),
                            stderr: format!(
                                "[plugin:{}] concurrent execution limit reached ({}/{})",
                                data.plugin_name, current, data.max_concurrent
                            ),
                            exit_code: -1,
                            truncated: false,
                            timed_out: false,
                        };
                        let response_bytes = serde_json::to_vec(&response)
                            .expect("CliExecResponse serialization is infallible");
                        let handle = plugin.memory_new(&response_bytes)?;
                        outputs[0] = plugin.memory_to_val(handle);
                        return Ok(());
                    }
                    // Try to increment the counter atomically
                    if data
                        .concurrent_count
                        .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        break;
                    }
                    // CAS failed, another thread incremented - retry
                }

                // RAII guard ensures counter is decremented even on panic/early return
                let _guard = ConcurrencyGuard {
                    counter: Arc::clone(&data.concurrent_count),
                };

                // Capture start time for duration measurement
                let start_time = Instant::now();

                // Execute the CLI command with validation
                let response = execute_cli_command(&data, &request);

                // Calculate duration and log the CLI execution
                let duration_ms =
                    u64::try_from(start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                let output_bytes = response.stdout.len() + response.stderr.len();
                let audit_entry = CliAuditEntry::new(
                    &data.plugin_name,
                    &request.command,
                    &request.args,
                    request.working_dir.clone(),
                    response.exit_code,
                    duration_ms,
                    output_bytes,
                    response.truncated,
                    response.timed_out,
                );

                // Log to audit trail (ignore errors to avoid failing the CLI call)
                let _ = data.audit.log_cli(&audit_entry);

                // Encode response as JSON and write to shared memory
                let response_bytes = serde_json::to_vec(&response)
                    .expect("CliExecResponse serialization is infallible");

                let handle = plugin.memory_new(&response_bytes)?;
                outputs[0] = plugin.memory_to_val(handle);

                Ok(())
            },
        )
    }

    /// Create a placeholder host function with the given name.
    ///
    /// The function accepts no WASM-level parameters and returns no values.
    /// Input/output is transferred via Extism shared memory (the JSON ABI),
    /// not through WASM function parameters.
    fn _stub_host_fn(name: &str) -> Function {
        Function::new(
            name,
            [],             // no wasm-level params — data goes via shared memory
            [ValType::I64], // return offset into shared memory
            UserData::new(()),
            |_plugin, _inputs, _outputs, _user_data| Ok(()),
        )
    }
}

/// Execute a CLI command with validation and security checks.
///
/// This function performs the following steps:
/// 1. Validates the command against the plugin's allowed commands list
/// 2. Validates arguments against allowed patterns and checks for shell metacharacters
/// 3. Validates path traversal in arguments
/// 4. Validates working directory is within allowed paths
/// 5. Sanitizes environment variables to only include allowed ones
/// 6. Spawns the process via `std::process::Command` (no shell)
/// 7. Captures stdout and stderr with size limits
/// 8. Handles command timeout
///
/// Made public to enable integration testing of CLI execution resource limits.
pub fn execute_cli_command(data: &CliExecData, request: &CliExecRequest) -> CliExecResponse {
    // Step 1: Validate command against allowed_commands list and resolve to path
    let command_path = match validate_command_allowlist(&request.command, &data.allowed_commands) {
        Ok(path) => path,
        Err(e) => {
            return CliExecResponse {
                stdout: String::new(),
                stderr: format!(
                    "[plugin:{}] command validation failed: {}",
                    data.plugin_name, e
                ),
                exit_code: -1,
                truncated: false,
                timed_out: false,
            };
        }
    };

    // Step 2: Validate arguments against allowed patterns and shell metacharacters
    // At Strict level, use exact matching (no glob patterns allowed)
    // At Default level, allow glob patterns but log warnings for broad ones
    let args_refs: Vec<&str> = request.args.iter().map(|s| s.as_str()).collect();

    let validation_result = if data.security_level == super::loader::NetworkSecurityLevel::Strict {
        validate_arguments_strict(&request.command, &args_refs, &data.allowed_args)
    } else {
        // At Default level, warn about broad patterns (e.g., patterns ending in '*')
        if data.security_level == super::loader::NetworkSecurityLevel::Default {
            warn_broad_cli_patterns(&data.plugin_name, &request.command, &data.allowed_args);
        }
        validate_arguments(&request.command, &args_refs, &data.allowed_args)
    };

    if let Err(e) = validation_result {
        return CliExecResponse {
            stdout: String::new(),
            stderr: format!(
                "[plugin:{}] argument validation failed: {}",
                data.plugin_name, e
            ),
            exit_code: -1,
            truncated: false,
            timed_out: false,
        };
    }

    // Step 3: Validate path traversal in arguments
    if let Err(e) = validate_path_traversal(&args_refs) {
        return CliExecResponse {
            stdout: String::new(),
            stderr: format!(
                "[plugin:{}] path traversal rejected: {}",
                data.plugin_name, e
            ),
            exit_code: -1,
            truncated: false,
            timed_out: false,
        };
    }

    // Step 4: Validate working directory is within allowed paths
    let working_dir = if let Some(ref wd) = request.working_dir {
        let wd_path = Path::new(wd);

        // Check if the working directory is within any of the plugin's allowed paths
        let is_within_allowed = data.allowed_paths.values().any(|allowed_path| {
            let allowed = Path::new(allowed_path);
            // Canonicalize both paths to handle symlinks and relative components
            match (wd_path.canonicalize(), allowed.canonicalize()) {
                (Ok(wd_canon), Ok(allowed_canon)) => wd_canon.starts_with(&allowed_canon),
                // If canonicalization fails, fall back to prefix check
                _ => wd_path.starts_with(allowed),
            }
        });

        if !is_within_allowed {
            return CliExecResponse {
                stdout: String::new(),
                stderr: format!(
                    "[plugin:{}] working directory '{}' is not within plugin's allowed_paths",
                    data.plugin_name, wd
                ),
                exit_code: -1,
                truncated: false,
                timed_out: false,
            };
        }

        Some(wd_path.to_path_buf())
    } else {
        // Default to first allowed_path (sorted alphabetically by key for determinism)
        data.allowed_paths
            .keys()
            .min()
            .and_then(|key| data.allowed_paths.get(key))
            .map(PathBuf::from)
    };

    // Step 5: Build the command with sanitized environment
    let mut cmd = Command::new(&command_path);
    cmd.args(&request.args);

    // Clear all environment variables and only set allowed ones
    cmd.env_clear();

    if let Some(ref env) = request.env {
        for (key, value) in env {
            // Only pass through environment variables that are in the allowed list
            if data.allowed_env.contains(key) {
                cmd.env(key, value);
            }
        }
    }

    // Set working directory if specified
    if let Some(ref wd) = working_dir {
        cmd.current_dir(wd);
    }

    // Step 6: Spawn the process and capture output with timeout
    let timeout = Duration::from_millis(data.timeout_ms);
    let start_time = Instant::now();

    // Use spawn + wait_with_output pattern for better timeout handling
    let child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return CliExecResponse {
                stdout: String::new(),
                stderr: format!(
                    "[plugin:{}] failed to spawn command '{}': {}",
                    data.plugin_name, request.command, e
                ),
                exit_code: -1,
                truncated: false,
                timed_out: false,
            };
        }
    };

    // Wait for the child process with timeout
    let (output_result, timed_out) = wait_with_timeout(child, timeout, start_time);

    match output_result {
        Ok(output) => {
            let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Step 7: Truncate output if it exceeds max_output_bytes
            let mut truncated = false;
            if stdout.len() > data.max_output_bytes {
                stdout.truncate(data.max_output_bytes);
                stdout.push_str("\n[output truncated]");
                truncated = true;
            }
            if stderr.len() > data.max_output_bytes {
                stderr.truncate(data.max_output_bytes);
                stderr.push_str("\n[output truncated]");
                truncated = true;
            }

            let exit_code = if timed_out {
                // Use 128 + 9 (SIGKILL) as the exit code for timeout
                137
            } else {
                output.status.code().unwrap_or(-1)
            };

            CliExecResponse {
                stdout,
                stderr,
                exit_code,
                truncated,
                timed_out,
            }
        }
        Err(e) => CliExecResponse {
            stdout: String::new(),
            stderr: format!(
                "[plugin:{}] command execution failed: {}",
                data.plugin_name, e
            ),
            exit_code: -1,
            truncated: false,
            timed_out,
        },
    }
}

/// Wait for a child process with timeout, killing it if necessary.
///
/// Returns the output result and whether the process was killed due to timeout.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
    start_time: Instant,
) -> (std::io::Result<std::process::Output>, bool) {
    // Use a simple polling approach with small sleep intervals
    let poll_interval = Duration::from_millis(10);

    loop {
        // Check if the process has finished
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Process finished, get the output
                let output = child.wait_with_output();
                return (output, false);
            }
            Ok(None) => {
                // Process still running, check timeout
                if start_time.elapsed() >= timeout {
                    // Kill the process
                    let _ = child.kill();
                    // Wait for it to actually exit and get output
                    let output = child.wait_with_output();
                    return (output, true);
                }
                // Sleep briefly before polling again
                std::thread::sleep(poll_interval);
            }
            Err(e) => {
                return (Err(e), false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_rate_limiter_allows_within_limit() {
        let limiter = CliRateLimiter::new();

        // Should allow 3 executions when limit is 3
        assert!(limiter.record_execution("test-plugin", 3).is_ok());
        assert!(limiter.record_execution("test-plugin", 3).is_ok());
        assert!(limiter.record_execution("test-plugin", 3).is_ok());

        // 4th should fail
        assert!(limiter.record_execution("test-plugin", 3).is_err());
    }

    #[test]
    fn cli_rate_limiter_zero_limit_allows_all() {
        let limiter = CliRateLimiter::new();

        // Zero limit means unlimited
        for _ in 0..100 {
            assert!(limiter.record_execution("test-plugin", 0).is_ok());
        }
    }

    #[test]
    fn cli_rate_limiter_tracks_per_plugin() {
        let limiter = CliRateLimiter::new();

        // Each plugin has independent limits
        assert!(limiter.record_execution("plugin-a", 2).is_ok());
        assert!(limiter.record_execution("plugin-a", 2).is_ok());
        assert!(limiter.record_execution("plugin-a", 2).is_err()); // plugin-a exhausted

        // plugin-b still has budget
        assert!(limiter.record_execution("plugin-b", 2).is_ok());
        assert!(limiter.record_execution("plugin-b", 2).is_ok());
        assert!(limiter.record_execution("plugin-b", 2).is_err()); // plugin-b exhausted
    }

    #[test]
    fn cli_rate_limiter_returns_retry_after() {
        let limiter = CliRateLimiter::new();

        // Exhaust the limit
        assert!(limiter.record_execution("test-plugin", 1).is_ok());

        // Should return retry_after seconds
        let err = limiter.record_execution("test-plugin", 1).unwrap_err();
        assert!(
            err > 0 && err <= 61,
            "retry_after should be reasonable: {}",
            err
        );
    }

    #[test]
    fn cli_rate_limiter_default_impl() {
        // Test Default trait implementation
        let limiter = CliRateLimiter::default();
        assert!(limiter.record_execution("test-plugin", 5).is_ok());
    }
}
