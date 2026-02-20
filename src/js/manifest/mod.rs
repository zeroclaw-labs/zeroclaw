// Plugin manifest parsing (plugin.toml + SKILL.md)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Parsed plugin.toml manifest v2
///
/// Extended with hook registrations and granular permissions for the
/// hooks/events/APIs system.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct PluginManifest {
    #[serde(default)]
    pub plugin: PluginMetadata,

    #[serde(default)]
    pub runtime: RuntimeManifestConfig,

    /// Hook registrations declared by this plugin
    #[serde(default, rename = "hooks")]
    pub registered_hooks: Vec<HookRegistration>,

    #[serde(default)]
    pub permissions: Permissions,

    #[serde(default)]
    pub tools: ToolDefinitions,

    #[serde(default)]
    pub skills: SkillDefinitions,
}

/// Plugin metadata section
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct PluginMetadata {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub openclaw_compatible: bool,
    #[serde(default)]
    pub openclaw_skill_id: Option<String>,
}

/// Runtime configuration section
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct RuntimeManifestConfig {
    /// Entry point file (e.g., "src/index.ts")
    #[serde(default)]
    pub entry: PathBuf,

    /// Optional SDK version requirement
    #[serde(default)]
    pub sdk_version: Option<String>,
}

/// Hook registration declaration
///
/// Represents a single hook that the plugin wants to register.
/// The name must match an event type (e.g., "message.received").
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct HookRegistration {
    /// Event name to hook (e.g., "message.received", "tool.call.pre")
    #[serde(default)]
    pub name: String,

    /// Execution priority (higher = runs earlier, default: 50)
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// Optional timeout in milliseconds (default: 5000)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

fn default_priority() -> i32 {
    50
}

/// Plugin permissions section v2
///
/// Extended with hooks, APIs, and channels permissions for the
/// hooks/events/APIs system.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct Permissions {
    /// Allowed hook events (supports wildcards like "message.*")
    #[serde(default)]
    pub hooks: Vec<String>,

    /// Allowed API modules (e.g., "channels", "session", "memory")
    #[serde(default)]
    pub apis: Vec<String>,

    /// Allowed channel types (e.g., "discord", "telegram")
    #[serde(default)]
    pub channels: Vec<String>,

    /// Allowed network hosts
    #[serde(default)]
    pub network: Vec<String>,

    /// Allowed file read paths (glob patterns)
    #[serde(default)]
    pub file_read: Vec<String>,

    /// Allowed file write paths (glob patterns, changed from bool for v2)
    #[serde(default)]
    pub file_write: Vec<String>,

    /// Allowed environment variables
    #[serde(default)]
    pub env_vars: Vec<String>,
}

/// Legacy alias for backward compatibility
pub type PluginPermissions = Permissions;

/// Tool definitions section
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ToolDefinitions {
    /// Tool definitions
    #[serde(default)]
    pub definitions: Vec<ToolDefinition>,
}

/// Individual tool definition
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Skill definitions section
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct SkillDefinitions {
    /// Skill definitions
    #[serde(default)]
    pub definitions: Vec<SkillDefinition>,
}

/// Individual skill definition
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    /// Intent patterns for matching
    #[serde(default)]
    pub patterns: Option<Vec<String>>,
    /// Example queries that trigger this skill
    #[serde(default)]
    pub examples: Option<Vec<String>>,
}

impl PluginManifest {
    /// Parse plugin.toml from a file path
    pub fn from_file(path: &PathBuf) -> Result<Self, ParseError> {
        let content = std::fs::read_to_string(path).map_err(|e| ParseError::Io {
            path: path.clone(),
            error: e.to_string(),
        })?;

        let manifest: PluginManifest = toml::from_str(&content).map_err(|e| ParseError::Toml {
            path: path.clone(),
            error: e.to_string(),
        })?;

        Ok(manifest)
    }

    /// Parse plugin.toml from a string
    pub fn from_str(content: &str) -> Result<Self, ParseError> {
        toml::from_str(content).map_err(|e| ParseError::Toml {
            path: "<string>".into(),
            error: e.to_string(),
        })
    }

    /// Check if a hook is allowed
    ///
    /// Supports exact match, wildcard ("*"), and prefix patterns ("message.*")
    pub fn allows_hook(&self, hook_name: &str) -> bool {
        self.is_allowed(&self.permissions.hooks, hook_name)
    }

    /// Check if an API module is allowed
    ///
    /// Supports exact match, wildcard ("*"), and prefix patterns
    pub fn allows_api(&self, api: &str) -> bool {
        self.is_allowed(&self.permissions.apis, api)
    }

    /// Check if a channel type is allowed
    ///
    /// Empty channels list or "*" means all channels are allowed
    pub fn allows_channel(&self, channel_type: &str) -> bool {
        self.permissions.channels.is_empty()
            || self.permissions.channels.contains(&"*".to_string())
            || self
                .permissions
                .channels
                .contains(&channel_type.to_string())
    }

