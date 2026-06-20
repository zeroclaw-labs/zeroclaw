//! WASM plugin system for ZeroClaw.
//!
//! Plugins are WebAssembly modules loaded via Extism that can extend
//! ZeroClaw with custom tools and channels. Enable with `--features plugins-wasm`.

pub mod error;
pub mod host;
pub mod runtime;
pub mod signature;
pub mod wasm_channel;
pub mod wasm_tool;

#[cfg(feature = "plugins-wasmtime")]
pub mod component;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::PluginError;

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
    /// Fine-grained sandbox permissions for component-model (WASIP2) plugins.
    ///
    /// Enables per-directory filesystem access, and per-host TCP/UDP/HTTP
    /// connectivity. Defaults to no access (fully sandboxed).
    #[serde(default)]
    pub fine_grained_permissions: Vec<FineGrainedPermission>,
    /// Ed25519 signature over the canonical manifest (base64url-encoded).
    /// Set by the plugin publisher when signing the manifest.
    #[serde(default)]
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 public key of the publisher who signed this manifest.
    #[serde(default)]
    pub publisher_key: Option<String>,
    /// Secret keys this plugin expects to read via `plugin-config.get-secret`
    /// (e.g. `["bot_token"]`). `get-secret` only ever resolves a key listed
    /// here — an undeclared key returns `none` even if the operator's config
    /// happens to hold a value under that name for an unrelated purpose.
    #[serde(default)]
    pub declared_secrets: Vec<String>,
}

/// Per-instance network and secrets configuration resolved by the host at
/// plugin-instantiation time, shared by tool, memory, and channel plugins.
///
/// `secrets` is deliberately a small, per-instance map — never a handle into
/// the global secret store. It is built once by the orchestrator from
/// already-decrypted config scoped to *this* plugin instance (e.g. one
/// channel alias's `bot_token`), filtered to the keys the plugin declared via
/// [`PluginManifest::declared_secrets`], and is never shared across plugin
/// instances.
#[derive(Debug, Clone, Default)]
pub struct PluginNetworkConfig {
    /// Selector matched against `ProxyConfig::services` when the global
    /// proxy scope is `services` (mirrors the `"channel.discord"`-style key
    /// native channels already pass to `build_channel_proxy_client`).
    /// Defaults to `format!("plugin.{name}")` at instantiation time.
    pub service_key: String,
    /// Per-plugin proxy override, equivalent to a channel's `proxy_url`.
    pub proxy_url: Option<String>,
    /// Already-decrypted secrets scoped to this plugin instance, filtered to
    /// `PluginManifest::declared_secrets`.
    pub secrets: std::collections::HashMap<String, String>,
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
    /// Can read its own resolved per-plugin config section
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

// ── AddressString ─────────────────────────────────────────────────────────────

/// A validated network address for use in plugin permissions.
///
/// Accepted forms:
/// - IPv4 literal: `"192.0.2.1"`
/// - IPv6 literal: `"2001:db8::1"`
/// - Second-level domain: `"example.com"`
/// - Subdomain: `"api.example.com"`
/// - Wildcard subdomain (level 3 and above): `"*.example.com"`,
///   `"id-*.docs.example.com"` — the TLD and SLD must not contain `*`.
///
/// When used in TCP/UDP/HTTP permissions the runtime enforces the address at
/// connect time: IP literals and resolved-domain IPs are matched exactly;
/// wildcard patterns are matched using string matching for HTTP, but using
/// reverse-DNS lookup for TCP/UDP. Operators should treat *.example.com TCP/UDP
/// grants as granting access to any host whose reverse-DNS record matches, not
/// purely the forward-DNS zone"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct AddressString(String);

impl AddressString {
    /// Parse and validate an address string.
    pub fn new(s: impl Into<String>) -> Result<Self, PluginError> {
        let s = s.into();
        Self::validate(&s)?;
        Ok(Self(s))
    }

