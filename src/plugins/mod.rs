//! WASM plugin system for ZeroClaw.
//!
//! Plugins are WebAssembly modules loaded via Extism that can extend
//! ZeroClaw with custom tools and channels. Enable with `--features plugins-wasm`.

pub mod error;
pub mod host;
pub mod host_functions;
pub mod loader;
pub mod signature;
pub mod wasm_channel;
pub mod wasm_tool;

use error::PluginError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    /// Set by the plugin publisher when signing the manifest.
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
    /// Host-side capabilities this plugin requests (memory, tool delegation, etc.).
    #[serde(default)]
    pub host_capabilities: PluginCapabilities,
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

/// Re-export from [`crate::tools::traits::RiskLevel`] so that existing code
/// that imports `crate::plugins::RiskLevel` continues to compile.
pub use crate::tools::traits::RiskLevel;

/// A tool declared in a plugin manifest via `[[tools]]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Tool name (must be unique within the plugin)
    pub name: String,
    /// Human-readable description of what the tool does
    pub description: String,
    /// WASM export function name the runtime calls to invoke this tool
    pub export: String,
    /// Risk level for this tool
    pub risk_level: RiskLevel,
    /// JSON Schema describing the tool's parameters (arbitrary JSON value).
    /// Accepts both `parameters_schema` (flat format) and `parameters` (nested format).
    #[serde(default, alias = "parameters")]
    pub parameters_schema: Option<serde_json::Value>,
}

/// Host-side capabilities a plugin may request via `[plugin.host_capabilities]`.
///
/// Each field maps to a subsystem the plugin wants to interact with through
/// host functions. All fields are optional and default to `None` — a plugin
/// that declares no host capabilities receives no host-function imports.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginCapabilities {
    /// Access to the agent memory subsystem.
    #[serde(default)]
    pub memory: Option<MemoryCapability>,
    /// Ability to delegate work to other tools registered in the agent.
    #[serde(default)]
    pub tool_delegation: Option<ToolDelegationCapability>,
    /// Ability to send messages through agent channels.
    #[serde(default)]
    pub messaging: Option<MessagingCapability>,
    /// Access to runtime context (session, user identity, agent config).
    #[serde(default)]
    pub context: Option<ContextCapability>,
}

/// Memory subsystem access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryCapability {
    /// Whether the plugin can read from agent memory.
    #[serde(default)]
    pub read: bool,
    /// Whether the plugin can write to agent memory.
    #[serde(default)]
    pub write: bool,
}

/// Tool delegation capability — allows a plugin to invoke other tools.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDelegationCapability {
    /// Tool names this plugin is allowed to delegate to.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Messaging capability — allows a plugin to send messages via channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessagingCapability {
    /// Channel names this plugin is allowed to send messages through.
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    /// Maximum messages per plugin per channel within the rate limit window.
    /// Defaults to 60 if not specified.
    #[serde(default = "default_messaging_rate_limit")]
    pub rate_limit_per_hour: u32,
}

fn default_messaging_rate_limit() -> u32 {
    60
}

impl Default for MessagingCapability {
    fn default() -> Self {
        Self {
            allowed_channels: Vec::new(),
            rate_limit_per_hour: default_messaging_rate_limit(),
        }
    }
}

/// Context access capability — controls what runtime context a plugin can read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextCapability {
    /// Access to session-level context (conversation state).
    #[serde(default)]
    pub session: bool,
    /// Access to user identity information.
    #[serde(default)]
    pub user_identity: bool,
    /// Access to agent configuration.
    #[serde(default)]
    pub agent_config: bool,
}

/// Information about a loaded plugin.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub capabilities: Vec<PluginCapability>,
    pub permissions: Vec<PluginPermission>,
    pub tools: Vec<ToolDefinition>,
    pub wasm_path: PathBuf,
    pub loaded: bool,
    /// Whether this plugin is enabled (user-togglable).
    pub enabled: bool,
    /// SHA-256 hash of the WASM binary recorded at install/discover time.
    pub wasm_sha256: Option<String>,
    /// Hosts the plugin is allowed to make network requests to.
    pub allowed_hosts: Vec<String>,
    /// Filesystem path mappings the plugin is allowed to access.
    pub allowed_paths: HashMap<String, String>,
    /// Plugin configuration key-value pairs from manifest.
    pub config: HashMap<String, serde_json::Value>,
    /// Host-side capabilities this plugin requests.
    pub host_capabilities: PluginCapabilities,
}

// ---------------------------------------------------------------------------
// Nested plugin.toml format: [plugin], [plugin.config], [plugin.network],
// [plugin.filesystem], [[tools]] with [tools.parameters]
// ---------------------------------------------------------------------------

/// Top-level wrapper for the nested `plugin.toml` spec format.
#[derive(Debug, Deserialize)]
struct NestedPluginToml {
    plugin: NestedPluginSection,
    #[serde(default)]
    tools: Vec<ToolDefinition>,
}

/// The `[plugin]` section in the nested format.
#[derive(Debug, Deserialize)]
struct NestedPluginSection {
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<String>,
    wasm_path: String,
    capabilities: Vec<PluginCapability>,
    #[serde(default)]
    permissions: Vec<PluginPermission>,
    #[serde(default = "default_wasi")]
    wasi: bool,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    publisher_key: Option<String>,
    #[serde(default)]
    config: HashMap<String, serde_json::Value>,
    #[serde(default)]
    network: Option<NetworkSection>,
    #[serde(default)]
    filesystem: Option<FilesystemSection>,
    #[serde(default)]
    host_capabilities: PluginCapabilities,
}

/// `[plugin.network]` section.
#[derive(Debug, Deserialize)]
struct NetworkSection {
    #[serde(default)]
    allowed_hosts: Vec<String>,
}

