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

use crate::channels::traits::{Channel, SendMessage};
use crate::memory::traits::{Memory, MemoryCategory};
use crate::plugins::PluginManifest;
use crate::security::audit::AuditLogger;
use crate::tools::traits::RiskLevel;
use crate::tools::traits::Tool;
use extism::Function;
use extism::UserData;
use extism::ValType;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
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
        let cutoff = Instant::now() - self.window;
        timestamps.retain(|t| *t > cutoff);

        if timestamps.len() as u32 >= self.max_per_window {
            return Err(format!(
                "Rate limit exceeded: plugin '{}' has exhausted its messaging budget for channel '{}'",
                plugin_name, channel
            ));
        }

        timestamps.push(Instant::now());
        Ok(())
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
    caller_max_risk: RiskLevel,
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
        Self {
            memory,
            tools,
            audit,
            channels: HashMap::new(),
            channel_rate_limiter,
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
            let caller_max_risk = manifest
                .tools
                .iter()
                .map(|t| t.risk_level)
                .max()
                .unwrap_or(RiskLevel::Low);
            fns.push(self.make_zeroclaw_tool_call_fn(
                &manifest.name,
                td.allowed_tools.clone(),
                caller_max_risk,
            ));
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
        caller_max_risk: RiskLevel,
    ) -> Function {
        let data = ToolCallData {
            tools: self.tools.clone(),
            allowed_tools,
            plugin_name: plugin_name.to_string(),
            caller_max_risk,
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
                    let handle = plugin.memory_new(&err.to_json_bytes())?;
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
                    let handle = plugin.memory_new(&err.to_json_bytes())?;
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
                        let handle = plugin.memory_new(&err.to_json_bytes())?;
                        outputs[0] = plugin.memory_to_val(handle);
                        return Ok(());
                    }
                };

                // Enforce risk level ceiling: the delegated tool's risk must
                // not exceed the calling plugin's maximum risk level.
                if tool.risk_level() > data.caller_max_risk {
                    let err = HostFunctionError::new(format!(
                        "[plugin:{}/zeroclaw_tool_call] risk level exceeded: tool '{}' is {:?} but caller ceiling is {:?}",
                        data.plugin_name, request.tool_name, tool.risk_level(), data.caller_max_risk
                    ));
                    let handle = plugin.memory_new(&err.to_json_bytes())?;
                    outputs[0] = plugin.memory_to_val(handle);
                    return Ok(());
                }

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
                    let handle = plugin.memory_new(&err.to_json_bytes())?;
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
                    let handle = plugin.memory_new(&err.to_json_bytes())?;
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
                        let handle = plugin.memory_new(&err.to_json_bytes())?;
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

    /// Create a placeholder host function with the given name.
    ///
    /// The function accepts no WASM-level parameters and returns no values.
    /// Input/output is transferred via Extism shared memory (the JSON ABI),
    /// not through WASM function parameters.
    fn stub_host_fn(name: &str) -> Function {
        Function::new(
            name,
            [],             // no wasm-level params — data goes via shared memory
            [ValType::I64], // return offset into shared memory
            UserData::new(()),
            |_plugin, _inputs, _outputs, _user_data| Ok(()),
        )
    }
}