    /// Returns the raw address string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns `true` if this address contains a wildcard label (`*`).
    pub fn is_wildcard(&self) -> bool {
        self.0.contains('*')
    }

    fn validate(s: &str) -> Result<(), PluginError> {
        // IPv4
        if s.parse::<std::net::Ipv4Addr>().is_ok() {
            return Ok(());
        }
        // IPv6
        if s.parse::<std::net::Ipv6Addr>().is_ok() {
            return Ok(());
        }
        // Domain name
        validate_domain(s)
    }
}

impl<'de> serde::Deserialize<'de> for AddressString {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for AddressString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn validate_domain(s: &str) -> Result<(), PluginError> {
    let labels: Vec<&str> = s.split('.').collect();
    // Allow 'localhost' as a special case for single-label domains
    if labels.len() == 1 {
        if labels[0].eq_ignore_ascii_case("localhost") {
            validate_dns_label(labels[0], s)?;
            return Ok(());
        }
        return Err(PluginError::AddressStringTooShort(s.to_string()));
    }
    if labels.len() < 2 {
        return Err(PluginError::AddressStringTooShort(s.to_string()));
    }
    let n = labels.len();
    for (i, &label) in labels.iter().enumerate() {
        // Level from right: n-i (so rightmost is level 1 = TLD).
        let level_from_right = n - i;
        if level_from_right <= 2 {
            // TLD and SLD must be plain DNS labels (no wildcards).
            validate_dns_label(label, s)?;
        } else {
            // Level 3 and above may contain '*'.
            validate_wildcard_label(label, s)?;
        }
    }
    Ok(())
}

fn validate_dns_label(label: &str, ctx: &str) -> Result<(), PluginError> {
    if label.is_empty() || label.len() > 63 {
        return Err(PluginError::AddressStringInvalidLabel(label.to_string()));
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(PluginError::AddressStringInvalidLabel(label.to_string()));
    }
    if label.contains('*') {
        return Err(PluginError::AddressStringWildcardNotAllowed(
            ctx.to_string(),
        ));
    }
    if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(PluginError::AddressStringInvalidLabel(label.to_string()));
    }
    Ok(())
}

fn validate_wildcard_label(label: &str, _ctx: &str) -> Result<(), PluginError> {
    if label.is_empty() {
        return Err(PluginError::AddressStringInvalidLabel(label.to_string()));
    }
    // Remove all '*' characters; the remainder must be alphanumeric-or-hyphen.
    let rest: String = label.chars().filter(|&c| c != '*').collect();
    if !rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(PluginError::AddressStringInvalidLabel(label.to_string()));
    }
    Ok(())
}

// ── PreopenedDir ──────────────────────────────────────────────────────────────

/// A directory on the host to expose to the WASM plugin.
///
/// Fields mirror the parameters of `WasiCtxBuilder::preopened_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreopenedDir {
    /// Path to the directory on the host.
    pub host_path: PathBuf,
    /// Path the plugin sees (e.g. `"."` or `"/data"`).
    pub guest_path: String,
    /// Allow the plugin to list and read directory entries (default: `true`).
    #[serde(default = "default_true")]
    pub dir_read: bool,
    /// Allow the plugin to create, rename, and delete directory entries (default: `false`).
    #[serde(default)]
    pub dir_write: bool,
    /// Allow the plugin to read file contents (default: `true`).
    #[serde(default = "default_true")]
    pub file_read: bool,
    /// Allow the plugin to write file contents (default: `false`).
    #[serde(default)]
    pub file_write: bool,
}

fn default_true() -> bool {
    true
}

// ── FineGrainedPermission ─────────────────────────────────────────────────────

