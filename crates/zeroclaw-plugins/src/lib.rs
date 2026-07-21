//! WASM plugin system for ZeroClaw.
//! Plugins are WebAssembly components loaded via wasmtime that can extend
//! ZeroClaw with custom tools and channels. Enable with a `plugins-wasm*` feature.

#[cfg(feature = "plugins-wasmtime")]
pub mod component;
#[cfg(feature = "plugins-wasmtime")]
mod component_config;
#[cfg(feature = "plugins-wasmtime")]
mod component_logging;
#[cfg(feature = "plugins-wasmtime")]
mod component_secrets;
#[cfg(feature = "plugins-wasmtime")]
mod component_state;
pub mod config;
pub mod egress;
pub mod endpoint;
pub mod error;
pub mod event;
pub mod host;
pub mod instance;
pub mod registry;
#[cfg(feature = "plugins-wasmtime")]
pub mod runtime;
#[cfg(feature = "plugins-wasmtime")]
pub mod services;
pub mod signature;
#[cfg(feature = "plugins-wasmtime")]
pub mod wasm_channel;
#[cfg(feature = "plugins-wasmtime")]
pub mod wasm_memory;
#[cfg(feature = "plugins-wasmtime")]
pub mod wasm_tool;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A plugin's declared manifest (loaded from manifest.toml alongside the .wasm).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin name (unique identifier)
    pub name: String,
    /// Plugin version
    pub version: String,
    /// Human-readable description
    pub description: Option<String>,
    /// Author name or organization
    pub author: Option<String>,
    /// Path to the .wasm file (relative to manifest).
    /// Required for tool/channel/memory/observer plugins; optional (and ignored)
    /// for skill-only plugins, which carry no WASM payload.
    #[serde(default)]
    pub wasm_path: Option<String>,
    /// Lowercase or uppercase hexadecimal SHA-256 of the exact WASM payload.
    /// Required for executable plugins when signature policy is strict.
    #[serde(default)]
    pub wasm_sha256: Option<String>,
    /// Capabilities this plugin provides
    pub capabilities: Vec<PluginCapability>,
    /// Permissions this plugin requests
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    /// Draft 2020-12 JSON Schema for this plugin's private config object.
    /// Required exactly when `config_read` is requested.
    /// Direct top-level string properties marked `x-secret: true` are withheld
    /// from public config and served through the scoped secrets import during
    /// tool execution or channel service calls. Channel calls obtain the
    /// remaining typed public object through the scoped config import.
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    /// Ed25519 signature over the canonical manifest (base64url-encoded).
    /// Set by the plugin publisher when signing the manifest.
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
}

/// What a plugin can do.
#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    /// Provides one or more tools
    Tool,
    /// Provides a channel implementation
    Channel,
    /// Provides a memory backend
    Memory,
    /// Provides an observer/metrics backend
    Observer,
    /// Provides one or more agentskills.io-format skills under `skills/`
    Skill,
}

/// Permissions a plugin may request.
#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    /// Can make HTTP requests
    HttpClient,
    /// Can open host-mediated outbound WebSocket connections.
    #[serde(rename = "websocket_client")]
    WebSocketClient,
    /// Can open host-mediated outbound TCP, TLS, and STARTTLS connections.
    SocketClient,
    /// Can read from the filesystem (within sandbox)
    FileRead,
    /// Can write to the filesystem (within sandbox)
    FileWrite,
    /// Can read its own resolved per-plugin config section
    #[serde(alias = "env_read")]
    ConfigRead,
    /// Can read agent memory
    MemoryRead,
    /// Can write agent memory
    MemoryWrite,
    /// Can read this exact plugin instance's encrypted durable state
    StateRead,
    /// Can write this exact plugin instance's encrypted durable state
    StateWrite,
}

/// Information about a loaded plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub capabilities: Vec<PluginCapability>,
    pub permissions: Vec<PluginPermission>,
    /// Resolved path to the WASM file. `None` for skill-only plugins.
    pub wasm_path: Option<PathBuf>,
    pub loaded: bool,
}