/// `[plugin.filesystem]` section — keys are logical names, values are paths.
#[derive(Debug, Deserialize)]
struct FilesystemSection {
    #[serde(flatten)]
    allowed_paths: HashMap<String, String>,
}

impl PluginManifest {
    /// Parse a plugin manifest from TOML text.
    ///
    /// Supports two formats:
    /// - **Flat format**: fields at the top level (legacy / simple manifests).
    /// - **Nested format**: fields under `[plugin]`, with `[plugin.config]`,
    ///   `[plugin.network]`, `[plugin.filesystem]` sub-sections.
    ///
    /// Returns clear [`PluginError`] variants for missing required fields and
    /// malformed TOML.
    pub fn parse(toml_str: &str) -> Result<Self, PluginError> {
        // First, try to parse the raw TOML to get a table we can inspect.
        let raw: toml::Value = toml::from_str(toml_str).map_err(|e| {
            PluginError::MalformedToml(e.to_string())
        })?;

        let table = raw.as_table().ok_or_else(|| {
            PluginError::MalformedToml("expected a TOML table at the top level".into())
        })?;

        // Decide which format we're dealing with.
        if table.contains_key("plugin") {
            Self::parse_nested(toml_str, table)
        } else {
            Self::parse_flat(toml_str, table)
        }
    }

    /// Parse the nested `[plugin]` format.
    fn parse_nested(toml_str: &str, table: &toml::map::Map<String, toml::Value>) -> Result<Self, PluginError> {
        // Validate the plugin section is a table.
        let plugin_val = table.get("plugin").unwrap();
        if !plugin_val.is_table() {
            return Err(PluginError::MalformedToml(
                "'plugin' must be a table section".into(),
            ));
        }

        let nested: NestedPluginToml = toml::from_str(toml_str).map_err(|e| {
            // Map common serde errors to clearer messages.
            let msg = e.to_string();
            if msg.contains("missing field") {
                if let Some(field) = Self::extract_field_name(&msg) {
                    return PluginError::MissingField { field };
                }
            }
            PluginError::MalformedToml(msg)
        })?;

        let p = nested.plugin;
        Ok(PluginManifest {
            name: p.name,
            version: p.version,
            description: p.description,
            author: p.author,
            wasm_path: p.wasm_path,
            capabilities: p.capabilities,
            permissions: p.permissions,
            allowed_hosts: p.network.map_or_else(Vec::new, |n| n.allowed_hosts),
            allowed_paths: p.filesystem.map_or_else(HashMap::new, |f| f.allowed_paths),
            tools: nested.tools,
            config: p.config,
            wasi: p.wasi,
            timeout_ms: p.timeout_ms,
            signature: p.signature,
            publisher_key: p.publisher_key,
            host_capabilities: p.host_capabilities,
        })
    }

    /// Parse the flat (legacy) format.
    fn parse_flat(toml_str: &str, table: &toml::map::Map<String, toml::Value>) -> Result<Self, PluginError> {
        // Check required fields up front for clear error messages.
        for field in &["name", "version", "wasm_path", "capabilities"] {
            if !table.contains_key(*field) {
                return Err(PluginError::MissingField {
                    field: field.to_string(),
                });
            }
        }

        toml::from_str(toml_str).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("missing field") {
                if let Some(field) = Self::extract_field_name(&msg) {
                    return PluginError::MissingField { field };
                }
            }
            PluginError::MalformedToml(msg)
        })
    }

    /// Extract a field name from a serde "missing field `foo`" error message.
    fn extract_field_name(msg: &str) -> Option<String> {
        let marker = "missing field `";
        let start = msg.find(marker)? + marker.len();
        let end = msg[start..].find('`')? + start;
        Some(msg[start..end].to_string())
    }
}

/// Build a human-readable audit summary of a plugin manifest.
///
/// Returns a formatted string showing the plugin's identity, network access,
/// filesystem access, host capabilities, and tool risk levels with approval
/// requirements per autonomy level — without installing anything.
pub fn format_audit_summary(manifest: &PluginManifest) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    // Header
    writeln!(out, "Plugin: {} v{}", manifest.name, manifest.version).unwrap();
    if let Some(desc) = &manifest.description {
        writeln!(out, "  Description: {desc}").unwrap();
    }
    if let Some(author) = &manifest.author {
        writeln!(out, "  Author: {author}").unwrap();
    }
    writeln!(out).unwrap();

    // Network access
    writeln!(out, "Network access:").unwrap();
    if manifest.allowed_hosts.is_empty() {
        writeln!(out, "  (none)").unwrap();
    } else {
        for host in &manifest.allowed_hosts {
            writeln!(out, "  \u{2713} {host}").unwrap();
        }
    }
    writeln!(out).unwrap();

    // Filesystem access
    writeln!(out, "Filesystem access:").unwrap();
    if manifest.allowed_paths.is_empty() {
        writeln!(out, "  (none)").unwrap();
    } else {
        for (logical, physical) in &manifest.allowed_paths {
            writeln!(out, "  \u{2713} {logical} \u{2192} {physical}").unwrap();
        }
    }
    writeln!(out).unwrap();

    // Host capabilities
    writeln!(out, "Host capabilities:").unwrap();
    if manifest.capabilities.is_empty() && manifest.permissions.is_empty() {
        writeln!(out, "  (none)").unwrap();
    } else {
        for cap in &manifest.capabilities {
            let label = match cap {
                PluginCapability::Tool => "tool provider",
                PluginCapability::Channel => "channel provider",
                PluginCapability::Memory => "memory backend",
                PluginCapability::Observer => "observer/metrics",
            };
            writeln!(out, "  \u{2713} {label}").unwrap();
        }
        for perm in &manifest.permissions {
            let label = match perm {
                PluginPermission::HttpClient => "http client",
                PluginPermission::FileRead => "filesystem (read)",
                PluginPermission::FileWrite => "filesystem (write)",
                PluginPermission::EnvRead => "environment variables (read)",
                PluginPermission::MemoryRead => "memory (read)",
                PluginPermission::MemoryWrite => "memory (write)",
            };
            writeln!(out, "  \u{2713} {label}").unwrap();
        }
    }
    writeln!(out).unwrap();

    // Risk levels
    writeln!(out, "Risk levels:").unwrap();
    if manifest.tools.is_empty() {
        writeln!(out, "  (no tools)").unwrap();
    } else {
        for tool in &manifest.tools {
            let (level_str, approval) = match tool.risk_level {
                RiskLevel::Low => ("low", ""),
                RiskLevel::Medium => {
                    ("medium", " (requires approval in supervised mode)")
                }
                RiskLevel::High => {
                    ("high", " (requires approval in supervised mode)")
                }
            };
            writeln!(
                out,
                "  \u{2022} {:<24}\u{2192} {level_str}{approval}",
                tool.name
            )
            .unwrap();
        }
    }

    // Trim trailing newline for clean output
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Display a human-readable audit summary of a plugin manifest.
///
/// Shows the plugin's identity, network access, filesystem access,
/// host capabilities, and tool risk levels without installing anything.
pub fn display_audit(manifest: &PluginManifest) {
    println!("{}", format_audit_summary(manifest));
}