/// Fine-grained sandbox permissions for WASM component-model plugins.
///
/// These are applied when building the per-store WASI context and provide
/// narrower control than the coarse-grained [`PluginPermission`] flags.
///
/// - `Dir` — exposes a specific host directory to the plugin's filesystem.
/// - `Http` — allows outbound HTTP connections to the given address.
/// - `Tcp` — allows outbound TCP connections to the given address.
/// - `Udp` — allows outbound UDP to the given address.
///
/// TCP (`TcpBind`) listening is never allowed regardless of permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FineGrainedPermission {
    Dir(PreopenedDir),
    Http(AddressString),
    Tcp(AddressString),
    Udp(AddressString),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── IP Address Tests ───────────────────────────────────────────────────

    #[test]
    fn test_valid_ipv4() {
        assert!(AddressString::new("127.0.0.1").is_ok());
        assert!(AddressString::new("192.0.2.1").is_ok());
    }

    #[test]
    fn test_valid_ipv6() {
        assert!(AddressString::new("::1").is_ok());
        assert!(AddressString::new("2001:db8::1").is_ok());
    }

    // ── Valid Domain Names ─────────────────────────────────────────────────

    #[test]
    fn test_valid_domains() {
        assert!(AddressString::new("localhost").is_ok());
        assert!(AddressString::new("example.com").is_ok());
        assert!(AddressString::new("api.example.com").is_ok());
        assert!(AddressString::new("my-domain.com").is_ok());
        assert!(AddressString::new("example123.com").is_ok());
    }

    #[test]
    fn test_valid_wildcards() {
        assert!(AddressString::new("*.example.com").is_ok());
        assert!(AddressString::new("id-*.docs.example.com").is_ok());
    }

    // ── Invalid: Too Short ─────────────────────────────────────────────────

    #[test]
    fn test_invalid_single_label() {
        let err = AddressString::new("com").unwrap_err();
        assert!(matches!(err, PluginError::AddressStringTooShort(_)));
    }

    // ── Invalid: Empty or Malformed Labels ────────────────────────────────

    #[test]
    fn test_invalid_empty_labels() {
        assert!(AddressString::new(".example.com").is_err());
        assert!(AddressString::new("example..com").is_err());
        assert!(AddressString::new("").is_err());
    }

    #[test]
    fn test_invalid_label_length() {
        let long_label = "a".repeat(64);
        assert!(AddressString::new(format!("{}.com", long_label)).is_err());

        let max_label = "a".repeat(63);
        assert!(AddressString::new(format!("{}.com", max_label)).is_ok());
    }

    #[test]
    fn test_invalid_hyphen_position() {
        assert!(AddressString::new("-example.com").is_err());
        assert!(AddressString::new("example-.com").is_err());
    }

    // ── Invalid: Wildcard Restrictions ────────────────────────────────────

    #[test]
    fn test_invalid_wildcard_in_tld_or_sld() {
        assert!(AddressString::new("example.*").is_err());
        assert!(AddressString::new("*.com").is_err());
    }

    // ── Invalid: Invalid Characters ───────────────────────────────────────

    #[test]
    fn test_invalid_characters() {
        assert!(AddressString::new("ex_ample.com").is_err());
        assert!(AddressString::new("ex ample.com").is_err());
        assert!(AddressString::new("ex@ample.com").is_err());
    }

    // ── AddressString Methods ──────────────────────────────────────────────

    #[test]
    fn test_address_string_methods() {
        let addr = AddressString::new("example.com").unwrap();
        assert_eq!(addr.as_str(), "example.com");
        assert!(!addr.is_wildcard());

        let wildcard = AddressString::new("*.example.com").unwrap();
        assert!(wildcard.is_wildcard());
    }

    // ── Serialization ──────────────────────────────────────────────────────

    #[test]
    fn test_serde() {
        let addr = AddressString::new("example.com").unwrap();
        let json = serde_json::to_string(&addr).unwrap();
        assert_eq!(json, "\"example.com\"");

        let deserialized: AddressString = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, addr);

        let invalid = serde_json::from_str::<AddressString>("\"com\"");
        assert!(invalid.is_err());
    }
}