    /// Check if a target matches any pattern in the allowed list
    ///
    /// Supports:
    /// - Exact match: "message.received" matches "message.received"
    /// - Wildcard: "*" matches anything
    /// - Prefix wildcard: "message.*" matches "message.received", "message.sent", etc.
    /// - Suffix wildcard: "message.*" is treated the same as prefix wildcard
    fn is_allowed(&self, allowed: &[String], target: &str) -> bool {
        // Empty list means deny by default (secure by default)
        if allowed.is_empty() {
            return false;
        }

        let target_str = target.to_string();

        // Check for exact match or wildcard
        for pattern in allowed {
            if pattern == "*" || pattern == &target_str {
                return true;
            }

            // Handle prefix wildcards: "message.*" matches "message.received"
            if pattern.ends_with(".*") {
                let prefix = &pattern[..pattern.len() - 2];
                if target.starts_with(prefix) {
                    return true;
                }
            }

            // Handle suffix wildcards: "message*" matches "message123"
            if pattern.ends_with('*') && !pattern.ends_with(".*") {
                let prefix = &pattern[..pattern.len() - 1];
                if target.starts_with(prefix) {
                    return true;
                }
            }
        }

        false
    }

    /// Validate the manifest
    ///
    /// Checks:
    /// - Required fields are present
    /// - Plugin name is valid
    /// - Declared hooks are in permissions.hooks
    pub fn validate(&self) -> Result<(), ParseError> {
        // Basic validation
        if self.plugin.name.is_empty() {
            return Err(ParseError::Validation("Plugin name cannot be empty".into()));
        }
        if self.plugin.name.contains(' ') || self.plugin.name.contains('/') {
            return Err(ParseError::Validation(
                "Plugin name must not contain spaces or slashes".into(),
            ));
        }
        if self.runtime.entry.as_os_str().is_empty() {
            return Err(ParseError::Validation(
                "Runtime entry point cannot be empty".into(),
            ));
        }

        // Validate declared hooks are in allowed list
        for hook in &self.registered_hooks {
            if !self.allows_hook(&hook.name) {
                return Err(ParseError::Validation(format!(
                    "Hook '{}' is not in permissions.hooks (secure-by-default)",
                    hook.name
                )));
            }
        }

        Ok(())
    }
}

/// Manifest parsing errors
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("I/O error reading {path}: {error}")]
    Io { path: PathBuf, error: String },

    #[error("TOML parsing error in {path}: {error}")]
    Toml { path: PathBuf, error: String },

    #[error("Validation error: {0}")]
    Validation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
[plugin]
name = "test-plugin"
version = "1.0.0"
description = "A test plugin"
author = "Test Author"
license = "MIT"

[runtime]
entry = "src/index.ts"
sdk_version = "^3.0.0"

[permissions]
network = ["api.example.com"]
file_read = ["./data/**"]
file_write = ["./output/**"]
env_vars = ["API_KEY"]
hooks = ["message.received"]
apis = ["channels"]
channels = ["discord"]

[[tools.definitions]]
name = "test_tool"
description = "A test tool"

