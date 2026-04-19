//! Plugin capability types, tool definitions, and CLI configuration.

use serde::{Deserialize, Serialize};

/// A tool declared in a plugin manifest via `[[tools]]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub export: String,
    #[serde(default)]
    pub risk_level: String,
    #[serde(default, alias = "parameters")]
    pub parameters_schema: Option<serde_json::Value>,
}

/// Host-side capabilities a plugin may request via `[plugin.host_capabilities]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginCapabilities {
    #[serde(default)]
    pub memory: Option<MemoryCapability>,
    #[serde(default)]
    pub tool_delegation: Option<ToolDelegationCapability>,
    #[serde(default)]
    pub messaging: Option<MessagingCapability>,
    #[serde(default)]
    pub context: Option<ContextCapability>,
    #[serde(default)]
    pub cli: Option<CliCapability>,
}

/// Memory subsystem access.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryCapability {
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
}

/// Tool delegation capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDelegationCapability {
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// Messaging capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessagingCapability {
    #[serde(default)]
    pub allowed_channels: Vec<String>,
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

/// Context access capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextCapability {
    #[serde(default)]
    pub session: bool,
    #[serde(default)]
    pub user_identity: bool,
    #[serde(default)]
    pub agent_config: bool,
}

/// Pattern for validating CLI arguments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArgPattern {
    pub command: String,
    #[serde(default)]
    pub patterns: Vec<String>,
}

impl ArgPattern {
    pub fn new(command: impl Into<String>, patterns: Vec<String>) -> Self {
        Self {
            command: command.into(),
            patterns,
        }
    }

    pub fn compile(&self) -> Result<Vec<glob::Pattern>, glob::PatternError> {
        self.patterns
            .iter()
            .map(|p| glob::Pattern::new(p))
            .collect()
    }

    pub fn matches(&self, cmd: &str, arg: &str) -> bool {
        if self.command != cmd {
            return false;
        }
        if self.patterns.is_empty() {
            return false;
        }
        self.patterns.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|p| p.matches(arg))
                .unwrap_or(false)
        })
    }

    pub fn matches_all(&self, cmd: &str, args: &[&str]) -> bool {
        if self.command != cmd {
            return false;
        }
        args.iter().all(|arg| self.matches(cmd, arg))
    }

    pub fn has_wildcards(&self) -> bool {
        self.patterns
            .iter()
            .any(|p| p.contains('*') || p.contains('?') || p.contains('['))
    }

    pub fn matches_exact(&self, cmd: &str, arg: &str) -> bool {
        if self.command != cmd {
            return false;
        }
        if self.patterns.is_empty() {
            return false;
        }
        self.patterns.iter().any(|pattern| pattern == arg)
    }

    pub fn get_broad_patterns(&self) -> Vec<&str> {
        self.patterns
            .iter()
            .filter(|p| p.ends_with('*'))
            .map(|p| p.as_str())
            .collect()
    }
}

pub const DEFAULT_CLI_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_CLI_MAX_OUTPUT_BYTES: usize = 1_048_576;
pub const DEFAULT_CLI_MAX_CONCURRENT: usize = 2;
pub const DEFAULT_CLI_RATE_LIMIT_PER_MINUTE: u32 = 10;

fn default_cli_timeout_ms() -> u64 {
    DEFAULT_CLI_TIMEOUT_MS
}
fn default_cli_max_output_bytes() -> usize {
    DEFAULT_CLI_MAX_OUTPUT_BYTES
}
fn default_cli_max_concurrent() -> usize {
    DEFAULT_CLI_MAX_CONCURRENT
}
fn default_cli_rate_limit_per_minute() -> u32 {
    DEFAULT_CLI_RATE_LIMIT_PER_MINUTE
}

/// CLI execution capability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CliCapability {
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub allowed_args: Vec<ArgPattern>,
    #[serde(default)]
    pub allowed_env: Vec<String>,
    #[serde(default = "default_cli_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_cli_max_output_bytes")]
    pub max_output_bytes: usize,
    #[serde(default = "default_cli_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "default_cli_rate_limit_per_minute")]
    pub rate_limit_per_minute: u32,
}

impl Default for CliCapability {
    fn default() -> Self {
        Self {
            allowed_commands: Vec::new(),
            allowed_args: Vec::new(),
            allowed_env: Vec::new(),
            timeout_ms: default_cli_timeout_ms(),
            max_output_bytes: default_cli_max_output_bytes(),
            max_concurrent: default_cli_max_concurrent(),
            rate_limit_per_minute: default_cli_rate_limit_per_minute(),
        }
    }
}
