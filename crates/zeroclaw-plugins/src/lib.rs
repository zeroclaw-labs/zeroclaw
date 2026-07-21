//! WASM plugin system for ZeroClaw.
//!
//! Plugins are WebAssembly components loaded via wasmtime that can extend
//! ZeroClaw with custom tools and channels. Enable with a `plugins-wasm*` feature.

#[cfg(feature = "plugins-wasmtime")]
pub mod component;
#[cfg(feature = "plugins-wasmtime")]
mod component_logging;
pub mod error;
pub mod host;
pub mod registry;
#[cfg(feature = "plugins-wasmtime")]
pub mod runtime;
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
    /// SHA-256 of the exact bytes at `wasm_path`, encoded as 64 hexadecimal
    /// characters. Strict signature mode requires this for executable plugins;
    /// the field is covered by the manifest signature.
    #[serde(default)]
    pub wasm_sha256: Option<String>,
    /// Capabilities this plugin provides
    pub capabilities: Vec<PluginCapability>,
    /// The compiled-in channel id this plugin *mirrors*, when it is a drop-in
    /// for a built-in channel — the snake_case config id (e.g. `"telegram"`,
    /// `"gmail_push"`). When set, the host builds one instance per configured
    /// `[channels.<id>.<alias>]` and feeds each that alias's canonical config,
    /// instead of a `[[plugins.entries]]` block. `None` (the default) means a
    /// novel plugin with no built-in equivalent, configured from its own
    /// `[[plugins.entries.<name>]]` map.
    #[serde(default)]
    pub provides: Option<String>,
    /// How this channel guest normalizes the sender identity string it emits.
    ///
    /// The host uses this manifest-owned contract when matching live
    /// `Config::peer_groups` membership. Omitted by older manifests, it
    /// defaults to case-sensitive exact matching.
    #[serde(default)]
    pub sender_match: SenderMatch,
    /// Permissions this plugin requests
    #[serde(default)]
    pub permissions: Vec<PluginPermission>,
    /// Ed25519 signature over the canonical manifest (base64url-encoded).
    /// Set by the plugin publisher when signing the manifest.
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
}

/// Sender-identity representation emitted by a channel plugin.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SenderMatch {
    /// Case-sensitive exact identity.
    #[default]
    Exact,
    /// ASCII case-insensitive exact identity.
    CaseInsensitive,
    /// User handle, compared after trimming and removing a leading `@`.
    Handle,
    /// Full email address or domain-class identity.
    Email,
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

/// Permissions a plugin may request.
#[derive(Debug, Clone, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginPermission {
    /// Can make HTTP requests
    HttpClient,
    /// Can read from the filesystem (within sandbox)
    FileRead,
    /// Can write to the filesystem (within sandbox)
    FileWrite,
    /// Can read host-selected configuration: its own resolved plugin section,
    /// or a mirrored channel's canonical alias section when `provides` is set.
    #[serde(alias = "env_read")]
    ConfigRead,
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
    /// Resolved path to the WASM file. `None` for skill-only plugins.
    pub wasm_path: Option<PathBuf>,
    pub loaded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL_MANIFEST: &str = r#"
name = "fixture-channel"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["channel"]
"#;

    #[test]
    fn sender_match_defaults_to_exact_for_existing_manifests() {
        let manifest: PluginManifest = toml::from_str(CHANNEL_MANIFEST).expect("valid manifest");
        assert_eq!(manifest.sender_match, SenderMatch::Exact);
    }

    #[test]
    fn sender_match_accepts_every_documented_value() {
        for (value, expected) in [
            ("exact", SenderMatch::Exact),
            ("case_insensitive", SenderMatch::CaseInsensitive),
            ("handle", SenderMatch::Handle),
            ("email", SenderMatch::Email),
        ] {
            let manifest_toml = format!("{CHANNEL_MANIFEST}\nsender_match = \"{value}\"\n");
            let manifest: PluginManifest =
                toml::from_str(&manifest_toml).expect("documented sender_match value");
            assert_eq!(manifest.sender_match, expected, "{value}");
        }
    }

    #[test]
    fn sender_match_rejects_unknown_values() {
        let manifest_toml = format!("{CHANNEL_MANIFEST}\nsender_match = \"username\"\n");
        assert!(toml::from_str::<PluginManifest>(&manifest_toml).is_err());
    }
}