[tools.definitions.parameters]
type = "object"
properties.query = { type = "string" }
required = ["query"]
"#;

    #[test]
    fn parse_valid_manifest() {
        let manifest =
            PluginManifest::from_str(VALID_MANIFEST).expect("Should parse valid manifest");

        assert_eq!(manifest.plugin.name, "test-plugin");
        assert_eq!(manifest.plugin.version, "1.0.0");
        assert_eq!(manifest.runtime.entry, PathBuf::from("src/index.ts"));
        assert_eq!(manifest.permissions.network.len(), 1);
        assert_eq!(manifest.tools.definitions.len(), 1);
    }

    #[test]
    fn validate_accepts_valid_manifest() {
        let manifest =
            PluginManifest::from_str(VALID_MANIFEST).expect("Should parse valid manifest");
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_name() {
        let invalid = r#"
[plugin]
name = ""
version = "1.0.0"
description = "A test plugin"
author = "Test Author"

[runtime]
entry = "src/index.ts"
"#;
        let manifest = PluginManifest::from_str(invalid).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn validate_rejects_name_with_spaces() {
        let invalid = r#"
[plugin]
name = "test plugin"
version = "1.0.0"
description = "A test plugin"
author = "Test Author"

[runtime]
entry = "src/index.ts"
"#;
        let manifest = PluginManifest::from_str(invalid).unwrap();
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn parse_default_permissions() {
        let minimal = r#"
[plugin]
name = "test"
version = "1.0.0"
description = "Test"
author = "Test"

[runtime]
entry = "index.ts"
"#;

        let manifest = PluginManifest::from_str(minimal).unwrap();
        assert!(manifest.permissions.network.is_empty());
        assert!(manifest.permissions.file_write.is_empty());
        assert!(manifest.tools.definitions.is_empty());
        assert!(manifest.permissions.hooks.is_empty());
        assert!(manifest.permissions.apis.is_empty());
        assert!(manifest.permissions.channels.is_empty());
    }

    #[test]
    fn parse_invalid_toml() {
        let invalid = r#"
[plugin
name = "test
"#;

        let result = PluginManifest::from_str(invalid);
        assert!(result.is_err());
    }

    #[test]
    fn tool_definition_serialization() {
        let manifest = PluginManifest::from_str(VALID_MANIFEST).unwrap();
        let tool = &manifest.tools.definitions[0];

        assert_eq!(tool.name, "test_tool");
        assert_eq!(tool.description, "A test tool");

        // Verify parameters are valid JSON
        let params = &tool.parameters;
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["query"]["type"] == "string");
    }

    // === New v2 permission tests ===

    #[test]
    fn allows_hook_exact_match() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                hooks: vec!["message.received".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_hook("message.received"));
        assert!(!manifest.allows_hook("tool.call.pre"));
    }

    #[test]
    fn allows_hook_wildcard() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                hooks: vec!["message.*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_hook("message.received"));
        assert!(manifest.allows_hook("message.sent"));
        assert!(!manifest.allows_hook("tool.call.pre"));
    }

    #[test]
    fn allows_hook_global_wildcard() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                hooks: vec!["*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_hook("message.received"));
        assert!(manifest.allows_hook("tool.call.pre"));
        assert!(manifest.allows_hook("any.event"));
    }

    #[test]
    fn allows_hook_empty_list_denies_all() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                hooks: vec![],
                ..Default::default()
            },
            ..Default::default()
        };

        // Empty list means deny by default (secure by default)
        assert!(!manifest.allows_hook("message.received"));
        assert!(!manifest.allows_hook("any.event"));
    }

    #[test]
    fn allows_api_exact_match() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                apis: vec!["channels".to_string(), "session".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_api("channels"));
        assert!(manifest.allows_api("session"));
        assert!(!manifest.allows_api("memory"));
    }

    #[test]
    fn allows_api_wildcard() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                apis: vec!["*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_api("channels"));
        assert!(manifest.allows_api("session"));
        assert!(manifest.allows_api("memory"));
    }

    #[test]
    fn allows_channel_empty_list_allows_all() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                channels: vec![],
                ..Default::default()
            },
            ..Default::default()
        };

        // Empty channels list allows all (channels are optional)
        assert!(manifest.allows_channel("discord"));
        assert!(manifest.allows_channel("telegram"));
    }

    #[test]
    fn allows_channel_specific() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                channels: vec!["discord".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_channel("discord"));
        assert!(!manifest.allows_channel("telegram"));
    }

    #[test]
    fn allows_channel_global_wildcard() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            permissions: Permissions {
                channels: vec!["*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.allows_channel("discord"));
        assert!(manifest.allows_channel("telegram"));
    }

    #[test]
    fn validate_hook_not_allowed() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            registered_hooks: vec![HookRegistration {
                name: "message.received".to_string(),
                priority: 50,
                timeout_ms: None,
            }],
            permissions: Permissions {
                hooks: vec!["tool.call.*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        let result = manifest.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not in permissions.hooks"));
    }

    #[test]
    fn validate_hook_allowed_by_wildcard() {
        let manifest = PluginManifest {
            plugin: PluginMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test".to_string(),
                author: "Test".to_string(),
                ..Default::default()
            },
            runtime: RuntimeManifestConfig {
                entry: PathBuf::from("index.ts"),
                sdk_version: None,
            },
            registered_hooks: vec![HookRegistration {
                name: "message.received".to_string(),
                priority: 50,
                timeout_ms: None,
            }],
            permissions: Permissions {
                hooks: vec!["*".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn hook_registration_default_priority() {
        let hook = HookRegistration {
            name: "test".to_string(),
            priority: default_priority(),
            ..Default::default()
        };

        assert_eq!(hook.priority, 50);
        assert!(hook.timeout_ms.is_none());
    }

    #[test]
    fn parse_manifest_with_hooks() {
        let manifest_str = r#"
[plugin]
name = "test"
version = "1.0.0"
description = "Test"
author = "Test"

[runtime]
entry = "index.ts"

[[hooks]]
name = "message.received"
priority = 100

[[hooks]]
name = "tool.call.pre"
priority = 50
timeout_ms = 10000

[permissions]
hooks = ["message.*", "tool.call.*"]
"#;

        let manifest = PluginManifest::from_str(manifest_str).unwrap();
        assert_eq!(manifest.registered_hooks.len(), 2);
        assert_eq!(manifest.registered_hooks[0].name, "message.received");
        assert_eq!(manifest.registered_hooks[0].priority, 100);
        assert_eq!(manifest.registered_hooks[1].name, "tool.call.pre");
        assert_eq!(manifest.registered_hooks[1].priority, 50);
        assert_eq!(manifest.registered_hooks[1].timeout_ms, Some(10000));
    }
}
