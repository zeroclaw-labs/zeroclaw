//! WASM plugin system for ZeroClaw.
//!
//! Plugins are WebAssembly **components** (WIT / wasmtime component model) that
//! extend ZeroClaw with custom tools. Discovery, manifests, and Ed25519
//! signatures live here; execution runs on the [`wit_runtime`] component runtime
//! with deny-by-default host capabilities. Enable with `--features plugins-wasm`.

pub mod error;
pub mod host;
pub mod signature;
pub mod wasm_channel;

// ── wasmtime component-model (WIT) runtime ────────────────────────────────
// The plugin execution path. Gated behind `plugins-wasmtime`.
#[cfg(feature = "plugins-wasmtime")]
mod bindings;
#[cfg(feature = "plugins-wasmtime")]
mod limiter;
#[cfg(feature = "plugins-wasmtime")]
mod store;
#[cfg(feature = "plugins-wasmtime")]
pub mod usage;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_config;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_error;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_host;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_runtime;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_tool;
#[cfg(feature = "plugins-wasmtime")]
pub mod wit_types;

#[cfg(feature = "plugins-wasmtime")]
pub use wit_config::{WIT_TOOL_VERSION, WitToolLimits, WitToolRuntimeConfig};
#[cfg(feature = "plugins-wasmtime")]
pub use wit_error::{WasmError, WasmHostError};
#[cfg(feature = "plugins-wasmtime")]
pub use wit_host::{
    DenyWasmHostHttp, DenyWasmHostSecrets, DenyWasmHostTools, DenyWasmHostWorkspace,
    RecordingWasmHostHttp, SystemWasmHostClock, WasmHostClock, WasmHostHttp, WasmHostSecrets,
    WasmHostTools, WasmHostWorkspace, WasmHttpRequest, WasmHttpResponse, WitToolHost,
};
#[cfg(feature = "plugins-wasmtime")]
pub use wit_runtime::WitToolRuntime;
#[cfg(feature = "plugins-wasmtime")]
pub use wit_tool::WitTool;
#[cfg(feature = "plugins-wasmtime")]
pub use wit_types::{
    PreparedWitTool, WasmLogLevel, WasmLogRecord, WitToolExecution, WitToolRequest,
};

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
    /// Capabilities this plugin provides
    pub capabilities: Vec<PluginCapability>,
    /// Permissions this plugin requests
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    /// Host-injected credential grants. Each names a secret the host injects
    /// into matching HTTP requests at the egress boundary; the guest never sees
    /// the value (it can only check existence via the `secret_exists` permission).
    /// Covered by the manifest signature, so a grant cannot be tampered with
    /// after signing.
    #[serde(default)]
    pub credentials: Vec<CredentialGrant>,
    /// Ed25519 signature over the canonical manifest (base64url-encoded).
    /// Set by the plugin publisher when signing the manifest.
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
}

/// A host-side credential injection rule (a `[[credentials]]` entry).
///
/// The host resolves `secret` to a value (from `[http].secrets` or the
/// environment) and injects it into the named header of outbound HTTP requests
/// whose URL matches `url_prefix`. The secret value is never exposed to the
/// guest — only the host adds the header, at the egress boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CredentialGrant {
    /// Name of the secret to inject (looked up host-side, never returned to WASM).
    pub secret: String,
    /// HTTP header to inject the credential into (e.g. `Authorization`).
    pub header: String,
    /// Header value template; `{secret}` is replaced with the resolved value
    /// (e.g. `"Bearer {secret}"`). Defaults to the bare secret value.
    #[serde(default = "default_credential_template")]
    pub value_template: String,
    /// Only inject when the request URL starts with this prefix. `None` injects
    /// for any request the plugin's HTTP allowlist already permits.
    #[serde(default)]
    pub url_prefix: Option<String>,
}

fn default_credential_template() -> String {
    "{secret}".to_string()
}

impl CredentialGrant {
    /// Whether this grant applies to the given request URL.
    pub fn matches_url(&self, url: &str) -> bool {
        match &self.url_prefix {
            Some(prefix) => url.starts_with(prefix),
            None => true,
        }
    }

    /// Render the header value for a resolved secret.
    pub fn render(&self, secret_value: &str) -> String {
        self.value_template.replace("{secret}", secret_value)
    }
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
    /// Provides one or more agentskills.io-format skills under `skills/`
    Skill,
}

/// Permissions a plugin may request. Each maps to a WIT `host` import; a
/// permission the manifest does not declare resolves to a deny-by-default host
/// service, so the capability is unreachable from the guest.
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    /// Make HTTP requests via `host.http-request` (credentials injected host-side).
    HttpClient,
    /// Read workspace files via `host.workspace-read` (rooted, no `..`).
    WorkspaceRead,
    /// Check secret existence via `host.secret-exists` (never the value).
    SecretExists,
    /// Invoke other agent tools by alias via `host.tool-invoke`.
    ToolInvoke,
    /// Read agent memory (reserved; no host import yet).
    MemoryRead,
    /// Write agent memory (reserved; no host import yet).
    MemoryWrite,
    /// Deprecated: the old Extism `env_read` capability. Accepted so existing
    /// manifests still deserialize, but it grants nothing — secrets are now
    /// host-injected at the HTTP boundary. Use [`SecretExists`](Self::SecretExists).
    EnvRead,
    /// Deprecated alias for [`WorkspaceRead`](Self::WorkspaceRead).
    FileRead,
    /// Deprecated: filesystem writes are not exposed to the component sandbox.
    FileWrite,
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