/// Returns `true` if a manifest config declaration marks the key as sensitive.
///
/// A key is considered sensitive when the declaration is an object containing
/// `"sensitive": true`. Bare-string and non-object declarations are not sensitive.
pub fn is_sensitive_key(decl: &serde_json::Value) -> bool {
    decl.as_object()
        .and_then(|obj| obj.get("sensitive"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Resolve plugin configuration for Extism from ZeroClaw's `config.toml`.
///
/// Takes the manifest's declared config keys (`manifest.config`) and the
/// per-plugin config values from `[plugins.<name>]` in `config.toml`.
///
/// Each manifest config entry can be:
/// - An object with `"required": true` — the key **must** be in `config_values`.
/// - An object with `"default": "<value>"` — used when `config_values` omits the key.
/// - An object with `"sensitive": true` — values are redacted in log output.
/// - A bare string — treated as a default value.
///
/// Returns a `BTreeMap<String, String>` suitable for passing to Extism, or a
/// [`PluginError::MissingConfig`] listing every missing required key.
pub fn resolve_plugin_config(
    plugin_name: &str,
    manifest_config: &HashMap<String, serde_json::Value>,
    config_values: Option<&HashMap<String, String>>,
) -> Result<BTreeMap<String, String>, PluginError> {
    let empty = HashMap::new();
    let values = config_values.unwrap_or(&empty);

    let mut resolved = BTreeMap::new();
    let mut missing = Vec::new();

    for (key, decl) in manifest_config {
        let sensitive = is_sensitive_key(decl);

        if let Some(val) = values.get(key) {
            let display_val = if sensitive {
                crate::security::redact(val)
            } else {
                val.clone()
            };
            tracing::debug!(
                plugin = %plugin_name,
                key = %key,
                value = %display_val,
                sensitive,
                "resolved config key from operator config"
            );
            resolved.insert(key.clone(), val.clone());
            continue;
        }

        // Extract default or required flag from the declaration.
        match decl {
            serde_json::Value::String(default) => {
                tracing::debug!(
                    plugin = %plugin_name,
                    key = %key,
                    "using bare-string default for config key"
                );
                resolved.insert(key.clone(), default.clone());
            }
            serde_json::Value::Object(obj) => {
                if obj
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    missing.push(key.clone());
                } else if let Some(default) = obj.get("default").and_then(|v| v.as_str()) {
                    let display_val = if sensitive {
                        crate::security::redact(default)
                    } else {
                        default.to_string()
                    };
                    tracing::debug!(
                        plugin = %plugin_name,
                        key = %key,
                        value = %display_val,
                        sensitive,
                        "using manifest default for config key"
                    );
                    resolved.insert(key.clone(), default.to_string());
                }
                // If neither required nor has a default, the key is simply omitted.
            }
            _ => {
                // Non-string, non-object declarations (e.g. numbers, bools) — use
                // the JSON representation as the string value default.
                resolved.insert(key.clone(), decl.to_string());
            }
        }
    }

    if !missing.is_empty() {
        missing.sort();
        tracing::warn!(
            plugin = %plugin_name,
            missing_keys = %missing.join(", "),
            "plugin config resolution failed — required keys missing"
        );
        return Err(PluginError::MissingConfig {
            plugin: plugin_name.to_string(),
            keys: missing.join(", "),
        });
    }

    // Also pass through any extra keys from config_values that aren't declared
    // in the manifest — operators may set ad-hoc config the plugin reads at
    // runtime.
    for (key, val) in values {
        if !resolved.contains_key(key) {
            tracing::debug!(
                plugin = %plugin_name,
                key = %key,
                "passing through undeclared config key from operator config"
            );
            resolved.insert(key.clone(), val.clone());
        }
    }

    tracing::info!(
        plugin = %plugin_name,
        keys = %resolved.keys().cloned().collect::<Vec<_>>().join(", "),
        "plugin config resolved successfully"
    );

    Ok(resolved)
}

/// Decrypt any encrypted config values (`enc2:` or legacy `enc:` prefix) in-place
/// using the provided [`SecretStore`](crate::security::SecretStore).
///
/// Non-encrypted values (no recognized prefix) pass through unchanged.
pub fn decrypt_plugin_config_values(
    values: &mut HashMap<String, String>,
    store: &crate::security::SecretStore,
) -> Result<(), PluginError> {
    for (key, val) in values.iter_mut() {
        if crate::security::SecretStore::is_encrypted(val) {
            *val = store.decrypt(val).map_err(|e| PluginError::ConfigDecrypt {
                key: key.clone(),
                source: e,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_allowed_hosts_and_paths() {
        let toml_str = r#"
name = "net-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_hosts = ["api.example.com", "cdn.example.com"]

[allowed_paths]
data = "/var/data"
cache = "/tmp/cache"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.allowed_hosts, vec!["api.example.com", "cdn.example.com"]);
        assert_eq!(manifest.allowed_paths.len(), 2);
        assert_eq!(manifest.allowed_paths["data"], "/var/data");
        assert_eq!(manifest.allowed_paths["cache"], "/tmp/cache");
    }

    #[test]
    fn test_tools_entries_deserialize_into_tool_definition() {
        let toml_str = r#"
name = "tool-plugin"
version = "1.0.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "search"
description = "Search the knowledge base"
export = "tool_search"
risk_level = "low"
parameters_schema = { type = "object", properties = { query = { type = "string" } }, required = ["query"] }

[[tools]]
name = "execute"
description = "Execute a command"
export = "tool_execute"
risk_level = "high"
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.tools.len(), 2);

        let search = &manifest.tools[0];
        assert_eq!(search.name, "search");
        assert_eq!(search.description, "Search the knowledge base");
        assert_eq!(search.export, "tool_search");
        assert_eq!(search.risk_level, RiskLevel::Low);
        assert!(search.parameters_schema.is_some());
        let schema = search.parameters_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert_eq!(schema["required"][0], "query");

        let execute = &manifest.tools[1];
        assert_eq!(execute.name, "execute");
        assert_eq!(execute.description, "Execute a command");
        assert_eq!(execute.export, "tool_execute");
        assert_eq!(execute.risk_level, RiskLevel::High);
        assert!(execute.parameters_schema.is_none());
    }

    #[test]
    fn test_tools_default_to_empty() {
        let toml_str = r#"
name = "no-tools"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["channel"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.tools.is_empty());
    }

    #[test]
    fn test_malformed_manifest_missing_required_field_name() {
        let toml_str = r#"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("name"), "error should mention missing field 'name': {msg}");
    }

    #[test]
    fn test_malformed_manifest_missing_required_field_version() {
        let toml_str = r#"
name = "test"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("version"), "error should mention missing field 'version': {msg}");
    }

    #[test]
    fn test_malformed_manifest_missing_required_field_wasm_path() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
capabilities = ["tool"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("wasm_path"), "error should mention missing field 'wasm_path': {msg}");
    }

    #[test]
    fn test_malformed_manifest_invalid_capability() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["nonexistent"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for invalid capability: {msg}");
    }

    #[test]
    fn test_malformed_manifest_invalid_toml_syntax() {
        let toml_str = r#"
name = "test"
version = "0.1.0
wasm_path = "plugin.wasm"
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for invalid TOML syntax: {msg}");
    }

    #[test]
    fn test_malformed_manifest_wrong_type_for_field() {
        let toml_str = r#"
name = 42
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("string"), "error should mention expected type: {msg}");
    }

    #[test]
    fn test_malformed_manifest_invalid_permission() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["fly_to_moon"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for invalid permission: {msg}");
    }

    #[test]
    fn test_malformed_manifest_tool_missing_required_fields() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "incomplete"
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for tool missing required fields: {msg}");
    }

    #[test]
    fn test_malformed_manifest_allowed_hosts_wrong_type() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_hosts = "not-a-list"
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for wrong type: {msg}");
    }

    #[test]
    fn test_malformed_manifest_allowed_paths_wrong_type() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
