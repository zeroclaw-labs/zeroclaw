//! WASM plugin system for ZeroClaw.
//!
//! Plugins are WebAssembly modules loaded via Extism that can extend
//! ZeroClaw with custom tools and channels. Enable with `--features plugins-wasm`.

pub mod capabilities;
pub mod error;
pub mod host;
pub mod signature;
pub mod wasm_channel;
pub mod wasm_tool;

pub use capabilities::{
    ArgPattern, CliCapability, ContextCapability, DEFAULT_CLI_MAX_CONCURRENT,
    DEFAULT_CLI_MAX_OUTPUT_BYTES, DEFAULT_CLI_RATE_LIMIT_PER_MINUTE, DEFAULT_CLI_TIMEOUT_MS,
    MemoryCapability, MessagingCapability, PluginCapabilities, ToolDefinition,
    ToolDelegationCapability,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Path to the .wasm file (relative to manifest)
    pub wasm_path: String,
    /// Capabilities this plugin provides
    pub capabilities: Vec<PluginCapability>,
    /// Permissions this plugin requests
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    /// Hosts the plugin is allowed to make network requests to.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Filesystem path mappings the plugin is allowed to access (logical name → path).
    #[serde(default)]
    pub allowed_paths: HashMap<String, String>,
    /// Tool definitions declared via `[[tools]]` sections.
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    /// Arbitrary plugin configuration from `[plugin.config]`.
    #[serde(default)]
    pub config: HashMap<String, serde_json::Value>,
    /// Whether this plugin runs in a WASI environment.
    #[serde(default = "default_wasi")]
    pub wasi: bool,
    /// Maximum execution time in milliseconds for plugin calls.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Ed25519 signature over the canonical manifest (base64url-encoded).
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
    /// Host-side capabilities this plugin requests (memory, tool delegation, etc.).
    #[serde(default)]
    pub host_capabilities: PluginCapabilities,
}

impl PluginManifest {
    /// Parse a manifest from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, error::PluginError> {
        toml::from_str(toml_str).map_err(|e| error::PluginError::InvalidManifest(e.to_string()))
    }
}

fn default_wasi() -> bool {
    true
}

fn default_timeout_ms() -> u64 {
    30_000
}

/// What a plugin can do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

/// Permissions a plugin may request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    /// Can make HTTP requests
    HttpClient,
    /// Can read from the filesystem (within sandbox)
    FileRead,
    /// Can write to the filesystem (within sandbox)
    FileWrite,
    /// Can access environment variables
    EnvRead,
    /// Can read agent memory
    MemoryRead,
    /// Can write agent memory
    MemoryWrite,
}

/// Information about a loaded plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub capabilities: Vec<PluginCapability>,
    pub permissions: Vec<PluginPermission>,
    pub wasm_path: PathBuf,
    pub loaded: bool,
    /// Tool definitions from the manifest.
    pub tools: Vec<ToolDefinition>,
    /// Whether this plugin is enabled (user-togglable).
    pub enabled: bool,
    /// SHA-256 hash of the WASM binary, hex-encoded.
    pub wasm_sha256: Option<String>,
    /// Hosts allowed for network requests.
    pub allowed_hosts: Vec<String>,
    /// Filesystem path mappings.
    pub allowed_paths: HashMap<String, String>,
    /// Plugin configuration declarations.
    pub config: HashMap<String, serde_json::Value>,
    /// Host-side capabilities.
    pub host_capabilities: PluginCapabilities,
}
