# Plugin SDK Reference

The `zeroclaw-plugin-sdk` crate provides ergonomic Rust wrappers for calling ZeroClaw host functions from inside a WASM plugin.

**Crate location:** `crates/zeroclaw-plugin-sdk/`

## Installation

```toml
[dependencies]
zeroclaw-plugin-sdk = { git = "https://github.com/zeroclaw-labs/zeroclaw", path = "crates/zeroclaw-plugin-sdk" }
extism-pdk = "1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

Your crate must compile to a WASM `cdylib`:

```toml
[lib]
crate-type = ["cdylib"]
```

---

## Modules

The SDK exports five modules matching the host capability groups:

```rust
use zeroclaw_plugin_sdk::memory;
use zeroclaw_plugin_sdk::tools;
use zeroclaw_plugin_sdk::messaging;
use zeroclaw_plugin_sdk::context;
use zeroclaw_plugin_sdk::cli;
```

Each function returns `Result<T, extism_pdk::Error>`.

---

## `memory` -- persistent storage

Store, recall, and forget key-value pairs in the agent's memory system. Requires `host_capabilities.memory` in the manifest.

### `memory::store(key, value)`

```rust
pub fn store(key: &str, value: &str) -> Result<(), Error>
```

Persist a key-value pair. Overwrites any existing entry with the same key.

**Manifest requirement:** `memory.write = true`

```rust
memory::store("user:preference", "dark-mode")?;
```

### `memory::recall(query)`

```rust
pub fn recall(query: &str) -> Result<String, Error>
```

Search memory for entries matching the query string. Returns the matching value(s) as a string (empty string if no match).

**Manifest requirement:** `memory.read = true`

```rust
let prefs = memory::recall("user:preference")?;
if !prefs.is_empty() {
    // Use remembered preference
}
```

### `memory::forget(key)`

```rust
pub fn forget(key: &str) -> Result<(), Error>
```

Delete a memory entry by exact key.

**Manifest requirement:** `memory.write = true`

```rust
memory::forget("user:preference")?;
```

---

## `tools` -- tool delegation

Call other tools registered in the agent. Requires `host_capabilities.tool_delegation` in the manifest.

### `tools::tool_call(tool_name, input)`

```rust
pub fn tool_call(tool_name: &str, input: serde_json::Value) -> Result<String, Error>
```

Invoke a named tool with JSON arguments. Returns the tool's output string on success.

**Manifest requirement:** tool must be listed in `tool_delegation.allowed_tools`

**Depth limit:** 5 levels of nested tool calls.

```rust
let result = tools::tool_call("web_search", serde_json::json!({
    "query": "rust wasm plugins"
}))?;
```

---

## `messaging` -- channel messaging

Send messages through the agent's configured channels. Requires `host_capabilities.messaging` in the manifest.

### `messaging::send(channel, recipient, message)`

```rust
pub fn send(channel: &str, recipient: &str, message: &str) -> Result<(), Error>
```

Send a message to a recipient on the specified channel.

**Manifest requirement:** channel must be listed in `messaging.allowed_channels`

**Rate limit:** enforced per `rate_limit_per_hour` (default 60).

```rust
messaging::send("slack", "#general", "Build completed successfully!")?;
```

### `messaging::get_channels()`

```rust
pub fn get_channels() -> Result<Vec<String>, Error>
```

List all channel names available in the agent.

```rust
let channels = messaging::get_channels()?;
for ch in &channels {
    println!("Available: {ch}");
}
```

---

## `context` -- session and identity

Access runtime context about the current invocation. Requires `host_capabilities.context` in the manifest.

> **Note:** All context access is denied in Paranoid security level.

### `context::session()`

```rust
pub fn session() -> Result<SessionContext, Error>
```

Returns the current session context.

**Manifest requirement:** `context.session = true`

```rust
pub struct SessionContext {
    /// Channel name (e.g. "telegram", "slack")
    pub channel_name: String,
    /// Opaque conversation/session ID
    pub conversation_id: String,
    /// ISO-8601 timestamp of the current request
    pub timestamp: String,
}
```

```rust
let session = context::session()?;
println!("Channel: {}, Conversation: {}", session.channel_name, session.conversation_id);
```

### `context::user_identity()`

```rust
pub fn user_identity() -> Result<UserIdentity, Error>
```

Returns information about the user who triggered this invocation.

**Manifest requirement:** `context.user_identity = true`

```rust
pub struct UserIdentity {
    /// Username (e.g. "jdoe")
    pub username: String,
    /// Display name (e.g. "Jane Doe")
    pub display_name: String,
    /// Channel-specific user identifier
    pub channel_user_id: String,
}
```

```rust
let user = context::user_identity()?;
println!("Hello, {}!", user.display_name);
```

### `context::agent_config()`

```rust
pub fn agent_config() -> Result<AgentConfig, Error>
```

Returns the agent's personality and identity configuration.

**Manifest requirement:** `context.agent_config = true`

```rust
pub struct AgentConfig {
    /// Agent's display name
    pub name: String,
    /// Personality traits (e.g. ["friendly", "concise"])
    pub personality_traits: Vec<String>,
    /// Arbitrary identity key-value pairs
    pub identity: HashMap<String, String>,
}
```

```rust
let config = context::agent_config()?;
let tone = if config.personality_traits.contains(&"formal".into()) {
    "formal"
} else {
    "casual"
};
```

---

## `cli` -- command execution

Execute shell commands with security constraints. Requires `capabilities.cli` in the manifest.

### `cli::cli_exec(command, args, working_dir, env)`

```rust
pub fn cli_exec(
    command: &str,
    args: &[&str],
    working_dir: Option<&str>,
    env: Option<HashMap<String, String>>,
) -> Result<CliResponse, Error>
```

Execute a CLI command. Returns stdout, stderr, exit code, and flags indicating truncation or timeout.

**Manifest requirement:** command must be in `capabilities.cli.allowed_commands`, arguments must match `allowed_args` patterns, and `working_dir` must be within `allowed_paths`.

```rust
use zeroclaw_plugin_sdk::cli::cli_exec;