allowed_paths = ["not", "a", "map"]
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for wrong type: {msg}");
    }

    #[test]
    fn test_manifest_allowed_hosts_and_paths_default_to_empty() {
        let toml_str = r#"
name = "minimal"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert!(manifest.allowed_hosts.is_empty());
        assert!(manifest.allowed_paths.is_empty());
    }

    #[test]
    fn test_full_valid_manifest_all_fields() {
        let toml_str = r#"
name = "full-plugin"
version = "2.3.1"
description = "A fully-featured plugin"
author = "ZeroClaw Labs"
wasm_path = "full_plugin.wasm"
capabilities = ["tool", "channel"]
permissions = ["http_client", "file_read", "env_read"]
allowed_hosts = ["api.example.com"]
wasi = false
timeout_ms = 60000
signature = "c2lnbmF0dXJl"
publisher_key = "abcdef1234567890"

[allowed_paths]
data = "/var/data"

[[tools]]
name = "greet"
description = "Say hello"
export = "tool_greet"
risk_level = "low"
parameters_schema = { type = "object", properties = { name = { type = "string" } } }
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.name, "full-plugin");
        assert_eq!(manifest.version, "2.3.1");
        assert_eq!(manifest.description.as_deref(), Some("A fully-featured plugin"));
        assert_eq!(manifest.author.as_deref(), Some("ZeroClaw Labs"));
        assert_eq!(manifest.wasm_path, "full_plugin.wasm");
        assert_eq!(manifest.capabilities, vec![PluginCapability::Tool, PluginCapability::Channel]);
        assert_eq!(manifest.permissions, vec![
            PluginPermission::HttpClient,
            PluginPermission::FileRead,
            PluginPermission::EnvRead,
        ]);
        assert_eq!(manifest.allowed_hosts, vec!["api.example.com"]);
        assert_eq!(manifest.allowed_paths["data"], "/var/data");
        assert!(!manifest.wasi);
        assert_eq!(manifest.timeout_ms, 60_000);
        assert_eq!(manifest.signature.as_deref(), Some("c2lnbmF0dXJl"));
        assert_eq!(manifest.publisher_key.as_deref(), Some("abcdef1234567890"));
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "greet");
    }

    #[test]
    fn test_valid_manifest_all_permissions() {
        let toml_str = r#"
name = "perm-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["http_client", "file_read", "file_write", "env_read", "memory_read", "memory_write"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.permissions.len(), 6);
        assert_eq!(manifest.permissions, vec![
            PluginPermission::HttpClient,
            PluginPermission::FileRead,
            PluginPermission::FileWrite,
            PluginPermission::EnvRead,
            PluginPermission::MemoryRead,
            PluginPermission::MemoryWrite,
        ]);
    }

    #[test]
    fn test_valid_manifest_all_capabilities() {
        let toml_str = r#"
name = "multi-cap"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool", "channel", "memory", "observer"]
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.capabilities, vec![
            PluginCapability::Tool,
            PluginCapability::Channel,
            PluginCapability::Memory,
            PluginCapability::Observer,
        ]);
    }

    #[test]
    fn test_malformed_manifest_missing_capabilities() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("capabilities"), "error should mention missing 'capabilities': {msg}");
    }

    #[test]
    fn test_valid_manifest_minimal_required_fields_only() {
        let toml_str = r#"
name = "bare-minimum"
version = "0.0.1"
wasm_path = "min.wasm"
capabilities = []
"#;
        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();

        assert_eq!(manifest.name, "bare-minimum");
        assert_eq!(manifest.version, "0.0.1");
        assert_eq!(manifest.wasm_path, "min.wasm");
        assert!(manifest.capabilities.is_empty());
        assert!(manifest.description.is_none());
        assert!(manifest.author.is_none());
        assert!(manifest.permissions.is_empty());
        assert!(manifest.allowed_hosts.is_empty());
        assert!(manifest.allowed_paths.is_empty());
        assert!(manifest.tools.is_empty());
        assert!(manifest.signature.is_none());
        assert!(manifest.publisher_key.is_none());
        assert!(manifest.wasi);
        assert_eq!(manifest.timeout_ms, 30_000);
    }

    #[test]
    fn test_malformed_manifest_empty_input() {
        let toml_str = "";
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        assert!(!err.to_string().is_empty(), "should produce an error for empty input");
    }

    #[test]
    fn test_malformed_manifest_tool_wrong_type_for_risk_level() {
        let toml_str = r#"
name = "test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "bad-tool"
description = "A tool"
export = "tool_bad"
risk_level = 42
"#;
        let err = toml::from_str::<PluginManifest>(toml_str).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.is_empty(), "should produce an error for wrong type in tool field: {msg}");
    }

    // -----------------------------------------------------------------------
    // Tests for PluginManifest::parse() — nested format and error variants
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_nested_format_full() {
        let toml_str = r#"
[plugin]
name = "nested-plugin"
version = "1.0.0"
description = "A plugin using nested format"
author = "ZeroClaw Labs"
wasm_path = "nested.wasm"
capabilities = ["tool"]
permissions = ["http_client"]

[plugin.config]
api_key = "placeholder"
max_retries = 3

[plugin.network]
allowed_hosts = ["api.example.com", "cdn.example.com"]

[plugin.filesystem]
data = "/var/data"
cache = "/tmp/cache"

[[tools]]
name = "search"
description = "Search the index"
export = "tool_search"
risk_level = "low"

[tools.parameters]
type = "object"
required = ["query"]
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.name, "nested-plugin");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.description.as_deref(), Some("A plugin using nested format"));
        assert_eq!(manifest.author.as_deref(), Some("ZeroClaw Labs"));
        assert_eq!(manifest.wasm_path, "nested.wasm");
        assert_eq!(manifest.capabilities, vec![PluginCapability::Tool]);
        assert_eq!(manifest.permissions, vec![PluginPermission::HttpClient]);
        assert_eq!(manifest.allowed_hosts, vec!["api.example.com", "cdn.example.com"]);
        assert_eq!(manifest.allowed_paths.len(), 2);
        assert_eq!(manifest.allowed_paths["data"], "/var/data");
        assert_eq!(manifest.allowed_paths["cache"], "/tmp/cache");
        assert_eq!(manifest.config.len(), 2);
        assert_eq!(manifest.config["api_key"], "placeholder");
        assert_eq!(manifest.config["max_retries"], 3);
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "search");
        assert_eq!(manifest.tools[0].risk_level, RiskLevel::Low);
        let schema = manifest.tools[0].parameters_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_parse_flat_format_still_works() {
        let toml_str = r#"
name = "flat-plugin"
version = "0.1.0"
wasm_path = "flat.wasm"
capabilities = ["tool"]
allowed_hosts = ["api.example.com"]

[allowed_paths]
data = "/var/data"

[[tools]]
name = "greet"
description = "Say hello"
export = "tool_greet"
risk_level = "low"
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.name, "flat-plugin");
        assert_eq!(manifest.allowed_hosts, vec!["api.example.com"]);
        assert_eq!(manifest.allowed_paths["data"], "/var/data");
        assert_eq!(manifest.tools.len(), 1);
    }

    #[test]
    fn test_parse_missing_field_error() {
        let toml_str = r#"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "name"),
            other => panic!("expected MissingField, got: {other}"),
        }
    }

    #[test]
    fn test_parse_nested_missing_field_error() {
        let toml_str = r#"
[plugin]
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "name"),
            other => panic!("expected MissingField, got: {other}"),
        }
    }

    #[test]
    fn test_parse_malformed_toml_error() {
        let toml_str = r#"
name = "test"
version = "0.1.0
wasm_path = "plugin.wasm"
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        assert!(matches!(err, PluginError::MalformedToml(_)));
    }

    #[test]
    fn test_parse_nested_minimal() {
        let toml_str = r#"
[plugin]
name = "minimal-nested"
version = "0.1.0"
wasm_path = "min.wasm"
capabilities = []
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.name, "minimal-nested");
        assert!(manifest.allowed_hosts.is_empty());
        assert!(manifest.allowed_paths.is_empty());
        assert!(manifest.config.is_empty());
        assert!(manifest.tools.is_empty());
    }

    #[test]
    fn test_parse_nested_network_only() {
        let toml_str = r#"
[plugin]
name = "net-only"
version = "0.1.0"
wasm_path = "net.wasm"
capabilities = ["tool"]

[plugin.network]
allowed_hosts = ["api.example.com"]
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.allowed_hosts, vec!["api.example.com"]);
        assert!(manifest.allowed_paths.is_empty());
    }

    #[test]
    fn test_parse_nested_filesystem_only() {
        let toml_str = r#"
[plugin]
name = "fs-only"
version = "0.1.0"
wasm_path = "fs.wasm"
capabilities = ["tool"]

[plugin.filesystem]
logs = "/var/log"
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert!(manifest.allowed_hosts.is_empty());
        assert_eq!(manifest.allowed_paths["logs"], "/var/log");
    }

    #[test]
    fn test_parse_tools_parameters_alias() {
        let toml_str = r#"
name = "alias-test"
version = "0.1.0"
wasm_path = "alias.wasm"
capabilities = ["tool"]

[[tools]]
name = "search"
description = "Search"
export = "tool_search"
risk_level = "low"
parameters = { type = "object", required = ["q"] }
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        let schema = manifest.tools[0].parameters_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "q");
    }

    // -----------------------------------------------------------------------
    // Comprehensive tests for US-ZCL-1-8
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_tools_all_risk_levels() {
        let toml_str = r#"
name = "risk-test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "read"
description = "Read data"
export = "tool_read"
risk_level = "low"

[[tools]]
name = "transform"
description = "Transform data"
export = "tool_transform"
risk_level = "medium"

[[tools]]
name = "delete"
description = "Delete data"
export = "tool_delete"
risk_level = "high"
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.tools.len(), 3);
        assert_eq!(manifest.tools[0].risk_level, RiskLevel::Low);
        assert_eq!(manifest.tools[1].risk_level, RiskLevel::Medium);
        assert_eq!(manifest.tools[2].risk_level, RiskLevel::High);
    }

    #[test]
    fn test_parse_empty_input() {
        let err = PluginManifest::parse("").unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "name"),
            PluginError::MalformedToml(_) => {} // also acceptable
            other => panic!("expected MissingField or MalformedToml, got: {other}"),
        }
    }

    #[test]
    fn test_parse_nested_plugin_not_a_table() {
        let toml_str = r#"
plugin = "not-a-table"
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MalformedToml(msg) => {
                assert!(
                    msg.contains("table"),
                    "error should mention 'table': {msg}"
                );
            }
            other => panic!("expected MalformedToml, got: {other}"),
        }
    }

    #[test]
    fn test_parse_nested_missing_wasm_path() {
        let toml_str = r#"
[plugin]
name = "no-wasm"
version = "0.1.0"
capabilities = ["tool"]
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "wasm_path"),
            PluginError::MalformedToml(_) => {}
            other => panic!("expected MissingField or MalformedToml, got: {other}"),
        }
    }

    #[test]
    fn test_parse_flat_missing_wasm_path() {
        let toml_str = r#"
name = "no-wasm"
version = "0.1.0"
capabilities = ["tool"]
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "wasm_path"),
            other => panic!("expected MissingField, got: {other}"),
        }
    }

    #[test]
    fn test_parse_flat_missing_capabilities() {
        let toml_str = r#"
name = "no-caps"
version = "0.1.0"
wasm_path = "plugin.wasm"
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        match &err {
            PluginError::MissingField { field } => assert_eq!(field, "capabilities"),
            other => panic!("expected MissingField, got: {other}"),
        }
    }

    #[test]
    fn test_parse_tool_definition_all_fields_via_parse() {
        let toml_str = r#"
name = "tool-test"
version = "1.0.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "query"
description = "Run a database query"
export = "tool_query"
risk_level = "medium"
parameters_schema = { type = "object", properties = { sql = { type = "string", description = "The SQL query" }, limit = { type = "integer", default = 100 } }, required = ["sql"] }
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.tools.len(), 1);
        let tool = &manifest.tools[0];
        assert_eq!(tool.name, "query");
        assert_eq!(tool.description, "Run a database query");
        assert_eq!(tool.export, "tool_query");
        assert_eq!(tool.risk_level, RiskLevel::Medium);

        let schema = tool.parameters_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["sql"]["type"], "string");
        assert_eq!(schema["properties"]["sql"]["description"], "The SQL query");
        assert_eq!(schema["properties"]["limit"]["type"], "integer");
        assert_eq!(schema["properties"]["limit"]["default"], 100);
        assert_eq!(schema["required"][0], "sql");
    }

    #[test]
    fn test_parse_nested_tools_with_parameters_section() {
        let toml_str = r#"
[plugin]
name = "nested-tools"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "create"
description = "Create a resource"
export = "tool_create"
risk_level = "medium"

[tools.parameters]
type = "object"
required = ["name", "type"]

[tools.parameters.properties.name]
type = "string"

[tools.parameters.properties.type]
type = "string"
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.tools.len(), 1);
        let schema = manifest.tools[0].parameters_schema.as_ref().unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"][0], "name");
        assert_eq!(schema["required"][1], "type");
        assert_eq!(schema["properties"]["name"]["type"], "string");
    }

    #[test]
    fn test_parse_flat_with_config() {
        let toml_str = r#"
name = "config-test"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[config]
api_url = "https://api.example.com"
debug = true
retries = 5
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.config["api_url"], "https://api.example.com");
        assert_eq!(manifest.config["debug"], true);
        assert_eq!(manifest.config["retries"], 5);
    }

    #[test]
    fn test_parse_valid_manifest_with_all_sections() {
        let toml_str = r#"
[plugin]
name = "complete-plugin"
version = "3.0.0"
description = "A plugin testing every section"
author = "Test Author"
wasm_path = "complete.wasm"
capabilities = ["tool", "channel", "memory", "observer"]
permissions = ["http_client", "file_read", "file_write", "env_read", "memory_read", "memory_write"]
wasi = true
timeout_ms = 45000
signature = "dGVzdC1zaWduYXR1cmU"
publisher_key = "deadbeef01234567"

[plugin.config]
endpoint = "https://example.com"
verbose = false

[plugin.network]
allowed_hosts = ["api.example.com", "cdn.example.com", "*.internal.io"]

[plugin.filesystem]
data = "/var/data"
logs = "/var/log/plugin"
tmp = "/tmp/plugin-work"

[[tools]]
name = "fetch"
description = "Fetch a remote resource"
export = "tool_fetch"
risk_level = "low"
parameters_schema = { type = "object", properties = { url = { type = "string" } }, required = ["url"] }

[[tools]]
name = "process"
description = "Process data"
export = "tool_process"
risk_level = "medium"

[[tools]]
name = "deploy"
description = "Deploy to production"
export = "tool_deploy"
risk_level = "high"
parameters_schema = { type = "object", properties = { target = { type = "string" }, force = { type = "boolean", default = false } }, required = ["target"] }
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        // Core fields
        assert_eq!(manifest.name, "complete-plugin");
        assert_eq!(manifest.version, "3.0.0");
        assert_eq!(manifest.description.as_deref(), Some("A plugin testing every section"));
        assert_eq!(manifest.author.as_deref(), Some("Test Author"));
        assert_eq!(manifest.wasm_path, "complete.wasm");
        assert!(manifest.wasi);
        assert_eq!(manifest.timeout_ms, 45_000);
        assert_eq!(manifest.signature.as_deref(), Some("dGVzdC1zaWduYXR1cmU"));
        assert_eq!(manifest.publisher_key.as_deref(), Some("deadbeef01234567"));

        // Capabilities & permissions
        assert_eq!(manifest.capabilities.len(), 4);
        assert_eq!(manifest.permissions.len(), 6);

        // Network
        assert_eq!(manifest.allowed_hosts.len(), 3);
        assert!(manifest.allowed_hosts.contains(&"*.internal.io".to_string()));

        // Filesystem
        assert_eq!(manifest.allowed_paths.len(), 3);
        assert_eq!(manifest.allowed_paths["logs"], "/var/log/plugin");

        // Config
        assert_eq!(manifest.config["endpoint"], "https://example.com");
        assert_eq!(manifest.config["verbose"], false);

        // Tools
        assert_eq!(manifest.tools.len(), 3);
        assert_eq!(manifest.tools[0].name, "fetch");
        assert_eq!(manifest.tools[0].risk_level, RiskLevel::Low);
        assert!(manifest.tools[0].parameters_schema.is_some());
        assert_eq!(manifest.tools[1].name, "process");
        assert_eq!(manifest.tools[1].risk_level, RiskLevel::Medium);
        assert!(manifest.tools[1].parameters_schema.is_none());
        assert_eq!(manifest.tools[2].name, "deploy");
        assert_eq!(manifest.tools[2].risk_level, RiskLevel::High);
        let deploy_schema = manifest.tools[2].parameters_schema.as_ref().unwrap();
        assert_eq!(deploy_schema["properties"]["force"]["default"], false);
    }

    #[test]
    fn test_parse_malformed_toml_unclosed_bracket() {
        let toml_str = r#"
[plugin
name = "broken"
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        assert!(matches!(err, PluginError::MalformedToml(_)));
    }

    #[test]
    fn test_parse_nested_empty_tools_list() {
        let toml_str = r#"
[plugin]
name = "no-tools-nested"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["channel"]
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();
        assert!(manifest.tools.is_empty());
    }

    #[test]
    fn test_parse_tool_without_parameters_schema() {
        let toml_str = r#"
name = "no-params"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "ping"
description = "Ping the service"
export = "tool_ping"
risk_level = "low"
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();

        assert_eq!(manifest.tools.len(), 1);
        assert!(manifest.tools[0].parameters_schema.is_none());
    }

    #[test]
    fn test_parse_defaults_wasi_true_timeout_30000() {
        let toml_str = r#"
name = "defaults"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = []
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();
        assert!(manifest.wasi);
        assert_eq!(manifest.timeout_ms, 30_000);
    }

    #[test]
    fn test_parse_nested_defaults_wasi_true_timeout_30000() {
        let toml_str = r#"
[plugin]
name = "defaults-nested"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = []
"#;
        let manifest = PluginManifest::parse(toml_str).unwrap();
        assert!(manifest.wasi);
        assert_eq!(manifest.timeout_ms, 30_000);
    }

    #[test]
    fn test_parse_invalid_risk_level_string() {
        let toml_str = r#"
name = "bad-risk"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]

[[tools]]
name = "tool"
description = "A tool"
export = "tool_fn"
risk_level = "critical"
"#;
        let err = PluginManifest::parse(toml_str).unwrap_err();
        assert!(
            matches!(err, PluginError::MalformedToml(_)),
            "invalid risk_level should produce MalformedToml, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // resolve_plugin_config tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_config_all_provided() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "api_key".to_string(),
            serde_json::json!({"required": true}),
        );
        manifest_config.insert(
            "model".to_string(),
            serde_json::json!("gpt-4"),
        );

        let mut values = HashMap::new();
        values.insert("api_key".to_string(), "sk-test".to_string());
        values.insert("model".to_string(), "gpt-3.5".to_string());

        let result = resolve_plugin_config("test-plugin", &manifest_config, Some(&values)).unwrap();
        assert_eq!(result["api_key"], "sk-test");
        assert_eq!(result["model"], "gpt-3.5");
    }

    #[test]
    fn test_resolve_config_uses_defaults() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "model".to_string(),
            serde_json::json!("gpt-4"),
        );
        manifest_config.insert(
            "temperature".to_string(),
            serde_json::json!({"default": "0.7"}),
        );

        let result = resolve_plugin_config("test-plugin", &manifest_config, None).unwrap();
        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["temperature"], "0.7");
    }

    #[test]
    fn test_resolve_config_missing_required_keys() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "api_key".to_string(),
            serde_json::json!({"required": true}),
        );
        manifest_config.insert(
            "secret".to_string(),
            serde_json::json!({"required": true}),
        );

        let err = resolve_plugin_config("my-plugin", &manifest_config, None).unwrap_err();
        match err {
            PluginError::MissingConfig { plugin, keys } => {
                assert_eq!(plugin, "my-plugin");
                assert!(keys.contains("api_key"));
                assert!(keys.contains("secret"));
            }
            other => panic!("expected MissingConfig, got: {other}"),
        }
    }

    #[test]
    fn test_resolve_config_passthrough_extra_keys() {
        let manifest_config = HashMap::new();

        let mut values = HashMap::new();
        values.insert("custom_key".to_string(), "custom_value".to_string());

        let result = resolve_plugin_config("test-plugin", &manifest_config, Some(&values)).unwrap();
        assert_eq!(result["custom_key"], "custom_value");
    }

    #[test]
    fn test_resolve_config_numeric_default() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert("max_retries".to_string(), serde_json::json!(3));

        let result = resolve_plugin_config("test-plugin", &manifest_config, None).unwrap();
        assert_eq!(result["max_retries"], "3");
    }

    #[test]
    fn test_resolve_config_empty_manifest_empty_values() {
        let result = resolve_plugin_config("empty", &HashMap::new(), None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_config_no_plugin_section_all_optional_succeeds() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert("model".to_string(), serde_json::json!("gpt-4"));
        manifest_config.insert(
            "timeout".to_string(),
            serde_json::json!({"default": "30"}),
        );
        manifest_config.insert(
            "optional_flag".to_string(),
            serde_json::json!({"sensitive": false}),
        );

        // None simulates no [plugins.<name>] section in config.toml
        let result = resolve_plugin_config("optional-plugin", &manifest_config, None).unwrap();
        assert_eq!(result["model"], "gpt-4");
        assert_eq!(result["timeout"], "30");
        // optional_flag has no default and is not required — omitted from result
        assert!(!result.contains_key("optional_flag"));
    }

    #[test]
    fn test_resolve_config_missing_required_error_contains_key_name() {
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "db_url".to_string(),
            serde_json::json!({"required": true}),
        );

        let err = resolve_plugin_config("db-plugin", &manifest_config, None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("db_url"), "error should name the missing key: {msg}");
        assert!(msg.contains("db-plugin"), "error should name the plugin: {msg}");
    }

    #[test]
    fn test_is_sensitive_key_true() {
        let decl = serde_json::json!({"required": true, "sensitive": true});
        assert!(is_sensitive_key(&decl));
    }

    #[test]
    fn test_is_sensitive_key_false_when_absent() {
        let decl = serde_json::json!({"required": true});
        assert!(!is_sensitive_key(&decl));
    }

    #[test]
    fn test_is_sensitive_key_false_for_bare_string() {
        let decl = serde_json::json!("default-value");
        assert!(!is_sensitive_key(&decl));
    }

    #[test]
    fn test_decrypt_plugin_config_values_decrypts_encrypted() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::security::SecretStore::new(temp.path(), false);

        let plaintext = "super-secret-key";
        let encrypted = store.encrypt(plaintext).unwrap();

        let mut values = HashMap::new();
        values.insert("api_key".to_string(), encrypted);
        values.insert("endpoint".to_string(), "https://api.test.com".to_string());

        decrypt_plugin_config_values(&mut values, &store).unwrap();

        assert_eq!(values["api_key"], plaintext);
        assert_eq!(values["endpoint"], "https://api.test.com");
    }

    #[test]
    fn test_decrypt_plugin_config_values_invalid_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::security::SecretStore::new(temp.path(), false);

        let mut values = HashMap::new();
        values.insert("broken".to_string(), "enc2:not-valid-hex".to_string());

        let err = decrypt_plugin_config_values(&mut values, &store).unwrap_err();
        match err {
            PluginError::ConfigDecrypt { key, .. } => {
                assert_eq!(key, "broken");
            }
            other => panic!("expected ConfigDecrypt, got: {other}"),
        }
    }

    #[test]
    fn test_resolve_config_sensitive_value_not_in_resolved_log_format() {
        // Sensitive values should still be present in the resolved config
        // (they're only redacted in log output, not in the actual map).
        let mut manifest_config = HashMap::new();
        manifest_config.insert(
            "token".to_string(),
            serde_json::json!({"required": true, "sensitive": true}),
        );

        let mut values = HashMap::new();
        values.insert("token".to_string(), "secret-token-value-12345".to_string());

        let result = resolve_plugin_config("sensitive-plugin", &manifest_config, Some(&values)).unwrap();
        // The actual value is passed through — redaction is only for logging
        assert_eq!(result["token"], "secret-token-value-12345");
    }
}