// Simple command
let response = cli_exec("git", &["status"], None, None)?;
println!("Exit: {}, Output: {}", response.exit_code, response.stdout);

// With working directory
let response = cli_exec(
    "cargo",
    &["build", "--release"],
    Some("/workspace/my-project"),
    None,
)?;

// With environment variables
use std::collections::HashMap;
let mut env = HashMap::new();
env.insert("GIT_AUTHOR_NAME".to_string(), "Bot".to_string());
let response = cli_exec("git", &["commit", "-m", "Auto"], Some("/repo"), Some(env))?;
```

### `CliResponse` struct

```rust
pub struct CliResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub truncated: bool,   // Output exceeded size limits
    pub timed_out: bool,   // Command exceeded timeout
}
```

### `CliError` enum

```rust
pub enum CliError {
    PermissionDenied,                        // Command/args not allowed
    RateLimited { retry_after_secs: u64 },   // Too many requests
    CommandNotFound,                         // Binary not on system
    ExecutionFailed { stderr: String },      // Non-zero exit
    Timeout,                                 // Exceeded time limit
    OutputTruncated,                         // Output too large
}
```

---

## Error handling

All SDK functions return `Result<T, extism_pdk::Error>`. Errors are propagated from the host function JSON response when `success: false` or when deserialization fails.

Idiomatic pattern for graceful degradation:

```rust
// Fall back to a default when a host function fails
let username = context::user_identity()
    .map(|u| u.display_name)
    .unwrap_or_else(|_| "friend".into());
```

---

## Complete example

See `tests/plugins/sdk-example-plugin/` for a full working plugin ("Smart Greeter") that combines context, memory, and tool delegation in a single workflow.

Build it with:

```bash
cd tests/plugins/sdk-example-plugin
cargo build --target wasm32-wasi --release
```
