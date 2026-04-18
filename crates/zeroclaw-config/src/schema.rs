use crate::autonomy::AutonomyLevel;
use crate::domain_matcher::DomainMatcher;
use crate::provider_aliases::{is_glm_alias, is_zai_alias};
use crate::traits::{ChannelConfig, HasPropKind, PropKind};
use anyhow::{Context, Result};
use directories::UserDirs;
#[cfg(feature = "schema-export")]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
#[cfg(unix)]
use tokio::fs::File;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use zeroclaw_macros::Configurable;

const SUPPORTED_PROXY_SERVICE_KEYS: &[&str] = &[
    "provider.anthropic",
    "provider.compatible",
    "provider.copilot",
    "provider.gemini",
    "provider.glm",
    "provider.ollama",
    "provider.openai",
    "provider.openrouter",
    "channel.dingtalk",
    "channel.discord",
    "channel.feishu",
    "channel.lark",
    "channel.matrix",
    "channel.mattermost",
    "channel.nextcloud_talk",
    "channel.qq",
    "channel.signal",
    "channel.slack",
    "channel.telegram",
    "channel.wati",
    "channel.whatsapp",
    "tool.browser",
    "tool.composio",
    "tool.http_request",
    "tool.pushover",
    "tool.web_search",
    "memory.embeddings",
    "tunnel.custom",
    "transcription.groq",
];

const SUPPORTED_PROXY_SERVICE_SELECTORS: &[&str] = &[
    "provider.*",
    "channel.*",
    "tool.*",
    "memory.*",
    "tunnel.*",
    "transcription.*",
];

static RUNTIME_PROXY_CONFIG: OnceLock<RwLock<ProxyConfig>> = OnceLock::new();
static RUNTIME_PROXY_CLIENT_CACHE: OnceLock<RwLock<HashMap<String, reqwest::Client>>> =
    OnceLock::new();

// ── Top-level config ──────────────────────────────────────────────

/// Top-level ZeroClaw configuration, loaded from `config.toml`.
///
/// Resolution order: `ZEROCLAW_WORKSPACE` env → `active_workspace.toml` marker → `~/.zeroclaw/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct Config {
    /// Workspace directory - computed from home, not serialized
    #[serde(skip)]
    pub workspace_dir: PathBuf,
    /// Path to config.toml - computed from home, not serialized
    #[serde(skip)]
    pub config_path: PathBuf,
    /// Config file schema version.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Provider configuration (`[providers]`).
    #[serde(default)]
    #[nested]
    pub providers: crate::providers::ProvidersConfig,

    /// Observability backend configuration (`[observability]`).
    #[serde(default)]
    #[nested]
    pub observability: ObservabilityConfig,

    /// Autonomy and security policy configuration (`[autonomy]`).
    #[serde(default)]
    #[nested]
    pub autonomy: AutonomyConfig,

    /// Trust scoring and regression detection configuration (`[trust]`).
    #[serde(default)]
    #[nested]
    pub trust: crate::scattered_types::TrustConfig,

    /// Security subsystem configuration (`[security]`).
    #[serde(default)]
    #[nested]
    pub security: SecurityConfig,

    /// Backup tool configuration (`[backup]`).
    #[serde(default)]
    #[nested]
    pub backup: BackupConfig,

    /// Data retention and purge configuration (`[data_retention]`).
    #[serde(default)]
    #[nested]
    pub data_retention: DataRetentionConfig,

    /// Cloud transformation accelerator configuration (`[cloud_ops]`).
    #[serde(default)]
    #[nested]
    pub cloud_ops: CloudOpsConfig,

    /// Conversational AI agent builder configuration (`[conversational_ai]`).
    ///
    /// Experimental / future feature — not yet wired into the agent runtime.
    /// Omitted from generated config files when disabled (the default).
    /// Existing configs that already contain this section will continue to
    /// deserialize correctly thanks to `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "ConversationalAiConfig::is_disabled")]
    #[nested]
    pub conversational_ai: ConversationalAiConfig,

    /// Managed cybersecurity service configuration (`[security_ops]`).
    #[serde(default)]
    #[nested]
    pub security_ops: SecurityOpsConfig,

    /// Runtime adapter configuration (`[runtime]`). Controls native vs Docker execution.
    #[serde(default)]
    #[nested]
    pub runtime: RuntimeConfig,

    /// Reliability settings: retries, fallback providers, backoff (`[reliability]`).
    #[serde(default)]
    #[nested]
    pub reliability: ReliabilityConfig,

    /// Scheduler configuration for periodic task execution (`[scheduler]`).
    #[serde(default)]
    #[nested]
    pub scheduler: SchedulerConfig,

    /// Agent orchestration settings (`[agent]`).
    #[serde(default)]
    #[nested]
    pub agent: AgentConfig,

    /// Pacing controls for slow/local LLM workloads (`[pacing]`).
    #[serde(default)]
    #[nested]
    pub pacing: PacingConfig,

    /// Skills loading and community repository behavior (`[skills]`).
    #[serde(default)]
    #[nested]
    pub skills: SkillsConfig,

    /// Pipeline tool configuration (`[pipeline]`).
    #[serde(default)]
    #[nested]
    pub pipeline: PipelineConfig,

    /// Automatic query classification — maps user messages to model hints.
    #[serde(default)]
    #[nested]
    pub query_classification: QueryClassificationConfig,

    /// Heartbeat configuration for periodic health pings (`[heartbeat]`).
    #[serde(default)]
    #[nested]
    pub heartbeat: HeartbeatConfig,

    /// Cron job configuration (`[cron]`).
    #[serde(default)]
    #[nested]
    pub cron: CronConfig,

    /// Channel configurations: Telegram, Discord, Slack, etc. (`[channels]`).
    #[serde(default, alias = "channels_config")]
    #[nested]
    pub channels: ChannelsConfig,

    /// Memory backend configuration: sqlite, markdown, embeddings (`[memory]`).
    #[serde(default)]
    #[nested]
    pub memory: MemoryConfig,

    /// Persistent storage provider configuration (`[storage]`).
    #[serde(default)]
    #[nested]
    pub storage: StorageConfig,

    /// Tunnel configuration for exposing the gateway publicly (`[tunnel]`).
    #[serde(default)]
    #[nested]
    pub tunnel: TunnelConfig,

    /// Gateway server configuration: host, port, pairing, rate limits (`[gateway]`).
    #[serde(default)]
    #[nested]
    pub gateway: GatewayConfig,

    /// Composio managed OAuth tools integration (`[composio]`).
    #[serde(default)]
    #[nested]
    pub composio: ComposioConfig,

    /// Microsoft 365 Graph API integration (`[microsoft365]`).
    #[serde(default)]
    #[nested]
    pub microsoft365: Microsoft365Config,

    /// Secrets encryption configuration (`[secrets]`).
    #[serde(default)]
    #[nested]
    pub secrets: SecretsConfig,

    /// Browser automation configuration (`[browser]`).
    #[serde(default)]
    #[nested]
    pub browser: BrowserConfig,

    /// Browser delegation configuration (`[browser_delegate]`).
    ///
    /// Delegates browser-based tasks to a browser-capable CLI subprocess (e.g.
    /// Claude Code with `claude-in-chrome` MCP tools). Useful for interacting
    /// with corporate web apps (Teams, Outlook, Jira, Confluence) that lack
    /// direct API access. A persistent Chrome profile can be configured so SSO
    /// sessions survive across invocations.
    ///
    /// Fields:
    /// - `enabled` (`bool`, default `false`) — enable the browser delegation tool.
    /// - `cli_binary` (`String`, default `"claude"`) — CLI binary to spawn for browser tasks.
    /// - `chrome_profile_dir` (`String`, default `""`) — Chrome user-data directory for
    ///   persistent SSO sessions. When empty, a fresh profile is used each invocation.
    /// - `allowed_domains` (`Vec<String>`, default `[]`) — allowlist of domains the browser
    ///   may navigate to. Empty means all non-blocked domains are permitted.
    /// - `blocked_domains` (`Vec<String>`, default `[]`) — denylist of domains. Blocked
    ///   domains take precedence over allowed domains.
    /// - `task_timeout_secs` (`u64`, default `120`) — per-task timeout in seconds.
    ///
    /// Compatibility: additive and disabled by default; existing configs remain valid when omitted.
    /// Rollback/migration: remove `[browser_delegate]` or keep `enabled = false` to disable.
    #[serde(default)]
    #[nested]
    pub browser_delegate: crate::scattered_types::BrowserDelegateConfig,

    /// HTTP request tool configuration (`[http_request]`).
    #[serde(default)]
    #[nested]
    pub http_request: HttpRequestConfig,

    /// Multimodal (image) handling configuration (`[multimodal]`).
    #[serde(default)]
    #[nested]
    pub multimodal: MultimodalConfig,

    /// Automatic media understanding pipeline (`[media_pipeline]`).
    #[serde(default)]
    #[nested]
    pub media_pipeline: MediaPipelineConfig,

    /// Web fetch tool configuration (`[web_fetch]`).
    #[serde(default)]
    #[nested]
    pub web_fetch: WebFetchConfig,

    /// Link enricher configuration (`[link_enricher]`).
    #[serde(default)]
    #[nested]
    pub link_enricher: LinkEnricherConfig,

    /// Text browser tool configuration (`[text_browser]`).
    #[serde(default)]
    #[nested]
    pub text_browser: TextBrowserConfig,

    /// Web search tool configuration (`[web_search]`).
    #[serde(default)]
    #[nested]
    pub web_search: WebSearchConfig,

    /// Project delivery intelligence configuration (`[project_intel]`).
    #[serde(default)]
    #[nested]
    pub project_intel: ProjectIntelConfig,

    /// Google Workspace CLI (`gws`) tool configuration (`[google_workspace]`).
    #[serde(default)]
    #[nested]
    pub google_workspace: GoogleWorkspaceConfig,

    /// Proxy configuration for outbound HTTP/HTTPS/SOCKS5 traffic (`[proxy]`).
    #[serde(default)]
    #[nested]
    pub proxy: ProxyConfig,

    /// Identity format configuration: OpenClaw or AIEOS (`[identity]`).
    #[serde(default)]
    #[nested]
    pub identity: IdentityConfig,

    /// Cost tracking and budget enforcement configuration (`[cost]`).
    #[serde(default)]
    #[nested]
    pub cost: CostConfig,

    /// Peripheral board configuration for hardware integration (`[peripherals]`).
    #[serde(default)]
    #[nested]
    pub peripherals: PeripheralsConfig,

    /// Delegate tool global default configuration (`[delegate]`).
    #[serde(default)]
    #[nested]
    pub delegate: DelegateToolConfig,

    /// Delegate agent configurations for multi-agent workflows.
    #[serde(default)]
    #[nested]
    pub agents: HashMap<String, DelegateAgentConfig>,

    /// Swarm configurations for multi-agent orchestration.
    #[serde(default)]
    pub swarms: HashMap<String, SwarmConfig>,

    /// Hooks configuration (lifecycle hooks and built-in hook toggles).
    #[serde(default)]
    #[nested]
    pub hooks: HooksConfig,

    /// Hardware configuration (wizard-driven physical world setup).
    #[serde(default)]
    #[nested]
    pub hardware: HardwareConfig,

    /// Voice transcription configuration (Whisper API via Groq).
    #[serde(default)]
    #[nested]
    pub transcription: TranscriptionConfig,

    /// Text-to-Speech configuration (`[tts]`).
    #[serde(default)]
    #[nested]
    pub tts: TtsConfig,

    /// External MCP server connections (`[mcp]`).
    #[serde(default, alias = "mcpServers")]
    #[nested]
    pub mcp: McpConfig,

    /// Dynamic node discovery configuration (`[nodes]`).
    #[serde(default)]
    #[nested]
    pub nodes: NodesConfig,

    /// Multi-client workspace isolation configuration (`[workspace]`).
    #[serde(default)]
    #[nested]
    pub workspace: WorkspaceConfig,

    /// Notion integration configuration (`[notion]`).
    #[serde(default)]
    #[nested]
    pub notion: NotionConfig,

    /// Jira integration configuration (`[jira]`).
    #[serde(default)]
    #[nested]
    pub jira: JiraConfig,

    /// Secure inter-node transport configuration (`[node_transport]`).
    #[serde(default)]
    #[nested]
    pub node_transport: NodeTransportConfig,

    /// Knowledge graph configuration (`[knowledge]`).
    #[serde(default)]
    #[nested]
    pub knowledge: KnowledgeConfig,

    /// LinkedIn integration configuration (`[linkedin]`).
    #[serde(default)]
    #[nested]
    pub linkedin: LinkedInConfig,

    /// Standalone image generation tool configuration (`[image_gen]`).
    #[serde(default)]
    #[nested]
    pub image_gen: ImageGenConfig,

    /// Plugin system configuration (`[plugins]`).
    #[serde(default)]
    #[nested]
    pub plugins: PluginsConfig,

    /// Locale for tool descriptions (e.g. `"en"`, `"zh-CN"`).
    ///
    /// When set, tool descriptions shown in system prompts are loaded from
    /// `tool_descriptions/<locale>.toml`. Falls back to English, then to
    /// hardcoded descriptions.
    ///
    /// If omitted or empty, the locale is auto-detected from `ZEROCLAW_LOCALE`,
    /// `LANG`, or `LC_ALL` environment variables (defaulting to `"en"`).
    #[serde(default)]
    pub locale: Option<String>,

    /// Verifiable Intent (VI) credential verification and issuance (`[verifiable_intent]`).
    #[serde(default)]
    #[nested]
    pub verifiable_intent: VerifiableIntentConfig,

    /// Claude Code tool configuration (`[claude_code]`).
    #[serde(default)]
    #[nested]
    pub claude_code: ClaudeCodeConfig,

    /// Claude Code task runner with Slack progress and SSH session handoff (`[claude_code_runner]`).
    #[serde(default)]
    #[nested]
    pub claude_code_runner: ClaudeCodeRunnerConfig,

    /// Codex CLI tool configuration (`[codex_cli]`).
    #[serde(default)]
    #[nested]
    pub codex_cli: CodexCliConfig,

    /// Gemini CLI tool configuration (`[gemini_cli]`).
    #[serde(default)]
    #[nested]
    pub gemini_cli: GeminiCliConfig,

    /// OpenCode CLI tool configuration (`[opencode_cli]`).
    #[serde(default)]
    #[nested]
    pub opencode_cli: OpenCodeCliConfig,

    /// Standard Operating Procedures engine configuration (`[sop]`).
    #[serde(default)]
    #[nested]
    pub sop: SopConfig,

    /// Shell tool configuration (`[shell_tool]`).
    #[serde(default)]
    #[nested]
    pub shell_tool: ShellToolConfig,
}

/// Multi-client workspace isolation configuration.
///
/// When enabled, each client engagement gets an isolated workspace with
/// separate memory, audit, secrets, and tool restrictions.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "workspace"]
pub struct WorkspaceConfig {
    /// Enable workspace isolation. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Currently active workspace name.
    #[serde(default)]
    pub active_workspace: Option<String>,
    /// Base directory for workspace profiles.
    #[serde(default = "default_workspaces_dir")]
    pub workspaces_dir: String,
    /// Isolate memory databases per workspace. Default: true.
    #[serde(default = "default_true")]
    pub isolate_memory: bool,
    /// Isolate secrets namespaces per workspace. Default: true.
    #[serde(default = "default_true")]
    pub isolate_secrets: bool,
    /// Isolate audit logs per workspace. Default: true.
    #[serde(default = "default_true")]
    pub isolate_audit: bool,
    /// Allow searching across workspaces. Default: false (security).
    #[serde(default)]
    pub cross_workspace_search: bool,
}

fn default_workspaces_dir() -> String {
    "~/.zeroclaw/workspaces".to_string()
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            active_workspace: None,
            workspaces_dir: default_workspaces_dir(),
            isolate_memory: true,
            isolate_secrets: true,
            isolate_audit: true,
            cross_workspace_search: false,
        }
    }
}

/// Named provider profile definition.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers.models"]
pub struct ModelProviderConfig {
    /// API key for this provider.
    #[secret]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Optional provider type/name override.
    #[serde(default)]
    pub name: Option<String>,
    /// Base URL for OpenAI-compatible endpoints.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Custom API path suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_path: Option<String>,
    /// Default model for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model temperature (0.0–2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// HTTP timeout in seconds for API calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Extra HTTP headers for API requests.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_headers: HashMap<String, String>,
    /// Provider protocol variant ("responses" or "chat_completions").
    #[serde(default)]
    pub wire_api: Option<String>,
    /// If true, load OpenAI auth material (OPENAI_API_KEY or ~/.codex/auth.json).
    #[serde(default)]
    pub requires_openai_auth: bool,
    /// Azure OpenAI resource name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_openai_resource: Option<String>,
    /// Azure OpenAI deployment name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_openai_deployment: Option<String>,
    /// Azure OpenAI API version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure_openai_api_version: Option<String>,
    /// Maximum output tokens for API requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Merge system messages into first user message.
    #[serde(default)]
    pub merge_system_into_user: bool,
}

// ── Delegate Tool Configuration ─────────────────────────────────

/// Global delegate tool configuration for default timeout values.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "delegate"]
pub struct DelegateToolConfig {
    /// Default timeout in seconds for non-agentic sub-agent provider calls.
    /// Can be overridden per-agent in `[agents.<name>]` config.
    /// Default: 120 seconds.
    #[serde(default = "default_delegate_timeout_secs")]
    pub timeout_secs: u64,
    /// Default timeout in seconds for agentic sub-agent runs.
    /// Can be overridden per-agent in `[agents.<name>]` config.
    /// Default: 300 seconds.
    #[serde(default = "default_delegate_agentic_timeout_secs")]
    pub agentic_timeout_secs: u64,
}

impl Default for DelegateToolConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_DELEGATE_TIMEOUT_SECS,
            agentic_timeout_secs: DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS,
        }
    }
}

// ── Delegate Agents ──────────────────────────────────────────────

/// Configuration for a delegate sub-agent used by the `delegate` tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "delegate-agent"]
pub struct DelegateAgentConfig {
    /// Provider name (e.g. "ollama", "openrouter", "anthropic")
    pub provider: String,
    /// Model name
    pub model: String,
    /// Optional system prompt for the sub-agent
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional API key override
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Temperature override
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Max recursion depth for nested delegation
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Enable agentic sub-agent mode (multi-turn tool-call loop).
    #[serde(default)]
    pub agentic: bool,
    /// Allowlist of tool names available to the sub-agent in agentic mode.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Maximum tool-call iterations in agentic mode.
    #[serde(default = "default_max_tool_iterations")]
    pub max_iterations: usize,
    /// Optional timeout in seconds for non-agentic sub-agent provider calls.
    /// When `None`, falls back to `[delegate].timeout_secs` (default: 120).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Optional timeout in seconds for agentic sub-agent runs.
    /// When `None`, falls back to `[delegate].agentic_timeout_secs` (default: 300).
    #[serde(default)]
    pub agentic_timeout_secs: Option<u64>,
    /// Optional skills directory path (relative to workspace root) for scoped skill loading.
    /// When unset or empty, the sub-agent falls back to the default workspace `skills/` directory.
    #[serde(default)]
    pub skills_directory: Option<String>,
    /// Optional memory namespace for isolation.
    /// When set, the sub-agent's memory operations are isolated to this namespace,
    /// preventing cross-contamination with memory from other agents.
    #[serde(default)]
    pub memory_namespace: Option<String>,
}

fn default_delegate_timeout_secs() -> u64 {
    DEFAULT_DELEGATE_TIMEOUT_SECS
}

fn default_delegate_agentic_timeout_secs() -> u64 {
    DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS
}

// ── Swarms ──────────────────────────────────────────────────────

/// Orchestration strategy for a swarm of agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SwarmStrategy {
    /// Run agents sequentially; each agent's output feeds into the next.
    Sequential,
    /// Run agents in parallel; collect all outputs.
    Parallel,
    /// Use the LLM to pick the best agent for the task.
    Router,
}

/// Configuration for a swarm of coordinated agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SwarmConfig {
    /// Ordered list of agent names (must reference keys in `agents`).
    pub agents: Vec<String>,
    /// Orchestration strategy.
    pub strategy: SwarmStrategy,
    /// System prompt for router strategy (used to pick the best agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_prompt: Option<String>,
    /// Optional description shown to the LLM when choosing swarms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Maximum total timeout for the swarm execution in seconds.
    #[serde(default = "default_swarm_timeout_secs")]
    pub timeout_secs: u64,
}

const DEFAULT_SWARM_TIMEOUT_SECS: u64 = 300;

fn default_swarm_timeout_secs() -> u64 {
    DEFAULT_SWARM_TIMEOUT_SECS
}

/// Valid temperature range for all paths (config, CLI, env override).
pub const TEMPERATURE_RANGE: std::ops::RangeInclusive<f64> = 0.0..=2.0;

/// Defaults to 0 so configs without an explicit `schema_version` are recognized
/// as pre-versioning and get migrated.
fn default_schema_version() -> u32 {
    0
}

/// Default delegate tool timeout for non-agentic calls: 120 seconds.
pub const DEFAULT_DELEGATE_TIMEOUT_SECS: u64 = 120;

/// Default delegate tool timeout for agentic runs: 300 seconds.
pub const DEFAULT_DELEGATE_AGENTIC_TIMEOUT_SECS: u64 = 300;

/// Validate that a temperature value is within the allowed range.
pub fn validate_temperature(value: f64) -> std::result::Result<f64, String> {
    if TEMPERATURE_RANGE.contains(&value) {
        Ok(value)
    } else {
        Err(format!(
            "temperature {value} is out of range (expected {}..={})",
            TEMPERATURE_RANGE.start(),
            TEMPERATURE_RANGE.end()
        ))
    }
}

fn normalize_reasoning_effort(value: &str) -> std::result::Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "minimal" | "low" | "medium" | "high" | "xhigh" => Ok(normalized),
        _ => Err(format!(
            "reasoning_effort {value:?} is invalid (expected one of: minimal, low, medium, high, xhigh)"
        )),
    }
}

fn deserialize_reasoning_effort_opt<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    value
        .map(|raw| normalize_reasoning_effort(&raw).map_err(serde::de::Error::custom))
        .transpose()
}

fn default_max_depth() -> u32 {
    3
}

fn default_max_tool_iterations() -> usize {
    10
}

// ── Hardware Config (wizard-driven) ─────────────────────────────

/// Hardware transport mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum HardwareTransport {
    #[default]
    None,
    Native,
    Serial,
    Probe,
}

impl std::fmt::Display for HardwareTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Native => write!(f, "native"),
            Self::Serial => write!(f, "serial"),
            Self::Probe => write!(f, "probe"),
        }
    }
}

/// Wizard-driven hardware configuration for physical world interaction.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "hardware"]
pub struct HardwareConfig {
    /// Whether hardware access is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Transport mode
    #[serde(default)]
    pub transport: HardwareTransport,
    /// Serial port path (e.g. "/dev/ttyACM0")
    #[serde(default)]
    pub serial_port: Option<String>,
    /// Serial baud rate
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,
    /// Probe target chip (e.g. "STM32F401RE")
    #[serde(default)]
    pub probe_target: Option<String>,
    /// Enable workspace datasheet RAG (index PDF schematics for AI pin lookups)
    #[serde(default)]
    pub workspace_datasheets: bool,
}

fn default_baud_rate() -> u32 {
    115_200
}

impl HardwareConfig {
    /// Return the active transport mode.
    pub fn transport_mode(&self) -> HardwareTransport {
        self.transport.clone()
    }
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transport: HardwareTransport::None,
            serial_port: None,
            baud_rate: default_baud_rate(),
            probe_target: None,
            workspace_datasheets: false,
        }
    }
}

// ── Transcription ────────────────────────────────────────────────

fn default_transcription_api_url() -> String {
    "https://api.groq.com/openai/v1/audio/transcriptions".into()
}

fn default_transcription_model() -> String {
    "whisper-large-v3-turbo".into()
}

fn default_transcription_max_duration_secs() -> u64 {
    120
}

fn default_transcription_provider() -> String {
    "groq".into()
}

fn default_openai_stt_model() -> String {
    "whisper-1".into()
}

fn default_deepgram_stt_model() -> String {
    "nova-2".into()
}

fn default_google_stt_language_code() -> String {
    "en-US".into()
}

/// Voice transcription configuration with multi-provider support.
///
/// The top-level `api_url`, `model`, and `api_key` fields remain for backward
/// compatibility with existing Groq-based configurations.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription"]
pub struct TranscriptionConfig {
    /// Enable voice transcription for channels that support it.
    #[serde(default)]
    pub enabled: bool,
    /// Default STT provider: "groq", "openai", "deepgram", "assemblyai", "google".
    #[serde(default = "default_transcription_provider")]
    pub default_provider: String,
    /// API key used for transcription requests (Groq provider).
    ///
    /// If unset, runtime falls back to `GROQ_API_KEY` for backward compatibility.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Whisper API endpoint URL (Groq provider).
    #[serde(default = "default_transcription_api_url")]
    pub api_url: String,
    /// Whisper model name (Groq provider).
    #[serde(default = "default_transcription_model")]
    pub model: String,
    /// Optional language hint (ISO-639-1, e.g. "en", "ru") for Groq provider.
    #[serde(default)]
    pub language: Option<String>,
    /// Optional initial prompt to bias transcription toward expected vocabulary
    /// (proper nouns, technical terms, etc.). Sent as the `prompt` field in the
    /// Whisper API request.
    #[serde(default)]
    pub initial_prompt: Option<String>,
    /// Maximum voice duration in seconds (messages longer than this are skipped).
    #[serde(default = "default_transcription_max_duration_secs")]
    pub max_duration_secs: u64,
    /// OpenAI Whisper STT provider configuration.
    #[serde(default)]
    #[nested]
    pub openai: Option<OpenAiSttConfig>,
    /// Deepgram STT provider configuration.
    #[serde(default)]
    #[nested]
    pub deepgram: Option<DeepgramSttConfig>,
    /// AssemblyAI STT provider configuration.
    #[serde(default)]
    #[nested]
    pub assemblyai: Option<AssemblyAiSttConfig>,
    /// Google Cloud Speech-to-Text provider configuration.
    #[serde(default)]
    #[nested]
    pub google: Option<GoogleSttConfig>,
    /// Local/self-hosted Whisper-compatible STT provider.
    #[serde(default)]
    #[nested]
    pub local_whisper: Option<LocalWhisperConfig>,
    /// Also transcribe non-PTT (forwarded/regular) audio messages on WhatsApp,
    /// not just voice notes.  Default: `false` (preserves legacy behavior).
    #[serde(default)]
    pub transcribe_non_ptt_audio: bool,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_provider: default_transcription_provider(),
            api_key: None,
            api_url: default_transcription_api_url(),
            model: default_transcription_model(),
            language: None,
            initial_prompt: None,
            max_duration_secs: default_transcription_max_duration_secs(),
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        }
    }
}

// ── MCP ─────────────────────────────────────────────────────────

/// Transport type for MCP server connections.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    /// Spawn a local process and communicate over stdin/stdout.
    #[default]
    Stdio,
    /// Connect via HTTP POST.
    Http,
    /// Connect via HTTP + Server-Sent Events.
    Sse,
}

/// Configuration for a single external MCP server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct McpServerConfig {
    /// Display name used as a tool prefix (`<server>__<tool>`).
    pub name: String,
    /// Transport type (default: stdio).
    #[serde(default)]
    pub transport: McpTransport,
    /// URL for HTTP/SSE transports.
    #[serde(default)]
    pub url: Option<String>,
    /// Executable to spawn for stdio transport.
    #[serde(default)]
    pub command: String,
    /// Command arguments for stdio transport.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional environment variables for stdio transport.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Optional HTTP headers for HTTP/SSE transports.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional per-call timeout in seconds (hard capped in validation).
    #[serde(default)]
    pub tool_timeout_secs: Option<u64>,
}

/// External MCP client configuration (`[mcp]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "mcp"]
pub struct McpConfig {
    /// Enable MCP tool loading.
    #[serde(default)]
    pub enabled: bool,
    /// Load MCP tool schemas on-demand via `tool_search` instead of eagerly
    /// including them in the LLM context window. When `true` (the default),
    /// only tool names are listed in the system prompt; the LLM must call
    /// `tool_search` to fetch full schemas before invoking a deferred tool.
    #[serde(default = "default_deferred_loading")]
    pub deferred_loading: bool,
    /// Configured MCP servers.
    #[serde(default, alias = "mcpServers")]
    pub servers: Vec<McpServerConfig>,
}

fn default_deferred_loading() -> bool {
    true
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            deferred_loading: default_deferred_loading(),
            servers: Vec::new(),
        }
    }
}

/// Verifiable Intent (VI) credential verification and issuance (`[verifiable_intent]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "verifiable-intent"]
pub struct VerifiableIntentConfig {
    /// Enable VI credential verification on commerce tool calls (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Strictness mode for constraint evaluation: "strict" (fail-closed on unknown
    /// constraint types) or "permissive" (skip unknown types with a warning).
    /// Default: "strict".
    #[serde(default = "default_vi_strictness")]
    pub strictness: String,
}

fn default_vi_strictness() -> String {
    "strict".to_owned()
}

impl Default for VerifiableIntentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            strictness: default_vi_strictness(),
        }
    }
}

// ── Nodes (Dynamic Node Discovery) ───────────────────────────────

/// Configuration for the dynamic node discovery system (`[nodes]`).
///
/// When enabled, external processes/devices can connect via WebSocket
/// at `/ws/nodes` and advertise their capabilities at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "nodes"]
pub struct NodesConfig {
    /// Enable dynamic node discovery endpoint.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of concurrent node connections.
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
    /// Optional bearer token for node authentication.
    #[serde(default)]
    pub auth_token: Option<String>,
}

fn default_max_nodes() -> usize {
    16
}

impl Default for NodesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_nodes: default_max_nodes(),
            auth_token: None,
        }
    }
}

// ── TTS (Text-to-Speech) ─────────────────────────────────────────

fn default_tts_provider() -> String {
    "openai".into()
}

fn default_tts_voice() -> String {
    "alloy".into()
}

fn default_tts_format() -> String {
    "mp3".into()
}

fn default_tts_max_text_length() -> usize {
    4096
}

fn default_openai_tts_model() -> String {
    "tts-1".into()
}

fn default_openai_tts_speed() -> f64 {
    1.0
}

fn default_elevenlabs_model_id() -> String {
    "eleven_monolingual_v1".into()
}

fn default_elevenlabs_stability() -> f64 {
    0.5
}

fn default_elevenlabs_similarity_boost() -> f64 {
    0.5
}

fn default_google_tts_language_code() -> String {
    "en-US".into()
}

fn default_edge_tts_binary_path() -> String {
    "edge-tts".into()
}

fn default_piper_tts_api_url() -> String {
    "http://127.0.0.1:5000/v1/audio/speech".into()
}

/// Text-to-Speech configuration (`[tts]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts"]
pub struct TtsConfig {
    /// Enable TTS synthesis.
    #[serde(default)]
    pub enabled: bool,
    /// Default TTS provider (`"openai"`, `"elevenlabs"`, `"google"`, `"edge"`).
    #[serde(default = "default_tts_provider")]
    pub default_provider: String,
    /// Default voice ID passed to the selected provider.
    #[serde(default = "default_tts_voice")]
    pub default_voice: String,
    /// Default audio output format (`"mp3"`, `"opus"`, `"wav"`).
    #[serde(default = "default_tts_format")]
    pub default_format: String,
    /// Maximum input text length in characters (default 4096).
    #[serde(default = "default_tts_max_text_length")]
    pub max_text_length: usize,
    /// OpenAI TTS provider configuration (`[tts.openai]`).
    #[serde(default)]
    #[nested]
    pub openai: Option<OpenAiTtsConfig>,
    /// ElevenLabs TTS provider configuration (`[tts.elevenlabs]`).
    #[serde(default)]
    #[nested]
    pub elevenlabs: Option<ElevenLabsTtsConfig>,
    /// Google Cloud TTS provider configuration (`[tts.google]`).
    #[serde(default)]
    #[nested]
    pub google: Option<GoogleTtsConfig>,
    /// Edge TTS provider configuration (`[tts.edge]`).
    #[serde(default)]
    #[nested]
    pub edge: Option<EdgeTtsConfig>,
    /// Piper TTS provider configuration (`[tts.piper]`).
    #[serde(default)]
    #[nested]
    pub piper: Option<PiperTtsConfig>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_provider: default_tts_provider(),
            default_voice: default_tts_voice(),
            default_format: default_tts_format(),
            max_text_length: default_tts_max_text_length(),
            openai: None,
            elevenlabs: None,
            google: None,
            edge: None,
            piper: None,
        }
    }
}

/// OpenAI TTS provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts.openai"]
pub struct OpenAiTtsConfig {
    /// API key for OpenAI TTS.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Model name (default `"tts-1"`).
    #[serde(default = "default_openai_tts_model")]
    pub model: String,
    /// Playback speed multiplier (default `1.0`).
    #[serde(default = "default_openai_tts_speed")]
    pub speed: f64,
}

/// ElevenLabs TTS provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts.elevenlabs"]
pub struct ElevenLabsTtsConfig {
    /// API key for ElevenLabs.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Model ID (default `"eleven_monolingual_v1"`).
    #[serde(default = "default_elevenlabs_model_id")]
    pub model_id: String,
    /// Voice stability (0.0-1.0, default `0.5`).
    #[serde(default = "default_elevenlabs_stability")]
    pub stability: f64,
    /// Similarity boost (0.0-1.0, default `0.5`).
    #[serde(default = "default_elevenlabs_similarity_boost")]
    pub similarity_boost: f64,
}

/// Google Cloud TTS provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts.google"]
pub struct GoogleTtsConfig {
    /// API key for Google Cloud TTS.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Language code (default `"en-US"`).
    #[serde(default = "default_google_tts_language_code")]
    pub language_code: String,
}

/// Edge TTS provider configuration (free, subprocess-based).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts.edge"]
pub struct EdgeTtsConfig {
    /// Path to the `edge-tts` binary (default `"edge-tts"`).
    #[serde(default = "default_edge_tts_binary_path")]
    pub binary_path: String,
}

impl Default for EdgeTtsConfig {
    fn default() -> Self {
        Self {
            binary_path: default_edge_tts_binary_path(),
        }
    }
}

/// Piper TTS provider configuration (local GPU-accelerated, OpenAI-compatible endpoint).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tts.piper"]
pub struct PiperTtsConfig {
    /// Base URL for the Piper TTS HTTP server (e.g. `"http://127.0.0.1:5000/v1/audio/speech"`).
    #[serde(default = "default_piper_tts_api_url")]
    pub api_url: String,
}

impl Default for PiperTtsConfig {
    fn default() -> Self {
        Self {
            api_url: default_piper_tts_api_url(),
        }
    }
}

/// Determines when a `ToolFilterGroup` is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolFilterGroupMode {
    /// Tools in this group are always included in every turn.
    Always,
    /// Tools in this group are included only when the user message contains
    /// at least one of the configured `keywords` (case-insensitive substring match).
    #[default]
    Dynamic,
}

/// A named group of MCP tool patterns with an activation mode.
///
/// Each group lists glob patterns for MCP tool names (prefix `mcp_`) and an
/// optional set of keywords that trigger inclusion in `dynamic` mode.
/// Built-in (non-MCP) tools always pass through and are never affected by
/// `tool_filter_groups`.
///
/// # Example
/// ```toml
/// [[agent.tool_filter_groups]]
/// mode = "always"
/// tools = ["mcp_filesystem_*"]
/// keywords = []
///
/// [[agent.tool_filter_groups]]
/// mode = "dynamic"
/// tools = ["mcp_browser_*"]
/// keywords = ["browse", "website", "url", "search"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ToolFilterGroup {
    /// Activation mode: `"always"` or `"dynamic"`.
    #[serde(default)]
    pub mode: ToolFilterGroupMode,
    /// Glob patterns matching MCP tool names (single `*` wildcard supported).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Keywords that activate this group in `dynamic` mode (case-insensitive substring).
    /// Ignored when `mode = "always"`.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// When true, also filter built-in tools (not just MCP tools).
    #[serde(default)]
    pub filter_builtins: bool,
}

/// OpenAI Whisper STT provider configuration (`[transcription.openai]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription.openai"]
pub struct OpenAiSttConfig {
    /// OpenAI API key for Whisper transcription.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Whisper model name (default: "whisper-1").
    #[serde(default = "default_openai_stt_model")]
    pub model: String,
}

/// Deepgram STT provider configuration (`[transcription.deepgram]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription.deepgram"]
pub struct DeepgramSttConfig {
    /// Deepgram API key.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Deepgram model name (default: "nova-2").
    #[serde(default = "default_deepgram_stt_model")]
    pub model: String,
}

/// AssemblyAI STT provider configuration (`[transcription.assemblyai]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription.assemblyai"]
pub struct AssemblyAiSttConfig {
    /// AssemblyAI API key.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
}

/// Google Cloud Speech-to-Text provider configuration (`[transcription.google]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription.google"]
pub struct GoogleSttConfig {
    /// Google Cloud API key.
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// BCP-47 language code (default: "en-US").
    #[serde(default = "default_google_stt_language_code")]
    pub language_code: String,
}

/// Local/self-hosted Whisper-compatible STT endpoint (`[transcription.local_whisper]`).
///
/// Configures a self-hosted STT endpoint. Can be on localhost, a private network host, or any reachable URL.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "transcription.local-whisper"]
pub struct LocalWhisperConfig {
    /// HTTP or HTTPS endpoint URL, e.g. `"http://10.10.0.1:8001/v1/transcribe"`.
    pub url: String,
    /// Bearer token for endpoint authentication.
    /// Omit for unauthenticated local endpoints.
    #[serde(default)]
    #[secret]
    pub bearer_token: Option<String>,
    /// Maximum audio file size in bytes accepted by this endpoint.
    /// Defaults to 25 MB — matching the cloud API cap for a safe out-of-the-box
    /// experience. Self-hosted endpoints can accept much larger files; raise this
    /// as needed, but note that each transcription call clones the audio buffer
    /// into a multipart payload, so peak memory per request is ~2× this value.
    #[serde(default = "default_local_whisper_max_audio_bytes")]
    pub max_audio_bytes: usize,
    /// Request timeout in seconds. Defaults to 300 (large files on local GPU).
    #[serde(default = "default_local_whisper_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_local_whisper_max_audio_bytes() -> usize {
    25 * 1024 * 1024
}

fn default_local_whisper_timeout_secs() -> u64 {
    300
}

/// Agent orchestration configuration (`[agent]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "agent"]
pub struct AgentConfig {
    /// When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models.
    #[serde(default)]
    pub compact_context: bool,
    /// Maximum tool-call loop turns per user message. Default: `10`.
    /// Setting to `0` falls back to the safe default of `10`.
    #[serde(default = "default_agent_max_tool_iterations")]
    pub max_tool_iterations: usize,
    /// Maximum conversation history messages retained per session. Default: `50`.
    #[serde(default = "default_agent_max_history_messages")]
    pub max_history_messages: usize,
    /// Maximum estimated tokens for conversation history before compaction triggers.
    /// Uses ~4 chars/token heuristic. When this threshold is exceeded, older messages
    /// are summarized to preserve context while staying within budget. Default: `32000`.
    #[serde(default = "default_agent_max_context_tokens")]
    pub max_context_tokens: usize,
    /// Enable parallel tool execution within a single iteration. Default: `false`.
    #[serde(default)]
    pub parallel_tools: bool,
    /// Tool dispatch strategy (e.g. `"auto"`). Default: `"auto"`.
    #[serde(default = "default_agent_tool_dispatcher")]
    pub tool_dispatcher: String,
    /// Tools exempt from the within-turn duplicate-call dedup check. Default: `[]`.
    #[serde(default)]
    pub tool_call_dedup_exempt: Vec<String>,
    /// Per-turn MCP tool schema filtering groups.
    ///
    /// When non-empty, only MCP tools matched by an active group are included in the
    /// tool schema sent to the LLM for that turn. Built-in tools always pass through.
    /// Default: `[]` (no filtering — all tools included).
    #[serde(default)]
    pub tool_filter_groups: Vec<ToolFilterGroup>,
    /// Maximum characters for the assembled system prompt. When `> 0`, the prompt
    /// is truncated to this limit after assembly (keeping the top portion which
    /// contains identity and safety instructions). `0` means unlimited.
    /// Useful for small-context models (e.g. glm-4.5-air ~8K tokens → set to 8000).
    #[serde(default = "default_max_system_prompt_chars")]
    pub max_system_prompt_chars: usize,
    /// Thinking/reasoning level control. Configures how deeply the model reasons
    /// per message. Users can override per-message with `/think:<level>` directives.
    #[nested]
    #[serde(default)]
    pub thinking: crate::scattered_types::ThinkingConfig,

    /// History pruning configuration for token efficiency.
    #[nested]
    #[serde(default)]
    pub history_pruning: crate::scattered_types::HistoryPrunerConfig,

    /// Enable context-aware tool filtering (only surface relevant tools per iteration).
    #[serde(default)]
    pub context_aware_tools: bool,

    /// Post-response quality evaluator configuration.
    #[nested]
    #[serde(default)]
    pub eval: crate::scattered_types::EvalConfig,

    /// Automatic complexity-based classification fallback.
    #[nested]
    #[serde(default)]
    pub auto_classify: Option<crate::scattered_types::AutoClassifyConfig>,

    /// Context compression configuration for automatic conversation compaction.
    #[nested]
    #[serde(default)]
    pub context_compression: crate::scattered_types::ContextCompressionConfig,

    /// Maximum characters for a single tool result before truncation.
    /// Head (2/3) and tail (1/3) are preserved with a truncation marker in the
    /// middle. Set to `0` to disable truncation. Default: `50000`.
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,

    /// Number of most recent conversation turns whose full tool-call/result
    /// messages are preserved in channel conversation history. Older turns
    /// keep only the final assistant text. Set to `0` to disable (previous
    /// behavior). Default: `2`.
    #[serde(default = "default_keep_tool_context_turns")]
    pub keep_tool_context_turns: usize,
}

fn default_max_tool_result_chars() -> usize {
    50_000
}

fn default_keep_tool_context_turns() -> usize {
    2
}

fn default_agent_max_tool_iterations() -> usize {
    10
}

fn default_agent_max_history_messages() -> usize {
    50
}

fn default_agent_max_context_tokens() -> usize {
    32_000
}

fn default_agent_tool_dispatcher() -> String {
    "auto".into()
}

fn default_max_system_prompt_chars() -> usize {
    0
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            compact_context: true,
            max_tool_iterations: default_agent_max_tool_iterations(),
            max_history_messages: default_agent_max_history_messages(),
            max_context_tokens: default_agent_max_context_tokens(),
            parallel_tools: false,
            tool_dispatcher: default_agent_tool_dispatcher(),
            tool_call_dedup_exempt: Vec::new(),
            tool_filter_groups: Vec::new(),
            max_system_prompt_chars: default_max_system_prompt_chars(),
            thinking: crate::scattered_types::ThinkingConfig::default(),
            history_pruning: crate::scattered_types::HistoryPrunerConfig::default(),
            context_aware_tools: false,
            eval: crate::scattered_types::EvalConfig::default(),
            auto_classify: None,
            context_compression: crate::scattered_types::ContextCompressionConfig::default(),
            max_tool_result_chars: default_max_tool_result_chars(),
            keep_tool_context_turns: default_keep_tool_context_turns(),
        }
    }
}

// ── Pacing ────────────────────────────────────────────────────────

/// Pacing controls for slow/local LLM workloads (`[pacing]` section).
///
/// All fields are optional and default to values that preserve existing
/// behavior. When set, they extend — not replace — the existing timeout
/// and loop-detection subsystems.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "pacing"]
pub struct PacingConfig {
    /// Per-step timeout in seconds: the maximum time allowed for a single
    /// LLM inference turn, independent of the total message budget.
    /// `None` means no per-step timeout (existing behavior).
    #[serde(default)]
    pub step_timeout_secs: Option<u64>,

    /// Minimum elapsed seconds before loop detection activates.
    /// Tasks completing under this threshold get aggressive loop protection;
    /// longer-running tasks receive a grace period before the detector starts
    /// counting. `None` means loop detection is always active (existing behavior).
    #[serde(default)]
    pub loop_detection_min_elapsed_secs: Option<u64>,

    /// Tool names excluded from identical-output / alternating-pattern loop
    /// detection. Useful for browser workflows where `browser_screenshot`
    /// structurally resembles a loop even when making progress.
    #[serde(default)]
    pub loop_ignore_tools: Vec<String>,

    /// Override for the hardcoded timeout scaling cap (default: 4).
    /// The channel message timeout budget is computed as:
    ///   `message_timeout_secs * min(max_tool_iterations, message_timeout_scale_max)`
    /// Raising this value lets long multi-step tasks with slow local models
    /// receive a proportionally larger budget without inflating the base timeout.
    #[serde(default)]
    pub message_timeout_scale_max: Option<u64>,

    /// Enable pattern-based loop detection (exact repeat, ping-pong,
    /// no-progress). Defaults to `true`.
    #[serde(default = "default_loop_detection_enabled")]
    pub loop_detection_enabled: bool,

    /// Sliding window size for the pattern-based loop detector.
    /// Defaults to 20.
    #[serde(default = "default_loop_detection_window_size")]
    pub loop_detection_window_size: usize,

    /// Number of consecutive identical tool+args calls before the first
    /// escalation (Warning). Defaults to 3.
    #[serde(default = "default_loop_detection_max_repeats")]
    pub loop_detection_max_repeats: usize,
}

fn default_loop_detection_enabled() -> bool {
    true
}

fn default_loop_detection_window_size() -> usize {
    20
}

fn default_loop_detection_max_repeats() -> usize {
    3
}

impl Default for PacingConfig {
    fn default() -> Self {
        Self {
            step_timeout_secs: None,
            loop_detection_min_elapsed_secs: None,
            loop_ignore_tools: Vec::new(),
            message_timeout_scale_max: None,
            loop_detection_enabled: default_loop_detection_enabled(),
            loop_detection_window_size: default_loop_detection_window_size(),
            loop_detection_max_repeats: default_loop_detection_max_repeats(),
        }
    }
}

/// Skills loading configuration (`[skills]` section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SkillsPromptInjectionMode {
    /// Inline full skill instructions and tool metadata into the system prompt.
    #[default]
    Full,
    /// Inline only compact skill metadata (name/description/location) and load details on demand.
    Compact,
}

fn parse_skills_prompt_injection_mode(raw: &str) -> Option<SkillsPromptInjectionMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "full" => Some(SkillsPromptInjectionMode::Full),
        "compact" => Some(SkillsPromptInjectionMode::Compact),
        _ => None,
    }
}

/// Skills loading configuration (`[skills]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "skills"]
pub struct SkillsConfig {
    /// Enable loading and syncing the community open-skills repository.
    /// Default: `false` (opt-in).
    #[serde(default)]
    pub open_skills_enabled: bool,
    /// Optional path to a local open-skills repository.
    /// If unset, defaults to `$HOME/open-skills` when enabled.
    #[serde(default)]
    pub open_skills_dir: Option<String>,
    /// Allow script-like files in skills (`.sh`, `.bash`, `.ps1`, shebang shell files).
    /// Default: `false` (secure by default).
    #[serde(default)]
    pub allow_scripts: bool,
    /// Controls how skills are injected into the system prompt.
    /// `full` preserves legacy behavior. `compact` keeps context small and loads skills on demand.
    #[serde(default)]
    pub prompt_injection_mode: SkillsPromptInjectionMode,
    /// Autonomous skill creation from successful multi-step task executions.
    #[serde(default)]
    #[nested]
    pub skill_creation: SkillCreationConfig,
    /// Automatic skill self-improvement after successful skill usage.
    #[serde(default)]
    #[nested]
    pub skill_improvement: SkillImprovementConfig,
    /// SkillForge external-source discovery + sandbox verification (P1-4, P3-2).
    #[serde(default)]
    #[nested]
    pub skill_forge: SkillForgeConfig,
    /// Confidence scoring + auto-deprecation of flaky skills (P3-1).
    #[serde(default)]
    #[nested]
    pub skill_confidence: SkillConfidenceConfig,
}

/// Autonomous skill creation configuration (`[skills.skill_creation]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "skills.skill-creation"]
#[serde(default)]
pub struct SkillCreationConfig {
    /// Enable automatic skill creation after successful multi-step tasks.
    /// Default: `false`.
    pub enabled: bool,
    /// Maximum number of auto-generated skills to keep.
    /// When exceeded, the oldest auto-generated skill is removed (LRU eviction).
    pub max_skills: usize,
    /// Embedding similarity threshold for deduplication.
    /// Skills with descriptions more similar than this value are skipped.
    pub similarity_threshold: f64,
}

impl Default for SkillCreationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_skills: 500,
            similarity_threshold: 0.85,
        }
    }
}

/// Skill self-improvement configuration (`[skills.auto_improve]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "skills.skill-improvement"]
pub struct SkillImprovementConfig {
    /// Enable automatic skill improvement after successful skill usage.
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minimum interval (in seconds) between improvements for the same skill.
    /// Default: `3600` (1 hour).
    #[serde(default = "default_skill_improvement_cooldown")]
    pub cooldown_secs: u64,
}

fn default_skill_improvement_cooldown() -> u64 {
    3600
}

impl Default for SkillImprovementConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cooldown_secs: 3600,
        }
    }
}

/// SkillForge — external skill discovery, evaluation, integration, and sandbox
/// verification (`[skills.skill_forge]` section).
///
/// Mirrors the runtime-side `zeroclaw_runtime::skillforge::SkillForgeConfig`.
/// Runtime code reads from here via a `From` conversion at daemon startup.
///
/// NOTE: `Debug` is hand-written (below) to redact `github_token`. Do not
/// `#[derive(Debug)]` — doing so would leak the PAT into every trace/error.
#[derive(Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "skills.skill-forge"]
pub struct SkillForgeConfig {
    /// Enable periodic SkillForge discovery runs. Default: `false` (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// When `true`, candidates that meet `min_score` are auto-written to
    /// `output_dir`. When `false`, they are recorded for manual review.
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub auto_integrate: bool,
    /// Sources to scout. Supported: `github`, `clawhub`, `huggingface`.
    /// Default: `["github", "clawhub"]`.
    #[serde(default = "default_forge_sources")]
    pub sources: Vec<String>,
    /// Hours between scheduled scans (heartbeat-driven). Default: `24`.
    #[serde(default = "default_forge_scan_interval")]
    pub scan_interval_hours: u64,
    /// Minimum evaluator score (0.0..=1.0) for auto-integration. Default: `0.7`.
    #[serde(default = "default_forge_min_score")]
    pub min_score: f64,
    /// Optional GitHub personal-access token for higher API rate limits.
    #[serde(default)]
    pub github_token: Option<String>,
    /// Directory to write integrated SKILL.toml/SKILL.md bundles. Relative
    /// paths are resolved against workspace_dir. Default: `./skills`.
    #[serde(default = "default_forge_output_dir")]
    pub output_dir: String,
    /// Run `TEST.sh` in a sandbox after integration (P3-2). Skills that fail
    /// their tests are rolled back. Skills without `TEST.sh` pass with a
    /// warning. Default: `true`.
    #[serde(default = "default_true")]
    pub sandbox_verify: bool,
    /// Per-test timeout in seconds during sandbox verification. Default: `30`.
    #[serde(default = "default_forge_sandbox_timeout")]
    pub sandbox_timeout_secs: u64,
}

fn default_forge_sources() -> Vec<String> {
    vec!["github".into(), "clawhub".into()]
}
fn default_forge_scan_interval() -> u64 {
    24
}
fn default_forge_min_score() -> f64 {
    0.7
}
fn default_forge_output_dir() -> String {
    "./skills".into()
}
fn default_forge_sandbox_timeout() -> u64 {
    30
}

impl Default for SkillForgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_integrate: true,
            sources: default_forge_sources(),
            scan_interval_hours: default_forge_scan_interval(),
            min_score: default_forge_min_score(),
            github_token: None,
            output_dir: default_forge_output_dir(),
            sandbox_verify: true,
            sandbox_timeout_secs: default_forge_sandbox_timeout(),
        }
    }
}

/// Hand-written `Debug` redacts `github_token`. A derived `Debug` would
/// leak the PAT into any `{:?}`-formatted log, error, or panic message.
impl std::fmt::Debug for SkillForgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillForgeConfig")
            .field("enabled", &self.enabled)
            .field("auto_integrate", &self.auto_integrate)
            .field("sources", &self.sources)
            .field("scan_interval_hours", &self.scan_interval_hours)
            .field("min_score", &self.min_score)
            .field("github_token", &self.github_token.as_ref().map(|_| "***"))
            .field("output_dir", &self.output_dir)
            .field("sandbox_verify", &self.sandbox_verify)
            .field("sandbox_timeout_secs", &self.sandbox_timeout_secs)
            .finish()
    }
}

/// Skill confidence scoring + deprecation policy (`[skills.skill_confidence]`).
///
/// Mirrors the runtime-side `zeroclaw_runtime::skills::confidence::ConfidencePolicy`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "skills.skill-confidence"]
pub struct SkillConfidenceConfig {
    /// Enable periodic confidence evaluation and auto-deprecation of flaky
    /// skills. When `false`, traces are still recorded but no skills are
    /// ever marked deprecated. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Hours between confidence scans (heartbeat-driven). Default: `6`.
    #[serde(default = "default_confidence_interval")]
    pub scan_interval_hours: u64,
    /// Call count at which the `usage_frequency` component saturates at 1.0.
    /// Skills used fewer times than this are penalized. Default: `20`.
    #[serde(default = "default_confidence_saturation")]
    pub saturation_calls: u32,
    /// Half-life for recency decay, in hours. Default: `720` (30 days).
    #[serde(default = "default_confidence_half_life")]
    pub recency_half_life_hours: f64,
    /// Minimum trace count before deprecation is considered. A transient
    /// failure spike (e.g. provider outage for 5 minutes) can easily cause
    /// 5 failures, which is why the default was raised from 5 to 15 —
    /// permanent removal should require more evidence than short-term
    /// noise. Default: `15`.
    #[serde(default = "default_confidence_min_samples")]
    pub min_samples_for_deprecation: usize,
    /// Confidence score at/below which a skill is auto-deprecated. Default: `0.3`.
    #[serde(default = "default_confidence_threshold")]
    pub deprecation_threshold: f64,
    /// Hours after deprecation before a skill is reconsidered. Set to `0`
    /// to disable automatic reinstatement. When > 0, the heartbeat scan
    /// re-scores deprecated skills and may clear the `DEPRECATED` marker
    /// if recent traces show recovery. Default: `168` (7 days).
    #[serde(default = "default_confidence_review_window")]
    pub review_window_hours: u64,
    /// Confidence score *above* which a deprecated skill is re-enabled.
    /// Must be strictly greater than `deprecation_threshold` — the gap is
    /// hysteresis that prevents flapping. Default: `0.5`.
    #[serde(default = "default_confidence_reinstate_threshold")]
    pub reinstate_threshold: f64,
}

fn default_confidence_interval() -> u64 {
    6
}
fn default_confidence_saturation() -> u32 {
    20
}
fn default_confidence_half_life() -> f64 {
    24.0 * 30.0
}
fn default_confidence_min_samples() -> usize {
    15
}
fn default_confidence_threshold() -> f64 {
    0.3
}
fn default_confidence_review_window() -> u64 {
    24 * 7
}
fn default_confidence_reinstate_threshold() -> f64 {
    0.5
}

impl Default for SkillConfidenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_hours: default_confidence_interval(),
            saturation_calls: default_confidence_saturation(),
            recency_half_life_hours: default_confidence_half_life(),
            min_samples_for_deprecation: default_confidence_min_samples(),
            deprecation_threshold: default_confidence_threshold(),
            review_window_hours: default_confidence_review_window(),
            reinstate_threshold: default_confidence_reinstate_threshold(),
        }
    }
}

/// Pipeline tool configuration (`[pipeline]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "pipeline"]
pub struct PipelineConfig {
    /// Enable the `execute_pipeline` meta-tool.
    /// Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of steps allowed in a single pipeline invocation.
    /// Default: `20`.
    #[serde(default = "default_pipeline_max_steps")]
    pub max_steps: usize,
    /// Tools allowed in pipeline steps. Steps referencing tools not on this
    /// list are rejected before execution.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

fn default_pipeline_max_steps() -> usize {
    20
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_steps: 20,
            allowed_tools: Vec::new(),
        }
    }
}

/// Multimodal (image) handling configuration (`[multimodal]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "multimodal"]
pub struct MultimodalConfig {
    /// Maximum number of image attachments accepted per request.
    #[serde(default = "default_multimodal_max_images")]
    pub max_images: usize,
    /// Maximum image payload size in MiB before base64 encoding.
    #[serde(default = "default_multimodal_max_image_size_mb")]
    pub max_image_size_mb: usize,
    /// Allow fetching remote image URLs (http/https). Disabled by default.
    #[serde(default)]
    pub allow_remote_fetch: bool,
    /// Provider name to use for vision/image messages (e.g. `"ollama"`).
    /// When set, messages containing `[IMAGE:]` markers are routed to this
    /// provider instead of the default text provider.
    #[serde(default)]
    pub vision_provider: Option<String>,
    /// Model to use when routing to the vision provider (e.g. `"llava:7b"`).
    /// Only used when `vision_provider` is set.
    #[serde(default)]
    pub vision_model: Option<String>,
}

fn default_multimodal_max_images() -> usize {
    4
}

fn default_multimodal_max_image_size_mb() -> usize {
    5
}

impl MultimodalConfig {
    /// Clamp configured values to safe runtime bounds.
    pub fn effective_limits(&self) -> (usize, usize) {
        let max_images = self.max_images.clamp(1, 16);
        let max_image_size_mb = self.max_image_size_mb.clamp(1, 20);
        (max_images, max_image_size_mb)
    }
}

impl Default for MultimodalConfig {
    fn default() -> Self {
        Self {
            max_images: default_multimodal_max_images(),
            max_image_size_mb: default_multimodal_max_image_size_mb(),
            allow_remote_fetch: false,
            vision_provider: None,
            vision_model: None,
        }
    }
}

// ── Media Pipeline ──────────────────────────────────────────────

/// Automatic media understanding pipeline configuration (`[media_pipeline]`).
///
/// When enabled, inbound channel messages with media attachments are
/// pre-processed before reaching the agent: audio is transcribed, images are
/// annotated, and videos are summarised.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "media-pipeline"]
pub struct MediaPipelineConfig {
    /// Master toggle for the media pipeline (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Transcribe audio attachments using the configured transcription provider.
    #[serde(default = "default_true")]
    pub transcribe_audio: bool,

    /// Add image descriptions when a vision-capable model is active.
    #[serde(default = "default_true")]
    pub describe_images: bool,

    /// Summarize video attachments (placeholder — requires external API).
    #[serde(default = "default_true")]
    pub summarize_video: bool,
}

impl Default for MediaPipelineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            transcribe_audio: true,
            describe_images: true,
            summarize_video: true,
        }
    }
}

// ── Identity (AIEOS / OpenClaw format) ──────────────────────────

/// Identity format configuration (`[identity]` section).
///
/// Supports `"openclaw"` (default) or `"aieos"` identity documents.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "identity"]
pub struct IdentityConfig {
    /// Identity format: "openclaw" (default) or "aieos"
    #[serde(default = "default_identity_format")]
    pub format: String,
    /// Path to AIEOS JSON file (relative to workspace)
    #[serde(default)]
    pub aieos_path: Option<String>,
    /// Inline AIEOS JSON (alternative to file path)
    #[serde(default)]
    pub aieos_inline: Option<String>,
}

fn default_identity_format() -> String {
    "openclaw".into()
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            format: default_identity_format(),
            aieos_path: None,
            aieos_inline: None,
        }
    }
}

// ── Cost tracking and budget enforcement ───────────────────────────

/// Cost tracking and budget enforcement configuration (`[cost]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "cost"]
pub struct CostConfig {
    /// Enable cost tracking (default: true)
    #[serde(default = "default_cost_enabled")]
    pub enabled: bool,

    /// Daily spending limit in USD (default: 10.00)
    #[serde(default = "default_daily_limit")]
    pub daily_limit_usd: f64,

    /// Monthly spending limit in USD (default: 100.00)
    #[serde(default = "default_monthly_limit")]
    pub monthly_limit_usd: f64,

    /// Warn when spending reaches this percentage of limit (default: 80)
    #[serde(default = "default_warn_percent")]
    pub warn_at_percent: u8,

    /// Allow requests to exceed budget with --override flag (default: false)
    #[serde(default)]
    pub allow_override: bool,

    /// Per-model pricing (USD per 1M tokens)
    #[serde(default)]
    pub prices: std::collections::HashMap<String, ModelPricing>,

    /// Cost enforcement behavior when budget limits are approached or exceeded.
    #[serde(default)]
    #[nested]
    pub enforcement: CostEnforcementConfig,
}

/// Configuration for cost enforcement behavior when budget limits are reached.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "cost.enforcement"]
pub struct CostEnforcementConfig {
    /// Enforcement mode: "warn", "block", or "route_down".
    #[serde(default = "default_cost_enforcement_mode")]
    pub mode: String,
    /// Model hint to route to when budget is exceeded (used with "route_down" mode).
    #[serde(default)]
    pub route_down_model: Option<String>,
    /// Reserve this percentage of budget for critical operations.
    #[serde(default = "default_reserve_percent")]
    pub reserve_percent: u8,
}

fn default_cost_enforcement_mode() -> String {
    "warn".to_string()
}

fn default_reserve_percent() -> u8 {
    10
}

impl Default for CostEnforcementConfig {
    fn default() -> Self {
        Self {
            mode: default_cost_enforcement_mode(),
            route_down_model: None,
            reserve_percent: default_reserve_percent(),
        }
    }
}

/// Per-model pricing entry (USD per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelPricing {
    /// Input price per 1M tokens
    #[serde(default)]
    pub input: f64,

    /// Output price per 1M tokens
    #[serde(default)]
    pub output: f64,
}

fn default_daily_limit() -> f64 {
    10.0
}

fn default_monthly_limit() -> f64 {
    100.0
}

fn default_warn_percent() -> u8 {
    80
}

fn default_cost_enabled() -> bool {
    true
}

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            daily_limit_usd: default_daily_limit(),
            monthly_limit_usd: default_monthly_limit(),
            warn_at_percent: default_warn_percent(),
            allow_override: false,
            prices: get_default_pricing(),
            enforcement: CostEnforcementConfig::default(),
        }
    }
}

/// Default pricing for popular models (USD per 1M tokens)
fn get_default_pricing() -> std::collections::HashMap<String, ModelPricing> {
    let mut prices = std::collections::HashMap::new();

    // Anthropic models
    prices.insert(
        "anthropic/claude-sonnet-4-20250514".into(),
        ModelPricing {
            input: 3.0,
            output: 15.0,
        },
    );
    prices.insert(
        "anthropic/claude-opus-4-20250514".into(),
        ModelPricing {
            input: 15.0,
            output: 75.0,
        },
    );
    prices.insert(
        "anthropic/claude-3.5-sonnet".into(),
        ModelPricing {
            input: 3.0,
            output: 15.0,
        },
    );
    prices.insert(
        "anthropic/claude-3-haiku".into(),
        ModelPricing {
            input: 0.25,
            output: 1.25,
        },
    );

    // OpenAI models
    prices.insert(
        "openai/gpt-4o".into(),
        ModelPricing {
            input: 5.0,
            output: 15.0,
        },
    );
    prices.insert(
        "openai/gpt-4o-mini".into(),
        ModelPricing {
            input: 0.15,
            output: 0.60,
        },
    );
    prices.insert(
        "openai/o1-preview".into(),
        ModelPricing {
            input: 15.0,
            output: 60.0,
        },
    );

    // Google models
    prices.insert(
        "google/gemini-2.0-flash".into(),
        ModelPricing {
            input: 0.10,
            output: 0.40,
        },
    );
    prices.insert(
        "google/gemini-1.5-pro".into(),
        ModelPricing {
            input: 1.25,
            output: 5.0,
        },
    );

    prices
}

// ── Peripherals (hardware: STM32, RPi GPIO, etc.) ────────────────────────

/// Peripheral board integration configuration (`[peripherals]` section).
///
/// Boards become agent tools when enabled.
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "peripherals"]
pub struct PeripheralsConfig {
    /// Enable peripheral support (boards become agent tools)
    #[serde(default)]
    pub enabled: bool,
    /// Board configurations (nucleo-f401re, rpi-gpio, etc.)
    #[serde(default)]
    pub boards: Vec<PeripheralBoardConfig>,
    /// Path to datasheet docs (relative to workspace) for RAG retrieval.
    /// Place .md/.txt files named by board (e.g. nucleo-f401re.md, rpi-gpio.md).
    #[serde(default)]
    pub datasheet_dir: Option<String>,
}

/// Configuration for a single peripheral board (e.g. STM32, RPi GPIO).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PeripheralBoardConfig {
    /// Board type: "nucleo-f401re", "rpi-gpio", "esp32", etc.
    pub board: String,
    /// Transport: "serial", "native", "websocket"
    #[serde(default = "default_peripheral_transport")]
    pub transport: String,
    /// Path for serial: "/dev/ttyACM0", "/dev/ttyUSB0"
    #[serde(default)]
    pub path: Option<String>,
    /// Baud rate for serial (default: 115200)
    #[serde(default = "default_peripheral_baud")]
    pub baud: u32,
}

fn default_peripheral_transport() -> String {
    "serial".into()
}

fn default_peripheral_baud() -> u32 {
    115_200
}

impl Default for PeripheralBoardConfig {
    fn default() -> Self {
        Self {
            board: String::new(),
            transport: default_peripheral_transport(),
            path: None,
            baud: default_peripheral_baud(),
        }
    }
}

// ── Gateway security ─────────────────────────────────────────────

/// Gateway server configuration (`[gateway]` section).
///
/// Controls the HTTP gateway for webhook and pairing endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gateway"]
#[allow(clippy::struct_excessive_bools)]
pub struct GatewayConfig {
    /// Gateway port (default: 42617)
    #[serde(default = "default_gateway_port")]
    pub port: u16,
    /// Gateway host (default: 127.0.0.1)
    #[serde(default = "default_gateway_host")]
    pub host: String,
    /// Require pairing before accepting requests (default: true)
    #[serde(default = "default_true")]
    pub require_pairing: bool,
    /// Allow binding to non-localhost without a tunnel (default: false)
    #[serde(default)]
    pub allow_public_bind: bool,
    /// Paired bearer tokens (managed automatically, not user-edited)
    #[serde(default)]
    #[secret]
    pub paired_tokens: Vec<String>,

    /// Max `/pair` requests per minute per client key.
    #[serde(default = "default_pair_rate_limit")]
    pub pair_rate_limit_per_minute: u32,

    /// Max `/webhook` requests per minute per client key.
    #[serde(default = "default_webhook_rate_limit")]
    pub webhook_rate_limit_per_minute: u32,

    /// Trust proxy-forwarded client IP headers (`X-Forwarded-For`, `X-Real-IP`).
    /// Disabled by default; enable only behind a trusted reverse proxy.
    #[serde(default)]
    pub trust_forwarded_headers: bool,

    /// Optional URL path prefix for reverse-proxy deployments.
    /// When set, all gateway routes are served under this prefix.
    /// Must start with `/` and must not end with `/`.
    #[serde(default)]
    pub path_prefix: Option<String>,

    /// Maximum distinct client keys tracked by gateway rate limiter maps.
    #[serde(default = "default_gateway_rate_limit_max_keys")]
    pub rate_limit_max_keys: usize,

    /// TTL for webhook idempotency keys.
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Maximum distinct idempotency keys retained in memory.
    #[serde(default = "default_gateway_idempotency_max_keys")]
    pub idempotency_max_keys: usize,

    /// Persist gateway WebSocket chat sessions to SQLite. Default: true.
    #[serde(default = "default_true")]
    pub session_persistence: bool,

    /// Auto-archive stale gateway sessions older than N hours. 0 = disabled. Default: 0.
    #[serde(default)]
    pub session_ttl_hours: u32,

    /// Pairing dashboard configuration
    #[serde(default)]
    #[nested]
    pub pairing_dashboard: PairingDashboardConfig,

    /// Path to the web dashboard `dist` directory.  When set, the gateway
    /// serves the compiled frontend from the filesystem instead of requiring
    /// it to be embedded in the binary.  Accepts absolute paths or paths
    /// relative to the working directory.  When omitted the gateway runs in
    /// API-only mode (no web dashboard) unless auto-detection finds it.
    #[serde(default)]
    pub web_dist_dir: Option<String>,

    /// TLS configuration for the gateway server (`[gateway.tls]`).
    #[serde(default)]
    #[nested]
    pub tls: Option<GatewayTlsConfig>,
}

fn default_gateway_port() -> u16 {
    42617
}

fn default_gateway_host() -> String {
    "127.0.0.1".into()
}

fn default_pair_rate_limit() -> u32 {
    10
}

fn default_webhook_rate_limit() -> u32 {
    60
}

fn default_idempotency_ttl_secs() -> u64 {
    300
}

fn default_gateway_rate_limit_max_keys() -> usize {
    10_000
}

fn default_gateway_idempotency_max_keys() -> usize {
    10_000
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_gateway_port(),
            host: default_gateway_host(),
            require_pairing: true,
            allow_public_bind: false,
            paired_tokens: Vec::new(),
            pair_rate_limit_per_minute: default_pair_rate_limit(),
            webhook_rate_limit_per_minute: default_webhook_rate_limit(),
            trust_forwarded_headers: false,
            path_prefix: None,
            rate_limit_max_keys: default_gateway_rate_limit_max_keys(),
            idempotency_ttl_secs: default_idempotency_ttl_secs(),
            idempotency_max_keys: default_gateway_idempotency_max_keys(),
            session_persistence: true,
            session_ttl_hours: 0,
            pairing_dashboard: PairingDashboardConfig::default(),
            web_dist_dir: None,
            tls: None,
        }
    }
}

/// Pairing dashboard configuration (`[gateway.pairing_dashboard]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gateway.pairing-dashboard"]
pub struct PairingDashboardConfig {
    /// Length of pairing codes (default: 8)
    #[serde(default = "default_pairing_code_length")]
    pub code_length: usize,
    /// Time-to-live for pending pairing codes in seconds (default: 3600)
    #[serde(default = "default_pairing_ttl")]
    pub code_ttl_secs: u64,
    /// Maximum concurrent pending pairing codes (default: 3)
    #[serde(default = "default_max_pending_codes")]
    pub max_pending_codes: usize,
    /// Maximum failed pairing attempts before lockout (default: 5)
    #[serde(default = "default_max_failed_attempts")]
    pub max_failed_attempts: u32,
    /// Lockout duration in seconds after max attempts (default: 300)
    #[serde(default = "default_pairing_lockout_secs")]
    pub lockout_secs: u64,
}

fn default_pairing_code_length() -> usize {
    8
}
fn default_pairing_ttl() -> u64 {
    3600
}
fn default_max_pending_codes() -> usize {
    3
}
fn default_max_failed_attempts() -> u32 {
    5
}
fn default_pairing_lockout_secs() -> u64 {
    300
}

impl Default for PairingDashboardConfig {
    fn default() -> Self {
        Self {
            code_length: default_pairing_code_length(),
            code_ttl_secs: default_pairing_ttl(),
            max_pending_codes: default_max_pending_codes(),
            max_failed_attempts: default_max_failed_attempts(),
            lockout_secs: default_pairing_lockout_secs(),
        }
    }
}

/// TLS configuration for the gateway server (`[gateway.tls]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gateway.tls"]
pub struct GatewayTlsConfig {
    /// Enable TLS for the gateway (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded server certificate file.
    pub cert_path: String,
    /// Path to the PEM-encoded server private key file.
    pub key_path: String,
    /// Client certificate authentication (mutual TLS) settings.
    #[serde(default)]
    #[nested]
    pub client_auth: Option<GatewayClientAuthConfig>,
}

/// Client certificate authentication (mTLS) configuration (`[gateway.tls.client_auth]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gateway.tls.client-auth"]
pub struct GatewayClientAuthConfig {
    /// Enable client certificate verification (default: false).
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded CA certificate used to verify client certs.
    #[serde(default)]
    pub ca_cert_path: String,
    /// Reject connections that do not present a valid client certificate (default: true).
    #[serde(default = "default_true")]
    pub require_client_cert: bool,
    /// Optional SHA-256 fingerprints for certificate pinning.
    /// When non-empty, only client certs matching one of these fingerprints are accepted.
    #[serde(default)]
    pub pinned_certs: Vec<String>,
}

impl Default for GatewayClientAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ca_cert_path: String::new(),
            require_client_cert: default_true(),
            pinned_certs: Vec::new(),
        }
    }
}

/// Secure transport configuration for inter-node communication (`[node_transport]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "node-transport"]
pub struct NodeTransportConfig {
    /// Enable the secure transport layer.
    #[serde(default = "default_node_transport_enabled")]
    pub enabled: bool,
    /// Shared secret for HMAC authentication between nodes.
    #[serde(default)]
    pub shared_secret: String,
    /// Maximum age of signed requests in seconds (replay protection).
    #[serde(default = "default_max_request_age")]
    pub max_request_age_secs: i64,
    /// Require HTTPS for all node communication.
    #[serde(default = "default_require_https")]
    pub require_https: bool,
    /// Allow specific node IPs/CIDRs.
    #[serde(default)]
    pub allowed_peers: Vec<String>,
    /// Path to TLS certificate file.
    #[serde(default)]
    pub tls_cert_path: Option<String>,
    /// Path to TLS private key file.
    #[serde(default)]
    pub tls_key_path: Option<String>,
    /// Require client certificates (mutual TLS).
    #[serde(default)]
    pub mutual_tls: bool,
    /// Maximum number of connections per peer.
    #[serde(default = "default_connection_pool_size")]
    pub connection_pool_size: usize,
}

fn default_node_transport_enabled() -> bool {
    true
}
fn default_max_request_age() -> i64 {
    300
}
fn default_require_https() -> bool {
    true
}
fn default_connection_pool_size() -> usize {
    4
}

impl Default for NodeTransportConfig {
    fn default() -> Self {
        Self {
            enabled: default_node_transport_enabled(),
            shared_secret: String::new(),
            max_request_age_secs: default_max_request_age(),
            require_https: default_require_https(),
            allowed_peers: Vec::new(),
            tls_cert_path: None,
            tls_key_path: None,
            mutual_tls: false,
            connection_pool_size: default_connection_pool_size(),
        }
    }
}

// ── Composio (managed tool surface) ─────────────────────────────

/// Composio managed OAuth tools integration (`[composio]` section).
///
/// Provides access to 1000+ OAuth-connected tools via the Composio platform.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "composio"]
pub struct ComposioConfig {
    /// Enable Composio integration for 1000+ OAuth tools
    #[serde(default, alias = "enable")]
    pub enabled: bool,
    /// Composio API key (stored encrypted when secrets.encrypt = true)
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Default entity ID for multi-user setups
    #[serde(default = "default_entity_id")]
    pub entity_id: String,
}

fn default_entity_id() -> String {
    "default".into()
}

impl Default for ComposioConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            entity_id: default_entity_id(),
        }
    }
}

// ── Microsoft 365 (Graph API integration) ───────────────────────

/// Microsoft 365 integration via Microsoft Graph API (`[microsoft365]` section).
///
/// Provides access to Outlook mail, Teams messages, Calendar events,
/// OneDrive files, and SharePoint search.
#[derive(Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "ms365"]
pub struct Microsoft365Config {
    /// Enable Microsoft 365 integration
    #[serde(default, alias = "enable")]
    pub enabled: bool,
    /// Azure AD tenant ID
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Azure AD application (client) ID
    #[serde(default)]
    pub client_id: Option<String>,
    /// Azure AD client secret (stored encrypted when secrets.encrypt = true)
    #[serde(default)]
    #[secret]
    pub client_secret: Option<String>,
    /// Authentication flow: "client_credentials" or "device_code"
    #[serde(default = "default_ms365_auth_flow")]
    pub auth_flow: String,
    /// OAuth scopes to request
    #[serde(default = "default_ms365_scopes")]
    pub scopes: Vec<String>,
    /// Encrypt the token cache file on disk
    #[serde(default = "default_true")]
    pub token_cache_encrypted: bool,
    /// User principal name or "me" (for delegated flows)
    #[serde(default)]
    pub user_id: Option<String>,
}

fn default_ms365_auth_flow() -> String {
    "client_credentials".to_string()
}

fn default_ms365_scopes() -> Vec<String> {
    vec!["https://graph.microsoft.com/.default".to_string()]
}

impl std::fmt::Debug for Microsoft365Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Microsoft365Config")
            .field("enabled", &self.enabled)
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "***"))
            .field("auth_flow", &self.auth_flow)
            .field("scopes", &self.scopes)
            .field("token_cache_encrypted", &self.token_cache_encrypted)
            .field("user_id", &self.user_id)
            .finish()
    }
}

impl Default for Microsoft365Config {
    fn default() -> Self {
        Self {
            enabled: false,
            tenant_id: None,
            client_id: None,
            client_secret: None,
            auth_flow: default_ms365_auth_flow(),
            scopes: default_ms365_scopes(),
            token_cache_encrypted: true,
            user_id: None,
        }
    }
}

// ── Secrets (encrypted credential store) ────────────────────────

/// Secrets encryption configuration (`[secrets]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "secrets"]
pub struct SecretsConfig {
    /// Enable encryption for API keys and tokens in config.toml
    #[serde(default = "default_true")]
    pub encrypt: bool,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self { encrypt: true }
    }
}

// ── Browser (friendly-service browsing only) ───────────────────

/// Computer-use sidecar configuration (`[browser.computer_use]` section).
///
/// Delegates OS-level mouse, keyboard, and screenshot actions to a local sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "browser.computer-use"]
pub struct BrowserComputerUseConfig {
    /// Sidecar endpoint for computer-use actions (OS-level mouse/keyboard/screenshot)
    #[serde(default = "default_browser_computer_use_endpoint")]
    pub endpoint: String,
    /// Optional bearer token for computer-use sidecar
    #[serde(default)]
    #[secret]
    pub api_key: Option<String>,
    /// Per-action request timeout in milliseconds
    #[serde(default = "default_browser_computer_use_timeout_ms")]
    pub timeout_ms: u64,
    /// Allow remote/public endpoint for computer-use sidecar (default: false)
    #[serde(default)]
    pub allow_remote_endpoint: bool,
    /// Optional window title/process allowlist forwarded to sidecar policy
    #[serde(default)]
    pub window_allowlist: Vec<String>,
    /// Optional X-axis boundary for coordinate-based actions
    #[serde(default)]
    pub max_coordinate_x: Option<i64>,
    /// Optional Y-axis boundary for coordinate-based actions
    #[serde(default)]
    pub max_coordinate_y: Option<i64>,
}

fn default_browser_computer_use_endpoint() -> String {
    "http://127.0.0.1:8787/v1/actions".into()
}

fn default_browser_computer_use_timeout_ms() -> u64 {
    15_000
}

impl Default for BrowserComputerUseConfig {
    fn default() -> Self {
        Self {
            endpoint: default_browser_computer_use_endpoint(),
            api_key: None,
            timeout_ms: default_browser_computer_use_timeout_ms(),
            allow_remote_endpoint: false,
            window_allowlist: Vec::new(),
            max_coordinate_x: None,
            max_coordinate_y: None,
        }
    }
}

/// Browser automation configuration (`[browser]` section).
///
/// Controls the `browser_open` tool and browser automation backends.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "browser"]
pub struct BrowserConfig {
    /// Enable `browser_open` tool (opens URLs in the system browser without scraping)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Allowed domains for `browser_open` (exact or subdomain match)
    #[serde(default = "default_browser_allowed_domains")]
    pub allowed_domains: Vec<String>,
    /// Browser session name (for agent-browser automation)
    #[serde(default)]
    pub session_name: Option<String>,
    /// Browser automation backend: "agent_browser" | "rust_native" | "computer_use" | "auto"
    #[serde(default = "default_browser_backend")]
    pub backend: String,
    /// Headless mode for rust-native backend
    #[serde(default = "default_true")]
    pub native_headless: bool,
    /// WebDriver endpoint URL for rust-native backend (e.g. http://127.0.0.1:9515)
    #[serde(default = "default_browser_webdriver_url")]
    pub native_webdriver_url: String,
    /// Optional Chrome/Chromium executable path for rust-native backend
    #[serde(default)]
    pub native_chrome_path: Option<String>,
    /// Computer-use sidecar configuration
    #[serde(default)]
    #[nested]
    pub computer_use: BrowserComputerUseConfig,
}

fn default_browser_allowed_domains() -> Vec<String> {
    vec!["*".into()]
}

fn default_browser_backend() -> String {
    "agent_browser".into()
}

fn default_browser_webdriver_url() -> String {
    "http://127.0.0.1:9515".into()
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_domains: vec!["*".into()],
            session_name: None,
            backend: default_browser_backend(),
            native_headless: default_true(),
            native_webdriver_url: default_browser_webdriver_url(),
            native_chrome_path: None,
            computer_use: BrowserComputerUseConfig::default(),
        }
    }
}

// ── HTTP request tool ───────────────────────────────────────────

/// HTTP request tool configuration (`[http_request]` section).
///
/// Domain filtering: `allowed_domains` controls which hosts are reachable (use `["*"]`
/// for all public hosts, which is the default). If `allowed_domains` is empty, all
/// requests are rejected.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "http-request"]
pub struct HttpRequestConfig {
    /// Enable `http_request` tool for API interactions
    #[serde(default)]
    pub enabled: bool,
    /// Allowed domains for HTTP requests (exact or subdomain match)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Maximum response size in bytes (default: 1MB, 0 = unlimited)
    #[serde(default = "default_http_max_response_size")]
    pub max_response_size: usize,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_http_timeout_secs")]
    pub timeout_secs: u64,
    /// Allow requests to private/LAN hosts (RFC 1918, loopback, link-local, .local).
    /// Default: false (deny private hosts for SSRF protection).
    #[serde(default)]
    pub allow_private_hosts: bool,
}

impl Default for HttpRequestConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_domains: vec!["*".into()],
            max_response_size: default_http_max_response_size(),
            timeout_secs: default_http_timeout_secs(),
            allow_private_hosts: false,
        }
    }
}

fn default_http_max_response_size() -> usize {
    1_000_000 // 1MB
}

fn default_http_timeout_secs() -> u64 {
    30
}

// ── Web fetch ────────────────────────────────────────────────────

/// Web fetch tool configuration (`[web_fetch]` section).
///
/// Fetches web pages and converts HTML to plain text for LLM consumption.
/// Domain filtering: `allowed_domains` controls which hosts are reachable (use `["*"]`
/// for all public hosts). `blocked_domains` takes priority over `allowed_domains`.
/// If `allowed_domains` is empty, all requests are rejected (deny-by-default).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "web-fetch"]
pub struct WebFetchConfig {
    /// Enable `web_fetch` tool for fetching web page content
    #[serde(default)]
    pub enabled: bool,
    /// Allowed domains for web fetch (exact or subdomain match; `["*"]` = all public hosts)
    #[serde(default = "default_web_fetch_allowed_domains")]
    pub allowed_domains: Vec<String>,
    /// Blocked domains (exact or subdomain match; always takes priority over allowed_domains)
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Private/internal hosts allowed to bypass SSRF protection (e.g. `["192.168.1.10", "internal.local"]`)
    #[serde(default)]
    pub allowed_private_hosts: Vec<String>,
    /// Maximum response size in bytes (default: 500KB, plain text is much smaller than raw HTML)
    #[serde(default = "default_web_fetch_max_response_size")]
    pub max_response_size: usize,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_web_fetch_timeout_secs")]
    pub timeout_secs: u64,
    /// Firecrawl fallback configuration (`[web_fetch.firecrawl]`)
    #[serde(default)]
    #[nested]
    pub firecrawl: FirecrawlConfig,
}

/// Firecrawl fallback mode: scrape a single page or crawl linked pages.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum FirecrawlMode {
    #[default]
    Scrape,
    /// Reserved for future multi-page crawl support. Accepted in config
    /// deserialization to avoid breaking existing files, but not yet
    /// implemented — `fetch_via_firecrawl` always uses the `/scrape` endpoint.
    Crawl,
}

/// Firecrawl fallback configuration for JS-heavy and bot-blocked sites.
///
/// When enabled, if the standard web fetch fails (HTTP error, empty body, or
/// body shorter than 100 characters suggesting a JS-only page), the tool
/// falls back to the Firecrawl API for stealth content extraction.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "web-fetch.firecrawl"]
pub struct FirecrawlConfig {
    /// Enable Firecrawl fallback
    #[serde(default)]
    pub enabled: bool,
    /// Environment variable name for the Firecrawl API key
    #[serde(default = "default_firecrawl_api_key_env")]
    pub api_key_env: String,
    /// Firecrawl API base URL
    #[serde(default = "default_firecrawl_api_url")]
    pub api_url: String,
    /// Firecrawl extraction mode
    #[serde(default)]
    pub mode: FirecrawlMode,
}

fn default_firecrawl_api_key_env() -> String {
    "FIRECRAWL_API_KEY".into()
}

fn default_firecrawl_api_url() -> String {
    "https://api.firecrawl.dev/v1".into()
}

impl Default for FirecrawlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key_env: default_firecrawl_api_key_env(),
            api_url: default_firecrawl_api_url(),
            mode: FirecrawlMode::default(),
        }
    }
}

fn default_web_fetch_max_response_size() -> usize {
    500_000 // 500KB
}

fn default_web_fetch_timeout_secs() -> u64 {
    30
}

fn default_web_fetch_allowed_domains() -> Vec<String> {
    vec!["*".into()]
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_domains: vec!["*".into()],
            blocked_domains: vec![],
            allowed_private_hosts: vec![],
            max_response_size: default_web_fetch_max_response_size(),
            timeout_secs: default_web_fetch_timeout_secs(),
            firecrawl: FirecrawlConfig::default(),
        }
    }
}

// ── Link enricher ─────────────────────────────────────────────────

/// Automatic link understanding for inbound channel messages (`[link_enricher]`).
///
/// When enabled, URLs in incoming messages are automatically fetched and
/// summarised. The summary is prepended to the message before the agent
/// processes it, giving the LLM context about linked pages without an
/// explicit tool call.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "link-enricher"]
pub struct LinkEnricherConfig {
    /// Enable the link enricher pipeline stage (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of links to fetch per message (default: 3)
    #[serde(default = "default_link_enricher_max_links")]
    pub max_links: usize,
    /// Per-link fetch timeout in seconds (default: 10)
    #[serde(default = "default_link_enricher_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_link_enricher_max_links() -> usize {
    3
}

fn default_link_enricher_timeout_secs() -> u64 {
    10
}

impl Default for LinkEnricherConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_links: default_link_enricher_max_links(),
            timeout_secs: default_link_enricher_timeout_secs(),
        }
    }
}

// ── Text browser ─────────────────────────────────────────────────

/// Text browser tool configuration (`[text_browser]` section).
///
/// Uses text-based browsers (lynx, links, w3m) to render web pages as plain
/// text. Designed for headless/SSH environments without graphical browsers.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "text-browser"]
pub struct TextBrowserConfig {
    /// Enable `text_browser` tool
    #[serde(default)]
    pub enabled: bool,
    /// Preferred text browser ("lynx", "links", or "w3m"). If unset, auto-detects.
    #[serde(default)]
    pub preferred_browser: Option<String>,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_text_browser_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_text_browser_timeout_secs() -> u64 {
    30
}

impl Default for TextBrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preferred_browser: None,
            timeout_secs: default_text_browser_timeout_secs(),
        }
    }
}

// ── Shell tool ───────────────────────────────────────────────────

/// Shell tool configuration (`[shell_tool]` section).
///
/// Controls the behaviour of the `shell` execution tool. The main
/// tunable is `timeout_secs` — the maximum wall-clock time a single
/// shell command may run before it is killed.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "shell-tool"]
pub struct ShellToolConfig {
    /// Maximum shell command execution time in seconds (default: 60).
    #[serde(default = "default_shell_tool_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_shell_tool_timeout_secs() -> u64 {
    60
}

impl Default for ShellToolConfig {
    fn default() -> Self {
        Self {
            timeout_secs: default_shell_tool_timeout_secs(),
        }
    }
}

// ── Web search ───────────────────────────────────────────────────

/// Web search tool configuration (`[web_search]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "web-search"]
pub struct WebSearchConfig {
    /// Enable `web_search_tool` for web searches
    #[serde(default)]
    pub enabled: bool,
    /// Search provider: "duckduckgo" (free), "brave" (requires API key), or "searxng" (self-hosted)
    #[serde(default = "default_web_search_provider")]
    pub provider: String,
    /// Brave Search API key (required if provider is "brave")
    #[serde(default)]
    #[secret]
    pub brave_api_key: Option<String>,
    /// SearXNG instance URL (required if provider is "searxng"), e.g. "https://searx.example.com"
    #[serde(default)]
    pub searxng_instance_url: Option<String>,
    /// Maximum results per search (1-10)
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    /// Request timeout in seconds
    #[serde(default = "default_web_search_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_web_search_provider() -> String {
    "duckduckgo".into()
}

fn default_web_search_max_results() -> usize {
    5
}

fn default_web_search_timeout_secs() -> u64 {
    15
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: default_web_search_provider(),
            brave_api_key: None,
            searxng_instance_url: None,
            max_results: default_web_search_max_results(),
            timeout_secs: default_web_search_timeout_secs(),
        }
    }
}

// ── Project Intelligence ────────────────────────────────────────

/// Project delivery intelligence configuration (`[project_intel]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "project-intel"]
pub struct ProjectIntelConfig {
    /// Enable the project_intel tool. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Default report language (en, de, fr, it). Default: "en".
    #[serde(default = "default_project_intel_language")]
    pub default_language: String,
    /// Output directory for generated reports.
    #[serde(default = "default_project_intel_report_dir")]
    pub report_output_dir: String,
    /// Optional custom templates directory.
    #[serde(default)]
    pub templates_dir: Option<String>,
    /// Risk detection sensitivity: low, medium, high. Default: "medium".
    #[serde(default = "default_project_intel_risk_sensitivity")]
    pub risk_sensitivity: String,
    /// Include git log data in reports. Default: true.
    #[serde(default = "default_true")]
    pub include_git_data: bool,
    /// Include Jira data in reports. Default: false.
    #[serde(default)]
    pub include_jira_data: bool,
    /// Jira instance base URL (required if include_jira_data is true).
    #[serde(default)]
    pub jira_base_url: Option<String>,
}

fn default_project_intel_language() -> String {
    "en".into()
}

fn default_project_intel_report_dir() -> String {
    "~/.zeroclaw/project-reports".into()
}

fn default_project_intel_risk_sensitivity() -> String {
    "medium".into()
}

impl Default for ProjectIntelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_language: default_project_intel_language(),
            report_output_dir: default_project_intel_report_dir(),
            templates_dir: None,
            risk_sensitivity: default_project_intel_risk_sensitivity(),
            include_git_data: true,
            include_jira_data: false,
            jira_base_url: None,
        }
    }
}

// ── Backup ──────────────────────────────────────────────────────

/// Backup tool configuration (`[backup]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "backup"]
pub struct BackupConfig {
    /// Enable the `backup` tool.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum number of backups to keep (oldest are pruned).
    #[serde(default = "default_backup_max_keep")]
    pub max_keep: usize,
    /// Workspace subdirectories to include in backups.
    #[serde(default = "default_backup_include_dirs")]
    pub include_dirs: Vec<String>,
    /// Output directory for backup archives (relative to workspace root).
    #[serde(default = "default_backup_destination_dir")]
    pub destination_dir: String,
    /// Optional cron expression for scheduled automatic backups.
    #[serde(default)]
    pub schedule_cron: Option<String>,
    /// IANA timezone for `schedule_cron`.
    #[serde(default)]
    pub schedule_timezone: Option<String>,
    /// Compress backup archives.
    #[serde(default = "default_true")]
    pub compress: bool,
    /// Encrypt backup archives (requires a configured secret store key).
    #[serde(default)]
    pub encrypt: bool,
}

fn default_backup_max_keep() -> usize {
    10
}

fn default_backup_include_dirs() -> Vec<String> {
    vec![
        "config".into(),
        "memory".into(),
        "audit".into(),
        "knowledge".into(),
    ]
}

fn default_backup_destination_dir() -> String {
    "state/backups".into()
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_keep: default_backup_max_keep(),
            include_dirs: default_backup_include_dirs(),
            destination_dir: default_backup_destination_dir(),
            schedule_cron: None,
            schedule_timezone: None,
            compress: true,
            encrypt: false,
        }
    }
}

// ── Data Retention ──────────────────────────────────────────────

/// Data retention and purge configuration (`[data_retention]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "data-retention"]
pub struct DataRetentionConfig {
    /// Enable the `data_management` tool.
    #[serde(default)]
    pub enabled: bool,
    /// Days of data to retain before purge eligibility.
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
    /// Preview what would be deleted without actually removing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Limit retention enforcement to specific data categories (empty = all).
    #[serde(default)]
    pub categories: Vec<String>,
}

fn default_retention_days() -> u64 {
    90
}

impl Default for DataRetentionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: default_retention_days(),
            dry_run: false,
            categories: Vec::new(),
        }
    }
}

// ── Google Workspace ─────────────────────────────────────────────

/// Built-in default service allowlist for the `google_workspace` tool.
///
/// Applied when `allowed_services` is empty. Defined here (not in the tool layer)
/// so that config validation can cross-check `allowed_operations` entries against
/// the effective service set in all cases, including when the operator relies on
/// the default.
pub const DEFAULT_GWS_SERVICES: &[&str] = &[
    "drive",
    "sheets",
    "gmail",
    "calendar",
    "docs",
    "slides",
    "tasks",
    "people",
    "chat",
    "classroom",
    "forms",
    "keep",
    "meet",
    "events",
];

/// Google Workspace CLI (`gws`) tool configuration (`[google_workspace]` section).
///
/// ## Defaults
/// - `enabled`: `false` (tool is not registered unless explicitly opted-in).
/// - `allowed_services`: empty vector, which grants access to the full default
///   service set: `drive`, `sheets`, `gmail`, `calendar`, `docs`, `slides`,
///   `tasks`, `people`, `chat`, `classroom`, `forms`, `keep`, `meet`, `events`.
/// - `credentials_path`: `None` (uses default `gws` credential discovery).
/// - `default_account`: `None` (uses the `gws` active account).
/// - `rate_limit_per_minute`: `60`.
/// - `timeout_secs`: `30`.
/// - `audit_log`: `false`.
/// - `credentials_path`: `None` (uses default `gws` credential discovery).
/// - `default_account`: `None` (uses the `gws` active account).
/// - `rate_limit_per_minute`: `60`.
/// - `timeout_secs`: `30`.
/// - `audit_log`: `false`.
///
/// ## Compatibility
/// Configs that omit the `[google_workspace]` section entirely are treated as
/// `GoogleWorkspaceConfig::default()` (disabled, all defaults allowed). Adding
/// the section is purely opt-in and does not affect other config sections.
///
/// ## Rollback / Migration
/// To revert, remove the `[google_workspace]` section from the config file (or
/// set `enabled = false`). No data migration is required; the tool simply stops
/// being registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GoogleWorkspaceAllowedOperation {
    /// Google Workspace service ID (for example `gmail` or `drive`).
    pub service: String,
    /// Top-level resource name for the service (for example `users` for Gmail or `files` for Drive).
    pub resource: String,
    /// Optional sub-resource for 4-segment gws commands
    /// (for example `messages` or `drafts` under `gmail users`).
    /// When present, the entry only matches calls that include this exact sub_resource.
    /// When absent, the entry only matches calls with no sub_resource.
    #[serde(default)]
    pub sub_resource: Option<String>,
    /// Allowed methods for the service/resource/sub_resource combination.
    #[serde(default)]
    pub methods: Vec<String>,
}

/// Google Workspace CLI (`gws`) tool configuration (`[google_workspace]` section).
///
/// ## Defaults
/// - `enabled`: `false` (tool is not registered unless explicitly opted-in).
/// - `allowed_services`: empty vector, which grants access to the full default
///   service set: `drive`, `sheets`, `gmail`, `calendar`, `docs`, `slides`,
///   `tasks`, `people`, `chat`, `classroom`, `forms`, `keep`, `meet`, `events`.
/// - `allowed_operations`: empty vector, which preserves the legacy behavior of
///   allowing any resource/method under the allowed service set.
/// - `credentials_path`: `None` (uses default `gws` credential discovery).
/// - `default_account`: `None` (uses the `gws` active account).
/// - `rate_limit_per_minute`: `60`.
/// - `timeout_secs`: `30`.
/// - `audit_log`: `false`.
///
/// ## Compatibility
/// Configs that omit the `[google_workspace]` section entirely are treated as
/// `GoogleWorkspaceConfig::default()` (disabled, all defaults allowed). Adding
/// the section is purely opt-in and does not affect other config sections.
///
/// ## Rollback / Migration
/// To revert, remove the `[google_workspace]` section from the config file (or
/// set `enabled = false`). No data migration is required; the tool simply stops
/// being registered.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "google-workspace"]
pub struct GoogleWorkspaceConfig {
    /// Enable the `google_workspace` tool. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Restrict which Google Workspace services the agent can access.
    ///
    /// When empty (the default), the full default service set is allowed (see
    /// struct-level docs). When non-empty, only the listed service IDs are
    /// permitted. Each entry must be non-empty, lowercase alphanumeric with
    /// optional underscores/hyphens, and unique.
    #[serde(default)]
    pub allowed_services: Vec<String>,
    /// Restrict which resource/method combinations the agent can access.
    ///
    /// When empty (the default), all methods under `allowed_services` remain
    /// available for backward compatibility. When non-empty, the runtime denies
    /// any `(service, resource, sub_resource, method)` combination that is not
    /// explicitly listed. `sub_resource` is optional per entry: an entry without
    /// it matches only 3-segment `gws` calls; an entry with it matches only calls
    /// that supply that exact sub_resource value.
    ///
    /// Each entry's `service` must appear in `allowed_services` when that list is
    /// non-empty; config validation rejects entries that would never match at
    /// runtime.
    #[serde(default)]
    pub allowed_operations: Vec<GoogleWorkspaceAllowedOperation>,
    /// Path to service account JSON or OAuth client credentials file.
    ///
    /// When `None`, the tool relies on the default `gws` credential discovery
    /// (`gws auth login`). Set this to point at a service-account key or an
    /// OAuth client-secrets JSON for headless / CI environments.
    #[serde(default)]
    pub credentials_path: Option<String>,
    /// Default Google account email to pass to `gws --account`.
    ///
    /// When `None`, the currently active `gws` account is used.
    #[serde(default)]
    pub default_account: Option<String>,
    /// Maximum number of `gws` API calls allowed per minute. Default: `60`.
    #[serde(default = "default_gws_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Command execution timeout in seconds. Default: `30`.
    #[serde(default = "default_gws_timeout_secs")]
    pub timeout_secs: u64,
    /// Enable audit logging of every `gws` invocation (service, resource,
    /// method, timestamp). Default: `false`.
    #[serde(default)]
    pub audit_log: bool,
}

fn default_gws_rate_limit() -> u32 {
    60
}

fn default_gws_timeout_secs() -> u64 {
    30
}

impl Default for GoogleWorkspaceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_services: Vec::new(),
            allowed_operations: Vec::new(),
            credentials_path: None,
            default_account: None,
            rate_limit_per_minute: default_gws_rate_limit(),
            timeout_secs: default_gws_timeout_secs(),
            audit_log: false,
        }
    }
}

// ── Knowledge ───────────────────────────────────────────────────

/// Knowledge graph configuration for capturing and reusing expertise.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "knowledge"]
pub struct KnowledgeConfig {
    /// Enable the knowledge graph tool. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the knowledge graph SQLite database.
    #[serde(default = "default_knowledge_db_path")]
    pub db_path: String,
    /// Maximum number of knowledge nodes. Default: 100000.
    #[serde(default = "default_knowledge_max_nodes")]
    pub max_nodes: usize,
    /// Automatically capture knowledge from conversations. Default: false.
    #[serde(default)]
    pub auto_capture: bool,
    /// Proactively suggest relevant knowledge on queries. Default: true.
    #[serde(default = "default_true")]
    pub suggest_on_query: bool,
    /// Allow searching across workspaces (disabled by default for client data isolation).
    #[serde(default)]
    pub cross_workspace_search: bool,
}

fn default_knowledge_db_path() -> String {
    "~/.zeroclaw/knowledge.db".into()
}

fn default_knowledge_max_nodes() -> usize {
    100_000
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            db_path: default_knowledge_db_path(),
            max_nodes: default_knowledge_max_nodes(),
            auto_capture: false,
            suggest_on_query: true,
            cross_workspace_search: false,
        }
    }
}

// ── LinkedIn ────────────────────────────────────────────────────

/// LinkedIn integration configuration (`[linkedin]` section).
///
/// When enabled, the `linkedin` tool is registered in the agent tool surface.
/// Requires `LINKEDIN_*` credentials in the workspace `.env` file.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin"]
pub struct LinkedInConfig {
    /// Enable the LinkedIn tool.
    #[serde(default)]
    pub enabled: bool,

    /// LinkedIn REST API version header (YYYYMM format).
    #[serde(default = "default_linkedin_api_version")]
    pub api_version: String,

    /// Content strategy for automated posting.
    #[serde(default)]
    #[nested]
    pub content: LinkedInContentConfig,

    /// Image generation for posts (`[linkedin.image]`).
    #[serde(default)]
    #[nested]
    pub image: LinkedInImageConfig,
}

impl Default for LinkedInConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_version: default_linkedin_api_version(),
            content: LinkedInContentConfig::default(),
            image: LinkedInImageConfig::default(),
        }
    }
}

fn default_linkedin_api_version() -> String {
    "202602".to_string()
}

/// Plugin system configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "plugins"]
pub struct PluginsConfig {
    /// Enable the plugin system (default: false)
    #[serde(default)]
    pub enabled: bool,
    /// Directory where plugins are stored
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: String,
    /// Auto-discover and load plugins on startup
    #[serde(default)]
    pub auto_discover: bool,
    /// Maximum number of plugins that can be loaded
    #[serde(default = "default_max_plugins")]
    pub max_plugins: usize,
    /// Plugin signature verification security settings
    #[serde(default)]
    #[nested]
    pub security: PluginSecurityConfig,
}

/// Plugin signature verification configuration (`[plugins.security]`).
///
/// Controls Ed25519 signature verification for plugin manifests.
/// In `strict` mode, only plugins signed by a trusted publisher key are loaded.
/// In `permissive` mode, unsigned or untrusted plugins produce warnings but are
/// still loaded. In `disabled` mode (the default), no signature checking occurs.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "plugins.security"]
pub struct PluginSecurityConfig {
    /// Signature enforcement mode: "disabled", "permissive", or "strict".
    #[serde(default = "default_signature_mode")]
    pub signature_mode: String,
    /// Hex-encoded Ed25519 public keys of trusted plugin publishers.
    #[serde(default)]
    pub trusted_publisher_keys: Vec<String>,
}

fn default_signature_mode() -> String {
    "disabled".to_string()
}

impl Default for PluginSecurityConfig {
    fn default() -> Self {
        Self {
            signature_mode: default_signature_mode(),
            trusted_publisher_keys: Vec::new(),
        }
    }
}

fn default_plugins_dir() -> String {
    "~/.zeroclaw/plugins".to_string()
}

fn default_max_plugins() -> usize {
    50
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            plugins_dir: default_plugins_dir(),
            auto_discover: false,
            max_plugins: default_max_plugins(),
            security: PluginSecurityConfig::default(),
        }
    }
}

/// Content strategy configuration for LinkedIn auto-posting (`[linkedin.content]`).
///
/// The agent reads this via the `linkedin get_content_strategy` action to know
/// what feeds to check, which repos to highlight, and how to write posts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.content"]
pub struct LinkedInContentConfig {
    /// RSS feed URLs to monitor for topic inspiration (titles only).
    #[serde(default)]
    pub rss_feeds: Vec<String>,

    /// GitHub usernames whose public activity to reference.
    #[serde(default)]
    pub github_users: Vec<String>,

    /// GitHub repositories to highlight (format: `owner/repo`).
    #[serde(default)]
    pub github_repos: Vec<String>,

    /// Topics of expertise and interest for post themes.
    #[serde(default)]
    pub topics: Vec<String>,

    /// Professional persona description (name, role, expertise).
    #[serde(default)]
    pub persona: String,

    /// Freeform posting instructions for the AI agent.
    #[serde(default)]
    pub instructions: String,
}

/// Image generation configuration for LinkedIn posts (`[linkedin.image]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.image"]
pub struct LinkedInImageConfig {
    /// Enable image generation for posts.
    #[serde(default)]
    pub enabled: bool,

    /// Provider priority order. Tried in sequence; first success wins.
    #[serde(default = "default_image_providers")]
    pub providers: Vec<String>,

    /// Generate a branded SVG text card when all AI providers fail.
    #[serde(default = "default_true")]
    pub fallback_card: bool,

    /// Accent color for the fallback card (CSS hex).
    #[serde(default = "default_card_accent_color")]
    pub card_accent_color: String,

    /// Temp directory for generated images, relative to workspace.
    #[serde(default = "default_image_temp_dir")]
    pub temp_dir: String,

    /// Stability AI provider settings.
    #[serde(default)]
    #[nested]
    pub stability: ImageProviderStabilityConfig,

    /// Google Imagen (Vertex AI) provider settings.
    #[serde(default)]
    #[nested]
    pub imagen: ImageProviderImagenConfig,

    /// OpenAI DALL-E provider settings.
    #[serde(default)]
    #[nested]
    pub dalle: ImageProviderDalleConfig,

    /// Flux (fal.ai) provider settings.
    #[serde(default)]
    #[nested]
    pub flux: ImageProviderFluxConfig,
}

fn default_image_providers() -> Vec<String> {
    vec![
        "stability".into(),
        "imagen".into(),
        "dalle".into(),
        "flux".into(),
    ]
}

fn default_card_accent_color() -> String {
    "#0A66C2".into()
}

fn default_image_temp_dir() -> String {
    "linkedin/images".into()
}

impl Default for LinkedInImageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            providers: default_image_providers(),
            fallback_card: true,
            card_accent_color: default_card_accent_color(),
            temp_dir: default_image_temp_dir(),
            stability: ImageProviderStabilityConfig::default(),
            imagen: ImageProviderImagenConfig::default(),
            dalle: ImageProviderDalleConfig::default(),
            flux: ImageProviderFluxConfig::default(),
        }
    }
}

/// Stability AI image generation settings (`[linkedin.image.stability]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.image.stability"]
pub struct ImageProviderStabilityConfig {
    /// Environment variable name holding the API key.
    #[serde(default = "default_stability_api_key_env")]
    pub api_key_env: String,
    /// Stability model identifier.
    #[serde(default = "default_stability_model")]
    pub model: String,
}

fn default_stability_api_key_env() -> String {
    "STABILITY_API_KEY".into()
}
fn default_stability_model() -> String {
    "stable-diffusion-xl-1024-v1-0".into()
}

impl Default for ImageProviderStabilityConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_stability_api_key_env(),
            model: default_stability_model(),
        }
    }
}

/// Google Imagen (Vertex AI) settings (`[linkedin.image.imagen]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.image.imagen"]
pub struct ImageProviderImagenConfig {
    /// Environment variable name holding the API key.
    #[serde(default = "default_imagen_api_key_env")]
    pub api_key_env: String,
    /// Environment variable for the Google Cloud project ID.
    #[serde(default = "default_imagen_project_id_env")]
    pub project_id_env: String,
    /// Vertex AI region.
    #[serde(default = "default_imagen_region")]
    pub region: String,
}

fn default_imagen_api_key_env() -> String {
    "GOOGLE_VERTEX_API_KEY".into()
}
fn default_imagen_project_id_env() -> String {
    "GOOGLE_CLOUD_PROJECT".into()
}
fn default_imagen_region() -> String {
    "us-central1".into()
}

impl Default for ImageProviderImagenConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_imagen_api_key_env(),
            project_id_env: default_imagen_project_id_env(),
            region: default_imagen_region(),
        }
    }
}

/// OpenAI DALL-E settings (`[linkedin.image.dalle]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.image.dalle"]
pub struct ImageProviderDalleConfig {
    /// Environment variable name holding the OpenAI API key.
    #[serde(default = "default_dalle_api_key_env")]
    pub api_key_env: String,
    /// DALL-E model identifier.
    #[serde(default = "default_dalle_model")]
    pub model: String,
    /// Image dimensions.
    #[serde(default = "default_dalle_size")]
    pub size: String,
}

fn default_dalle_api_key_env() -> String {
    "OPENAI_API_KEY".into()
}
fn default_dalle_model() -> String {
    "dall-e-3".into()
}
fn default_dalle_size() -> String {
    "1024x1024".into()
}

impl Default for ImageProviderDalleConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_dalle_api_key_env(),
            model: default_dalle_model(),
            size: default_dalle_size(),
        }
    }
}

/// Flux (fal.ai) image generation settings (`[linkedin.image.flux]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "linkedin.image.flux"]
pub struct ImageProviderFluxConfig {
    /// Environment variable name holding the fal.ai API key.
    #[serde(default = "default_flux_api_key_env")]
    pub api_key_env: String,
    /// Flux model identifier.
    #[serde(default = "default_flux_model")]
    pub model: String,
}

fn default_flux_api_key_env() -> String {
    "FAL_API_KEY".into()
}
fn default_flux_model() -> String {
    "fal-ai/flux/schnell".into()
}

impl Default for ImageProviderFluxConfig {
    fn default() -> Self {
        Self {
            api_key_env: default_flux_api_key_env(),
            model: default_flux_model(),
        }
    }
}

// ── Standalone Image Generation ─────────────────────────────────

/// Standalone image generation tool configuration (`[image_gen]`).
///
/// When enabled, registers an `image_gen` tool that generates images via
/// fal.ai's synchronous API (Flux / Nano Banana models) and saves them
/// to the workspace `images/` directory.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "image-gen"]
pub struct ImageGenConfig {
    /// Enable the standalone image generation tool. Default: false.
    #[serde(default)]
    pub enabled: bool,

    /// Default fal.ai model identifier.
    #[serde(default = "default_image_gen_model")]
    pub default_model: String,

    /// Environment variable name holding the fal.ai API key.
    #[serde(default = "default_image_gen_api_key_env")]
    pub api_key_env: String,
}

fn default_image_gen_model() -> String {
    "fal-ai/flux/schnell".into()
}

fn default_image_gen_api_key_env() -> String {
    "FAL_API_KEY".into()
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_model: default_image_gen_model(),
            api_key_env: default_image_gen_api_key_env(),
        }
    }
}

// ── Claude Code ─────────────────────────────────────────────────

/// Claude Code CLI tool configuration (`[claude_code]` section).
///
/// Delegates coding tasks to the `claude -p` CLI. Authentication uses the
/// binary's own OAuth session (Max subscription) by default — no API key
/// needed unless `env_passthrough` includes `ANTHROPIC_API_KEY`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "claude-code"]
pub struct ClaudeCodeConfig {
    /// Enable the `claude_code` tool
    #[serde(default)]
    pub enabled: bool,
    /// Maximum execution time in seconds (coding tasks can be long)
    #[serde(default = "default_claude_code_timeout_secs")]
    pub timeout_secs: u64,
    /// Claude Code tools the subprocess is allowed to use
    #[serde(default = "default_claude_code_allowed_tools")]
    pub allowed_tools: Vec<String>,
    /// Optional system prompt appended to Claude Code invocations
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Maximum output size in bytes (2MB default)
    #[serde(default = "default_claude_code_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Extra env vars passed to the claude subprocess (e.g. ANTHROPIC_API_KEY for API-key billing)
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}

fn default_claude_code_timeout_secs() -> u64 {
    600
}

fn default_claude_code_allowed_tools() -> Vec<String> {
    vec!["Read".into(), "Edit".into(), "Bash".into(), "Write".into()]
}

fn default_claude_code_max_output_bytes() -> usize {
    2_097_152
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_secs: default_claude_code_timeout_secs(),
            allowed_tools: default_claude_code_allowed_tools(),
            system_prompt: None,
            max_output_bytes: default_claude_code_max_output_bytes(),
            env_passthrough: Vec::new(),
        }
    }
}

// ── Claude Code Runner ──────────────────────────────────────────

/// Claude Code task runner configuration (`[claude_code_runner]` section).
///
/// Spawns Claude Code in a tmux session with HTTP hooks that POST tool
/// execution events back to ZeroClaw's gateway, updating a Slack message
/// in-place with progress plus an SSH handoff link.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "claude-code-runner"]
pub struct ClaudeCodeRunnerConfig {
    /// Enable the `claude_code_runner` tool
    #[serde(default)]
    pub enabled: bool,
    /// SSH host for session handoff links (e.g. "myhost.example.com")
    #[serde(default)]
    pub ssh_host: Option<String>,
    /// Prefix for tmux session names (default: "zc-claude-")
    #[serde(default = "default_claude_code_runner_tmux_prefix")]
    pub tmux_prefix: String,
    /// Session time-to-live in seconds before auto-cleanup (default: 3600)
    #[serde(default = "default_claude_code_runner_session_ttl")]
    pub session_ttl: u64,
}

fn default_claude_code_runner_tmux_prefix() -> String {
    "zc-claude-".into()
}

fn default_claude_code_runner_session_ttl() -> u64 {
    3600
}

impl Default for ClaudeCodeRunnerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ssh_host: None,
            tmux_prefix: default_claude_code_runner_tmux_prefix(),
            session_ttl: default_claude_code_runner_session_ttl(),
        }
    }
}

// ── Codex CLI ───────────────────────────────────────────────────

/// Codex CLI tool configuration (`[codex_cli]` section).
///
/// Delegates coding tasks to the `codex -q` CLI. Authentication uses the
/// binary's own session by default — no API key needed unless
/// `env_passthrough` includes `OPENAI_API_KEY`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "codex-cli"]
pub struct CodexCliConfig {
    /// Enable the `codex_cli` tool
    #[serde(default)]
    pub enabled: bool,
    /// Maximum execution time in seconds (coding tasks can be long)
    #[serde(default = "default_codex_cli_timeout_secs")]
    pub timeout_secs: u64,
    /// Maximum output size in bytes (2MB default)
    #[serde(default = "default_codex_cli_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Extra env vars passed to the codex subprocess (e.g. OPENAI_API_KEY)
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}

fn default_codex_cli_timeout_secs() -> u64 {
    600
}

fn default_codex_cli_max_output_bytes() -> usize {
    2_097_152
}

impl Default for CodexCliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_secs: default_codex_cli_timeout_secs(),
            max_output_bytes: default_codex_cli_max_output_bytes(),
            env_passthrough: Vec::new(),
        }
    }
}

// ── Gemini CLI ──────────────────────────────────────────────────

/// Gemini CLI tool configuration (`[gemini_cli]` section).
///
/// Delegates coding tasks to the `gemini -p` CLI. Authentication uses the
/// binary's own session by default — no API key needed unless
/// `env_passthrough` includes `GOOGLE_API_KEY`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "gemini-cli"]
pub struct GeminiCliConfig {
    /// Enable the `gemini_cli` tool
    #[serde(default)]
    pub enabled: bool,
    /// Maximum execution time in seconds (coding tasks can be long)
    #[serde(default = "default_gemini_cli_timeout_secs")]
    pub timeout_secs: u64,
    /// Maximum output size in bytes (2MB default)
    #[serde(default = "default_gemini_cli_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Extra env vars passed to the gemini subprocess (e.g. GOOGLE_API_KEY)
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}

fn default_gemini_cli_timeout_secs() -> u64 {
    600
}

fn default_gemini_cli_max_output_bytes() -> usize {
    2_097_152
}

impl Default for GeminiCliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_secs: default_gemini_cli_timeout_secs(),
            max_output_bytes: default_gemini_cli_max_output_bytes(),
            env_passthrough: Vec::new(),
        }
    }
}

// ── OpenCode CLI ───────────────────────────────────────────────

/// OpenCode CLI tool configuration (`[opencode_cli]` section).
///
/// Delegates coding tasks to the `opencode run` CLI. Authentication uses the
/// binary's own session by default — no API key needed unless
/// `env_passthrough` includes provider-specific keys.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "opencode-cli"]
pub struct OpenCodeCliConfig {
    /// Enable the `opencode_cli` tool
    #[serde(default)]
    pub enabled: bool,
    /// Maximum execution time in seconds (coding tasks can be long)
    #[serde(default = "default_opencode_cli_timeout_secs")]
    pub timeout_secs: u64,
    /// Maximum output size in bytes (2MB default)
    #[serde(default = "default_opencode_cli_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Extra env vars passed to the opencode subprocess
    #[serde(default)]
    pub env_passthrough: Vec<String>,
}

fn default_opencode_cli_timeout_secs() -> u64 {
    600
}

fn default_opencode_cli_max_output_bytes() -> usize {
    2_097_152
}

impl Default for OpenCodeCliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_secs: default_opencode_cli_timeout_secs(),
            max_output_bytes: default_opencode_cli_max_output_bytes(),
            env_passthrough: Vec::new(),
        }
    }
}

// ── Proxy ───────────────────────────────────────────────────────

/// Proxy application scope — determines which outbound traffic uses the proxy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProxyScope {
    /// Use system environment proxy variables only.
    Environment,
    /// Apply proxy to all ZeroClaw-managed HTTP traffic (default).
    #[default]
    Zeroclaw,
    /// Apply proxy only to explicitly listed service selectors.
    Services,
}

/// Proxy configuration for outbound HTTP/HTTPS/SOCKS5 traffic (`[proxy]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "proxy"]
pub struct ProxyConfig {
    /// Enable proxy support for selected scope.
    #[serde(default)]
    pub enabled: bool,
    /// Proxy URL for HTTP requests (supports http, https, socks5, socks5h).
    #[serde(default)]
    pub http_proxy: Option<String>,
    /// Proxy URL for HTTPS requests (supports http, https, socks5, socks5h).
    #[serde(default)]
    pub https_proxy: Option<String>,
    /// Fallback proxy URL for all schemes.
    #[serde(default)]
    pub all_proxy: Option<String>,
    /// No-proxy bypass list. Same format as NO_PROXY.
    #[serde(default)]
    pub no_proxy: Vec<String>,
    /// Proxy application scope.
    #[serde(default)]
    pub scope: ProxyScope,
    /// Service selectors used when scope = "services".
    #[serde(default)]
    pub services: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http_proxy: None,
            https_proxy: None,
            all_proxy: None,
            no_proxy: Vec::new(),
            scope: ProxyScope::Zeroclaw,
            services: Vec::new(),
        }
    }
}

impl ProxyConfig {
    pub fn supported_service_keys() -> &'static [&'static str] {
        SUPPORTED_PROXY_SERVICE_KEYS
    }

    pub fn supported_service_selectors() -> &'static [&'static str] {
        SUPPORTED_PROXY_SERVICE_SELECTORS
    }

    pub fn has_any_proxy_url(&self) -> bool {
        normalize_proxy_url_option(self.http_proxy.as_deref()).is_some()
            || normalize_proxy_url_option(self.https_proxy.as_deref()).is_some()
            || normalize_proxy_url_option(self.all_proxy.as_deref()).is_some()
    }

    pub fn normalized_services(&self) -> Vec<String> {
        normalize_service_list(self.services.clone())
    }

    pub fn normalized_no_proxy(&self) -> Vec<String> {
        normalize_no_proxy_list(self.no_proxy.clone())
    }

    pub fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("http_proxy", self.http_proxy.as_deref()),
            ("https_proxy", self.https_proxy.as_deref()),
            ("all_proxy", self.all_proxy.as_deref()),
        ] {
            if let Some(url) = normalize_proxy_url_option(value) {
                validate_proxy_url(field, &url)?;
            }
        }

        for selector in self.normalized_services() {
            if !is_supported_proxy_service_selector(&selector) {
                anyhow::bail!(
                    "Unsupported proxy service selector '{selector}'. Use tool `proxy_config` action `list_services` for valid values"
                );
            }
        }

        if self.enabled && !self.has_any_proxy_url() {
            anyhow::bail!(
                "Proxy is enabled but no proxy URL is configured. Set at least one of http_proxy, https_proxy, or all_proxy"
            );
        }

        if self.enabled
            && self.scope == ProxyScope::Services
            && self.normalized_services().is_empty()
        {
            anyhow::bail!(
                "proxy.scope='services' requires a non-empty proxy.services list when proxy is enabled"
            );
        }

        Ok(())
    }

    pub fn should_apply_to_service(&self, service_key: &str) -> bool {
        if !self.enabled {
            return false;
        }

        match self.scope {
            ProxyScope::Environment => false,
            ProxyScope::Zeroclaw => true,
            ProxyScope::Services => {
                let service_key = service_key.trim().to_ascii_lowercase();
                if service_key.is_empty() {
                    return false;
                }

                self.normalized_services()
                    .iter()
                    .any(|selector| service_selector_matches(selector, &service_key))
            }
        }
    }

    pub fn apply_to_reqwest_builder(
        &self,
        mut builder: reqwest::ClientBuilder,
        service_key: &str,
    ) -> reqwest::ClientBuilder {
        if !self.should_apply_to_service(service_key) {
            return builder;
        }

        let no_proxy = self.no_proxy_value();

        if let Some(url) = normalize_proxy_url_option(self.all_proxy.as_deref()) {
            match reqwest::Proxy::all(&url) {
                Ok(proxy) => {
                    builder = builder.proxy(apply_no_proxy(proxy, no_proxy.clone()));
                }
                Err(error) => {
                    tracing::warn!(
                        proxy_url = %url,
                        service_key,
                        "Ignoring invalid all_proxy URL: {error}"
                    );
                }
            }
        }

        if let Some(url) = normalize_proxy_url_option(self.http_proxy.as_deref()) {
            match reqwest::Proxy::http(&url) {
                Ok(proxy) => {
                    builder = builder.proxy(apply_no_proxy(proxy, no_proxy.clone()));
                }
                Err(error) => {
                    tracing::warn!(
                        proxy_url = %url,
                        service_key,
                        "Ignoring invalid http_proxy URL: {error}"
                    );
                }
            }
        }

        if let Some(url) = normalize_proxy_url_option(self.https_proxy.as_deref()) {
            match reqwest::Proxy::https(&url) {
                Ok(proxy) => {
                    builder = builder.proxy(apply_no_proxy(proxy, no_proxy));
                }
                Err(error) => {
                    tracing::warn!(
                        proxy_url = %url,
                        service_key,
                        "Ignoring invalid https_proxy URL: {error}"
                    );
                }
            }
        }

        builder
    }

    pub fn apply_to_process_env(&self) {
        set_proxy_env_pair("HTTP_PROXY", self.http_proxy.as_deref());
        set_proxy_env_pair("HTTPS_PROXY", self.https_proxy.as_deref());
        set_proxy_env_pair("ALL_PROXY", self.all_proxy.as_deref());

        let no_proxy_joined = {
            let list = self.normalized_no_proxy();
            (!list.is_empty()).then(|| list.join(","))
        };
        set_proxy_env_pair("NO_PROXY", no_proxy_joined.as_deref());
    }

    pub fn clear_process_env() {
        clear_proxy_env_pair("HTTP_PROXY");
        clear_proxy_env_pair("HTTPS_PROXY");
        clear_proxy_env_pair("ALL_PROXY");
        clear_proxy_env_pair("NO_PROXY");
    }

    fn no_proxy_value(&self) -> Option<reqwest::NoProxy> {
        let joined = {
            let list = self.normalized_no_proxy();
            (!list.is_empty()).then(|| list.join(","))
        };
        joined.as_deref().and_then(reqwest::NoProxy::from_string)
    }
}

fn apply_no_proxy(proxy: reqwest::Proxy, no_proxy: Option<reqwest::NoProxy>) -> reqwest::Proxy {
    proxy.no_proxy(no_proxy)
}

fn normalize_proxy_url_option(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn normalize_no_proxy_list(values: Vec<String>) -> Vec<String> {
    normalize_comma_values(values)
}

fn normalize_service_list(values: Vec<String>) -> Vec<String> {
    let mut normalized = normalize_comma_values(values)
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    normalized
}

fn normalize_comma_values(values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    for value in values {
        for part in value.split(',') {
            let normalized = part.trim();
            if normalized.is_empty() {
                continue;
            }
            output.push(normalized.to_string());
        }
    }
    output.sort_unstable();
    output.dedup();
    output
}

fn is_supported_proxy_service_selector(selector: &str) -> bool {
    if SUPPORTED_PROXY_SERVICE_KEYS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(selector))
    {
        return true;
    }

    SUPPORTED_PROXY_SERVICE_SELECTORS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(selector))
}

fn service_selector_matches(selector: &str, service_key: &str) -> bool {
    if selector == service_key {
        return true;
    }

    if let Some(prefix) = selector.strip_suffix(".*") {
        return service_key.starts_with(prefix)
            && service_key
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('.'));
    }

    false
}

const MCP_MAX_TOOL_TIMEOUT_SECS: u64 = 600;

fn validate_mcp_config(config: &McpConfig) -> Result<()> {
    let mut seen_names = std::collections::HashSet::new();
    for (i, server) in config.servers.iter().enumerate() {
        let name = server.name.trim();
        if name.is_empty() {
            anyhow::bail!("mcp.servers[{i}].name must not be empty");
        }
        if !seen_names.insert(name.to_ascii_lowercase()) {
            anyhow::bail!("mcp.servers contains duplicate name: {name}");
        }

        if let Some(timeout) = server.tool_timeout_secs {
            if timeout == 0 {
                anyhow::bail!("mcp.servers[{i}].tool_timeout_secs must be greater than 0");
            }
            if timeout > MCP_MAX_TOOL_TIMEOUT_SECS {
                anyhow::bail!(
                    "mcp.servers[{i}].tool_timeout_secs exceeds max {MCP_MAX_TOOL_TIMEOUT_SECS}"
                );
            }
        }

        match server.transport {
            McpTransport::Stdio => {
                if server.command.trim().is_empty() {
                    anyhow::bail!(
                        "mcp.servers[{i}] with transport=stdio requires non-empty command"
                    );
                }
            }
            McpTransport::Http | McpTransport::Sse => {
                let url = server
                    .url
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "mcp.servers[{i}] with transport={} requires url",
                            match server.transport {
                                McpTransport::Http => "http",
                                McpTransport::Sse => "sse",
                                McpTransport::Stdio => "stdio",
                            }
                        )
                    })?;
                let parsed = reqwest::Url::parse(url)
                    .with_context(|| format!("mcp.servers[{i}].url is not a valid URL"))?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    anyhow::bail!("mcp.servers[{i}].url must use http/https");
                }
            }
        }
    }
    Ok(())
}

fn validate_proxy_url(field: &str, url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("Invalid {field} URL: '{url}' is not a valid URL"))?;

    match parsed.scheme() {
        "http" | "https" | "socks5" | "socks5h" | "socks" => {}
        scheme => {
            anyhow::bail!(
                "Invalid {field} URL scheme '{scheme}'. Allowed: http, https, socks5, socks5h, socks"
            );
        }
    }

    if parsed.host_str().is_none() {
        anyhow::bail!("Invalid {field} URL: host is required");
    }

    Ok(())
}

fn set_proxy_env_pair(key: &str, value: Option<&str>) {
    let lowercase_key = key.to_ascii_lowercase();
    if let Some(value) = value.and_then(|candidate| normalize_proxy_url_option(Some(candidate))) {
        // SAFETY: called during single-threaded config init before async runtime starts.
        unsafe {
            std::env::set_var(key, &value);
            std::env::set_var(lowercase_key, value);
        }
    } else {
        // SAFETY: called during single-threaded config init before async runtime starts.
        unsafe {
            std::env::remove_var(key);
            std::env::remove_var(lowercase_key);
        }
    }
}

fn clear_proxy_env_pair(key: &str) {
    // SAFETY: called during single-threaded config init before async runtime starts.
    unsafe {
        std::env::remove_var(key);
        std::env::remove_var(key.to_ascii_lowercase());
    }
}

fn runtime_proxy_state() -> &'static RwLock<ProxyConfig> {
    RUNTIME_PROXY_CONFIG.get_or_init(|| RwLock::new(ProxyConfig::default()))
}

fn runtime_proxy_client_cache() -> &'static RwLock<HashMap<String, reqwest::Client>> {
    RUNTIME_PROXY_CLIENT_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn clear_runtime_proxy_client_cache() {
    match runtime_proxy_client_cache().write() {
        Ok(mut guard) => {
            guard.clear();
        }
        Err(poisoned) => {
            poisoned.into_inner().clear();
        }
    }
}

fn runtime_proxy_cache_key(
    service_key: &str,
    timeout_secs: Option<u64>,
    connect_timeout_secs: Option<u64>,
) -> String {
    format!(
        "{}|timeout={}|connect_timeout={}",
        service_key.trim().to_ascii_lowercase(),
        timeout_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        connect_timeout_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string())
    )
}

fn runtime_proxy_cached_client(cache_key: &str) -> Option<reqwest::Client> {
    match runtime_proxy_client_cache().read() {
        Ok(guard) => guard.get(cache_key).cloned(),
        Err(poisoned) => poisoned.into_inner().get(cache_key).cloned(),
    }
}

fn set_runtime_proxy_cached_client(cache_key: String, client: reqwest::Client) {
    match runtime_proxy_client_cache().write() {
        Ok(mut guard) => {
            guard.insert(cache_key, client);
        }
        Err(poisoned) => {
            poisoned.into_inner().insert(cache_key, client);
        }
    }
}

pub fn set_runtime_proxy_config(config: ProxyConfig) {
    match runtime_proxy_state().write() {
        Ok(mut guard) => {
            *guard = config;
        }
        Err(poisoned) => {
            *poisoned.into_inner() = config;
        }
    }

    clear_runtime_proxy_client_cache();
}

pub fn runtime_proxy_config() -> ProxyConfig {
    match runtime_proxy_state().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub fn apply_runtime_proxy_to_builder(
    builder: reqwest::ClientBuilder,
    service_key: &str,
) -> reqwest::ClientBuilder {
    runtime_proxy_config().apply_to_reqwest_builder(builder, service_key)
}

pub fn build_runtime_proxy_client(service_key: &str) -> reqwest::Client {
    let cache_key = runtime_proxy_cache_key(service_key, None, None);
    if let Some(client) = runtime_proxy_cached_client(&cache_key) {
        return client;
    }

    let builder = apply_runtime_proxy_to_builder(reqwest::Client::builder(), service_key);
    let client = builder.build().unwrap_or_else(|error| {
        tracing::warn!(service_key, "Failed to build proxied client: {error}");
        reqwest::Client::new()
    });
    set_runtime_proxy_cached_client(cache_key, client.clone());
    client
}

pub fn build_runtime_proxy_client_with_timeouts(
    service_key: &str,
    timeout_secs: u64,
    connect_timeout_secs: u64,
) -> reqwest::Client {
    let cache_key =
        runtime_proxy_cache_key(service_key, Some(timeout_secs), Some(connect_timeout_secs));
    if let Some(client) = runtime_proxy_cached_client(&cache_key) {
        return client;
    }

    let builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs));
    let builder = apply_runtime_proxy_to_builder(builder, service_key);
    let client = builder.build().unwrap_or_else(|error| {
        tracing::warn!(
            service_key,
            "Failed to build proxied timeout client: {error}"
        );
        reqwest::Client::new()
    });
    set_runtime_proxy_cached_client(cache_key, client.clone());
    client
}

/// Build an HTTP client for a channel, using an explicit per-channel proxy URL
/// when configured.  Falls back to the global runtime proxy when `proxy_url` is
/// `None` or empty.
pub fn build_channel_proxy_client(service_key: &str, proxy_url: Option<&str>) -> reqwest::Client {
    match normalize_proxy_url_option(proxy_url) {
        Some(url) => build_explicit_proxy_client(service_key, &url, None, None),
        None => build_runtime_proxy_client(service_key),
    }
}

/// Build an HTTP client for a channel with custom timeouts, using an explicit
/// per-channel proxy URL when configured.  Falls back to the global runtime
/// proxy when `proxy_url` is `None` or empty.
pub fn build_channel_proxy_client_with_timeouts(
    service_key: &str,
    proxy_url: Option<&str>,
    timeout_secs: u64,
    connect_timeout_secs: u64,
) -> reqwest::Client {
    match normalize_proxy_url_option(proxy_url) {
        Some(url) => build_explicit_proxy_client(
            service_key,
            &url,
            Some(timeout_secs),
            Some(connect_timeout_secs),
        ),
        None => build_runtime_proxy_client_with_timeouts(
            service_key,
            timeout_secs,
            connect_timeout_secs,
        ),
    }
}

/// Apply an explicit proxy URL to a `reqwest::ClientBuilder`, returning the
/// modified builder.  Used by channels that specify a per-channel `proxy_url`.
pub fn apply_channel_proxy_to_builder(
    builder: reqwest::ClientBuilder,
    service_key: &str,
    proxy_url: Option<&str>,
) -> reqwest::ClientBuilder {
    match normalize_proxy_url_option(proxy_url) {
        Some(url) => apply_explicit_proxy_to_builder(builder, service_key, &url),
        None => apply_runtime_proxy_to_builder(builder, service_key),
    }
}

/// Build a client with a single explicit proxy URL (http+https via `Proxy::all`).
fn build_explicit_proxy_client(
    service_key: &str,
    proxy_url: &str,
    timeout_secs: Option<u64>,
    connect_timeout_secs: Option<u64>,
) -> reqwest::Client {
    let cache_key = format!(
        "explicit|{}|{}|timeout={}|connect_timeout={}",
        service_key.trim().to_ascii_lowercase(),
        proxy_url,
        timeout_secs
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
        connect_timeout_secs
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    if let Some(client) = runtime_proxy_cached_client(&cache_key) {
        return client;
    }

    let mut builder = reqwest::Client::builder();
    if let Some(t) = timeout_secs {
        builder = builder.timeout(std::time::Duration::from_secs(t));
    }
    if let Some(ct) = connect_timeout_secs {
        builder = builder.connect_timeout(std::time::Duration::from_secs(ct));
    }
    builder = apply_explicit_proxy_to_builder(builder, service_key, proxy_url);
    let client = builder.build().unwrap_or_else(|error| {
        tracing::warn!(
            service_key,
            proxy_url,
            "Failed to build channel proxy client: {error}"
        );
        reqwest::Client::new()
    });
    set_runtime_proxy_cached_client(cache_key, client.clone());
    client
}

/// Apply a single explicit proxy URL to a builder via `Proxy::all`.
fn apply_explicit_proxy_to_builder(
    mut builder: reqwest::ClientBuilder,
    service_key: &str,
    proxy_url: &str,
) -> reqwest::ClientBuilder {
    match reqwest::Proxy::all(proxy_url) {
        Ok(proxy) => {
            builder = builder.proxy(proxy);
        }
        Err(error) => {
            tracing::warn!(
                proxy_url,
                service_key,
                "Ignoring invalid channel proxy_url: {error}"
            );
        }
    }
    builder
}

// ── Proxy-aware WebSocket connect ────────────────────────────────
//
// `tokio_tungstenite::connect_async` does not honour proxy settings.
// The helpers below resolve the effective proxy URL for a given service
// key and, when a proxy is active, establish a tunnelled TCP connection
// (HTTP CONNECT for http/https proxies, SOCKS5 for socks5/socks5h)
// before handing the stream to `tokio_tungstenite` for the WebSocket
// handshake.

/// Combined async IO trait for boxed WebSocket transport streams.
trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// A boxed async IO stream used when a WebSocket connection is tunnelled
/// through a proxy.  The concrete type varies depending on the proxy
/// kind (HTTP CONNECT vs SOCKS5) and the target scheme (ws vs wss).
///
/// We wrap in a newtype so we can implement `AsyncRead` and `AsyncWrite`
/// via delegation, since Rust trait objects cannot combine multiple
/// non-auto traits.
pub struct BoxedIo(Box<dyn AsyncReadWrite>);

impl tokio::io::AsyncRead for BoxedIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for BoxedIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut *self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.0).poll_shutdown(cx)
    }
}

impl Unpin for BoxedIo {}

/// Convenience alias for the WebSocket stream returned by the proxy-aware
/// connect helpers.
pub type ProxiedWsStream = tokio_tungstenite::WebSocketStream<BoxedIo>;

/// Resolve the effective proxy URL for a WebSocket connection to the
/// given `ws_url`, taking into account the per-channel `proxy_url`
/// override, the runtime proxy config, scope and no_proxy list.
fn resolve_ws_proxy_url(
    service_key: &str,
    ws_url: &str,
    channel_proxy_url: Option<&str>,
) -> Option<String> {
    // 1. Explicit per-channel proxy always wins.
    if let Some(url) = normalize_proxy_url_option(channel_proxy_url) {
        return Some(url);
    }

    // 2. Consult the runtime proxy config.
    let cfg = runtime_proxy_config();
    if !cfg.should_apply_to_service(service_key) {
        return None;
    }

    // Check the no_proxy list against the WebSocket target host.
    if let Ok(parsed) = reqwest::Url::parse(ws_url)
        && let Some(host) = parsed.host_str()
    {
        let no_proxy_entries = cfg.normalized_no_proxy();
        if !no_proxy_entries.is_empty() {
            let host_lower = host.to_ascii_lowercase();
            let matches_no_proxy = no_proxy_entries.iter().any(|entry| {
                let entry = entry.trim().to_ascii_lowercase();
                if entry == "*" {
                    return true;
                }
                if host_lower == entry {
                    return true;
                }
                // Support ".example.com" matching "foo.example.com"
                if let Some(suffix) = entry.strip_prefix('.') {
                    return host_lower.ends_with(suffix) || host_lower == suffix;
                }
                // Support "example.com" also matching "foo.example.com"
                host_lower.ends_with(&format!(".{entry}"))
            });
            if matches_no_proxy {
                return None;
            }
        }
    }

    // For wss:// prefer https_proxy, for ws:// prefer http_proxy, fall
    // back to all_proxy in both cases.
    let is_secure = ws_url.starts_with("wss://") || ws_url.starts_with("wss:");
    let preferred = if is_secure {
        normalize_proxy_url_option(cfg.https_proxy.as_deref())
    } else {
        normalize_proxy_url_option(cfg.http_proxy.as_deref())
    };
    preferred.or_else(|| normalize_proxy_url_option(cfg.all_proxy.as_deref()))
}

/// Connect a WebSocket through the configured proxy (if any).
///
/// When no proxy applies, this is a thin wrapper around
/// `tokio_tungstenite::connect_async`.  When a proxy is active the
/// function tunnels the TCP connection through the proxy before
/// performing the WebSocket upgrade.
///
/// `service_key` is the proxy-service selector (e.g. `"channel.discord"`).
/// `channel_proxy_url` is the optional per-channel proxy override.
pub async fn ws_connect_with_proxy(
    ws_url: &str,
    service_key: &str,
    channel_proxy_url: Option<&str>,
) -> anyhow::Result<(
    ProxiedWsStream,
    tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
)> {
    let proxy_url = resolve_ws_proxy_url(service_key, ws_url, channel_proxy_url);

    match proxy_url {
        None => {
            // No proxy — delegate directly.
            let (stream, resp) = tokio_tungstenite::connect_async(ws_url).await?;
            // Re-wrap the inner stream into our boxed type so the caller
            // always gets `ProxiedWsStream`.
            let inner = stream.into_inner();
            let boxed = BoxedIo(Box::new(inner));
            let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
                boxed,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await;
            Ok((ws, resp))
        }
        Some(proxy) => ws_connect_via_proxy(ws_url, &proxy).await,
    }
}

/// Establish a WebSocket connection tunnelled through the given proxy URL.
async fn ws_connect_via_proxy(
    ws_url: &str,
    proxy_url: &str,
) -> anyhow::Result<(
    ProxiedWsStream,
    tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt as _};
    use tokio::net::TcpStream;

    let target =
        reqwest::Url::parse(ws_url).with_context(|| format!("Invalid WebSocket URL: {ws_url}"))?;
    let target_host = target
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("WebSocket URL has no host: {ws_url}"))?
        .to_string();
    let target_port = target
        .port_or_known_default()
        .unwrap_or(if target.scheme() == "wss" { 443 } else { 80 });

    let proxy = reqwest::Url::parse(proxy_url)
        .with_context(|| format!("Invalid proxy URL: {proxy_url}"))?;

    let stream: BoxedIo = match proxy.scheme() {
        "socks5" | "socks5h" | "socks" => {
            let proxy_addr = format!(
                "{}:{}",
                proxy.host_str().unwrap_or("127.0.0.1"),
                proxy.port_or_known_default().unwrap_or(1080)
            );
            let target_addr = format!("{target_host}:{target_port}");
            let socks_stream = if proxy.username().is_empty() {
                tokio_socks::tcp::Socks5Stream::connect(proxy_addr.as_str(), target_addr.as_str())
                    .await
                    .with_context(|| format!("SOCKS5 connect to {target_addr} via {proxy_addr}"))?
            } else {
                let password = proxy.password().unwrap_or("");
                tokio_socks::tcp::Socks5Stream::connect_with_password(
                    proxy_addr.as_str(),
                    target_addr.as_str(),
                    proxy.username(),
                    password,
                )
                .await
                .with_context(|| format!("SOCKS5 auth connect to {target_addr} via {proxy_addr}"))?
            };
            let tcp: TcpStream = socks_stream.into_inner();
            BoxedIo(Box::new(tcp))
        }
        "http" | "https" => {
            let proxy_host = proxy.host_str().unwrap_or("127.0.0.1");
            let proxy_port = proxy.port_or_known_default().unwrap_or(8080);
            let proxy_addr = format!("{proxy_host}:{proxy_port}");

            let mut tcp = TcpStream::connect(&proxy_addr)
                .await
                .with_context(|| format!("TCP connect to HTTP proxy {proxy_addr}"))?;

            // Send HTTP CONNECT request.
            let connect_req = format!(
                "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n\r\n"
            );
            tcp.write_all(connect_req.as_bytes()).await?;

            // Read the response (we only need the status line).
            let mut buf = vec![0u8; 4096];
            let mut total = 0usize;
            loop {
                let n = tcp.read(&mut buf[total..]).await?;
                if n == 0 {
                    anyhow::bail!("HTTP CONNECT proxy closed connection before response");
                }
                total += n;
                // Look for end of HTTP headers.
                if let Some(pos) = find_header_end(&buf[..total]) {
                    let status_line = std::str::from_utf8(&buf[..pos])
                        .unwrap_or("")
                        .lines()
                        .next()
                        .unwrap_or("");
                    if !status_line.contains("200") {
                        anyhow::bail!(
                            "HTTP CONNECT proxy returned non-200 response: {status_line}"
                        );
                    }
                    break;
                }
                if total >= buf.len() {
                    anyhow::bail!("HTTP CONNECT proxy response too large");
                }
            }

            BoxedIo(Box::new(tcp))
        }
        scheme => {
            anyhow::bail!("Unsupported proxy scheme '{scheme}' for WebSocket connections");
        }
    };

    // If the target is wss://, wrap in TLS.
    let is_secure = target.scheme() == "wss";
    let stream: BoxedIo = if is_secure {
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        let connector = tokio_rustls::TlsConnector::from(tls_config);
        let server_name = rustls_pki_types::ServerName::try_from(target_host.clone())
            .with_context(|| format!("Invalid TLS server name: {target_host}"))?;

        // `stream` is `BoxedIo` — we need a concrete `AsyncRead + AsyncWrite`
        // for `TlsConnector::connect`.  Since `BoxedIo` already satisfies
        // those bounds we can pass it directly.
        let tls_stream = connector
            .connect(server_name, stream)
            .await
            .with_context(|| format!("TLS handshake with {target_host}"))?;
        BoxedIo(Box::new(tls_stream))
    } else {
        stream
    };

    // Perform the WebSocket client handshake over the tunnelled stream.
    let ws_request = tokio_tungstenite::tungstenite::http::Request::builder()
        .uri(ws_url)
        .header("Host", format!("{target_host}:{target_port}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .with_context(|| "Failed to build WebSocket upgrade request")?;

    let (ws_stream, response) = tokio_tungstenite::client_async(ws_request, stream)
        .await
        .with_context(|| format!("WebSocket handshake failed for {ws_url}"))?;

    Ok((ws_stream, response))
}

/// Find the `\r\n\r\n` boundary marking the end of HTTP headers.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn parse_proxy_scope(raw: &str) -> Option<ProxyScope> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "environment" | "env" => Some(ProxyScope::Environment),
        "zeroclaw" | "internal" | "core" => Some(ProxyScope::Zeroclaw),
        "services" | "service" => Some(ProxyScope::Services),
        _ => None,
    }
}

fn parse_proxy_enabled(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
// ── Memory ───────────────────────────────────────────────────

/// Persistent storage configuration (`[storage]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "storage"]
pub struct StorageConfig {
    /// Storage provider settings (e.g. sqlite, postgres).
    #[serde(default)]
    #[nested]
    pub provider: StorageProviderSection,
}

/// Wrapper for the storage provider configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "storage.provider"]
pub struct StorageProviderSection {
    /// Storage provider backend settings.
    #[serde(default)]
    #[nested]
    pub config: StorageProviderConfig,
}

/// Storage provider backend configuration for remote storage backends.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "storage.provider"]
pub struct StorageProviderConfig {
    /// Storage engine key (e.g. "sqlite", "qdrant").
    #[serde(default)]
    pub provider: String,

    /// Connection URL for remote providers.
    /// Accepts legacy aliases: dbURL, database_url, databaseUrl.
    #[serde(
        default,
        alias = "dbURL",
        alias = "database_url",
        alias = "databaseUrl"
    )]
    #[secret]
    pub db_url: Option<String>,

    /// Database schema for SQL backends.
    #[serde(default = "default_storage_schema")]
    pub schema: String,

    /// Table name for memory entries.
    #[serde(default = "default_storage_table")]
    pub table: String,

    /// Optional connection timeout in seconds for remote providers.
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
}

fn default_storage_schema() -> String {
    "public".into()
}

fn default_storage_table() -> String {
    "memories".into()
}

impl Default for StorageProviderConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            db_url: None,
            schema: default_storage_schema(),
            table: default_storage_table(),
            connect_timeout_secs: None,
        }
    }
}

/// Memory backend configuration (`[memory]` section).
///
/// Controls conversation memory storage, embeddings, hybrid search, response caching,
/// and memory snapshot/hydration.
/// Configuration for Qdrant vector database backend (`[memory.qdrant]`).
/// Used when `[memory].backend = "qdrant"`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "memory.qdrant"]
pub struct QdrantConfig {
    /// Qdrant server URL (e.g. "http://localhost:6333").
    /// Falls back to `QDRANT_URL` env var if not set.
    #[serde(default)]
    pub url: Option<String>,
    /// Qdrant collection name for storing memories.
    /// Falls back to `QDRANT_COLLECTION` env var, or default "zeroclaw_memories".
    #[serde(default = "default_qdrant_collection")]
    pub collection: String,
    /// Optional API key for Qdrant Cloud or secured instances.
    /// Falls back to `QDRANT_API_KEY` env var if not set.
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_qdrant_collection() -> String {
    "zeroclaw_memories".into()
}

impl Default for QdrantConfig {
    fn default() -> Self {
        Self {
            url: None,
            collection: default_qdrant_collection(),
            api_key: None,
        }
    }
}

/// Search strategy for memory recall.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Pure keyword search (FTS5 BM25)
    Bm25,
    /// Pure vector/semantic search
    Embedding,
    /// Weighted combination of keyword + vector (default)
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "memory"]
#[allow(clippy::struct_excessive_bools)]
pub struct MemoryConfig {
    /// "sqlite" | "lucid" | "qdrant" | "markdown" | "none" (`none` = explicit no-op memory)
    ///
    /// `qdrant` uses `[memory.qdrant]` config or `QDRANT_URL` env var.
    pub backend: String,
    /// Auto-save user-stated conversation input to memory (assistant output is excluded)
    pub auto_save: bool,
    /// Run memory/session hygiene (archiving + retention cleanup)
    #[serde(default = "default_hygiene_enabled")]
    pub hygiene_enabled: bool,
    /// Archive daily/session files older than this many days
    #[serde(default = "default_archive_after_days")]
    pub archive_after_days: u32,
    /// Purge archived files older than this many days
    #[serde(default = "default_purge_after_days")]
    pub purge_after_days: u32,
    /// For sqlite backend: prune conversation rows older than this many days
    #[serde(default = "default_conversation_retention_days")]
    pub conversation_retention_days: u32,
    /// Embedding provider: "none" | "openai" | "custom:URL"
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,
    /// Embedding model name (e.g. "text-embedding-3-small")
    #[serde(default = "default_embedding_model")]
    pub embedding_model: String,
    /// Embedding vector dimensions
    #[serde(default = "default_embedding_dims")]
    pub embedding_dimensions: usize,
    /// Weight for vector similarity in hybrid search (0.0–1.0)
    #[serde(default = "default_vector_weight")]
    pub vector_weight: f64,
    /// Weight for keyword BM25 in hybrid search (0.0–1.0)
    #[serde(default = "default_keyword_weight")]
    pub keyword_weight: f64,
    /// Search strategy: bm25 (keyword only), embedding (vector only), or hybrid (both).
    #[serde(default)]
    pub search_mode: SearchMode,
    /// Minimum hybrid score (0.0–1.0) for a memory to be included in context.
    /// Memories scoring below this threshold are dropped to prevent irrelevant
    /// context from bleeding into conversations. Default: 0.4
    #[serde(default = "default_min_relevance_score")]
    pub min_relevance_score: f64,
    /// Max embedding cache entries before LRU eviction
    #[serde(default = "default_cache_size")]
    pub embedding_cache_size: usize,
    /// Max tokens per chunk for document splitting
    #[serde(default = "default_chunk_size")]
    pub chunk_max_tokens: usize,

    // ── Response Cache (saves tokens on repeated prompts) ──────
    /// Enable LLM response caching to avoid paying for duplicate prompts
    #[serde(default)]
    pub response_cache_enabled: bool,
    /// TTL in minutes for cached responses (default: 60)
    #[serde(default = "default_response_cache_ttl")]
    pub response_cache_ttl_minutes: u32,
    /// Max number of cached responses before LRU eviction (default: 5000)
    #[serde(default = "default_response_cache_max")]
    pub response_cache_max_entries: usize,
    /// Max in-memory hot cache entries for the two-tier response cache (default: 256)
    #[serde(default = "default_response_cache_hot_entries")]
    pub response_cache_hot_entries: usize,

    // ── Memory Snapshot (soul backup to Markdown) ─────────────
    /// Enable periodic export of core memories to MEMORY_SNAPSHOT.md
    #[serde(default)]
    pub snapshot_enabled: bool,
    /// Run snapshot during hygiene passes (heartbeat-driven)
    #[serde(default)]
    pub snapshot_on_hygiene: bool,
    /// Auto-hydrate from MEMORY_SNAPSHOT.md when brain.db is missing
    #[serde(default = "default_true")]
    pub auto_hydrate: bool,

    // ── Retrieval Pipeline ─────────────────────────────────────
    /// Retrieval stages to execute in order. Valid: "cache", "fts", "vector".
    #[serde(default = "default_retrieval_stages")]
    pub retrieval_stages: Vec<String>,
    /// Enable LLM reranking when candidate count exceeds threshold.
    #[serde(default)]
    pub rerank_enabled: bool,
    /// Minimum candidate count to trigger reranking.
    #[serde(default = "default_rerank_threshold")]
    pub rerank_threshold: usize,
    /// FTS score above which to early-return without vector search (0.0–1.0).
    #[serde(default = "default_fts_early_return_score")]
    pub fts_early_return_score: f64,

    // ── Namespace Isolation ─────────────────────────────────────
    /// Default namespace for memory entries.
    #[serde(default = "default_namespace")]
    pub default_namespace: String,

    // ── Conflict Resolution ─────────────────────────────────────
    /// Cosine similarity threshold for conflict detection (0.0–1.0).
    #[serde(default = "default_conflict_threshold")]
    pub conflict_threshold: f64,

    // ── Audit Trail ─────────────────────────────────────────────
    /// Enable audit logging of memory operations.
    #[serde(default)]
    pub audit_enabled: bool,
    /// Retention period for audit entries in days (default: 30).
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u32,

    // ── Policy Engine ───────────────────────────────────────────
    /// Memory policy configuration.
    #[serde(default)]
    #[nested]
    pub policy: MemoryPolicyConfig,

    // ── SQLite backend options ─────────────────────────────────
    /// For sqlite backend: max seconds to wait when opening the DB (e.g. file locked).
    /// None = wait indefinitely (default). Recommended max: 300.
    #[serde(default)]
    pub sqlite_open_timeout_secs: Option<u64>,

    // ── Qdrant backend options ─────────────────────────────────
    /// Configuration for Qdrant vector database backend.
    /// Only used when `backend = "qdrant"`.
    #[serde(default)]
    #[nested]
    pub qdrant: QdrantConfig,
}

/// Memory policy configuration (`[memory.policy]` section).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "memory.policy"]
pub struct MemoryPolicyConfig {
    /// Maximum entries per namespace (0 = unlimited).
    #[serde(default)]
    pub max_entries_per_namespace: usize,
    /// Maximum entries per category (0 = unlimited).
    #[serde(default)]
    pub max_entries_per_category: usize,
    /// Retention days by category (overrides global). Keys: "core", "daily", "conversation".
    #[serde(default)]
    pub retention_days_by_category: std::collections::HashMap<String, u32>,
    /// Namespaces that are read-only (writes are rejected).
    #[serde(default)]
    pub read_only_namespaces: Vec<String>,
}

fn default_retrieval_stages() -> Vec<String> {
    vec!["cache".into(), "fts".into(), "vector".into()]
}
fn default_rerank_threshold() -> usize {
    5
}
fn default_fts_early_return_score() -> f64 {
    0.85
}
fn default_namespace() -> String {
    "default".into()
}
fn default_conflict_threshold() -> f64 {
    0.85
}
fn default_audit_retention_days() -> u32 {
    30
}

fn default_embedding_provider() -> String {
    "none".into()
}
fn default_hygiene_enabled() -> bool {
    true
}
fn default_archive_after_days() -> u32 {
    7
}
fn default_purge_after_days() -> u32 {
    30
}
fn default_conversation_retention_days() -> u32 {
    30
}
fn default_embedding_model() -> String {
    "text-embedding-3-small".into()
}
fn default_embedding_dims() -> usize {
    1536
}
fn default_vector_weight() -> f64 {
    0.7
}
fn default_keyword_weight() -> f64 {
    0.3
}
fn default_min_relevance_score() -> f64 {
    0.4
}
fn default_cache_size() -> usize {
    10_000
}
fn default_chunk_size() -> usize {
    512
}
fn default_response_cache_ttl() -> u32 {
    60
}
fn default_response_cache_max() -> usize {
    5_000
}

fn default_response_cache_hot_entries() -> usize {
    256
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: "sqlite".into(),
            auto_save: true,
            hygiene_enabled: default_hygiene_enabled(),
            archive_after_days: default_archive_after_days(),
            purge_after_days: default_purge_after_days(),
            conversation_retention_days: default_conversation_retention_days(),
            embedding_provider: default_embedding_provider(),
            embedding_model: default_embedding_model(),
            embedding_dimensions: default_embedding_dims(),
            vector_weight: default_vector_weight(),
            keyword_weight: default_keyword_weight(),
            search_mode: SearchMode::default(),
            min_relevance_score: default_min_relevance_score(),
            embedding_cache_size: default_cache_size(),
            chunk_max_tokens: default_chunk_size(),
            response_cache_enabled: false,
            response_cache_ttl_minutes: default_response_cache_ttl(),
            response_cache_max_entries: default_response_cache_max(),
            response_cache_hot_entries: default_response_cache_hot_entries(),
            snapshot_enabled: false,
            snapshot_on_hygiene: false,
            auto_hydrate: true,
            retrieval_stages: default_retrieval_stages(),
            rerank_enabled: false,
            rerank_threshold: default_rerank_threshold(),
            fts_early_return_score: default_fts_early_return_score(),
            default_namespace: default_namespace(),
            conflict_threshold: default_conflict_threshold(),
            audit_enabled: false,
            audit_retention_days: default_audit_retention_days(),
            policy: MemoryPolicyConfig::default(),
            sqlite_open_timeout_secs: None,
            qdrant: QdrantConfig::default(),
        }
    }
}

// ── Observability ─────────────────────────────────────────────────

/// Observability backend configuration (`[observability]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "observability"]
pub struct ObservabilityConfig {
    /// "none" | "log" | "verbose" | "prometheus" | "otel"
    pub backend: String,

    /// OTLP endpoint (e.g. "http://localhost:4318"). Only used when backend = "otel".
    #[serde(default)]
    pub otel_endpoint: Option<String>,

    /// Service name reported to the OTel collector. Defaults to "zeroclaw".
    #[serde(default)]
    pub otel_service_name: Option<String>,

    /// Optional HTTP headers sent with every OTLP export request (e.g. authorization).
    /// Specified as key-value pairs in TOML:
    /// ```toml
    /// [observability.otel_headers]
    /// Authorization = "Bearer sk-..."
    /// ```
    #[serde(default)]
    pub otel_headers: Option<std::collections::HashMap<String, String>>,

    /// Runtime trace storage mode: "none" | "rolling" | "full".
    /// Controls whether model replies and tool-call diagnostics are persisted.
    #[serde(default = "default_runtime_trace_mode")]
    pub runtime_trace_mode: String,

    /// Runtime trace file path. Relative paths are resolved under workspace_dir.
    #[serde(default = "default_runtime_trace_path")]
    pub runtime_trace_path: String,

    /// Maximum entries retained when runtime_trace_mode = "rolling".
    #[serde(default = "default_runtime_trace_max_entries")]
    pub runtime_trace_max_entries: usize,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            backend: "none".into(),
            otel_endpoint: None,
            otel_service_name: None,
            otel_headers: None,
            runtime_trace_mode: default_runtime_trace_mode(),
            runtime_trace_path: default_runtime_trace_path(),
            runtime_trace_max_entries: default_runtime_trace_max_entries(),
        }
    }
}

fn default_runtime_trace_mode() -> String {
    "none".to_string()
}

fn default_runtime_trace_path() -> String {
    "state/runtime-trace.jsonl".to_string()
}

fn default_runtime_trace_max_entries() -> usize {
    200
}

// ── Hooks ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "hooks"]
pub struct HooksConfig {
    /// Enable lifecycle hook execution.
    ///
    /// Hooks run in-process with the same privileges as the main runtime.
    /// Keep enabled hook handlers narrowly scoped and auditable.
    pub enabled: bool,
    #[serde(default)]
    #[nested]
    pub builtin: BuiltinHooksConfig,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin: BuiltinHooksConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "hooks.builtin"]
pub struct BuiltinHooksConfig {
    /// Enable the command-logger hook (logs tool calls for auditing).
    pub command_logger: bool,
    /// Configuration for the webhook-audit hook.
    ///
    /// When enabled, POSTs a JSON payload to `url` for every tool invocation
    /// that matches one of `tool_patterns`.
    #[serde(default)]
    #[nested]
    pub webhook_audit: WebhookAuditConfig,
}

/// Configuration for the webhook-audit builtin hook.
///
/// Sends an HTTP POST with a JSON body to an external endpoint each time
/// a tool call matches one of the configured patterns. Useful for
/// centralised audit logging, SIEM ingestion, or compliance pipelines.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "hooks.builtin.webhook-audit"]
pub struct WebhookAuditConfig {
    /// Enable the webhook-audit hook. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Target URL that will receive the audit POST requests.
    #[serde(default)]
    pub url: String,
    /// Glob patterns for tool names to audit (e.g. `["Bash", "Write"]`).
    /// An empty list means **no** tools are audited.
    #[serde(default)]
    pub tool_patterns: Vec<String>,
    /// Include tool call arguments in the audit payload. Default: `false`.
    ///
    /// Be mindful of sensitive data — arguments may contain secrets or PII.
    #[serde(default)]
    pub include_args: bool,
    /// Maximum size (in bytes) of serialised arguments included in a single
    /// audit payload. Arguments exceeding this limit are truncated.
    /// Default: `4096`.
    #[serde(default = "default_max_args_bytes")]
    pub max_args_bytes: u64,
}

fn default_max_args_bytes() -> u64 {
    4096
}

impl Default for WebhookAuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            tool_patterns: Vec::new(),
            include_args: false,
            max_args_bytes: default_max_args_bytes(),
        }
    }
}

// ── Autonomy / Security ──────────────────────────────────────────

/// Autonomy and security policy configuration (`[autonomy]` section).
///
/// Controls what the agent is allowed to do: shell commands, filesystem access,
/// risk approval gates, and per-policy budgets.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "autonomy"]
#[serde(default)]
pub struct AutonomyConfig {
    /// Autonomy level: `read_only`, `supervised` (default), or `full`.
    pub level: AutonomyLevel,
    /// Restrict absolute filesystem paths to workspace-relative references. Default: `true`.
    /// Resolved paths outside the workspace still require `allowed_roots`.
    pub workspace_only: bool,
    /// Allowlist of executable names permitted for shell execution.
    pub allowed_commands: Vec<String>,
    /// Explicit path denylist. Default includes system-critical paths and sensitive dotdirs.
    pub forbidden_paths: Vec<String>,
    /// Maximum actions allowed per hour per policy. Default: `100`.
    pub max_actions_per_hour: u32,
    /// Maximum cost per day in cents per policy. Default: `1000`.
    pub max_cost_per_day_cents: u32,

    /// Require explicit approval for medium-risk shell commands.
    #[serde(default = "default_true")]
    pub require_approval_for_medium_risk: bool,

    /// Block high-risk shell commands even if allowlisted.
    #[serde(default = "default_true")]
    pub block_high_risk_commands: bool,

    /// Additional environment variables allowed for shell tool subprocesses.
    ///
    /// These names are explicitly allowlisted and merged with the built-in safe
    /// baseline (`PATH`, `HOME`, etc.) after `env_clear()`.
    #[serde(default)]
    pub shell_env_passthrough: Vec<String>,

    /// Tools that never require approval (e.g. read-only tools).
    #[serde(default = "default_auto_approve")]
    pub auto_approve: Vec<String>,

    /// Tools that always require interactive approval, even after "Always".
    #[serde(default = "default_always_ask")]
    pub always_ask: Vec<String>,

    /// Extra directory roots the agent may read/write outside the workspace.
    /// Supports absolute, `~/...`, and workspace-relative entries.
    /// Resolved paths under any of these roots pass `is_resolved_path_allowed`.
    #[serde(default)]
    pub allowed_roots: Vec<String>,

    /// Tools to exclude from non-CLI channels (e.g. Telegram, Discord).
    ///
    /// When a tool is listed here, non-CLI channels will not expose it to the
    /// model in tool specs.
    #[serde(default)]
    pub non_cli_excluded_tools: Vec<String>,

    /// Timeout in seconds for shell tool subprocesses. Default: 60.
    #[serde(default = "default_shell_timeout_secs")]
    pub shell_timeout_secs: u64,
}

fn default_shell_timeout_secs() -> u64 {
    60
}

fn default_auto_approve() -> Vec<String> {
    vec![
        "file_read".into(),
        "memory_recall".into(),
        "web_search_tool".into(),
        "web_fetch".into(),
        "calculator".into(),
        "glob_search".into(),
        "content_search".into(),
        "image_info".into(),
        "weather".into(),
        "browser".into(),
        "browser_open".into(),
    ]
}

fn default_always_ask() -> Vec<String> {
    vec![]
}

impl AutonomyConfig {
    /// Merge the built-in default `auto_approve` entries into the current
    /// list, preserving any user-supplied additions.
    pub fn ensure_default_auto_approve(&mut self) {
        let defaults = default_auto_approve();
        for entry in defaults {
            if !self.auto_approve.iter().any(|existing| existing == &entry) {
                self.auto_approve.push(entry);
            }
        }
    }
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            level: AutonomyLevel::Supervised,
            workspace_only: true,
            allowed_commands: vec![
                "git".into(),
                "npm".into(),
                "cargo".into(),
                "ls".into(),
                "cat".into(),
                "grep".into(),
                "find".into(),
                "echo".into(),
                "pwd".into(),
                "wc".into(),
                "head".into(),
                "tail".into(),
                "date".into(),
                "python".into(),
                "python3".into(),
                "pip".into(),
                "node".into(),
            ],
            forbidden_paths: vec![
                "/etc".into(),
                "/root".into(),
                "/home".into(),
                "/usr".into(),
                "/bin".into(),
                "/sbin".into(),
                "/lib".into(),
                "/opt".into(),
                "/boot".into(),
                "/dev".into(),
                "/proc".into(),
                "/sys".into(),
                "/var".into(),
                "/tmp".into(),
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
                "~/.config".into(),
            ],
            max_actions_per_hour: 20,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            shell_env_passthrough: vec![],
            auto_approve: default_auto_approve(),
            always_ask: default_always_ask(),
            allowed_roots: Vec::new(),
            non_cli_excluded_tools: Vec::new(),
            shell_timeout_secs: default_shell_timeout_secs(),
        }
    }
}

// ── Runtime ──────────────────────────────────────────────────────

/// Runtime adapter configuration (`[runtime]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "runtime"]
pub struct RuntimeConfig {
    /// Runtime kind (`native` | `docker`).
    #[serde(default = "default_runtime_kind")]
    pub kind: String,

    /// Docker runtime settings (used when `kind = "docker"`).
    #[serde(default)]
    #[nested]
    pub docker: DockerRuntimeConfig,

    /// Global reasoning override for providers that expose explicit controls.
    /// - `None`: provider default behavior
    /// - `Some(true)`: request reasoning/thinking when supported
    /// - `Some(false)`: disable reasoning/thinking when supported
    #[serde(default)]
    pub reasoning_enabled: Option<bool>,
    /// Optional reasoning effort for providers that expose a level control.
    #[serde(default, deserialize_with = "deserialize_reasoning_effort_opt")]
    pub reasoning_effort: Option<String>,
}

/// Docker runtime configuration (`[runtime.docker]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "runtime.docker"]
pub struct DockerRuntimeConfig {
    /// Runtime image used to execute shell commands.
    #[serde(default = "default_docker_image")]
    pub image: String,

    /// Docker network mode (`none`, `bridge`, etc.).
    #[serde(default = "default_docker_network")]
    pub network: String,

    /// Optional memory limit in MB (`None` = no explicit limit).
    #[serde(default = "default_docker_memory_limit_mb")]
    pub memory_limit_mb: Option<u64>,

    /// Optional CPU limit (`None` = no explicit limit).
    #[serde(default = "default_docker_cpu_limit")]
    pub cpu_limit: Option<f64>,

    /// Mount root filesystem as read-only.
    #[serde(default = "default_true")]
    pub read_only_rootfs: bool,

    /// Mount configured workspace into `/workspace`.
    #[serde(default = "default_true")]
    pub mount_workspace: bool,

    /// Optional workspace root allowlist for Docker mount validation.
    #[serde(default)]
    pub allowed_workspace_roots: Vec<String>,
}

fn default_runtime_kind() -> String {
    "native".into()
}

fn default_docker_image() -> String {
    "alpine:3.20".into()
}

fn default_docker_network() -> String {
    "none".into()
}

fn default_docker_memory_limit_mb() -> Option<u64> {
    Some(512)
}

fn default_docker_cpu_limit() -> Option<f64> {
    Some(1.0)
}

impl Default for DockerRuntimeConfig {
    fn default() -> Self {
        Self {
            image: default_docker_image(),
            network: default_docker_network(),
            memory_limit_mb: default_docker_memory_limit_mb(),
            cpu_limit: default_docker_cpu_limit(),
            read_only_rootfs: true,
            mount_workspace: true,
            allowed_workspace_roots: Vec::new(),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            kind: default_runtime_kind(),
            docker: DockerRuntimeConfig::default(),
            reasoning_enabled: None,
            reasoning_effort: None,
        }
    }
}

// ── Reliability / supervision ────────────────────────────────────

/// Reliability and supervision configuration (`[reliability]` section).
///
/// Controls provider retries, fallback chains, API key rotation, and channel restart backoff.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "reliability"]
pub struct ReliabilityConfig {
    /// Retries per provider before failing over.
    #[serde(default = "default_provider_retries")]
    pub provider_retries: u32,
    /// Base backoff (ms) for provider retry delay.
    #[serde(default = "default_provider_backoff_ms")]
    pub provider_backoff_ms: u64,
    /// Fallback provider chain (e.g. `["anthropic", "openai"]`).
    #[serde(default)]
    pub fallback_providers: Vec<String>,
    /// Additional API keys for round-robin rotation on rate-limit (429) errors.
    /// The primary `api_key` is always tried first; these are extras.
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Per-model fallback chains. When a model fails, try these alternatives in order.
    /// Example: `{ "claude-opus-4-20250514" = ["claude-sonnet-4-20250514", "gpt-4o"] }`
    #[serde(default)]
    pub model_fallbacks: std::collections::HashMap<String, Vec<String>>,
    /// Initial backoff for channel/daemon restarts.
    #[serde(default = "default_channel_backoff_secs")]
    pub channel_initial_backoff_secs: u64,
    /// Max backoff for channel/daemon restarts.
    #[serde(default = "default_channel_backoff_max_secs")]
    pub channel_max_backoff_secs: u64,
    /// Scheduler polling cadence in seconds.
    #[serde(default = "default_scheduler_poll_secs")]
    pub scheduler_poll_secs: u64,
    /// Max retries for cron job execution attempts.
    #[serde(default = "default_scheduler_retries")]
    pub scheduler_retries: u32,
}

fn default_provider_retries() -> u32 {
    2
}

fn default_provider_backoff_ms() -> u64 {
    500
}

fn default_channel_backoff_secs() -> u64 {
    2
}

fn default_channel_backoff_max_secs() -> u64 {
    60
}

fn default_scheduler_poll_secs() -> u64 {
    15
}

fn default_scheduler_retries() -> u32 {
    2
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            provider_retries: default_provider_retries(),
            provider_backoff_ms: default_provider_backoff_ms(),
            fallback_providers: Vec::new(),
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: default_channel_backoff_secs(),
            channel_max_backoff_secs: default_channel_backoff_max_secs(),
            scheduler_poll_secs: default_scheduler_poll_secs(),
            scheduler_retries: default_scheduler_retries(),
        }
    }
}

// ── Scheduler ────────────────────────────────────────────────────

/// Scheduler configuration for periodic task execution (`[scheduler]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "scheduler"]
pub struct SchedulerConfig {
    /// Enable the built-in scheduler loop.
    #[serde(default = "default_scheduler_enabled")]
    pub enabled: bool,
    /// Maximum number of persisted scheduled tasks.
    #[serde(default = "default_scheduler_max_tasks")]
    pub max_tasks: usize,
    /// Maximum tasks executed per scheduler polling cycle.
    #[serde(default = "default_scheduler_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_scheduler_enabled() -> bool {
    true
}

fn default_scheduler_max_tasks() -> usize {
    64
}

fn default_scheduler_max_concurrent() -> usize {
    4
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            enabled: default_scheduler_enabled(),
            max_tasks: default_scheduler_max_tasks(),
            max_concurrent: default_scheduler_max_concurrent(),
        }
    }
}

// ── Model routing ────────────────────────────────────────────────

/// Route a task hint to a specific provider + model.
///
/// ```toml
/// [[model_routes]]
/// hint = "reasoning"
/// provider = "openrouter"
/// model = "anthropic/claude-opus-4-20250514"
///
/// [[model_routes]]
/// hint = "fast"
/// provider = "groq"
/// model = "llama-3.3-70b-versatile"
/// ```
///
/// Usage: pass `hint:reasoning` as the model parameter to route the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelRouteConfig {
    /// Task hint name (e.g. "reasoning", "fast", "code", "summarize")
    pub hint: String,
    /// Provider to route to (must match a known provider name)
    pub provider: String,
    /// Model to use with that provider
    pub model: String,
    /// Optional API key override for this route's provider
    #[serde(default)]
    pub api_key: Option<String>,
}

// ── Embedding routing ───────────────────────────────────────────

/// Route an embedding hint to a specific provider + model.
///
/// ```toml
/// [[embedding_routes]]
/// hint = "semantic"
/// provider = "openai"
/// model = "text-embedding-3-small"
/// dimensions = 1536
///
/// [memory]
/// embedding_model = "hint:semantic"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EmbeddingRouteConfig {
    /// Route hint name (e.g. "semantic", "archive", "faq")
    pub hint: String,
    /// Embedding provider (`none`, `openai`, or `custom:<url>`)
    pub provider: String,
    /// Embedding model to use with that provider
    pub model: String,
    /// Optional embedding dimension override for this route
    #[serde(default)]
    pub dimensions: Option<usize>,
    /// Optional API key override for this route's provider
    #[serde(default)]
    pub api_key: Option<String>,
}

// ── Query Classification ─────────────────────────────────────────

/// Automatic query classification — classifies user messages by keyword/pattern
/// and routes to the appropriate model hint. Disabled by default.
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "query-classification"]
pub struct QueryClassificationConfig {
    /// Enable automatic query classification. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Classification rules evaluated in priority order.
    #[serde(default)]
    pub rules: Vec<ClassificationRule>,
}

/// A single classification rule mapping message patterns to a model hint.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ClassificationRule {
    /// Must match a `[[model_routes]]` hint value.
    pub hint: String,
    /// Case-insensitive substring matches.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Case-sensitive literal matches (for "```", "fn ", etc.).
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Only match if message length >= N chars.
    #[serde(default)]
    pub min_length: Option<usize>,
    /// Only match if message length <= N chars.
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Higher priority rules are checked first.
    #[serde(default)]
    pub priority: i32,
}

// ── Heartbeat ────────────────────────────────────────────────────

/// Heartbeat configuration for periodic health pings (`[heartbeat]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "heartbeat"]
#[allow(clippy::struct_excessive_bools)]
pub struct HeartbeatConfig {
    /// Enable periodic heartbeat pings. Default: `true`.
    pub enabled: bool,
    /// Interval in minutes between heartbeat pings. Minimum: `1`. Default: `30`.
    #[serde(default = "default_heartbeat_interval")]
    pub interval_minutes: u32,
    /// Enable two-phase heartbeat: Phase 1 asks LLM whether to run, Phase 2
    /// executes only when the LLM decides there is work to do. Saves API cost
    /// during quiet periods. Default: `true`.
    #[serde(default = "default_two_phase")]
    pub two_phase: bool,
    /// Optional fallback task text when `HEARTBEAT.md` has no task entries.
    #[serde(default)]
    pub message: Option<String>,
    /// Optional delivery channel for heartbeat output (for example: `telegram`).
    /// When omitted, auto-selects the first configured channel.
    #[serde(default, alias = "channel")]
    pub target: Option<String>,
    /// Optional delivery recipient/chat identifier (required when `target` is
    /// explicitly set).
    #[serde(default, alias = "recipient")]
    pub to: Option<String>,
    /// Enable adaptive intervals that back off on failures and speed up for
    /// high-priority tasks. Default: `false`.
    #[serde(default)]
    pub adaptive: bool,
    /// Minimum interval in minutes when adaptive mode is enabled. Default: `5`.
    #[serde(default = "default_heartbeat_min_interval")]
    pub min_interval_minutes: u32,
    /// Maximum interval in minutes when adaptive mode backs off. Default: `120`.
    #[serde(default = "default_heartbeat_max_interval")]
    pub max_interval_minutes: u32,
    /// Dead-man's switch timeout in minutes. If the heartbeat has not ticked
    /// within this window, an alert is sent. `0` disables. Default: `0`.
    #[serde(default)]
    pub deadman_timeout_minutes: u32,
    /// Channel for dead-man's switch alerts (e.g. `telegram`). Falls back to
    /// the heartbeat delivery channel.
    #[serde(default)]
    pub deadman_channel: Option<String>,
    /// Recipient for dead-man's switch alerts. Falls back to `to`.
    #[serde(default)]
    pub deadman_to: Option<String>,
    /// Maximum number of heartbeat run history records to retain. Default: `100`.
    #[serde(default = "default_heartbeat_max_run_history")]
    pub max_run_history: u32,
    /// Load the channel session history before each heartbeat task execution so
    /// the LLM has conversational context. Default: `false`.
    ///
    /// When `true`, the session file for the configured `target`/`to` is passed
    /// to the agent as `session_state_file`, giving it access to the recent
    /// conversation history — just as if the user had sent a message.
    #[serde(default)]
    pub load_session_context: bool,
    /// Maximum wall-clock seconds allowed for a single agent invocation
    /// (Phase 1 decision or Phase 2 task execution). `0` disables.
    /// Default: `600` (10 minutes).
    #[serde(default = "default_heartbeat_task_timeout")]
    pub task_timeout_secs: u64,
}

fn default_heartbeat_interval() -> u32 {
    30
}

fn default_two_phase() -> bool {
    true
}

fn default_heartbeat_min_interval() -> u32 {
    5
}

fn default_heartbeat_max_interval() -> u32 {
    120
}

fn default_heartbeat_max_run_history() -> u32 {
    100
}

fn default_heartbeat_task_timeout() -> u64 {
    600
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: default_heartbeat_interval(),
            two_phase: true,
            message: None,
            target: None,
            to: None,
            adaptive: false,
            min_interval_minutes: default_heartbeat_min_interval(),
            max_interval_minutes: default_heartbeat_max_interval(),
            deadman_timeout_minutes: 0,
            deadman_channel: None,
            deadman_to: None,
            max_run_history: default_heartbeat_max_run_history(),
            load_session_context: false,
            task_timeout_secs: default_heartbeat_task_timeout(),
        }
    }
}

// ── Cron ────────────────────────────────────────────────────────

/// Cron job configuration (`[cron]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "cron"]
pub struct CronConfig {
    /// Enable the cron subsystem. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Run all overdue jobs at scheduler startup. Default: `true`.
    ///
    /// When the machine boots late or the daemon restarts, jobs whose
    /// `next_run` is in the past are considered "missed". With this
    /// option enabled the scheduler fires them once before entering
    /// the normal polling loop. Disable if you prefer missed jobs to
    /// simply wait for their next scheduled occurrence.
    #[serde(default = "default_true")]
    pub catch_up_on_startup: bool,
    /// Maximum number of historical cron run records to retain. Default: `50`.
    #[serde(default = "default_max_run_history")]
    pub max_run_history: u32,
    /// Declarative cron job definitions (`[[cron.jobs]]`).
    ///
    /// Jobs declared here are synced into the database at scheduler startup.
    /// They use `source = "declarative"` to distinguish them from jobs
    /// created imperatively via CLI or API. Declarative config takes
    /// precedence on each sync: if the config changes, the DB is updated
    /// to match. Imperative jobs are never deleted by the sync process.
    #[serde(default)]
    pub jobs: Vec<CronJobDecl>,
}

/// A declarative cron job definition for the `[[cron.jobs]]` config array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CronJobDecl {
    /// Stable identifier used for merge semantics across syncs.
    pub id: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Job type: `"shell"` (default) or `"agent"`.
    #[serde(default = "default_job_type_decl")]
    pub job_type: String,
    /// Schedule for the job.
    pub schedule: CronScheduleDecl,
    /// Shell command to run (required when `job_type = "shell"`).
    #[serde(default)]
    pub command: Option<String>,
    /// Agent prompt (required when `job_type = "agent"`).
    #[serde(default)]
    pub prompt: Option<String>,
    /// Whether the job is enabled. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Model override for agent jobs.
    #[serde(default)]
    pub model: Option<String>,
    /// Allowlist of tool names for agent jobs.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Session target: `"isolated"` (default) or `"main"`.
    #[serde(default)]
    pub session_target: Option<String>,
    /// Delivery configuration.
    #[serde(default)]
    pub delivery: Option<DeliveryConfigDecl>,
}

/// Schedule variant for declarative cron jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CronScheduleDecl {
    /// Classic cron expression.
    Cron {
        expr: String,
        #[serde(default)]
        tz: Option<String>,
    },
    /// Interval in milliseconds.
    Every { every_ms: u64 },
    /// One-shot at an RFC 3339 timestamp.
    At { at: String },
}

/// Delivery configuration for declarative cron jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DeliveryConfigDecl {
    /// Delivery mode: `"none"` or `"announce"`.
    #[serde(default = "default_delivery_mode")]
    pub mode: String,
    /// Channel name (e.g. `"telegram"`, `"discord"`).
    #[serde(default)]
    pub channel: Option<String>,
    /// Target/recipient identifier.
    #[serde(default)]
    pub to: Option<String>,
    /// Best-effort delivery. Default: `true`.
    #[serde(default = "default_true")]
    pub best_effort: bool,
}

fn default_job_type_decl() -> String {
    "shell".to_string()
}

fn default_delivery_mode() -> String {
    "none".to_string()
}

fn default_max_run_history() -> u32 {
    50
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            catch_up_on_startup: true,
            max_run_history: default_max_run_history(),
            jobs: Vec::new(),
        }
    }
}

// ── Tunnel ──────────────────────────────────────────────────────

/// Tunnel configuration for exposing the gateway publicly (`[tunnel]` section).
///
/// Supported providers: `"none"` (default), `"cloudflare"`, `"tailscale"`, `"ngrok"`, `"openvpn"`, `"pinggy"`, `"custom"`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel"]
pub struct TunnelConfig {
    /// Tunnel provider: `"none"`, `"cloudflare"`, `"tailscale"`, `"ngrok"`, `"openvpn"`, `"pinggy"`, or `"custom"`. Default: `"none"`.
    pub provider: String,

    /// Cloudflare Tunnel configuration (used when `provider = "cloudflare"`).
    #[serde(default)]
    #[nested]
    pub cloudflare: Option<CloudflareTunnelConfig>,

    /// Tailscale Funnel/Serve configuration (used when `provider = "tailscale"`).
    #[serde(default)]
    #[nested]
    pub tailscale: Option<TailscaleTunnelConfig>,

    /// ngrok tunnel configuration (used when `provider = "ngrok"`).
    #[serde(default)]
    #[nested]
    pub ngrok: Option<NgrokTunnelConfig>,

    /// OpenVPN tunnel configuration (used when `provider = "openvpn"`).
    #[serde(default)]
    #[nested]
    pub openvpn: Option<OpenVpnTunnelConfig>,

    /// Custom tunnel command configuration (used when `provider = "custom"`).
    #[serde(default)]
    #[nested]
    pub custom: Option<CustomTunnelConfig>,

    /// Pinggy tunnel configuration (used when `provider = "pinggy"`).
    #[serde(default)]
    #[nested]
    pub pinggy: Option<PinggyTunnelConfig>,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            cloudflare: None,
            tailscale: None,
            ngrok: None,
            openvpn: None,
            custom: None,
            pinggy: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.cloudflare"]
pub struct CloudflareTunnelConfig {
    /// Cloudflare Tunnel token (from Zero Trust dashboard)
    #[serde(default)]
    #[secret]
    pub token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.tailscale"]
pub struct TailscaleTunnelConfig {
    /// Use Tailscale Funnel (public internet) vs Serve (tailnet only)
    #[serde(default)]
    pub funnel: bool,
    /// Optional hostname override
    #[serde(default)]
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.ngrok"]
pub struct NgrokTunnelConfig {
    /// ngrok auth token
    #[serde(default)]
    #[secret]
    pub auth_token: String,
    /// Optional custom domain
    #[serde(default)]
    pub domain: Option<String>,
}

/// OpenVPN tunnel configuration (`[tunnel.openvpn]`).
///
/// Required when `tunnel.provider = "openvpn"`. Omitting this section entirely
/// preserves previous behavior. Setting `tunnel.provider = "none"` (or removing
/// the `[tunnel.openvpn]` block) cleanly reverts to no-tunnel mode.
///
/// Defaults: `connect_timeout_secs = 30`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.openvpn"]
pub struct OpenVpnTunnelConfig {
    /// Path to `.ovpn` configuration file (must not be empty).
    pub config_file: String,
    /// Optional path to auth credentials file (`--auth-user-pass`).
    #[serde(default)]
    pub auth_file: Option<String>,
    /// Advertised address once VPN is connected (e.g., `"10.8.0.2:42617"`).
    /// When omitted the tunnel falls back to `http://{local_host}:{local_port}`.
    #[serde(default)]
    pub advertise_address: Option<String>,
    /// Connection timeout in seconds (default: 30, must be > 0).
    #[serde(default = "default_openvpn_timeout")]
    pub connect_timeout_secs: u64,
    /// Extra openvpn CLI arguments forwarded verbatim.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

fn default_openvpn_timeout() -> u64 {
    30
}

impl Default for OpenVpnTunnelConfig {
    fn default() -> Self {
        Self {
            config_file: String::new(),
            auth_file: None,
            advertise_address: None,
            connect_timeout_secs: default_openvpn_timeout(),
            extra_args: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.pinggy"]
pub struct PinggyTunnelConfig {
    /// Pinggy access token (optional — free tier works without one).
    #[serde(default)]
    #[secret]
    pub token: Option<String>,
    /// Server region: `"us"` (USA), `"eu"` (Europe), `"ap"` (Asia), `"br"` (South America), `"au"` (Australia), or omit for auto.
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "tunnel.custom"]
pub struct CustomTunnelConfig {
    /// Command template to start the tunnel. Use {port} and {host} placeholders.
    /// Example: "bore local {port} --to bore.pub"
    #[serde(default)]
    pub start_command: String,
    /// Optional URL to check tunnel health
    #[serde(default)]
    pub health_url: Option<String>,
    /// Optional regex to extract public URL from command stdout
    #[serde(default)]
    pub url_pattern: Option<String>,
}

// ── Channels ─────────────────────────────────────────────────────

struct ConfigWrapper<T: ChannelConfig>(std::marker::PhantomData<T>);

impl<T: ChannelConfig> ConfigWrapper<T> {
    fn new(_: Option<&T>) -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: ChannelConfig> crate::traits::ConfigHandle for ConfigWrapper<T> {
    fn name(&self) -> &'static str {
        T::name()
    }
    fn desc(&self) -> &'static str {
        T::desc()
    }
}

/// Top-level channel configurations (`[channels]` section).
///
/// Each channel sub-section (e.g. `telegram`, `discord`) is optional;
/// setting it to `Some(...)` enables that channel.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels"]
pub struct ChannelsConfig {
    /// Enable the CLI interactive channel. Default: `true`.
    #[serde(default = "default_true")]
    pub cli: bool,
    /// Telegram bot channel configuration.
    #[nested]
    pub telegram: Option<TelegramConfig>,
    /// Discord bot channel configuration.
    #[nested]
    pub discord: Option<DiscordConfig>,
    /// Discord history channel — logs ALL messages and forwards @mentions to agent.
    #[nested]
    pub discord_history: Option<DiscordHistoryConfig>,
    /// Slack bot channel configuration.
    #[nested]
    pub slack: Option<SlackConfig>,
    /// Mattermost bot channel configuration.
    #[nested]
    pub mattermost: Option<MattermostConfig>,
    /// Webhook channel configuration.
    #[nested]
    pub webhook: Option<WebhookConfig>,
    /// iMessage channel configuration (macOS only).
    #[nested]
    pub imessage: Option<IMessageConfig>,
    /// Matrix channel configuration.
    #[nested]
    pub matrix: Option<MatrixConfig>,
    /// Signal channel configuration.
    #[nested]
    pub signal: Option<SignalConfig>,
    /// WhatsApp channel configuration (Cloud API or Web mode).
    #[nested]
    pub whatsapp: Option<WhatsAppConfig>,
    /// Linq Partner API channel configuration.
    #[nested]
    pub linq: Option<LinqConfig>,
    /// WATI WhatsApp Business API channel configuration.
    #[nested]
    pub wati: Option<WatiConfig>,
    /// Nextcloud Talk bot channel configuration.
    #[nested]
    pub nextcloud_talk: Option<NextcloudTalkConfig>,
    /// Email channel configuration.
    #[nested]
    pub email: Option<crate::scattered_types::EmailConfig>,
    /// Gmail Pub/Sub push notification channel configuration.
    #[nested]
    pub gmail_push: Option<crate::scattered_types::GmailPushConfig>,
    /// IRC channel configuration.
    #[nested]
    pub irc: Option<IrcConfig>,
    /// Lark channel configuration.
    #[nested]
    pub lark: Option<LarkConfig>,
    /// LINE Messaging API channel configuration.
    #[nested]
    pub line: Option<LineConfig>,
    /// Feishu channel configuration.
    #[nested]
    pub feishu: Option<FeishuConfig>,
    /// DingTalk channel configuration.
    #[nested]
    pub dingtalk: Option<DingTalkConfig>,
    /// WeCom (WeChat Enterprise) Bot Webhook channel configuration.
    #[nested]
    pub wecom: Option<WeComConfig>,
    /// QQ Official Bot channel configuration.
    #[nested]
    pub qq: Option<QQConfig>,
    /// X/Twitter channel configuration.
    #[nested]
    pub twitter: Option<TwitterConfig>,
    /// Mochat customer service channel configuration.
    #[nested]
    pub mochat: Option<MochatConfig>,
    #[cfg(feature = "channel-nostr")]
    #[nested]
    pub nostr: Option<NostrConfig>,
    /// ClawdTalk voice channel configuration.
    #[nested]
    pub clawdtalk: Option<crate::scattered_types::ClawdTalkConfig>,
    /// Reddit channel configuration (OAuth2 bot).
    #[nested]
    pub reddit: Option<RedditConfig>,
    /// Bluesky channel configuration (AT Protocol).
    #[nested]
    pub bluesky: Option<BlueskyConfig>,
    /// Voice call channel configuration (Twilio/Telnyx/Plivo).
    #[nested]
    pub voice_call: Option<crate::scattered_types::VoiceCallConfig>,
    /// One2X WebSocket real-time channel configuration.
    #[cfg(feature = "one2x")]
    #[nested]
    pub web: Option<crate::scattered_types::WebChannelConfig>,
    /// Voice wake word detection channel configuration.
    #[cfg(feature = "voice-wake")]
    #[nested]
    pub voice_wake: Option<VoiceWakeConfig>,
    /// MQTT channel configuration (SOP listener).
    #[nested]
    pub mqtt: Option<MqttConfig>,
    /// Base timeout in seconds for processing a single channel message (LLM + tools).
    /// Runtime uses this as a per-turn budget that scales with tool-loop depth
    /// (up to 4x, capped) so one slow/retried model call does not consume the
    /// entire conversation budget.
    /// Default: 300s for on-device LLMs (Ollama) which are slower than cloud APIs.
    #[serde(default = "default_channel_message_timeout_secs")]
    pub message_timeout_secs: u64,
    /// Whether to add acknowledgement reactions (👀 on receipt, ✅/⚠️ on
    /// completion) to incoming channel messages. Default: `true`.
    #[serde(default = "default_true")]
    pub ack_reactions: bool,
    /// Whether to send tool-call notification messages (e.g. `🔧 web_search_tool: …`)
    /// to channel users. When `false`, tool calls are still logged server-side but
    /// not forwarded as individual channel messages. Default: `false`.
    #[serde(default = "default_false")]
    pub show_tool_calls: bool,
    /// Persist channel conversation history to JSONL files so sessions survive
    /// daemon restarts. Files are stored in `{workspace}/sessions/`. Default: `true`.
    #[serde(default = "default_true")]
    pub session_persistence: bool,
    /// Session persistence backend: `"jsonl"` (legacy) or `"sqlite"` (new default).
    /// SQLite provides FTS5 search, metadata tracking, and TTL cleanup.
    #[serde(default = "default_session_backend")]
    pub session_backend: String,
    /// Auto-archive stale sessions older than this many hours. `0` disables. Default: `0`.
    #[serde(default)]
    pub session_ttl_hours: u32,
    /// Inbound message debounce window in milliseconds. When a sender fires
    /// multiple messages within this window, they are accumulated and dispatched
    /// as a single concatenated message. `0` disables debouncing. Default: `0`.
    #[serde(default)]
    pub debounce_ms: u64,
}

impl ChannelsConfig {
    /// Backfill `enabled = true` for channel sections present in the raw TOML
    /// that don't have an explicit `enabled` key. This preserves backward
    /// compatibility: configs written before `enabled` was introduced continue
    /// to activate their channels.
    pub fn backfill_enabled(&mut self, raw_toml: &str) {
        let mut table = match raw_toml.parse::<toml::Table>() {
            Ok(t) => t,
            Err(_) => return,
        };
        crate::migration::prepare_table(&mut table);
        let channels = match table.get("channels").and_then(|v| v.as_table()) {
            Some(t) => t,
            None => return,
        };
        for (key, value) in channels {
            let is_section = value.as_table().is_some();
            let has_explicit_enabled = value.as_table().is_some_and(|t| t.contains_key("enabled"));
            if is_section && !has_explicit_enabled {
                // Section exists without explicit `enabled` — backfill true
                let prop_path = format!("channels.{}.enabled", key.replace('_', "-"));
                if let Err(e) = self.set_prop(&prop_path, "true") {
                    tracing::warn!("backfill_enabled: failed to set {prop_path}: {e}");
                }
            }
        }
    }

    /// get channels' metadata and `.is_some()`, except webhook
    #[rustfmt::skip]
    pub fn channels_except_webhook(&self) -> Vec<(Box<dyn super::traits::ConfigHandle>, bool)> {
        vec![
            (
                Box::new(ConfigWrapper::new(self.telegram.as_ref())),
                self.telegram.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.discord.as_ref())),
                self.discord.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.slack.as_ref())),
                self.slack.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.mattermost.as_ref())),
                self.mattermost.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.imessage.as_ref())),
                self.imessage.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.matrix.as_ref())),
                self.matrix.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.signal.as_ref())),
                self.signal.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.whatsapp.as_ref())),
                self.whatsapp.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.linq.as_ref())),
                self.linq.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.wati.as_ref())),
                self.wati.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.nextcloud_talk.as_ref())),
                self.nextcloud_talk.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.email.as_ref())),
                self.email.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.gmail_push.as_ref())),
                self.gmail_push.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.irc.as_ref())),
                self.irc.is_some()
            ),
            (
                Box::new(ConfigWrapper::new(self.lark.as_ref())),
                self.lark.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.feishu.as_ref())),
                self.feishu.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.dingtalk.as_ref())),
                self.dingtalk.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.wecom.as_ref())),
                self.wecom.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.qq.as_ref())),
                self.qq.is_some()
            ),
            #[cfg(feature = "channel-nostr")]
            (
                Box::new(ConfigWrapper::new(self.nostr.as_ref())),
                self.nostr.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.clawdtalk.as_ref())),
                self.clawdtalk.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.reddit.as_ref())),
                self.reddit.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.bluesky.as_ref())),
                self.bluesky.is_some(),
            ),
            #[cfg(feature = "one2x")]
            (
                Box::new(ConfigWrapper::new(self.web.as_ref())),
                self.web.is_some(),
            ),
            #[cfg(feature = "voice-wake")]
            (
                Box::new(ConfigWrapper::new(self.voice_wake.as_ref())),
                self.voice_wake.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.mqtt.as_ref())),
                self.mqtt.is_some(),
            ),
        ]
    }

    pub fn channels(&self) -> Vec<(Box<dyn super::traits::ConfigHandle>, bool)> {
        let mut ret = self.channels_except_webhook();
        ret.push((
            Box::new(ConfigWrapper::new(self.webhook.as_ref())),
            self.webhook.is_some(),
        ));
        ret
    }
}

fn default_channel_message_timeout_secs() -> u64 {
    300
}

fn default_session_backend() -> String {
    "sqlite".into()
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            cli: true,
            telegram: None,
            discord: None,
            discord_history: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: None,
            matrix: None,
            signal: None,
            whatsapp: None,
            linq: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            gmail_push: None,
            irc: None,
            lark: None,
            line: None,
            feishu: None,
            dingtalk: None,
            wecom: None,
            qq: None,
            twitter: None,
            mochat: None,
            #[cfg(feature = "channel-nostr")]
            nostr: None,
            clawdtalk: None,
            reddit: None,
            bluesky: None,
            voice_call: None,
            #[cfg(feature = "one2x")]
            web: None,
            #[cfg(feature = "voice-wake")]
            voice_wake: None,
            mqtt: None,
            message_timeout_secs: default_channel_message_timeout_secs(),
            ack_reactions: true,
            show_tool_calls: false,
            session_persistence: true,
            session_backend: default_session_backend(),
            session_ttl_hours: 0,
            debounce_ms: 0,
        }
    }
}

/// Streaming mode for channels that support progressive message updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    /// No streaming -- send the complete response as a single message (default).
    #[default]
    Off,
    /// Update a draft message with every flush interval.
    Partial,
    /// Send the response as multiple separate messages at paragraph boundaries.
    #[serde(rename = "multi_message")]
    MultiMessage,
}

fn default_draft_update_interval_ms() -> u64 {
    1000
}

fn default_multi_message_delay_ms() -> u64 {
    800
}

fn default_telegram_approval_timeout_secs() -> u64 {
    120
}

fn default_matrix_draft_update_interval_ms() -> u64 {
    1500
}

/// Telegram bot channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.telegram"]
pub struct TelegramConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Telegram Bot API token (from @BotFather).
    #[secret]
    pub bot_token: String,
    /// Allowed Telegram user IDs or usernames. Empty = deny all.
    pub allowed_users: Vec<String>,
    /// Streaming mode for progressive response delivery via message edits.
    #[serde(default)]
    pub stream_mode: StreamMode,
    /// Minimum interval (ms) between draft message edits to avoid rate limits.
    #[serde(default = "default_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// When true, a newer Telegram message from the same sender in the same chat
    /// cancels the in-flight request and starts a fresh response with preserved history.
    #[serde(default)]
    pub interrupt_on_new_message: bool,
    /// When true, only respond to messages that @-mention the bot in groups.
    /// Direct messages are always processed.
    #[serde(default)]
    pub mention_only: bool,
    /// Override for the top-level `ack_reactions` setting. When `None`, the
    /// channel falls back to `[channels].ack_reactions`. When set
    /// explicitly, it takes precedence.
    #[serde(default)]
    pub ack_reactions: Option<bool>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// How long (seconds) to wait for the operator to tap an inline-keyboard
    /// button on a tool approval prompt before auto-denying. Default: 120.
    #[serde(default = "default_telegram_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
}

impl ChannelConfig for TelegramConfig {
    fn name() -> &'static str {
        "Telegram"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

/// Discord bot channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.discord"]
#[allow(clippy::struct_excessive_bools)]
pub struct DiscordConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Discord bot token (from Discord Developer Portal).
    #[secret]
    pub bot_token: String,
    /// Optional guild (server) ID to restrict the bot to a single guild.
    pub guild_id: Option<String>,
    /// Allowed Discord user IDs. Empty = deny all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// When true, process messages from other bots (not just humans).
    /// The bot still ignores its own messages to prevent feedback loops.
    #[serde(default)]
    pub listen_to_bots: bool,
    /// When true, a newer Discord message from the same sender in the same channel
    /// cancels the in-flight request and starts a fresh response with preserved history.
    #[serde(default)]
    pub interrupt_on_new_message: bool,
    /// When true, only respond to messages that @-mention the bot.
    /// Other messages in the guild are silently ignored.
    #[serde(default)]
    pub mention_only: bool,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Streaming mode for progressive response delivery.
    /// `off` (default): single message. `partial`: editable draft updates.
    /// `multi_message`: split response into separate messages at paragraph boundaries.
    #[serde(default)]
    pub stream_mode: StreamMode,
    /// Minimum interval (ms) between draft message edits to avoid rate limits.
    /// Only used when `stream_mode = "partial"`.
    #[serde(default = "default_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// Delay (ms) between sending each message chunk in multi-message mode.
    /// Only used when `stream_mode = "multi_message"`.
    #[serde(default = "default_multi_message_delay_ms")]
    pub multi_message_delay_ms: u64,
    /// Stall-watchdog timeout in seconds. When non-zero, the bot will abort
    /// and retry if no progress is made within this duration. 0 = disabled.
    #[serde(default)]
    pub stall_timeout_secs: u64,
}

impl ChannelConfig for DiscordConfig {
    fn name() -> &'static str {
        "Discord"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

/// Discord history channel — logs ALL messages to discord.db and forwards @mentions to the agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.discord-history"]
pub struct DiscordHistoryConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Discord bot token (from Discord Developer Portal).
    #[secret]
    pub bot_token: String,
    /// Optional guild (server) ID to restrict logging to a single guild.
    pub guild_id: Option<String>,
    /// Allowed Discord user IDs. Empty = allow all (open logging).
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Discord channel IDs to watch. Empty = watch all channels.
    #[serde(default)]
    pub channel_ids: Vec<String>,
    /// When true (default), store Direct Messages in discord.db.
    #[serde(default = "default_true")]
    pub store_dms: bool,
    /// When true (default), respond to @mentions in Direct Messages.
    #[serde(default = "default_true")]
    pub respond_to_dms: bool,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for DiscordHistoryConfig {
    fn name() -> &'static str {
        "Discord History"
    }
    fn desc() -> &'static str {
        "log all messages and forward @mentions"
    }
}

/// Slack bot channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.slack"]
#[allow(clippy::struct_excessive_bools)]
pub struct SlackConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Slack bot OAuth token (xoxb-...).
    #[secret]
    pub bot_token: String,
    /// Slack app-level token for Socket Mode (xapp-...).
    #[secret]
    pub app_token: Option<String>,
    /// Explicit list of channel IDs to watch.
    /// Empty = listen across all accessible channels.
    /// Migrated from the legacy `channel_id` singular field.
    #[serde(default)]
    pub channel_ids: Vec<String>,
    /// Allowed Slack user IDs. Empty = deny all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// When true, a newer Slack message from the same sender in the same channel
    /// cancels the in-flight request and starts a fresh response with preserved history.
    #[serde(default)]
    pub interrupt_on_new_message: bool,
    /// When true (default), replies stay in the originating Slack thread.
    /// When false, replies go to the channel root instead.
    #[serde(default)]
    pub thread_replies: Option<bool>,
    /// When true, only respond to messages that @-mention the bot in groups.
    /// Direct messages remain allowed.
    #[serde(default)]
    pub mention_only: bool,
    /// Use the newer Slack `markdown` block type (12 000 char limit, richer formatting).
    /// Defaults to false (uses universally supported `section` blocks with `mrkdwn`).
    /// Enable this only if your Slack workspace supports the `markdown` block type.
    #[serde(default)]
    pub use_markdown_blocks: bool,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Enable progressive draft message streaming via `chat.update`.
    #[serde(default)]
    pub stream_drafts: bool,
    /// Minimum interval (ms) between draft message edits to avoid Slack rate limits.
    #[serde(default = "default_slack_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// Emoji reaction name (without colons) that cancels an in-flight request.
    /// For example, `"x"` means reacting with `:x:` cancels the task.
    /// Leave unset to disable reaction-based cancellation.
    #[serde(default)]
    pub cancel_reaction: Option<String>,
}

fn default_slack_draft_update_interval_ms() -> u64 {
    1200
}

impl ChannelConfig for SlackConfig {
    fn name() -> &'static str {
        "Slack"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

/// Mattermost bot channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.mattermost"]
pub struct MattermostConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Mattermost server URL (e.g. `"https://mattermost.example.com"`).
    pub url: String,
    /// Mattermost bot access token.
    #[secret]
    pub bot_token: String,
    /// Optional channel ID to restrict the bot to a single channel.
    pub channel_id: Option<String>,
    /// Allowed Mattermost user IDs. Empty = deny all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// When true (default), replies thread on the original post.
    /// When false, replies go to the channel root.
    #[serde(default)]
    pub thread_replies: Option<bool>,
    /// When true, only respond to messages that @-mention the bot.
    /// Other messages in the channel are silently ignored.
    #[serde(default)]
    pub mention_only: Option<bool>,
    /// When true, a newer Mattermost message from the same sender in the same channel
    /// cancels the in-flight request and starts a fresh response with preserved history.
    #[serde(default)]
    pub interrupt_on_new_message: bool,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for MattermostConfig {
    fn name() -> &'static str {
        "Mattermost"
    }
    fn desc() -> &'static str {
        "connect to your bot"
    }
}

/// Webhook channel configuration.
///
/// Receives messages via HTTP POST and sends replies to a configurable outbound URL.
/// This is the "universal adapter" for any system that supports webhooks.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.webhook"]
pub struct WebhookConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Port to listen on for incoming webhooks.
    pub port: u16,
    /// URL path to listen on (default: `/webhook`).
    #[serde(default)]
    pub listen_path: Option<String>,
    /// URL to POST/PUT outbound messages to.
    #[serde(default)]
    pub send_url: Option<String>,
    /// HTTP method for outbound messages (`POST` or `PUT`). Default: `POST`.
    #[serde(default)]
    pub send_method: Option<String>,
    /// Optional `Authorization` header value for outbound requests.
    #[serde(default)]
    pub auth_header: Option<String>,
    /// Optional shared secret for webhook signature verification (HMAC-SHA256).
    #[secret]
    pub secret: Option<String>,
}

impl ChannelConfig for WebhookConfig {
    fn name() -> &'static str {
        "Webhook"
    }
    fn desc() -> &'static str {
        "HTTP endpoint"
    }
}

/// iMessage channel configuration (macOS only).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.imessage"]
pub struct IMessageConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Allowed iMessage contacts (phone numbers or email addresses). Empty = deny all.
    pub allowed_contacts: Vec<String>,
}

impl ChannelConfig for IMessageConfig {
    fn name() -> &'static str {
        "iMessage"
    }
    fn desc() -> &'static str {
        "macOS only"
    }
}

/// Matrix channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.matrix"]
pub struct MatrixConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Matrix homeserver URL (e.g. `"https://matrix.org"`).
    pub homeserver: String,
    /// Matrix access token for the bot account.
    #[secret]
    pub access_token: String,
    /// Optional Matrix user ID (e.g. `"@bot:matrix.org"`).
    #[serde(default)]
    pub user_id: Option<String>,
    /// Optional Matrix device ID.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Allowed Matrix user IDs. Empty = deny all.
    pub allowed_users: Vec<String>,
    /// Allowed Matrix room IDs or aliases. Empty = allow all rooms.
    /// Supports canonical room IDs (`!abc:server`) and aliases (`#room:server`).
    #[serde(default)]
    pub allowed_rooms: Vec<String>,
    /// Whether to interrupt an in-flight agent response when a new message arrives.
    #[serde(default)]
    pub interrupt_on_new_message: bool,
    /// Streaming mode for progressive response delivery.
    /// `"off"` (default): single message. `"partial"`: edit-in-place draft.
    /// `"multi_message"`: paragraph-split delivery.
    #[serde(default)]
    pub stream_mode: StreamMode,
    /// Minimum interval (ms) between draft message edits in Partial mode.
    #[serde(default = "default_matrix_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// Delay (ms) between sending each paragraph in MultiMessage mode.
    #[serde(default = "default_multi_message_delay_ms")]
    pub multi_message_delay_ms: u64,
    /// When true, only respond to messages that @-mention the bot in groups.
    /// Direct messages are always processed.
    #[serde(default)]
    pub mention_only: bool,
    /// Optional Matrix recovery key for automatic E2EE key backup restore.
    /// When set, ZeroClaw recovers room keys and cross-signing secrets on startup.
    #[secret]
    #[serde(default)]
    pub recovery_key: Option<String>,
    /// Optional login password for Matrix account (used for initial login flow).
    #[secret]
    #[serde(default)]
    pub password: Option<String>,
}

impl ChannelConfig for MatrixConfig {
    fn name() -> &'static str {
        "Matrix"
    }
    fn desc() -> &'static str {
        "self-hosted chat"
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.signal"]
pub struct SignalConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Base URL for the signal-cli HTTP daemon (e.g. "http://127.0.0.1:8686").
    pub http_url: String,
    /// E.164 phone number of the signal-cli account (e.g. "+1234567890").
    pub account: String,
    /// Optional group ID to filter messages.
    /// - `None` or omitted: accept all messages (DMs and groups)
    /// - `"dm"`: only accept direct messages
    /// - Specific group ID: only accept messages from that group
    #[serde(default)]
    pub group_id: Option<String>,
    /// Allowed sender phone numbers (E.164) or "*" for all.
    #[serde(default)]
    pub allowed_from: Vec<String>,
    /// Skip messages that are attachment-only (no text body).
    #[serde(default)]
    pub ignore_attachments: bool,
    /// Skip incoming story messages.
    #[serde(default)]
    pub ignore_stories: bool,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for SignalConfig {
    fn name() -> &'static str {
        "Signal"
    }
    fn desc() -> &'static str {
        "An open-source, encrypted messaging service"
    }
}

/// WhatsApp Web usage mode.
///
/// `Personal` treats the account as a personal phone — the bot only responds to
/// incoming messages that pass the DM/group/self-chat policy filters.
/// `Business` (default) responds to all incoming messages, subject only to the
/// `allowed_numbers` allowlist.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WhatsAppWebMode {
    /// Respond to all messages passing the allowlist (default).
    #[default]
    Business,
    /// Apply per-chat-type policies (dm_policy, group_policy, self_chat_mode).
    Personal,
}

/// Policy for a particular WhatsApp chat type (DMs or groups) when
/// `mode = "personal"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WhatsAppChatPolicy {
    /// Only respond to senders on the `allowed_numbers` list (default).
    #[default]
    Allowlist,
    /// Ignore all messages in this chat type.
    Ignore,
    /// Respond to every message regardless of allowlist.
    All,
}

/// WhatsApp channel configuration (Cloud API or Web mode).
///
/// Set `phone_number_id` for Cloud API mode, or `session_path` for Web mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.whatsapp"]
pub struct WhatsAppConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Access token from Meta Business Suite (Cloud API mode)
    #[serde(default)]
    #[secret]
    pub access_token: Option<String>,
    /// Phone number ID from Meta Business API (Cloud API mode)
    #[serde(default)]
    pub phone_number_id: Option<String>,
    /// Webhook verify token (you define this, Meta sends it back for verification)
    /// Only used in Cloud API mode
    #[serde(default)]
    #[secret]
    pub verify_token: Option<String>,
    /// App secret from Meta Business Suite (for webhook signature verification)
    /// Can also be set via `ZEROCLAW_WHATSAPP_APP_SECRET` environment variable
    /// Only used in Cloud API mode
    #[serde(default)]
    #[secret]
    pub app_secret: Option<String>,
    /// Session database path for WhatsApp Web client (Web mode)
    /// When set, enables native WhatsApp Web mode with wa-rs
    #[serde(default)]
    pub session_path: Option<String>,
    /// Phone number for pair code linking (Web mode, optional)
    /// Format: country code + number (e.g., "15551234567")
    /// If not set, QR code pairing will be used
    #[serde(default)]
    pub pair_phone: Option<String>,
    /// Custom pair code for linking (Web mode, optional)
    /// Leave empty to let WhatsApp generate one
    #[serde(default)]
    pub pair_code: Option<String>,
    /// Allowed phone numbers (E.164 format: +1234567890) or "*" for all
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
    /// When true, only respond to messages that @-mention the bot in groups (Web mode only).
    /// Direct messages are always processed.
    /// Bot identity is resolved from the wa-rs device at runtime; `pair_phone` seeds it on first connect.
    #[serde(default)]
    pub mention_only: bool,
    /// Usage mode for WhatsApp Web: "business" (default) or "personal".
    /// In personal mode the bot applies dm_policy, group_policy, and
    /// self_chat_mode to decide which chats to respond in.
    #[serde(default)]
    pub mode: WhatsAppWebMode,
    /// Policy for direct messages when mode = "personal".
    /// "allowlist" (default) | "ignore" | "all".
    #[serde(default)]
    pub dm_policy: WhatsAppChatPolicy,
    /// Policy for group chats when mode = "personal".
    /// "allowlist" (default) | "ignore" | "all".
    #[serde(default)]
    pub group_policy: WhatsAppChatPolicy,
    /// When true and mode = "personal", always respond to messages in the
    /// user's own self-chat (Notes to Self). Defaults to false.
    #[serde(default)]
    pub self_chat_mode: bool,
    /// Regex patterns for DM mention gating (case-insensitive).
    /// When non-empty, only direct messages matching at least one pattern are
    /// processed; matched fragments are stripped from the forwarded content.
    /// Example: `["@?ZeroClaw", "\\+?15555550123"]`
    #[serde(default)]
    pub dm_mention_patterns: Vec<String>,
    /// Regex patterns for group-chat mention gating (case-insensitive).
    /// When non-empty, only group messages matching at least one pattern are
    /// processed; matched fragments are stripped from the forwarded content.
    /// Example: `["@?ZeroClaw", "\\+?15555550123"]`
    #[serde(default)]
    pub group_mention_patterns: Vec<String>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for WhatsAppConfig {
    fn name() -> &'static str {
        "WhatsApp"
    }
    fn desc() -> &'static str {
        "Business Cloud API"
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.linq"]
pub struct LinqConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Linq Partner API token (Bearer auth)
    #[secret]
    pub api_token: String,
    /// Phone number to send from (E.164 format)
    pub from_phone: String,
    /// Webhook signing secret for signature verification
    #[serde(default)]
    #[secret]
    pub signing_secret: Option<String>,
    /// Allowed sender handles (phone numbers) or "*" for all
    #[serde(default)]
    pub allowed_senders: Vec<String>,
}

impl ChannelConfig for LinqConfig {
    fn name() -> &'static str {
        "Linq"
    }
    fn desc() -> &'static str {
        "iMessage/RCS/SMS via Linq API"
    }
}

/// WATI WhatsApp Business API channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.wati"]
pub struct WatiConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// WATI API token (Bearer auth).
    #[secret]
    pub api_token: String,
    /// WATI API base URL (default: https://live-mt-server.wati.io).
    #[serde(default = "default_wati_api_url")]
    pub api_url: String,
    /// Tenant ID for multi-channel setups (optional).
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// Allowed phone numbers (E.164 format) or "*" for all.
    #[serde(default)]
    pub allowed_numbers: Vec<String>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

fn default_wati_api_url() -> String {
    "https://live-mt-server.wati.io".to_string()
}

impl ChannelConfig for WatiConfig {
    fn name() -> &'static str {
        "WATI"
    }
    fn desc() -> &'static str {
        "WhatsApp via WATI Business API"
    }
}

/// Nextcloud Talk bot configuration (webhook receive + OCS send API).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.nextcloud-talk"]
pub struct NextcloudTalkConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Nextcloud base URL (e.g. "https://cloud.example.com").
    pub base_url: String,
    /// Bot app token used for OCS API bearer auth.
    #[secret]
    pub app_token: String,
    /// Shared secret for webhook signature verification.
    ///
    /// Can also be set via `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET`.
    #[serde(default)]
    #[secret]
    pub webhook_secret: Option<String>,
    /// Allowed Nextcloud actor IDs (`[]` = deny all, `"*"` = allow all).
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Display name of the bot in Nextcloud Talk (e.g. "zeroclaw").
    /// Used to filter out the bot's own messages and prevent feedback loops.
    /// If not set, defaults to an empty string (no self-message filtering by name).
    #[serde(default)]
    pub bot_name: Option<String>,
}

impl ChannelConfig for NextcloudTalkConfig {
    fn name() -> &'static str {
        "NextCloud Talk"
    }
    fn desc() -> &'static str {
        "NextCloud Talk platform"
    }
}

impl WhatsAppConfig {
    /// Detect which backend to use based on config fields.
    /// Returns "cloud" if phone_number_id is set, "web" if session_path is set.
    pub fn backend_type(&self) -> &'static str {
        if self.phone_number_id.is_some() {
            "cloud"
        } else if self.session_path.is_some() {
            "web"
        } else {
            // Default to Cloud API for backward compatibility
            "cloud"
        }
    }

    /// Check if this is a valid Cloud API config
    pub fn is_cloud_config(&self) -> bool {
        self.phone_number_id.is_some() && self.access_token.is_some() && self.verify_token.is_some()
    }

    /// Check if this is a valid Web config
    pub fn is_web_config(&self) -> bool {
        self.session_path.is_some()
    }

    /// Returns true when both Cloud and Web selectors are present.
    ///
    /// Runtime currently prefers Cloud mode in this case for backward compatibility.
    pub fn is_ambiguous_config(&self) -> bool {
        self.phone_number_id.is_some() && self.session_path.is_some()
    }
}

/// MQTT channel configuration (SOP listener).
///
/// Subscribes to MQTT topics and dispatches incoming messages
/// to the SOP engine for processing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.mqtt"]
pub struct MqttConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// MQTT broker URL (e.g., `mqtt://localhost:1883` or `mqtts://broker.example.com:8883`).
    /// Use `mqtt://` for plain connections or `mqtts://` for TLS.
    pub broker_url: String,
    /// MQTT client ID (must be unique per broker).
    pub client_id: String,
    /// Topics to subscribe to (e.g., `sensors/#`, `alerts/+/critical`).
    /// At least one topic is required.
    #[serde(default)]
    pub topics: Vec<String>,
    /// MQTT QoS level (0 = at-most-once, 1 = at-least-once, 2 = exactly-once). Default: 1.
    #[serde(default = "default_mqtt_qos")]
    pub qos: u8,
    /// Username for authentication (optional).
    pub username: Option<String>,
    /// Password for authentication (optional).
    #[secret]
    pub password: Option<String>,
    /// Enable TLS encryption. Must match the broker_url scheme:
    /// - `mqtt://` → `use_tls: false`
    /// - `mqtts://` → `use_tls: true`
    #[serde(default)]
    pub use_tls: bool,
    /// Keep-alive interval in seconds (default: 30). Prevents broker disconnect on idle.
    #[serde(default = "default_mqtt_keep_alive_secs")]
    pub keep_alive_secs: u64,
}

impl MqttConfig {
    /// Validate the MQTT configuration.
    ///
    /// Checks:
    /// - QoS is 0, 1, or 2
    /// - broker_url uses valid scheme (`mqtt://` or `mqtts://`)
    /// - `use_tls` flag matches broker_url scheme
    /// - At least one topic is configured
    /// - client_id is non-empty
    pub fn validate(&self) -> anyhow::Result<()> {
        // QoS validation
        if self.qos > 2 {
            anyhow::bail!("qos must be 0, 1, or 2, got {}", self.qos);
        }

        // Broker URL validation
        let is_tls_scheme = self.broker_url.starts_with("mqtts://");
        let is_mqtt_scheme = self.broker_url.starts_with("mqtt://");

        if !is_tls_scheme && !is_mqtt_scheme {
            anyhow::bail!(
                "broker_url must start with 'mqtt://' or 'mqtts://', got: {}",
                self.broker_url
            );
        }

        // TLS flag validation
        if is_mqtt_scheme && self.use_tls {
            anyhow::bail!("use_tls is true but broker_url uses 'mqtt://' (not 'mqtts://')");
        }

        if is_tls_scheme && !self.use_tls {
            anyhow::bail!(
                "use_tls is false but broker_url uses 'mqtts://' (requires use_tls: true)"
            );
        }

        // Topics validation
        if self.topics.is_empty() {
            anyhow::bail!("at least one topic must be configured");
        }

        // Client ID validation
        if self.client_id.is_empty() {
            anyhow::bail!("client_id must not be empty");
        }

        Ok(())
    }
}

impl ChannelConfig for MqttConfig {
    fn name() -> &'static str {
        "MQTT"
    }
    fn desc() -> &'static str {
        "MQTT SOP Listener"
    }
}

fn default_mqtt_qos() -> u8 {
    1
}

fn default_mqtt_keep_alive_secs() -> u64 {
    30
}

/// IRC channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.irc"]
pub struct IrcConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// IRC server hostname
    pub server: String,
    /// IRC server port (default: 6697 for TLS)
    #[serde(default = "default_irc_port")]
    pub port: u16,
    /// Bot nickname
    pub nickname: String,
    /// Username (defaults to nickname if not set)
    pub username: Option<String>,
    /// Channels to join on connect
    #[serde(default)]
    pub channels: Vec<String>,
    /// Allowed nicknames (case-insensitive) or "*" for all
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Server password (for bouncers like ZNC)
    #[secret]
    pub server_password: Option<String>,
    /// NickServ IDENTIFY password
    #[secret]
    pub nickserv_password: Option<String>,
    /// SASL PLAIN password (IRCv3)
    #[secret]
    pub sasl_password: Option<String>,
    /// Verify TLS certificate (default: true)
    pub verify_tls: Option<bool>,
}

impl ChannelConfig for IrcConfig {
    fn name() -> &'static str {
        "IRC"
    }
    fn desc() -> &'static str {
        "IRC over TLS"
    }
}

fn default_irc_port() -> u16 {
    6697
}

/// How ZeroClaw receives events from Feishu / Lark.
///
/// - `websocket` (default) — persistent WSS long-connection; no public URL required.
/// - `webhook`             — HTTP callback server; requires a public HTTPS endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum LarkReceiveMode {
    #[default]
    Websocket,
    Webhook,
}

/// Lark/Feishu configuration for messaging integration.
/// Lark is the international version; Feishu is the Chinese version.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.lark"]
pub struct LarkConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// App ID from Lark/Feishu developer console
    pub app_id: String,
    /// App Secret from Lark/Feishu developer console
    #[secret]
    pub app_secret: String,
    /// Encrypt key for webhook message decryption (optional)
    #[serde(default)]
    #[secret]
    pub encrypt_key: Option<String>,
    /// Verification token for webhook validation (optional)
    #[serde(default)]
    #[secret]
    pub verification_token: Option<String>,
    /// Allowed user IDs or union IDs (empty = deny all, "*" = allow all)
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// When true, only respond to messages that @-mention the bot in groups.
    /// Direct messages are always processed.
    #[serde(default)]
    pub mention_only: bool,
    /// Whether to use the Feishu (Chinese) endpoint instead of Lark (International)
    #[serde(default)]
    pub use_feishu: bool,
    /// Event receive mode: "websocket" (default) or "webhook"
    #[serde(default)]
    pub receive_mode: LarkReceiveMode,
    /// HTTP port for webhook mode only. Must be set when receive_mode = "webhook".
    /// Not required (and ignored) for websocket mode.
    #[serde(default)]
    pub port: Option<u16>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for LarkConfig {
    fn name() -> &'static str {
        "Lark"
    }
    fn desc() -> &'static str {
        "Lark Bot"
    }
}

/// DM (1:1 chat) access policy for the LINE channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum LineDmPolicy {
    /// Respond to every DM regardless of who sent it.
    Open,
    /// Require a one-time `/bind <code>` handshake before responding (default).
    /// ZeroClaw prints the bind code on startup; send it once to unlock access.
    #[default]
    Pairing,
    /// Respond only to LINE user IDs listed in `allowed_users`.
    Allowlist,
}

/// Group / multi-person chat policy for the LINE channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum LineGroupPolicy {
    /// Respond to every message in group/room chats.
    Open,
    /// Respond only when the bot is @mentioned (default).
    #[default]
    Mention,
    /// Ignore all messages in group/room chats.
    Disabled,
}

/// LINE Messaging API channel configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.line"]
pub struct LineConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Long-lived channel access token (from LINE Developers Console).
    /// Used for both the Reply API and the Push API fallback.
    /// Falls back to the `LINE_CHANNEL_ACCESS_TOKEN` environment variable if empty.
    #[serde(default)]
    #[secret]
    pub channel_access_token: String,
    /// Channel secret (from LINE Developers Console).
    /// Used to verify the `X-Line-Signature` header on incoming webhooks.
    /// Falls back to the `LINE_CHANNEL_SECRET` environment variable if empty.
    #[serde(default)]
    #[secret]
    pub channel_secret: String,
    /// DM (1:1 chat) access policy. Default: `pairing`.
    ///
    /// - `open`      — respond to everyone
    /// - `pairing`   — require one-time `/bind <code>` handshake on first contact
    /// - `allowlist` — respond only to user IDs listed in `allowed_users`
    #[serde(default)]
    pub dm_policy: LineDmPolicy,
    /// Group / multi-person chat policy. Default: `mention`.
    ///
    /// - `open`     — respond to every message
    /// - `mention`  — respond only when @mentioned
    /// - `disabled` — ignore all group messages
    #[serde(default)]
    pub group_policy: LineGroupPolicy,
    /// LINE user IDs that are allowed to interact with the bot.
    /// Used when `dm_policy = allowlist`. `["*"]` accepts everyone.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// TCP port the embedded webhook server listens on. Default: `8443`.
    #[serde(default = "default_line_webhook_port")]
    pub webhook_port: u16,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

fn default_line_webhook_port() -> u16 {
    8443
}

impl ChannelConfig for LineConfig {
    fn name() -> &'static str {
        "LINE"
    }
    fn desc() -> &'static str {
        "connect your LINE bot"
    }
}

/// Feishu configuration for messaging integration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.feishu"]
pub struct FeishuConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// App ID from Feishu developer console
    pub app_id: String,
    /// App Secret from Feishu developer console
    #[secret]
    pub app_secret: String,
    /// Encrypt key for webhook message decryption (optional)
    #[serde(default)]
    #[secret]
    pub encrypt_key: Option<String>,
    /// Verification token for webhook validation (optional)
    #[serde(default)]
    #[secret]
    pub verification_token: Option<String>,
    /// Allowed user IDs or union IDs (empty = deny all, "*" = allow all)
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Event receive mode: "websocket" (default) or "webhook"
    #[serde(default)]
    pub receive_mode: LarkReceiveMode,
    /// HTTP port for webhook mode only. Must be set when receive_mode = "webhook".
    /// Not required (and ignored) for websocket mode.
    #[serde(default)]
    pub port: Option<u16>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for FeishuConfig {
    fn name() -> &'static str {
        "Feishu"
    }
    fn desc() -> &'static str {
        "Feishu Bot"
    }
}

// ── Security Config ─────────────────────────────────────────────────

/// Security configuration for sandboxing, resource limits, and audit logging
#[derive(Debug, Clone, Serialize, Deserialize, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security"]
pub struct SecurityConfig {
    /// Sandbox configuration
    #[serde(default)]
    #[nested]
    pub sandbox: SandboxConfig,

    /// Resource limits
    #[serde(default)]
    #[nested]
    pub resources: ResourceLimitsConfig,

    /// Audit logging configuration
    #[serde(default)]
    #[nested]
    pub audit: AuditConfig,

    /// OTP gating configuration for sensitive actions/domains.
    #[serde(default)]
    #[nested]
    pub otp: OtpConfig,

    /// Emergency-stop state machine configuration.
    #[serde(default)]
    #[nested]
    pub estop: EstopConfig,

    /// Nevis IAM integration for SSO/MFA authentication and role-based access.
    #[serde(default)]
    #[nested]
    pub nevis: NevisConfig,

    /// WebAuthn / FIDO2 hardware key authentication configuration.
    #[serde(default)]
    #[nested]
    pub webauthn: WebAuthnConfig,
}

/// WebAuthn / FIDO2 hardware key authentication configuration (`[security.webauthn]`).
///
/// Enables registration and authentication via hardware security keys
/// (YubiKey, SoloKey, etc.) and platform authenticators (Touch ID, Windows Hello).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.webauthn"]
pub struct WebAuthnConfig {
    /// Enable WebAuthn authentication. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Relying Party ID (domain name, e.g. "example.com"). Default: "localhost".
    #[serde(default = "default_webauthn_rp_id")]
    pub rp_id: String,
    /// Relying Party origin URL (e.g. "https://example.com"). Default: "http://localhost:42617".
    #[serde(default = "default_webauthn_rp_origin")]
    pub rp_origin: String,
    /// Relying Party display name. Default: "ZeroClaw".
    #[serde(default = "default_webauthn_rp_name")]
    pub rp_name: String,
}

impl Default for WebAuthnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            rp_id: default_webauthn_rp_id(),
            rp_origin: default_webauthn_rp_origin(),
            rp_name: default_webauthn_rp_name(),
        }
    }
}

fn default_webauthn_rp_id() -> String {
    "localhost".into()
}

fn default_webauthn_rp_origin() -> String {
    "http://localhost:42617".into()
}

fn default_webauthn_rp_name() -> String {
    "ZeroClaw".into()
}

/// OTP validation strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum OtpMethod {
    /// Time-based one-time password (RFC 6238).
    #[default]
    Totp,
    /// Future method for paired-device confirmations.
    Pairing,
    /// Future method for local CLI challenge prompts.
    CliPrompt,
}

/// Security OTP configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.otp"]
#[serde(deny_unknown_fields)]
pub struct OtpConfig {
    /// Enable OTP gating. Defaults to disabled for backward compatibility.
    #[serde(default)]
    pub enabled: bool,

    /// OTP method.
    #[serde(default)]
    pub method: OtpMethod,

    /// TOTP time-step in seconds.
    #[serde(default = "default_otp_token_ttl_secs")]
    pub token_ttl_secs: u64,

    /// Reuse window for recently validated OTP codes.
    #[serde(default = "default_otp_cache_valid_secs")]
    pub cache_valid_secs: u64,

    /// Tool/action names gated by OTP.
    #[serde(default = "default_otp_gated_actions")]
    pub gated_actions: Vec<String>,

    /// Explicit domain patterns gated by OTP.
    #[serde(default)]
    pub gated_domains: Vec<String>,

    /// Domain-category presets expanded into `gated_domains`.
    #[serde(default)]
    pub gated_domain_categories: Vec<String>,

    /// Maximum number of OTP challenge attempts before lockout.
    #[serde(default = "default_otp_challenge_max_attempts")]
    pub challenge_max_attempts: u32,
}

fn default_otp_token_ttl_secs() -> u64 {
    30
}

fn default_otp_cache_valid_secs() -> u64 {
    300
}

fn default_otp_challenge_max_attempts() -> u32 {
    3
}

fn default_otp_gated_actions() -> Vec<String> {
    vec![
        "shell".to_string(),
        "file_write".to_string(),
        "browser_open".to_string(),
        "browser".to_string(),
        "memory_forget".to_string(),
    ]
}

impl Default for OtpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            method: OtpMethod::Totp,
            token_ttl_secs: default_otp_token_ttl_secs(),
            cache_valid_secs: default_otp_cache_valid_secs(),
            gated_actions: default_otp_gated_actions(),
            gated_domains: Vec::new(),
            gated_domain_categories: Vec::new(),
            challenge_max_attempts: default_otp_challenge_max_attempts(),
        }
    }
}

/// Emergency stop configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.estop"]
#[serde(deny_unknown_fields)]
pub struct EstopConfig {
    /// Enable emergency stop controls.
    #[serde(default)]
    pub enabled: bool,

    /// File path used to persist estop state.
    #[serde(default = "default_estop_state_file")]
    pub state_file: String,

    /// Require a valid OTP before resume operations.
    #[serde(default = "default_true")]
    pub require_otp_to_resume: bool,
}

fn default_estop_state_file() -> String {
    "~/.zeroclaw/estop-state.json".to_string()
}

impl Default for EstopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            state_file: default_estop_state_file(),
            require_otp_to_resume: true,
        }
    }
}

/// Nevis IAM integration configuration.
///
/// When `enabled` is true, ZeroClaw validates incoming requests against a Nevis
/// Security Suite instance and maps Nevis roles to tool/workspace permissions.
#[derive(Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.nevis"]
#[serde(deny_unknown_fields)]
pub struct NevisConfig {
    /// Enable Nevis IAM integration. Defaults to false for backward compatibility.
    #[serde(default)]
    pub enabled: bool,

    /// Base URL of the Nevis instance (e.g. `https://nevis.example.com`).
    #[serde(default)]
    pub instance_url: String,

    /// Nevis realm to authenticate against.
    #[serde(default = "default_nevis_realm")]
    pub realm: String,

    /// OAuth2 client ID registered in Nevis.
    #[serde(default)]
    pub client_id: String,

    /// OAuth2 client secret. Encrypted via SecretStore when stored on disk.
    #[serde(default)]
    #[secret]
    pub client_secret: Option<String>,

    /// Token validation strategy: `"local"` (JWKS) or `"remote"` (introspection).
    #[serde(default = "default_nevis_token_validation")]
    pub token_validation: String,

    /// JWKS endpoint URL for local token validation.
    #[serde(default)]
    pub jwks_url: Option<String>,

    /// Nevis role to ZeroClaw permission mappings.
    #[serde(default)]
    pub role_mapping: Vec<NevisRoleMappingConfig>,

    /// Require MFA verification for all Nevis-authenticated requests.
    #[serde(default)]
    pub require_mfa: bool,

    /// Session timeout in seconds.
    #[serde(default = "default_nevis_session_timeout_secs")]
    pub session_timeout_secs: u64,
}

impl std::fmt::Debug for NevisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NevisConfig")
            .field("enabled", &self.enabled)
            .field("instance_url", &self.instance_url)
            .field("realm", &self.realm)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_validation", &self.token_validation)
            .field("jwks_url", &self.jwks_url)
            .field("role_mapping", &self.role_mapping)
            .field("require_mfa", &self.require_mfa)
            .field("session_timeout_secs", &self.session_timeout_secs)
            .finish()
    }
}

impl NevisConfig {
    /// Validate that required fields are present when Nevis is enabled.
    ///
    /// Call at config load time to fail fast on invalid configuration rather
    /// than deferring errors to the first authentication request.
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        if self.instance_url.trim().is_empty() {
            return Err("nevis.instance_url is required when Nevis IAM is enabled".into());
        }

        if self.client_id.trim().is_empty() {
            return Err("nevis.client_id is required when Nevis IAM is enabled".into());
        }

        if self.realm.trim().is_empty() {
            return Err("nevis.realm is required when Nevis IAM is enabled".into());
        }

        match self.token_validation.as_str() {
            "local" | "remote" => {}
            other => {
                return Err(format!(
                    "nevis.token_validation has invalid value '{other}': \
                     expected 'local' or 'remote'"
                ));
            }
        }

        if self.token_validation == "local" && self.jwks_url.is_none() {
            return Err("nevis.jwks_url is required when token_validation is 'local'".into());
        }

        if self.session_timeout_secs == 0 {
            return Err("nevis.session_timeout_secs must be greater than 0".into());
        }

        Ok(())
    }
}

fn default_nevis_realm() -> String {
    "master".into()
}

fn default_nevis_token_validation() -> String {
    "local".into()
}

fn default_nevis_session_timeout_secs() -> u64 {
    3600
}

impl Default for NevisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            instance_url: String::new(),
            realm: default_nevis_realm(),
            client_id: String::new(),
            client_secret: None,
            token_validation: default_nevis_token_validation(),
            jwks_url: None,
            role_mapping: Vec::new(),
            require_mfa: false,
            session_timeout_secs: default_nevis_session_timeout_secs(),
        }
    }
}

/// Maps a Nevis role to ZeroClaw tool permissions and workspace access.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NevisRoleMappingConfig {
    /// Nevis role name (case-insensitive).
    pub nevis_role: String,

    /// Tool names this role can access. Use `"all"` for unrestricted tool access.
    #[serde(default)]
    pub zeroclaw_permissions: Vec<String>,

    /// Workspace names this role can access. Use `"all"` for unrestricted.
    #[serde(default)]
    pub workspace_access: Vec<String>,
}

/// Sandbox configuration for OS-level isolation
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.sandbox"]
pub struct SandboxConfig {
    /// Enable sandboxing (None = auto-detect, Some = explicit)
    #[serde(default)]
    pub enabled: Option<bool>,

    /// Sandbox backend to use
    #[serde(default)]
    pub backend: SandboxBackend,

    /// Custom Firejail arguments (when backend = firejail)
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: None, // Auto-detect
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        }
    }
}

/// Sandbox backend selection
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    /// Auto-detect best available (default)
    #[default]
    Auto,
    /// Landlock (Linux kernel LSM, native)
    Landlock,
    /// Firejail (user-space sandbox)
    Firejail,
    /// Bubblewrap (user namespaces)
    Bubblewrap,
    /// Docker container isolation
    Docker,
    /// macOS sandbox-exec (Seatbelt)
    #[serde(alias = "sandbox-exec")]
    SandboxExec,
    /// No sandboxing (application-layer only)
    None,
}

/// Resource limits for command execution
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.resources"]
pub struct ResourceLimitsConfig {
    /// Maximum memory in MB per command
    #[serde(default = "default_max_memory_mb")]
    pub max_memory_mb: u32,

    /// Maximum CPU time in seconds per command
    #[serde(default = "default_max_cpu_time_seconds")]
    pub max_cpu_time_seconds: u64,

    /// Maximum number of subprocesses
    #[serde(default = "default_max_subprocesses")]
    pub max_subprocesses: u32,

    /// Enable memory monitoring
    #[serde(default = "default_memory_monitoring_enabled")]
    pub memory_monitoring: bool,
}

fn default_max_memory_mb() -> u32 {
    512
}

fn default_max_cpu_time_seconds() -> u64 {
    60
}

fn default_max_subprocesses() -> u32 {
    10
}

fn default_memory_monitoring_enabled() -> bool {
    true
}

impl Default for ResourceLimitsConfig {
    fn default() -> Self {
        Self {
            max_memory_mb: default_max_memory_mb(),
            max_cpu_time_seconds: default_max_cpu_time_seconds(),
            max_subprocesses: default_max_subprocesses(),
            memory_monitoring: default_memory_monitoring_enabled(),
        }
    }
}

/// Audit logging configuration
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.audit"]
pub struct AuditConfig {
    /// Enable audit logging
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,

    /// Path to audit log file (relative to zeroclaw dir)
    #[serde(default = "default_audit_log_path")]
    pub log_path: String,

    /// Maximum log size in MB before rotation
    #[serde(default = "default_audit_max_size_mb")]
    pub max_size_mb: u32,

    /// Sign events with HMAC for tamper evidence
    #[serde(default)]
    pub sign_events: bool,
}

fn default_audit_enabled() -> bool {
    true
}

fn default_audit_log_path() -> String {
    "audit.log".to_string()
}

fn default_audit_max_size_mb() -> u32 {
    100
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            log_path: default_audit_log_path(),
            max_size_mb: default_audit_max_size_mb(),
            sign_events: false,
        }
    }
}

/// DingTalk configuration for Stream Mode messaging
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.dingtalk"]
pub struct DingTalkConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Client ID (AppKey) from DingTalk developer console
    pub client_id: String,
    /// Client Secret (AppSecret) from DingTalk developer console
    #[secret]
    pub client_secret: String,
    /// Allowed user IDs (staff IDs). Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for DingTalkConfig {
    fn name() -> &'static str {
        "DingTalk"
    }
    fn desc() -> &'static str {
        "DingTalk Stream Mode"
    }
}

/// WeCom (WeChat Enterprise) Bot Webhook configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.wecom"]
pub struct WeComConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Webhook key from WeCom Bot configuration
    #[secret]
    pub webhook_key: String,
    /// Allowed user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl ChannelConfig for WeComConfig {
    fn name() -> &'static str {
        "WeCom"
    }
    fn desc() -> &'static str {
        "WeCom Bot Webhook"
    }
}

/// QQ Official Bot configuration (Tencent QQ Bot SDK)
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.qq"]
pub struct QQConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// App ID from QQ Bot developer console
    pub app_id: String,
    /// App Secret from QQ Bot developer console
    #[secret]
    pub app_secret: String,
    /// Allowed user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Per-channel proxy URL (http, https, socks5, socks5h).
    /// Overrides the global `[proxy]` setting for this channel only.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

impl ChannelConfig for QQConfig {
    fn name() -> &'static str {
        "QQ Official"
    }
    fn desc() -> &'static str {
        "Tencent QQ Bot"
    }
}

/// X/Twitter channel configuration (Twitter API v2)
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.twitter"]
pub struct TwitterConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Twitter API v2 Bearer Token (OAuth 2.0)
    #[secret]
    pub bearer_token: String,
    /// Allowed usernames or user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl ChannelConfig for TwitterConfig {
    fn name() -> &'static str {
        "X/Twitter"
    }
    fn desc() -> &'static str {
        "X/Twitter Bot via API v2"
    }
}

/// Mochat channel configuration (Mochat customer service API)
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.mochat"]
pub struct MochatConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Mochat API base URL
    pub api_url: String,
    /// Mochat API token
    #[secret]
    pub api_token: String,
    /// Allowed user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Poll interval in seconds for new messages. Default: 5
    #[serde(default = "default_mochat_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_mochat_poll_interval() -> u64 {
    5
}

impl ChannelConfig for MochatConfig {
    fn name() -> &'static str {
        "Mochat"
    }
    fn desc() -> &'static str {
        "Mochat Customer Service"
    }
}

/// Reddit channel configuration (OAuth2 bot).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.reddit"]
pub struct RedditConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Reddit OAuth2 client ID.
    pub client_id: String,
    /// Reddit OAuth2 client secret.
    #[secret]
    pub client_secret: String,
    /// Reddit OAuth2 refresh token for persistent access.
    #[secret]
    pub refresh_token: String,
    /// Reddit bot username (without `u/` prefix).
    pub username: String,
    /// Optional subreddit to filter messages (without `r/` prefix).
    /// When set, only messages from this subreddit are processed.
    #[serde(default)]
    pub subreddit: Option<String>,
}

impl ChannelConfig for RedditConfig {
    fn name() -> &'static str {
        "Reddit"
    }
    fn desc() -> &'static str {
        "Reddit bot (OAuth2)"
    }
}

/// Bluesky channel configuration (AT Protocol).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.bluesky"]
pub struct BlueskyConfig {
    /// Whether this channel is active (must be explicitly enabled). Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Bluesky handle (e.g. `"mybot.bsky.social"`).
    pub handle: String,
    /// App-specific password (from Bluesky settings).
    #[secret]
    pub app_password: String,
}

impl ChannelConfig for BlueskyConfig {
    fn name() -> &'static str {
        "Bluesky"
    }
    fn desc() -> &'static str {
        "AT Protocol"
    }
}

/// Voice wake word detection channel configuration.
///
/// Listens on the default microphone for a configurable wake word,
/// then captures the following utterance and transcribes it via the
/// existing transcription API.
#[cfg(feature = "voice-wake")]
#[derive(Debug, Clone, Serialize, Deserialize, zeroclaw_macros::Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "voice-wake"]
pub struct VoiceWakeConfig {
    /// Wake word phrase to listen for (case-insensitive substring match).
    /// Default: `"hey zeroclaw"`.
    #[serde(default = "default_voice_wake_word")]
    pub wake_word: String,
    /// Silence timeout in milliseconds — how long to wait after the last
    /// energy spike before finalizing a capture window. Default: `2000`.
    #[serde(default = "default_voice_wake_silence_timeout_ms")]
    pub silence_timeout_ms: u32,
    /// RMS energy threshold for voice activity detection. Samples below
    /// this level are treated as silence. Default: `0.01`.
    #[serde(default = "default_voice_wake_energy_threshold")]
    pub energy_threshold: f32,
    /// Maximum capture duration in seconds before forcing transcription.
    /// Default: `30`.
    #[serde(default = "default_voice_wake_max_capture_secs")]
    pub max_capture_secs: u32,
}

#[cfg(feature = "voice-wake")]
fn default_voice_wake_word() -> String {
    "hey zeroclaw".into()
}

#[cfg(feature = "voice-wake")]
fn default_voice_wake_silence_timeout_ms() -> u32 {
    2000
}

#[cfg(feature = "voice-wake")]
fn default_voice_wake_energy_threshold() -> f32 {
    0.01
}

#[cfg(feature = "voice-wake")]
fn default_voice_wake_max_capture_secs() -> u32 {
    30
}

#[cfg(feature = "voice-wake")]
impl Default for VoiceWakeConfig {
    fn default() -> Self {
        Self {
            wake_word: default_voice_wake_word(),
            silence_timeout_ms: default_voice_wake_silence_timeout_ms(),
            energy_threshold: default_voice_wake_energy_threshold(),
            max_capture_secs: default_voice_wake_max_capture_secs(),
        }
    }
}

#[cfg(feature = "voice-wake")]
impl ChannelConfig for VoiceWakeConfig {
    fn name() -> &'static str {
        "VoiceWake"
    }
    fn desc() -> &'static str {
        "voice wake word detection"
    }
}

/// Nostr channel configuration (NIP-04 + NIP-17 private messages)
#[cfg(feature = "channel-nostr")]
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "channels.nostr"]
pub struct NostrConfig {
    /// Whether this channel is active. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Private key in hex or nsec bech32 format
    #[secret]
    pub private_key: String,
    /// Relay URLs (wss://). Defaults to popular public relays if omitted.
    #[serde(default = "default_nostr_relays")]
    pub relays: Vec<String>,
    /// Allowed sender public keys (hex or npub). Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_pubkeys: Vec<String>,
}

#[cfg(feature = "channel-nostr")]
impl ChannelConfig for NostrConfig {
    fn name() -> &'static str {
        "Nostr"
    }
    fn desc() -> &'static str {
        "Nostr DMs"
    }
}

#[cfg(feature = "channel-nostr")]
pub fn default_nostr_relays() -> Vec<String> {
    vec![
        "wss://relay.damus.io".to_string(),
        "wss://nos.lol".to_string(),
        "wss://relay.primal.net".to_string(),
        "wss://relay.snort.social".to_string(),
    ]
}

// -- Notion --

/// Notion integration configuration (`[notion]`).
///
/// When `enabled = true`, the agent polls a Notion database for pending tasks
/// and exposes a `notion` tool for querying, reading, creating, and updating pages.
/// Requires `api_key` (or the `NOTION_API_KEY` env var) and `database_id`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "notion"]
pub struct NotionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    #[secret]
    pub api_key: String,
    #[serde(default)]
    pub database_id: String,
    #[serde(default = "default_notion_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default = "default_notion_status_prop")]
    pub status_property: String,
    #[serde(default = "default_notion_input_prop")]
    pub input_property: String,
    #[serde(default = "default_notion_result_prop")]
    pub result_property: String,
    #[serde(default = "default_notion_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default = "default_notion_recover_stale")]
    pub recover_stale: bool,
}

fn default_notion_poll_interval() -> u64 {
    5
}
fn default_notion_status_prop() -> String {
    "Status".into()
}
fn default_notion_input_prop() -> String {
    "Input".into()
}
fn default_notion_result_prop() -> String {
    "Result".into()
}
fn default_notion_max_concurrent() -> usize {
    4
}
fn default_notion_recover_stale() -> bool {
    true
}

impl Default for NotionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            database_id: String::new(),
            poll_interval_secs: default_notion_poll_interval(),
            status_property: default_notion_status_prop(),
            input_property: default_notion_input_prop(),
            result_property: default_notion_result_prop(),
            max_concurrent: default_notion_max_concurrent(),
            recover_stale: default_notion_recover_stale(),
        }
    }
}

/// Jira integration configuration (`[jira]`).
///
/// When `enabled = true`, registers the `jira` tool which can get tickets,
/// search with JQL, and add comments. Requires `base_url` and `api_token`
/// (or the `JIRA_API_TOKEN` env var).
///
/// ## Defaults
/// - `enabled`: `false`
/// - `allowed_actions`: `["get_ticket"]` — read-only by default.
///   Add `"search_tickets"` or `"comment_ticket"` to unlock them.
/// - `timeout_secs`: `30`
///
/// ## Auth
/// Jira Cloud uses HTTP Basic auth: `email` + `api_token`.
/// `api_token` is stored encrypted at rest; set it here or via `JIRA_API_TOKEN`.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "jira"]
pub struct JiraConfig {
    /// Enable the `jira` tool. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Atlassian instance base URL, e.g. `https://yourco.atlassian.net`.
    #[serde(default)]
    pub base_url: String,
    /// Jira account email used for Basic auth.
    #[serde(default)]
    pub email: String,
    /// Jira API token. Encrypted at rest. Falls back to `JIRA_API_TOKEN` env var.
    #[serde(default)]
    #[secret]
    pub api_token: String,
    /// Actions the agent is permitted to call.
    /// Valid values: `"get_ticket"`, `"search_tickets"`, `"comment_ticket"`.
    /// Defaults to `["get_ticket"]` (read-only).
    #[serde(default = "default_jira_allowed_actions")]
    pub allowed_actions: Vec<String>,
    /// Request timeout in seconds. Default: `30`.
    #[serde(default = "default_jira_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_jira_allowed_actions() -> Vec<String> {
    vec!["get_ticket".to_string()]
}

fn default_jira_timeout_secs() -> u64 {
    30
}

impl Default for JiraConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            email: String::new(),
            api_token: String::new(),
            allowed_actions: default_jira_allowed_actions(),
            timeout_secs: default_jira_timeout_secs(),
        }
    }
}

///
/// Controls the read-only cloud transformation analysis tools:
/// IaC review, migration assessment, cost analysis, and architecture review.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "cloud-ops"]
pub struct CloudOpsConfig {
    /// Enable cloud operations tools. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Default cloud provider for analysis context. Default: "aws".
    #[serde(default = "default_cloud_ops_cloud")]
    pub default_cloud: String,
    /// Supported cloud providers. Default: [`aws`, `azure`, `gcp`].
    #[serde(default = "default_cloud_ops_supported_clouds")]
    pub supported_clouds: Vec<String>,
    /// Supported IaC tools for review. Default: [`terraform`].
    #[serde(default = "default_cloud_ops_iac_tools")]
    pub iac_tools: Vec<String>,
    /// Monthly USD threshold to flag cost items. Default: 100.0.
    #[serde(default = "default_cloud_ops_cost_threshold")]
    pub cost_threshold_monthly_usd: f64,
    /// Well-Architected Frameworks to check against. Default: [`aws-waf`].
    #[serde(default = "default_cloud_ops_waf")]
    pub well_architected_frameworks: Vec<String>,
}

impl Default for CloudOpsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_cloud: default_cloud_ops_cloud(),
            supported_clouds: default_cloud_ops_supported_clouds(),
            iac_tools: default_cloud_ops_iac_tools(),
            cost_threshold_monthly_usd: default_cloud_ops_cost_threshold(),
            well_architected_frameworks: default_cloud_ops_waf(),
        }
    }
}

impl CloudOpsConfig {
    pub fn validate(&self) -> Result<()> {
        if self.enabled {
            if self.default_cloud.trim().is_empty() {
                anyhow::bail!(
                    "cloud_ops.default_cloud must not be empty when cloud_ops is enabled"
                );
            }
            if self.supported_clouds.is_empty() {
                anyhow::bail!(
                    "cloud_ops.supported_clouds must not be empty when cloud_ops is enabled"
                );
            }
            for (i, cloud) in self.supported_clouds.iter().enumerate() {
                if cloud.trim().is_empty() {
                    anyhow::bail!("cloud_ops.supported_clouds[{i}] must not be empty");
                }
            }
            if !self.supported_clouds.contains(&self.default_cloud) {
                anyhow::bail!(
                    "cloud_ops.default_cloud '{}' is not in cloud_ops.supported_clouds {:?}",
                    self.default_cloud,
                    self.supported_clouds
                );
            }
            if self.cost_threshold_monthly_usd < 0.0 {
                anyhow::bail!(
                    "cloud_ops.cost_threshold_monthly_usd must be non-negative, got {}",
                    self.cost_threshold_monthly_usd
                );
            }
            if self.iac_tools.is_empty() {
                anyhow::bail!("cloud_ops.iac_tools must not be empty when cloud_ops is enabled");
            }
        }
        Ok(())
    }
}

fn default_cloud_ops_cloud() -> String {
    "aws".into()
}

fn default_cloud_ops_supported_clouds() -> Vec<String> {
    vec!["aws".into(), "azure".into(), "gcp".into()]
}

fn default_cloud_ops_iac_tools() -> Vec<String> {
    vec!["terraform".into()]
}

fn default_cloud_ops_cost_threshold() -> f64 {
    100.0
}

fn default_cloud_ops_waf() -> Vec<String> {
    vec!["aws-waf".into()]
}

// ── Conversational AI ──────────────────────────────────────────────

fn default_conversational_ai_language() -> String {
    "en".into()
}

fn default_conversational_ai_supported_languages() -> Vec<String> {
    vec!["en".into(), "de".into(), "fr".into(), "it".into()]
}

fn default_conversational_ai_escalation_threshold() -> f64 {
    0.3
}

fn default_conversational_ai_max_turns() -> usize {
    50
}

fn default_conversational_ai_timeout_secs() -> u64 {
    1800
}

/// Conversational AI agent builder configuration (`[conversational_ai]` section).
///
/// **Status: Reserved for future use.** This configuration is parsed but not yet
/// consumed by the runtime. Setting `enabled = true` will produce a startup warning.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "conversational-ai"]
pub struct ConversationalAiConfig {
    /// Enable conversational AI features. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Default language for conversations (BCP-47 tag). Default: "en".
    #[serde(default = "default_conversational_ai_language")]
    pub default_language: String,
    /// Supported languages for conversations. Default: [`en`, `de`, `fr`, `it`].
    #[serde(default = "default_conversational_ai_supported_languages")]
    pub supported_languages: Vec<String>,
    /// Automatically detect user language from message content. Default: true.
    #[serde(default = "default_true")]
    pub auto_detect_language: bool,
    /// Intent confidence below this threshold triggers escalation. Default: 0.3.
    #[serde(default = "default_conversational_ai_escalation_threshold")]
    pub escalation_confidence_threshold: f64,
    /// Maximum conversation turns before auto-ending. Default: 50.
    #[serde(default = "default_conversational_ai_max_turns")]
    pub max_conversation_turns: usize,
    /// Conversation timeout in seconds (inactivity). Default: 1800.
    #[serde(default = "default_conversational_ai_timeout_secs")]
    pub conversation_timeout_secs: u64,
    /// Enable conversation analytics tracking. Default: false (privacy-by-default).
    #[serde(default)]
    pub analytics_enabled: bool,
    /// Optional tool name for RAG-based knowledge base lookup during conversations.
    #[serde(default)]
    pub knowledge_base_tool: Option<String>,
}

impl ConversationalAiConfig {
    /// Returns `true` when the feature is disabled (the default).
    ///
    /// Used by `#[serde(skip_serializing_if)]` to omit the entire
    /// `[conversational_ai]` section from newly-generated config files,
    /// avoiding user confusion over an undocumented / experimental section.
    pub fn is_disabled(&self) -> bool {
        !self.enabled
    }
}

impl Default for ConversationalAiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_language: default_conversational_ai_language(),
            supported_languages: default_conversational_ai_supported_languages(),
            auto_detect_language: true,
            escalation_confidence_threshold: default_conversational_ai_escalation_threshold(),
            max_conversation_turns: default_conversational_ai_max_turns(),
            conversation_timeout_secs: default_conversational_ai_timeout_secs(),
            analytics_enabled: false,
            knowledge_base_tool: None,
        }
    }
}

// ── Security ops config ─────────────────────────────────────────

/// Managed Cybersecurity Service (MCSS) dashboard agent configuration (`[security_ops]`).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security-ops"]
pub struct SecurityOpsConfig {
    /// Enable security operations tools.
    #[serde(default)]
    pub enabled: bool,
    /// Directory containing incident response playbook definitions (JSON).
    #[serde(default = "default_playbooks_dir")]
    pub playbooks_dir: String,
    /// Automatically triage incoming alerts without user prompt.
    #[serde(default)]
    pub auto_triage: bool,
    /// Require human approval before executing playbook actions.
    #[serde(default = "default_require_approval")]
    pub require_approval_for_actions: bool,
    /// Maximum severity level that can be auto-remediated without approval.
    /// One of: "low", "medium", "high", "critical". Default: "low".
    #[serde(default = "default_max_auto_severity")]
    pub max_auto_severity: String,
    /// Directory for generated security reports.
    #[serde(default = "default_report_output_dir")]
    pub report_output_dir: String,
    /// Optional SIEM webhook URL for alert ingestion.
    #[serde(default)]
    pub siem_integration: Option<String>,
}

fn default_playbooks_dir() -> String {
    "~/.zeroclaw/playbooks".into()
}

fn default_require_approval() -> bool {
    true
}

fn default_max_auto_severity() -> String {
    "low".into()
}

fn default_report_output_dir() -> String {
    "~/.zeroclaw/security-reports".into()
}

impl Default for SecurityOpsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            playbooks_dir: default_playbooks_dir(),
            auto_triage: false,
            require_approval_for_actions: true,
            max_auto_severity: default_max_auto_severity(),
            report_output_dir: default_report_output_dir(),
            siem_integration: None,
        }
    }
}

// ── Config impl ──────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        let home =
            UserDirs::new().map_or_else(|| PathBuf::from("."), |u| u.home_dir().to_path_buf());
        let zeroclaw_dir = home.join(".zeroclaw");

        Self {
            workspace_dir: zeroclaw_dir.join("workspace"),
            config_path: zeroclaw_dir.join("config.toml"),
            schema_version: crate::migration::CURRENT_SCHEMA_VERSION,
            providers: crate::providers::ProvidersConfig::default(),
            observability: ObservabilityConfig::default(),
            autonomy: AutonomyConfig::default(),
            trust: crate::scattered_types::TrustConfig::default(),
            backup: BackupConfig::default(),
            data_retention: DataRetentionConfig::default(),
            cloud_ops: CloudOpsConfig::default(),
            conversational_ai: ConversationalAiConfig::default(),
            security: SecurityConfig::default(),
            security_ops: SecurityOpsConfig::default(),
            runtime: RuntimeConfig::default(),
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            agent: AgentConfig::default(),
            pacing: PacingConfig::default(),
            skills: SkillsConfig::default(),
            pipeline: PipelineConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            cron: CronConfig::default(),
            channels: ChannelsConfig::default(),
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            microsoft365: Microsoft365Config::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            browser_delegate: crate::scattered_types::BrowserDelegateConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            media_pipeline: MediaPipelineConfig::default(),
            web_fetch: WebFetchConfig::default(),
            link_enricher: LinkEnricherConfig::default(),
            text_browser: TextBrowserConfig::default(),
            web_search: WebSearchConfig::default(),
            project_intel: ProjectIntelConfig::default(),
            google_workspace: GoogleWorkspaceConfig::default(),
            proxy: ProxyConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            peripherals: PeripheralsConfig::default(),
            delegate: DelegateToolConfig::default(),
            agents: HashMap::new(),
            swarms: HashMap::new(),
            hooks: HooksConfig::default(),
            hardware: HardwareConfig::default(),
            query_classification: QueryClassificationConfig::default(),
            transcription: TranscriptionConfig::default(),
            tts: TtsConfig::default(),
            mcp: McpConfig::default(),
            nodes: NodesConfig::default(),
            workspace: WorkspaceConfig::default(),
            notion: NotionConfig::default(),
            jira: JiraConfig::default(),
            node_transport: NodeTransportConfig::default(),
            knowledge: KnowledgeConfig::default(),
            linkedin: LinkedInConfig::default(),
            image_gen: ImageGenConfig::default(),
            plugins: PluginsConfig::default(),
            locale: None,
            verifiable_intent: VerifiableIntentConfig::default(),
            claude_code: ClaudeCodeConfig::default(),
            claude_code_runner: ClaudeCodeRunnerConfig::default(),
            codex_cli: CodexCliConfig::default(),
            gemini_cli: GeminiCliConfig::default(),
            opencode_cli: OpenCodeCliConfig::default(),
            sop: SopConfig::default(),
            shell_tool: ShellToolConfig::default(),
        }
    }
}

fn default_config_and_workspace_dirs() -> Result<(PathBuf, PathBuf)> {
    let config_dir = default_config_dir()?;
    Ok((config_dir.clone(), config_dir.join("workspace")))
}

const ACTIVE_WORKSPACE_STATE_FILE: &str = "active_workspace.toml";

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
struct ActiveWorkspaceState {
    config_dir: String,
}

fn default_config_dir() -> Result<PathBuf> {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".zeroclaw"));
    }

    let home = UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home.join(".zeroclaw"))
}

fn active_workspace_state_path(default_dir: &Path) -> PathBuf {
    default_dir.join(ACTIVE_WORKSPACE_STATE_FILE)
}

/// Returns `true` if `path` lives under the OS temp directory.
fn is_temp_directory(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    // Canonicalize when possible to handle symlinks (macOS /var → /private/var)
    let canon_temp = temp.canonicalize().unwrap_or_else(|_| temp.clone());
    let canon_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canon_path.starts_with(&canon_temp)
}

async fn load_persisted_workspace_dirs(
    default_config_dir: &Path,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let state_path = active_workspace_state_path(default_config_dir);
    if !state_path.exists() {
        return Ok(None);
    }

    let contents = match fs::read_to_string(&state_path).await {
        Ok(contents) => contents,
        Err(error) => {
            tracing::warn!(
                "Failed to read active workspace marker {}: {error}",
                state_path.display()
            );
            return Ok(None);
        }
    };

    let state: ActiveWorkspaceState = match toml::from_str(&contents) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                "Failed to parse active workspace marker {}: {error}",
                state_path.display()
            );
            return Ok(None);
        }
    };

    let raw_config_dir = state.config_dir.trim();
    if raw_config_dir.is_empty() {
        tracing::warn!(
            "Ignoring active workspace marker {} because config_dir is empty",
            state_path.display()
        );
        return Ok(None);
    }

    let parsed_dir = expand_tilde_path(raw_config_dir);
    let config_dir = if parsed_dir.is_absolute() {
        parsed_dir
    } else {
        default_config_dir.join(parsed_dir)
    };
    Ok(Some((config_dir.clone(), config_dir.join("workspace"))))
}

pub async fn persist_active_workspace_config_dir(config_dir: &Path) -> Result<()> {
    persist_active_workspace_config_dir_in(config_dir, &default_config_dir()?).await
}

/// Inner implementation that accepts the default config directory explicitly,
/// so callers (including tests) control where the marker is written without
/// manipulating process-wide environment variables.
async fn persist_active_workspace_config_dir_in(
    config_dir: &Path,
    default_config_dir: &Path,
) -> Result<()> {
    let state_path = active_workspace_state_path(default_config_dir);

    // Guard: refuse to write a temp-directory config_dir into a non-temp
    // default location. This prevents transient test runs or one-off
    // invocations from hijacking the real user's daemon config resolution.
    // When both paths are temp (e.g. in tests), the write is harmless.
    if is_temp_directory(config_dir) && !is_temp_directory(default_config_dir) {
        tracing::warn!(
            path = %config_dir.display(),
            "Refusing to persist temp directory as active workspace marker"
        );
        return Ok(());
    }

    if config_dir == default_config_dir {
        if state_path.exists() {
            fs::remove_file(&state_path).await.with_context(|| {
                format!(
                    "Failed to clear active workspace marker: {}",
                    state_path.display()
                )
            })?;
        }
        return Ok(());
    }

    fs::create_dir_all(&default_config_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to create default config directory: {}",
                default_config_dir.display()
            )
        })?;

    let state = ActiveWorkspaceState {
        config_dir: config_dir.to_string_lossy().into_owned(),
    };
    let serialized =
        toml::to_string_pretty(&state).context("Failed to serialize active workspace marker")?;

    let temp_path = default_config_dir.join(format!(
        ".{ACTIVE_WORKSPACE_STATE_FILE}.tmp-{}",
        uuid::Uuid::new_v4()
    ));
    fs::write(&temp_path, serialized).await.with_context(|| {
        format!(
            "Failed to write temporary active workspace marker: {}",
            temp_path.display()
        )
    })?;

    if let Err(error) = fs::rename(&temp_path, &state_path).await {
        let _ = fs::remove_file(&temp_path).await;
        anyhow::bail!(
            "Failed to atomically persist active workspace marker {}: {error}",
            state_path.display()
        );
    }

    sync_directory(default_config_dir).await?;
    Ok(())
}

pub fn resolve_config_dir_for_workspace(workspace_dir: &Path) -> (PathBuf, PathBuf) {
    let workspace_config_dir = workspace_dir.to_path_buf();
    if workspace_config_dir.join("config.toml").exists() {
        return (
            workspace_config_dir.clone(),
            workspace_config_dir.join("workspace"),
        );
    }

    let legacy_config_dir = workspace_dir
        .parent()
        .map(|parent| parent.join(".zeroclaw"));
    if let Some(legacy_dir) = legacy_config_dir {
        if legacy_dir.join("config.toml").exists() {
            return (legacy_dir, workspace_config_dir);
        }

        if workspace_dir
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new("workspace"))
        {
            return (legacy_dir, workspace_config_dir);
        }
    }

    (
        workspace_config_dir.clone(),
        workspace_config_dir.join("workspace"),
    )
}

/// Resolve the current runtime config/workspace directories for onboarding flows.
///
/// This mirrors the same precedence used by `Config::load_or_init()`:
/// `ZEROCLAW_CONFIG_DIR` > `ZEROCLAW_WORKSPACE` > active workspace marker > defaults.
pub async fn resolve_runtime_dirs_for_onboarding() -> Result<(PathBuf, PathBuf)> {
    let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;
    let (config_dir, workspace_dir, _) =
        resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;
    Ok((config_dir, workspace_dir))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigResolutionSource {
    EnvConfigDir,
    EnvWorkspace,
    ActiveWorkspaceMarker,
    DefaultConfigDir,
}

impl ConfigResolutionSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EnvConfigDir => "ZEROCLAW_CONFIG_DIR",
            Self::EnvWorkspace => "ZEROCLAW_WORKSPACE",
            Self::ActiveWorkspaceMarker => "active_workspace.toml",
            Self::DefaultConfigDir => "default",
        }
    }
}

/// Expand tilde in paths, falling back to `UserDirs` when HOME is unset.
///
/// In non-TTY environments (e.g. cron), HOME may not be set, causing
/// `shellexpand::tilde` to return the literal `~` unexpanded. This helper
/// detects that case and uses `directories::UserDirs` as a fallback.
fn expand_tilde_path(path: &str) -> PathBuf {
    let expanded = shellexpand::tilde(path);
    let expanded_str = expanded.as_ref();

    // If the path still starts with '~', tilde expansion failed (HOME unset)
    if expanded_str.starts_with('~') {
        if let Some(user_dirs) = UserDirs::new() {
            let home = user_dirs.home_dir();
            // Replace leading ~ with home directory
            if let Some(rest) = expanded_str.strip_prefix('~') {
                return home.join(rest.trim_start_matches(['/', '\\']));
            }
        }
        // If UserDirs also fails, log a warning and use the literal path
        tracing::warn!(
            path = path,
            "Failed to expand tilde: HOME environment variable is not set and UserDirs failed. \
             In cron/non-TTY environments, use absolute paths or set HOME explicitly."
        );
    }

    PathBuf::from(expanded_str)
}

async fn resolve_runtime_config_dirs(
    default_zeroclaw_dir: &Path,
    default_workspace_dir: &Path,
) -> Result<(PathBuf, PathBuf, ConfigResolutionSource)> {
    if let Ok(custom_config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let custom_config_dir = custom_config_dir.trim();
        if !custom_config_dir.is_empty() {
            let zeroclaw_dir = expand_tilde_path(custom_config_dir);
            return Ok((
                zeroclaw_dir.clone(),
                zeroclaw_dir.join("workspace"),
                ConfigResolutionSource::EnvConfigDir,
            ));
        }
    }

    if let Ok(custom_workspace) = std::env::var("ZEROCLAW_WORKSPACE")
        && !custom_workspace.is_empty()
    {
        let expanded = expand_tilde_path(&custom_workspace);
        let (zeroclaw_dir, workspace_dir) = resolve_config_dir_for_workspace(&expanded);
        return Ok((
            zeroclaw_dir,
            workspace_dir,
            ConfigResolutionSource::EnvWorkspace,
        ));
    }

    if let Some((zeroclaw_dir, workspace_dir)) =
        load_persisted_workspace_dirs(default_zeroclaw_dir).await?
    {
        return Ok((
            zeroclaw_dir,
            workspace_dir,
            ConfigResolutionSource::ActiveWorkspaceMarker,
        ));
    }

    Ok((
        default_zeroclaw_dir.to_path_buf(),
        default_workspace_dir.to_path_buf(),
        ConfigResolutionSource::DefaultConfigDir,
    ))
}

fn config_dir_creation_error(path: &Path) -> String {
    format!(
        "Failed to create config directory: {}. If running as an OpenRC service, \
         ensure this path is writable by user 'zeroclaw'.",
        path.display()
    )
}

fn is_local_ollama_endpoint(api_url: Option<&str>) -> bool {
    let Some(raw) = api_url.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };

    reqwest::Url::parse(raw)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"))
}

fn has_ollama_cloud_credential(config_api_key: Option<&str>) -> bool {
    let config_key_present = config_api_key
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if config_key_present {
        return true;
    }

    ["OLLAMA_API_KEY", "ZEROCLAW_API_KEY", "API_KEY"]
        .iter()
        .any(|name| {
            std::env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        })
}

/// Parse the `ZEROCLAW_EXTRA_HEADERS` environment variable value.
///
/// Format: `Key:Value,Key2:Value2`
///
/// Entries without a colon or with an empty key are silently skipped.
/// Leading/trailing whitespace on both key and value is trimmed.
pub fn parse_extra_headers_env(raw: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((key, value)) = entry.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                tracing::warn!("Ignoring extra header with empty name in ZEROCLAW_EXTRA_HEADERS");
                continue;
            }
            result.push((key.to_string(), value.to_string()));
        } else {
            tracing::warn!("Ignoring malformed extra header entry (missing ':'): {entry}");
        }
    }
    result
}

fn normalize_wire_api(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "responses" | "openai-responses" | "open-ai-responses" => Some("responses"),
        "chat_completions"
        | "chat-completions"
        | "chat"
        | "chatcompletions"
        | "openai-chat-completions"
        | "open-ai-chat-completions" => Some("chat_completions"),
        _ => None,
    }
}

fn read_codex_openai_api_key() -> Option<String> {
    let home = UserDirs::new()?.home_dir().to_path_buf();
    let auth_path = home.join(".codex").join("auth.json");
    let raw = std::fs::read_to_string(auth_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;

    parsed
        .get("OPENAI_API_KEY")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Ensure that essential bootstrap files exist in the workspace directory.
///
/// When the workspace is created outside of `zeroclaw onboard` (e.g., non-tty
/// daemon/cron sessions), these files would otherwise be missing. This function
/// creates sensible defaults that allow the agent to operate with a basic identity.
async fn ensure_bootstrap_files(workspace_dir: &Path) -> Result<()> {
    let defaults: &[(&str, &str)] = &[
        (
            "IDENTITY.md",
            "# IDENTITY.md — Who Am I?\n\n\
             I am ZeroClaw, an autonomous AI agent.\n\n\
             ## Traits\n\
             - Helpful, precise, and safety-conscious\n\
             - I prioritize clarity and correctness\n",
        ),
        (
            "SOUL.md",
            "# SOUL.md — Who You Are\n\n\
             You are ZeroClaw, an autonomous AI agent.\n\n\
             ## Core Principles\n\
             - Be helpful and accurate\n\
             - Respect user intent and boundaries\n\
             - Ask before taking destructive actions\n\
             - Prefer safe, reversible operations\n",
        ),
    ];

    for (filename, content) in defaults {
        let path = workspace_dir.join(filename);
        if !path.exists() {
            fs::write(&path, content)
                .await
                .with_context(|| format!("Failed to create default {filename} in workspace"))?;
        }
    }

    Ok(())
}

impl Config {
    /// Return top-level TOML keys in `raw_toml` that Config does not recognise.
    ///
    /// Keys present in `Config::default()` serialization pass immediately.
    /// Remaining keys are probed: the key is deserialized in isolation and
    /// the result compared to the default — a changed output means serde
    /// consumed it (covers `Option<T>` fields and `#[serde(alias)]` names).
    /// V1 legacy keys (consumed by migration) are also accepted.
    pub fn unknown_keys(raw_toml: &str) -> Vec<String> {
        let raw: toml::Table = match raw_toml.parse() {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        static DEFAULTS: OnceLock<toml::Table> = OnceLock::new();
        let defaults = DEFAULTS.get_or_init(|| {
            toml::to_string(&Config::default())
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_default()
        });
        raw.keys()
            .filter(|key| {
                if defaults.contains_key(key.as_str()) {
                    return false;
                }
                if crate::migration::V1_LEGACY_KEYS.contains(&key.as_str()) {
                    return false;
                }
                let mut t = toml::Table::new();
                t.insert((*key).clone(), raw[key.as_str()].clone());
                let consumed = toml::to_string(&t)
                    .ok()
                    .and_then(|s| toml::from_str::<Config>(&s).ok())
                    .and_then(|c| toml::to_string(&c).ok())
                    .and_then(|s| s.parse::<toml::Table>().ok())
                    .is_some_and(|t| t != *defaults);
                !consumed
            })
            .cloned()
            .collect()
    }

    pub async fn load_or_init() -> Result<Self> {
        let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;

        let (zeroclaw_dir, workspace_dir, resolution_source) =
            resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;

        let config_path = zeroclaw_dir.join("config.toml");

        fs::create_dir_all(&zeroclaw_dir)
            .await
            .with_context(|| config_dir_creation_error(&zeroclaw_dir))?;
        fs::create_dir_all(&workspace_dir)
            .await
            .context("Failed to create workspace directory")?;

        ensure_bootstrap_files(&workspace_dir).await?;

        if config_path.exists() {
            // Warn if config file is world-readable (may contain API keys)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = fs::metadata(&config_path).await
                    && meta.permissions().mode() & 0o004 != 0
                {
                    tracing::warn!(
                        "Config file {:?} is world-readable (mode {:o}). \
                             Consider restricting with: chmod 600 {:?}",
                        config_path,
                        meta.permissions().mode() & 0o777,
                        config_path,
                    );
                }
            }

            let contents = fs::read_to_string(&config_path)
                .await
                .context("Failed to read config file")?;

            // Deserialize the config with the standard TOML parser.
            //
            // Previously this used `serde_ignored::deserialize` for both
            // deserialization and unknown-key detection.  However,
            // `serde_ignored` silently drops field values inside nested
            // structs that carry `#[serde(default)]` (e.g. the entire
            // `[autonomy]` table), causing user-supplied values to be
            // replaced by defaults.  See #4171.
            //
            // We now deserialize with `toml::from_str` (which is correct)
            // and run `serde_ignored` separately just for diagnostics.
            //
            // Before deserialization, run `prepare_table` to handle nested
            // field migrations (e.g. room_id → allowed_rooms in matrix)
            // that `#[serde(flatten)]` cannot capture.
            let mut table: toml::Table =
                toml::from_str(&contents).context("Failed to parse config as TOML table")?;
            crate::migration::prepare_table(&mut table);
            let table_str =
                toml::to_string(&table).context("Failed to re-serialize prepared table")?;
            let compat: crate::migration::V1Compat =
                toml::from_str(&table_str).context("Failed to deserialize config file")?;
            let mut config: Config = compat.into_config();

            // Ensure the built-in default auto_approve entries are always
            // present.  When a user specifies `auto_approve` in their TOML
            // (e.g. to add a custom tool), serde replaces the default list
            // instead of merging.  This caused default-safe tools like
            // `weather` or `calculator` to lose their auto-approve status
            // and get silently denied in non-interactive channel runs.
            // See #4247.
            //
            // Users who want to require approval for a default tool can
            // add it to `always_ask`, which takes precedence over
            // `auto_approve` in the approval decision (see approval/mod.rs).
            config.autonomy.ensure_default_auto_approve();

            // Backward-compatible `enabled` backfill: if a channel section
            // exists in the TOML but has no explicit `enabled` key, the user
            // configured it before `enabled` was introduced — treat it as
            // enabled so existing setups don't silently break.
            config.channels.backfill_enabled(&contents);

            // Detect unknown top-level config keys by comparing the raw
            // TOML table keys against what Config actually deserializes.
            // This replaces the previous serde_ignored-based approach which
            // had false-positive issues with #[serde(default)] nested structs.
            for key in Self::unknown_keys(&contents) {
                tracing::warn!(
                    "Unknown config key ignored: \"{key}\". Check config.toml for typos or deprecated options.",
                );
            }
            // Set computed paths that are skipped during serialization
            config.config_path = config_path.clone();
            config.workspace_dir = workspace_dir;
            let store = crate::secrets::SecretStore::new(&zeroclaw_dir, config.secrets.encrypt);
            // Decrypt all #[secret]-annotated fields via Configurable derive
            config.decrypt_secrets(&store)?;

            config.apply_env_overrides();
            config.validate()?;
            tracing::info!(
                path = %config.config_path.display(),
                workspace = %config.workspace_dir.display(),
                source = resolution_source.as_str(),
                initialized = true,
                "Config loaded"
            );
            Ok(config)
        } else {
            let mut config = Config {
                config_path: config_path.clone(),
                workspace_dir,
                ..Config::default()
            };
            config.save().await?;

            // Restrict permissions on newly created config file (may contain API keys)
            #[cfg(unix)]
            {
                use std::{fs::Permissions, os::unix::fs::PermissionsExt};
                let _ = fs::set_permissions(&config_path, Permissions::from_mode(0o600)).await;
            }

            config.apply_env_overrides();
            config.validate()?;
            tracing::info!(
                path = %config.config_path.display(),
                workspace = %config.workspace_dir.display(),
                source = resolution_source.as_str(),
                initialized = true,
                "Config loaded"
            );
            Ok(config)
        }
    }

    fn lookup_model_provider_profile(
        &self,
        provider_name: &str,
    ) -> Option<(String, ModelProviderConfig)> {
        let needle = provider_name.trim();
        if needle.is_empty() {
            return None;
        }

        self.providers
            .models
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(needle))
            .map(|(name, profile)| (name.clone(), profile.clone()))
    }

    fn apply_named_model_provider_profile(&mut self) {
        let Some(current_provider) = self.providers.fallback.clone() else {
            return;
        };

        let Some((profile_key, profile)) = self.lookup_model_provider_profile(&current_provider)
        else {
            return;
        };

        let base_url = profile
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        {
            let fallback_provider = self.providers.fallback_provider();
            let current_url = fallback_provider
                .and_then(|e| e.base_url.as_deref())
                .map(str::trim);
            if current_url.is_none_or(|value| value.is_empty())
                && let Some(base_url) = base_url.as_ref()
                && let Some(entry) = self.providers.fallback_provider_mut()
            {
                entry.base_url = Some(base_url.clone());
            }
        }

        // Propagate api_path from the profile when not already set on fallback entry.
        {
            let has_api_path = self
                .providers
                .fallback_provider()
                .and_then(|e| e.api_path.as_ref())
                .is_some();
            if !has_api_path && let Some(ref path) = profile.api_path {
                let trimmed = path.trim();
                if !trimmed.is_empty()
                    && let Some(entry) = self.providers.fallback_provider_mut()
                {
                    entry.api_path = Some(trimmed.to_string());
                }
            }
        }

        // Propagate max_tokens from the profile when not already set on fallback entry.
        {
            let has_max_tokens = self
                .providers
                .fallback_provider()
                .and_then(|e| e.max_tokens)
                .is_some();
            if !has_max_tokens
                && let Some(max_tokens) = profile.max_tokens
                && let Some(entry) = self.providers.fallback_provider_mut()
            {
                entry.max_tokens = Some(max_tokens);
            }
        }

        if profile.requires_openai_auth {
            let needs_key = self
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref())
                .map(str::trim)
                .is_none_or(|value| value.is_empty());
            if needs_key {
                let codex_key = std::env::var("OPENAI_API_KEY")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .or_else(read_codex_openai_api_key);
                if let Some(codex_key) = codex_key
                    && let Some(entry) = self.providers.fallback_provider_mut()
                {
                    entry.api_key = Some(codex_key);
                }
            }
        }

        let normalized_wire_api = profile.wire_api.as_deref().and_then(normalize_wire_api);
        let profile_name = profile
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if normalized_wire_api == Some("responses") {
            self.providers.fallback = Some("openai-codex".to_string());
            return;
        }

        if let Some(profile_name) = profile_name
            && !profile_name.eq_ignore_ascii_case(&profile_key)
        {
            self.providers.fallback = Some(profile_name.to_string());
            return;
        }

        if let Some(base_url) = base_url {
            self.providers.fallback = Some(format!("custom:{base_url}"));
        }
    }

    /// Validate configuration values that would cause runtime failures.
    ///
    /// Called after TOML deserialization and env-override application to catch
    /// obviously invalid values early instead of failing at arbitrary runtime points.
    pub fn validate(&self) -> Result<()> {
        // Tunnel — OpenVPN
        if self.tunnel.provider.trim() == "openvpn" {
            let openvpn = self.tunnel.openvpn.as_ref().ok_or_else(|| {
                anyhow::anyhow!("tunnel.provider='openvpn' requires [tunnel.openvpn]")
            })?;

            if openvpn.config_file.trim().is_empty() {
                anyhow::bail!("tunnel.openvpn.config_file must not be empty");
            }
            if openvpn.connect_timeout_secs == 0 {
                anyhow::bail!("tunnel.openvpn.connect_timeout_secs must be greater than 0");
            }
        }

        // Gateway
        if self.gateway.host.trim().is_empty() {
            anyhow::bail!("gateway.host must not be empty");
        }
        if let Some(ref prefix) = self.gateway.path_prefix {
            // Validate the raw value — no silent trimming so the stored
            // value is exactly what was validated.
            if !prefix.is_empty() {
                if !prefix.starts_with('/') {
                    anyhow::bail!("gateway.path_prefix must start with '/'");
                }
                if prefix.ends_with('/') {
                    anyhow::bail!("gateway.path_prefix must not end with '/' (including bare '/')");
                }
                // Reject characters unsafe for URL paths or HTML/JS injection.
                // Whitespace is intentionally excluded from the allowed set.
                if let Some(bad) = prefix.chars().find(|c| {
                    !matches!(c, '/' | '-' | '_' | '.' | '~'
                        | 'a'..='z' | 'A'..='Z' | '0'..='9'
                        | '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '='
                        | ':' | '@')
                }) {
                    anyhow::bail!(
                        "gateway.path_prefix contains invalid character '{bad}'; \
                         only unreserved and sub-delim URI characters are allowed"
                    );
                }
            }
        }

        // Autonomy
        if self.autonomy.max_actions_per_hour == 0 {
            anyhow::bail!("autonomy.max_actions_per_hour must be greater than 0");
        }
        for (i, env_name) in self.autonomy.shell_env_passthrough.iter().enumerate() {
            if !is_valid_env_var_name(env_name) {
                anyhow::bail!(
                    "autonomy.shell_env_passthrough[{i}] is invalid ({env_name}); expected [A-Za-z_][A-Za-z0-9_]*"
                );
            }
        }

        // Security OTP / estop
        if self.security.otp.challenge_max_attempts == 0 {
            anyhow::bail!("security.otp.challenge_max_attempts must be greater than 0");
        }
        if self.security.otp.token_ttl_secs == 0 {
            anyhow::bail!("security.otp.token_ttl_secs must be greater than 0");
        }
        if self.security.otp.cache_valid_secs == 0 {
            anyhow::bail!("security.otp.cache_valid_secs must be greater than 0");
        }
        if self.security.otp.cache_valid_secs < self.security.otp.token_ttl_secs {
            anyhow::bail!(
                "security.otp.cache_valid_secs must be greater than or equal to security.otp.token_ttl_secs"
            );
        }
        if self.security.otp.challenge_max_attempts == 0 {
            anyhow::bail!("security.otp.challenge_max_attempts must be greater than 0");
        }
        for (i, action) in self.security.otp.gated_actions.iter().enumerate() {
            let normalized = action.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.otp.gated_actions[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "security.otp.gated_actions[{i}] contains invalid characters: {normalized}"
                );
            }
        }
        DomainMatcher::new(
            &self.security.otp.gated_domains,
            &self.security.otp.gated_domain_categories,
        )
        .with_context(
            || "Invalid security.otp.gated_domains or security.otp.gated_domain_categories",
        )?;
        if self.security.estop.state_file.trim().is_empty() {
            anyhow::bail!("security.estop.state_file must not be empty");
        }

        // Scheduler
        if self.scheduler.max_concurrent == 0 {
            anyhow::bail!("scheduler.max_concurrent must be greater than 0");
        }
        if self.scheduler.max_tasks == 0 {
            anyhow::bail!("scheduler.max_tasks must be greater than 0");
        }

        // Model routes
        for (i, route) in self.providers.model_routes.iter().enumerate() {
            if route.hint.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].hint must not be empty");
            }
            if route.provider.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].provider must not be empty");
            }
            if route.model.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].model must not be empty");
            }
        }

        // Embedding routes
        for (i, route) in self.providers.embedding_routes.iter().enumerate() {
            if route.hint.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].hint must not be empty");
            }
            if route.provider.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].provider must not be empty");
            }
            if route.model.trim().is_empty() {
                anyhow::bail!("embedding_routes[{i}].model must not be empty");
            }
        }

        for (profile_key, profile) in &self.providers.models {
            let profile_name = profile_key.trim();
            if profile_name.is_empty() {
                anyhow::bail!("model_providers contains an empty profile name");
            }

            let has_name = profile
                .name
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());
            let has_base_url = profile
                .base_url
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty());

            // Entries created by migration from top-level fields use the provider
            // name as the map key and may not have explicit `name` or `base_url`
            // (the provider factory resolves known names). Only reject entries that
            // have no identifying information at all.
            let has_api_key = profile
                .api_key
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty());
            let has_model = profile
                .model
                .as_deref()
                .is_some_and(|v| !v.trim().is_empty());
            if !has_name && !has_base_url && !has_api_key && !has_model {
                anyhow::bail!(
                    "providers.models.{profile_name} must define at least one of `name`, `base_url`, `api_key`, or `model`"
                );
            }

            if let Some(base_url) = profile.base_url.as_deref().map(str::trim)
                && !base_url.is_empty()
            {
                let parsed = reqwest::Url::parse(base_url).with_context(|| {
                    format!("model_providers.{profile_name}.base_url is not a valid URL")
                })?;
                if !matches!(parsed.scheme(), "http" | "https") {
                    anyhow::bail!("model_providers.{profile_name}.base_url must use http/https");
                }
            }

            if let Some(wire_api) = profile.wire_api.as_deref().map(str::trim)
                && !wire_api.is_empty()
                && normalize_wire_api(wire_api).is_none()
            {
                anyhow::bail!(
                    "model_providers.{profile_name}.wire_api must be one of: responses, chat_completions"
                );
            }

            if let Some(temp) = profile.temperature {
                validate_temperature(temp).map_err(|e| {
                    anyhow::anyhow!("providers.models.{profile_name}.temperature: {e}")
                })?;
            }
        }

        // Providers — fallback reference check
        if let Some(ref fallback_key) = self.providers.fallback
            && !self.providers.models.contains_key(fallback_key)
        {
            tracing::warn!(
                "providers.fallback references '{}' which does not exist in providers.models; \
                 provider resolution will fail at runtime",
                fallback_key
            );
        }

        // Ollama cloud-routing safety checks
        if self
            .providers
            .fallback
            .as_deref()
            .is_some_and(|provider| provider.trim().eq_ignore_ascii_case("ollama"))
            && self
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref())
                .is_some_and(|model| model.trim().ends_with(":cloud"))
        {
            if is_local_ollama_endpoint(
                self.providers
                    .fallback_provider()
                    .and_then(|e| e.base_url.as_deref()),
            ) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but api_url is local or unset. Set api_url to a remote Ollama endpoint (for example https://ollama.com)."
                );
            }

            if !has_ollama_cloud_credential(
                self.providers
                    .fallback_provider()
                    .and_then(|e| e.api_key.as_deref()),
            ) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but no API key is configured. Set api_key or OLLAMA_API_KEY."
                );
            }
        }

        // Microsoft 365
        if self.microsoft365.enabled {
            let tenant = self
                .microsoft365
                .tenant_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if tenant.is_none() {
                anyhow::bail!(
                    "microsoft365.tenant_id must not be empty when microsoft365 is enabled"
                );
            }
            let client = self
                .microsoft365
                .client_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if client.is_none() {
                anyhow::bail!(
                    "microsoft365.client_id must not be empty when microsoft365 is enabled"
                );
            }
            let flow = self.microsoft365.auth_flow.trim();
            if flow != "client_credentials" && flow != "device_code" {
                anyhow::bail!(
                    "microsoft365.auth_flow must be 'client_credentials' or 'device_code'"
                );
            }
            if flow == "client_credentials"
                && self
                    .microsoft365
                    .client_secret
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
            {
                anyhow::bail!(
                    "microsoft365.client_secret must not be empty when auth_flow is 'client_credentials'"
                );
            }
        }

        // Microsoft 365
        if self.microsoft365.enabled {
            let tenant = self
                .microsoft365
                .tenant_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if tenant.is_none() {
                anyhow::bail!(
                    "microsoft365.tenant_id must not be empty when microsoft365 is enabled"
                );
            }
            let client = self
                .microsoft365
                .client_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if client.is_none() {
                anyhow::bail!(
                    "microsoft365.client_id must not be empty when microsoft365 is enabled"
                );
            }
            let flow = self.microsoft365.auth_flow.trim();
            if flow != "client_credentials" && flow != "device_code" {
                anyhow::bail!("microsoft365.auth_flow must be client_credentials or device_code");
            }
            if flow == "client_credentials"
                && self
                    .microsoft365
                    .client_secret
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
            {
                anyhow::bail!(
                    "microsoft365.client_secret must not be empty when auth_flow is client_credentials"
                );
            }
        }

        // MCP
        if self.mcp.enabled {
            validate_mcp_config(&self.mcp)?;
        }

        // Knowledge graph
        if self.knowledge.enabled {
            if self.knowledge.max_nodes == 0 {
                anyhow::bail!("knowledge.max_nodes must be greater than 0");
            }
            if self.knowledge.db_path.trim().is_empty() {
                anyhow::bail!("knowledge.db_path must not be empty");
            }
        }

        // Google Workspace allowed_services validation
        let mut seen_gws_services = std::collections::HashSet::new();
        for (i, service) in self.google_workspace.allowed_services.iter().enumerate() {
            let normalized = service.trim();
            if normalized.is_empty() {
                anyhow::bail!("google_workspace.allowed_services[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_services[{i}] contains invalid characters: {normalized}"
                );
            }
            if !seen_gws_services.insert(normalized.to_string()) {
                anyhow::bail!(
                    "google_workspace.allowed_services contains duplicate entry: {normalized}"
                );
            }
        }

        // Build the effective allowed-services set for cross-validation.
        // When the operator leaves allowed_services empty the tool falls back to
        // DEFAULT_GWS_SERVICES; use the same constant here so validation is
        // consistent in both cases.
        let effective_services: std::collections::HashSet<&str> =
            if self.google_workspace.allowed_services.is_empty() {
                DEFAULT_GWS_SERVICES.iter().copied().collect()
            } else {
                self.google_workspace
                    .allowed_services
                    .iter()
                    .map(|s| s.trim())
                    .collect()
            };

        let mut seen_gws_operations = std::collections::HashSet::new();
        for (i, operation) in self.google_workspace.allowed_operations.iter().enumerate() {
            let service = operation.service.trim();
            let resource = operation.resource.trim();

            if service.is_empty() {
                anyhow::bail!("google_workspace.allowed_operations[{i}].service must not be empty");
            }
            if resource.is_empty() {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].resource must not be empty"
                );
            }

            if !effective_services.contains(service) {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].service '{service}' is not in the \
                     effective allowed_services; this entry can never match at runtime"
                );
            }
            if !service
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].service contains invalid characters: {service}"
                );
            }
            if !resource
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "google_workspace.allowed_operations[{i}].resource contains invalid characters: {resource}"
                );
            }

            if let Some(ref sub_resource) = operation.sub_resource {
                let sub = sub_resource.trim();
                if sub.is_empty() {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].sub_resource must not be empty when present"
                    );
                }
                if !sub
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].sub_resource contains invalid characters: {sub}"
                    );
                }
            }

            if operation.methods.is_empty() {
                anyhow::bail!("google_workspace.allowed_operations[{i}].methods must not be empty");
            }

            let mut seen_methods = std::collections::HashSet::new();
            for (j, method) in operation.methods.iter().enumerate() {
                let normalized = method.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods[{j}] must not be empty"
                    );
                }
                if !normalized
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
                {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods[{j}] contains invalid characters: {normalized}"
                    );
                }
                if !seen_methods.insert(normalized.to_string()) {
                    anyhow::bail!(
                        "google_workspace.allowed_operations[{i}].methods contains duplicate entry: {normalized}"
                    );
                }
            }

            let sub_key = operation
                .sub_resource
                .as_deref()
                .map(str::trim)
                .unwrap_or("");
            let operation_key = format!("{service}:{resource}:{sub_key}");
            if !seen_gws_operations.insert(operation_key.clone()) {
                anyhow::bail!(
                    "google_workspace.allowed_operations contains duplicate service/resource/sub_resource entry: {operation_key}"
                );
            }
        }

        // Project intelligence
        if self.project_intel.enabled {
            let lang = &self.project_intel.default_language;
            if !["en", "de", "fr", "it"].contains(&lang.as_str()) {
                anyhow::bail!(
                    "project_intel.default_language must be one of: en, de, fr, it (got '{lang}')"
                );
            }
            let sens = &self.project_intel.risk_sensitivity;
            if !["low", "medium", "high"].contains(&sens.as_str()) {
                anyhow::bail!(
                    "project_intel.risk_sensitivity must be one of: low, medium, high (got '{sens}')"
                );
            }
            if let Some(ref tpl_dir) = self.project_intel.templates_dir
                && !std::path::Path::new(tpl_dir).exists()
            {
                anyhow::bail!("project_intel.templates_dir path does not exist: {tpl_dir}");
            }
        }

        // Proxy (delegate to existing validation)
        self.proxy.validate()?;
        self.cloud_ops.validate()?;

        // Notion
        if self.notion.enabled {
            if self.notion.database_id.trim().is_empty() {
                anyhow::bail!("notion.database_id must not be empty when notion.enabled = true");
            }
            if self.notion.poll_interval_secs == 0 {
                anyhow::bail!("notion.poll_interval_secs must be greater than 0");
            }
            if self.notion.max_concurrent == 0 {
                anyhow::bail!("notion.max_concurrent must be greater than 0");
            }
            if self.notion.status_property.trim().is_empty() {
                anyhow::bail!("notion.status_property must not be empty");
            }
            if self.notion.input_property.trim().is_empty() {
                anyhow::bail!("notion.input_property must not be empty");
            }
            if self.notion.result_property.trim().is_empty() {
                anyhow::bail!("notion.result_property must not be empty");
            }
        }

        // Pinggy tunnel region — validate allowed values (case-insensitive, auto-lowercased at runtime).
        if let Some(ref pinggy) = self.tunnel.pinggy
            && let Some(ref region) = pinggy.region
        {
            let r = region.trim().to_ascii_lowercase();
            if !r.is_empty() && !matches!(r.as_str(), "us" | "eu" | "ap" | "br" | "au") {
                anyhow::bail!(
                    "tunnel.pinggy.region must be one of: us, eu, ap, br, au (or omitted for auto)"
                );
            }
        }

        // Jira
        if self.jira.enabled {
            if self.jira.base_url.trim().is_empty() {
                anyhow::bail!("jira.base_url must not be empty when jira.enabled = true");
            }
            if self.jira.email.trim().is_empty() {
                anyhow::bail!("jira.email must not be empty when jira.enabled = true");
            }
            if self.jira.api_token.trim().is_empty()
                && std::env::var("JIRA_API_TOKEN")
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                anyhow::bail!(
                    "jira.api_token must be set (or JIRA_API_TOKEN env var) when jira.enabled = true"
                );
            }
            let valid_actions = ["get_ticket", "search_tickets", "comment_ticket"];
            for action in &self.jira.allowed_actions {
                if !valid_actions.contains(&action.as_str()) {
                    anyhow::bail!(
                        "jira.allowed_actions contains unknown action: '{}'. \
                         Valid: get_ticket, search_tickets, comment_ticket",
                        action
                    );
                }
            }
        }

        // Nevis IAM — delegate to NevisConfig::validate() for field-level checks
        if let Err(msg) = self.security.nevis.validate() {
            anyhow::bail!("security.nevis: {msg}");
        }

        // Delegate agent timeouts
        const MAX_DELEGATE_TIMEOUT_SECS: u64 = 3600;
        for (name, agent) in &self.agents {
            if let Some(timeout) = agent.timeout_secs {
                if timeout == 0 {
                    anyhow::bail!("agents.{name}.timeout_secs must be greater than 0");
                }
                if timeout > MAX_DELEGATE_TIMEOUT_SECS {
                    anyhow::bail!(
                        "agents.{name}.timeout_secs exceeds max {MAX_DELEGATE_TIMEOUT_SECS}"
                    );
                }
            }
            if let Some(timeout) = agent.agentic_timeout_secs {
                if timeout == 0 {
                    anyhow::bail!("agents.{name}.agentic_timeout_secs must be greater than 0");
                }
                if timeout > MAX_DELEGATE_TIMEOUT_SECS {
                    anyhow::bail!(
                        "agents.{name}.agentic_timeout_secs exceeds max {MAX_DELEGATE_TIMEOUT_SECS}"
                    );
                }
            }
        }

        // Transcription
        {
            let dp = self.transcription.default_provider.trim();
            match dp {
                "groq" | "openai" | "deepgram" | "assemblyai" | "google" | "local_whisper" => {}
                other => {
                    anyhow::bail!(
                        "transcription.default_provider must be one of: groq, openai, deepgram, assemblyai, google, local_whisper (got '{other}')"
                    );
                }
            }
        }

        // Delegate tool global defaults
        if self.delegate.timeout_secs == 0 {
            anyhow::bail!("delegate.timeout_secs must be greater than 0");
        }
        if self.delegate.agentic_timeout_secs == 0 {
            anyhow::bail!("delegate.agentic_timeout_secs must be greater than 0");
        }

        // Per-agent delegate timeout overrides
        for (name, agent) in &self.agents {
            if let Some(t) = agent.timeout_secs
                && t == 0
            {
                anyhow::bail!("agents.{name}.timeout_secs must be greater than 0");
            }
            if let Some(t) = agent.agentic_timeout_secs
                && t == 0
            {
                anyhow::bail!("agents.{name}.agentic_timeout_secs must be greater than 0");
            }
        }

        Ok(())
    }

    /// Ensure the fallback provider entry exists, creating it if necessary.
    pub fn ensure_fallback_provider(&mut self) -> &mut ModelProviderConfig {
        let fallback = self
            .providers
            .fallback
            .clone()
            .unwrap_or_else(|| "default".into());
        if self.providers.fallback.is_none() {
            self.providers.fallback = Some(fallback.clone());
        }
        self.providers.models.entry(fallback).or_default()
    }

    /// Apply environment variable overrides to config
    pub fn apply_env_overrides(&mut self) {
        // API Key: ZEROCLAW_API_KEY or API_KEY (generic)
        if let Ok(key) = std::env::var("ZEROCLAW_API_KEY").or_else(|_| std::env::var("API_KEY"))
            && !key.is_empty()
        {
            self.ensure_fallback_provider().api_key = Some(key);
        }
        // API Key: GLM_API_KEY overrides when provider is a GLM/Zhipu variant.
        if self.providers.fallback.as_deref().is_some_and(is_glm_alias)
            && let Ok(key) = std::env::var("GLM_API_KEY")
            && !key.is_empty()
        {
            self.ensure_fallback_provider().api_key = Some(key);
        }

        // API Key: ZAI_API_KEY overrides when provider is a Z.AI variant.
        if self.providers.fallback.as_deref().is_some_and(is_zai_alias)
            && let Ok(key) = std::env::var("ZAI_API_KEY")
            && !key.is_empty()
        {
            self.ensure_fallback_provider().api_key = Some(key);
        }

        // Provider override precedence:
        // 1) ZEROCLAW_PROVIDER always wins when set.
        // 2) ZEROCLAW_MODEL_PROVIDER/MODEL_PROVIDER (Codex app-server style).
        // 3) Legacy PROVIDER is honored only when config still uses default provider.
        if let Ok(provider) = std::env::var("ZEROCLAW_PROVIDER")
            && !provider.is_empty()
        {
            self.providers.fallback = Some(provider);
        } else if let Ok(provider) =
            std::env::var("ZEROCLAW_MODEL_PROVIDER").or_else(|_| std::env::var("MODEL_PROVIDER"))
            && !provider.is_empty()
        {
            self.providers.fallback = Some(provider);
        } else if let Ok(provider) = std::env::var("PROVIDER") {
            let should_apply_legacy_provider = self
                .providers
                .fallback
                .as_deref()
                .is_none_or(|configured| configured.trim().eq_ignore_ascii_case("openrouter"));
            if should_apply_legacy_provider && !provider.is_empty() {
                self.providers.fallback = Some(provider);
            }
        }

        // Model: ZEROCLAW_MODEL or MODEL
        if let Ok(model) = std::env::var("ZEROCLAW_MODEL").or_else(|_| std::env::var("MODEL"))
            && !model.is_empty()
        {
            self.ensure_fallback_provider().model = Some(model);
        }

        // Provider HTTP timeout: ZEROCLAW_PROVIDER_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_PROVIDER_TIMEOUT_SECS")
            && let Ok(timeout_secs) = timeout_secs.parse::<u64>()
            && timeout_secs > 0
        {
            self.ensure_fallback_provider().timeout_secs = Some(timeout_secs);
        }

        // Extra provider headers: ZEROCLAW_EXTRA_HEADERS
        // Format: "Key:Value,Key2:Value2"
        // Env var headers override config file headers with the same name.
        if let Ok(raw) = std::env::var("ZEROCLAW_EXTRA_HEADERS") {
            let entry = self.ensure_fallback_provider();
            for header in parse_extra_headers_env(&raw) {
                entry.extra_headers.insert(header.0, header.1);
            }
        }

        // Apply named provider profile remapping (Codex app-server compatibility).
        self.apply_named_model_provider_profile();

        // Workspace directory: ZEROCLAW_WORKSPACE
        if let Ok(workspace) = std::env::var("ZEROCLAW_WORKSPACE")
            && !workspace.is_empty()
        {
            let expanded = expand_tilde_path(&workspace);
            let (_, workspace_dir) = resolve_config_dir_for_workspace(&expanded);
            self.workspace_dir = workspace_dir;
        }

        // Open-skills opt-in flag: ZEROCLAW_OPEN_SKILLS_ENABLED
        if let Ok(flag) = std::env::var("ZEROCLAW_OPEN_SKILLS_ENABLED")
            && !flag.trim().is_empty()
        {
            match flag.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => self.skills.open_skills_enabled = true,
                "0" | "false" | "no" | "off" => self.skills.open_skills_enabled = false,
                _ => tracing::warn!(
                    "Ignoring invalid ZEROCLAW_OPEN_SKILLS_ENABLED (valid: 1|0|true|false|yes|no|on|off)"
                ),
            }
        }

        // Open-skills directory override: ZEROCLAW_OPEN_SKILLS_DIR
        if let Ok(path) = std::env::var("ZEROCLAW_OPEN_SKILLS_DIR") {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                self.skills.open_skills_dir = Some(trimmed.to_string());
            }
        }

        // Skills script-file audit override: ZEROCLAW_SKILLS_ALLOW_SCRIPTS
        if let Ok(flag) = std::env::var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS")
            && !flag.trim().is_empty()
        {
            match flag.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => self.skills.allow_scripts = true,
                "0" | "false" | "no" | "off" => self.skills.allow_scripts = false,
                _ => tracing::warn!(
                    "Ignoring invalid ZEROCLAW_SKILLS_ALLOW_SCRIPTS (valid: 1|0|true|false|yes|no|on|off)"
                ),
            }
        }

        // Skills prompt mode override: ZEROCLAW_SKILLS_PROMPT_MODE
        if let Ok(mode) = std::env::var("ZEROCLAW_SKILLS_PROMPT_MODE")
            && !mode.trim().is_empty()
        {
            if let Some(parsed) = parse_skills_prompt_injection_mode(&mode) {
                self.skills.prompt_injection_mode = parsed;
            } else {
                tracing::warn!(
                    "Ignoring invalid ZEROCLAW_SKILLS_PROMPT_MODE (valid: full|compact)"
                );
            }
        }

        // Gateway port: ZEROCLAW_GATEWAY_PORT or PORT
        if let Ok(port_str) =
            std::env::var("ZEROCLAW_GATEWAY_PORT").or_else(|_| std::env::var("PORT"))
            && let Ok(port) = port_str.parse::<u16>()
        {
            self.gateway.port = port;
        }

        // Gateway host: ZEROCLAW_GATEWAY_HOST or HOST
        if let Ok(host) = std::env::var("ZEROCLAW_GATEWAY_HOST").or_else(|_| std::env::var("HOST"))
            && !host.is_empty()
        {
            self.gateway.host = host;
        }

        // Allow public bind: ZEROCLAW_ALLOW_PUBLIC_BIND
        if let Ok(val) = std::env::var("ZEROCLAW_ALLOW_PUBLIC_BIND") {
            self.gateway.allow_public_bind = val == "1" || val.eq_ignore_ascii_case("true");
        }

        // Require pairing: ZEROCLAW_REQUIRE_PAIRING
        if let Ok(val) = std::env::var("ZEROCLAW_REQUIRE_PAIRING") {
            self.gateway.require_pairing = val == "1" || val.eq_ignore_ascii_case("true");
        }

        // Web dist dir: ZEROCLAW_WEB_DIST_DIR
        if let Ok(path) = std::env::var("ZEROCLAW_WEB_DIST_DIR") {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                self.gateway.web_dist_dir = Some(trimmed.to_string());
            }
        }

        // Temperature: ZEROCLAW_TEMPERATURE
        if let Ok(temp_str) = std::env::var("ZEROCLAW_TEMPERATURE") {
            match temp_str.parse::<f64>() {
                Ok(temp) if TEMPERATURE_RANGE.contains(&temp) => {
                    self.ensure_fallback_provider().temperature = Some(temp);
                }
                Ok(temp) => {
                    tracing::warn!(
                        "Ignoring ZEROCLAW_TEMPERATURE={temp}: \
                         value out of range (expected {}..={})",
                        TEMPERATURE_RANGE.start(),
                        TEMPERATURE_RANGE.end()
                    );
                }
                Err(_) => {
                    tracing::warn!(
                        "Ignoring ZEROCLAW_TEMPERATURE={temp_str:?}: not a valid number"
                    );
                }
            }
        }

        // Reasoning override: ZEROCLAW_REASONING_ENABLED or REASONING_ENABLED
        if let Ok(flag) = std::env::var("ZEROCLAW_REASONING_ENABLED")
            .or_else(|_| std::env::var("REASONING_ENABLED"))
        {
            let normalized = flag.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => self.runtime.reasoning_enabled = Some(true),
                "0" | "false" | "no" | "off" => self.runtime.reasoning_enabled = Some(false),
                _ => {}
            }
        }

        if let Ok(raw) = std::env::var("ZEROCLAW_REASONING_EFFORT")
            .or_else(|_| std::env::var("REASONING_EFFORT"))
            .or_else(|_| std::env::var("ZEROCLAW_CODEX_REASONING_EFFORT"))
        {
            match normalize_reasoning_effort(&raw) {
                Ok(effort) => self.runtime.reasoning_effort = Some(effort),
                Err(message) => tracing::warn!("Ignoring reasoning effort env override: {message}"),
            }
        }

        // Web search enabled: ZEROCLAW_WEB_SEARCH_ENABLED or WEB_SEARCH_ENABLED
        if let Ok(enabled) = std::env::var("ZEROCLAW_WEB_SEARCH_ENABLED")
            .or_else(|_| std::env::var("WEB_SEARCH_ENABLED"))
        {
            self.web_search.enabled = enabled == "1" || enabled.eq_ignore_ascii_case("true");
        }

        // Web search provider: ZEROCLAW_WEB_SEARCH_PROVIDER or WEB_SEARCH_PROVIDER
        if let Ok(provider) = std::env::var("ZEROCLAW_WEB_SEARCH_PROVIDER")
            .or_else(|_| std::env::var("WEB_SEARCH_PROVIDER"))
        {
            let provider = provider.trim();
            if !provider.is_empty() {
                self.web_search.provider = provider.to_string();
            }
        }

        // Brave API key: ZEROCLAW_BRAVE_API_KEY or BRAVE_API_KEY
        if let Ok(api_key) =
            std::env::var("ZEROCLAW_BRAVE_API_KEY").or_else(|_| std::env::var("BRAVE_API_KEY"))
        {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                self.web_search.brave_api_key = Some(api_key.to_string());
            }
        }

        // SearXNG instance URL: ZEROCLAW_SEARXNG_INSTANCE_URL or SEARXNG_INSTANCE_URL
        if let Ok(instance_url) = std::env::var("ZEROCLAW_SEARXNG_INSTANCE_URL")
            .or_else(|_| std::env::var("SEARXNG_INSTANCE_URL"))
        {
            let instance_url = instance_url.trim();
            if !instance_url.is_empty() {
                self.web_search.searxng_instance_url = Some(instance_url.to_string());
            }
        }

        // Web search max results: ZEROCLAW_WEB_SEARCH_MAX_RESULTS or WEB_SEARCH_MAX_RESULTS
        if let Ok(max_results) = std::env::var("ZEROCLAW_WEB_SEARCH_MAX_RESULTS")
            .or_else(|_| std::env::var("WEB_SEARCH_MAX_RESULTS"))
            && let Ok(max_results) = max_results.parse::<usize>()
            && (1..=10).contains(&max_results)
        {
            self.web_search.max_results = max_results;
        }

        // Web search timeout: ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS or WEB_SEARCH_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS")
            .or_else(|_| std::env::var("WEB_SEARCH_TIMEOUT_SECS"))
            && let Ok(timeout_secs) = timeout_secs.parse::<u64>()
            && timeout_secs > 0
        {
            self.web_search.timeout_secs = timeout_secs;
        }

        // Storage provider key (optional backend override): ZEROCLAW_STORAGE_PROVIDER
        if let Ok(provider) = std::env::var("ZEROCLAW_STORAGE_PROVIDER") {
            let provider = provider.trim();
            if !provider.is_empty() {
                self.storage.provider.config.provider = provider.to_string();
            }
        }

        // Storage connection URL (for remote backends): ZEROCLAW_STORAGE_DB_URL
        if let Ok(db_url) = std::env::var("ZEROCLAW_STORAGE_DB_URL") {
            let db_url = db_url.trim();
            if !db_url.is_empty() {
                self.storage.provider.config.db_url = Some(db_url.to_string());
            }
        }

        // Storage connect timeout: ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS")
            && let Ok(timeout_secs) = timeout_secs.parse::<u64>()
            && timeout_secs > 0
        {
            self.storage.provider.config.connect_timeout_secs = Some(timeout_secs);
        }
        // Proxy enabled flag: ZEROCLAW_PROXY_ENABLED
        let explicit_proxy_enabled = std::env::var("ZEROCLAW_PROXY_ENABLED")
            .ok()
            .as_deref()
            .and_then(parse_proxy_enabled);
        if let Some(enabled) = explicit_proxy_enabled {
            self.proxy.enabled = enabled;
        }

        // Proxy URLs: ZEROCLAW_* wins, then generic *PROXY vars.
        let mut proxy_url_overridden = false;
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_HTTP_PROXY").or_else(|_| std::env::var("HTTP_PROXY"))
        {
            self.proxy.http_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_HTTPS_PROXY").or_else(|_| std::env::var("HTTPS_PROXY"))
        {
            self.proxy.https_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(proxy_url) =
            std::env::var("ZEROCLAW_ALL_PROXY").or_else(|_| std::env::var("ALL_PROXY"))
        {
            self.proxy.all_proxy = normalize_proxy_url_option(Some(&proxy_url));
            proxy_url_overridden = true;
        }
        if let Ok(no_proxy) =
            std::env::var("ZEROCLAW_NO_PROXY").or_else(|_| std::env::var("NO_PROXY"))
        {
            self.proxy.no_proxy = normalize_no_proxy_list(vec![no_proxy]);
        }

        if explicit_proxy_enabled.is_none()
            && proxy_url_overridden
            && self.proxy.has_any_proxy_url()
        {
            self.proxy.enabled = true;
        }

        // Proxy scope and service selectors.
        if let Ok(scope_raw) = std::env::var("ZEROCLAW_PROXY_SCOPE") {
            if let Some(scope) = parse_proxy_scope(&scope_raw) {
                self.proxy.scope = scope;
            } else {
                tracing::warn!(
                    scope = %scope_raw,
                    "Ignoring invalid ZEROCLAW_PROXY_SCOPE (valid: environment|zeroclaw|services)"
                );
            }
        }

        if let Ok(services_raw) = std::env::var("ZEROCLAW_PROXY_SERVICES") {
            self.proxy.services = normalize_service_list(vec![services_raw]);
        }

        if let Err(error) = self.proxy.validate() {
            tracing::warn!("Invalid proxy configuration ignored: {error}");
            self.proxy.enabled = false;
        }

        if self.proxy.enabled && self.proxy.scope == ProxyScope::Environment {
            self.proxy.apply_to_process_env();
        }

        set_runtime_proxy_config(self.proxy.clone());

        if self.conversational_ai.enabled {
            tracing::warn!(
                "conversational_ai.enabled = true but conversational AI features are not yet \
                 implemented; this section is reserved for future use and will be ignored"
            );
        }
    }

    async fn resolve_config_path_for_save(&self) -> Result<PathBuf> {
        if self
            .config_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            return Ok(self.config_path.clone());
        }

        let (default_zeroclaw_dir, default_workspace_dir) = default_config_and_workspace_dirs()?;
        let (zeroclaw_dir, _workspace_dir, source) =
            resolve_runtime_config_dirs(&default_zeroclaw_dir, &default_workspace_dir).await?;
        let file_name = self
            .config_path
            .file_name()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| std::ffi::OsStr::new("config.toml"));
        let resolved = zeroclaw_dir.join(file_name);
        tracing::warn!(
            path = %self.config_path.display(),
            resolved = %resolved.display(),
            source = source.as_str(),
            "Config path missing parent directory; resolving from runtime environment"
        );
        Ok(resolved)
    }

    pub async fn save(&self) -> Result<()> {
        // Encrypt secrets before serialization
        let mut config_to_save = self.clone();
        let config_path = self.resolve_config_path_for_save().await?;
        let zeroclaw_dir = config_path
            .parent()
            .context("Config path must have a parent directory")?;
        let store = crate::secrets::SecretStore::new(zeroclaw_dir, self.secrets.encrypt);

        // Encrypt all #[secret]-annotated fields via Configurable derive
        config_to_save.encrypt_secrets(&store)?;

        let new_toml =
            toml::to_string_pretty(&config_to_save).context("Failed to serialize config")?;

        // If an existing config file is present, sync the new values onto it
        // to preserve comments and formatting. Otherwise, use the fresh serialization.
        let toml_str = if config_path.exists() {
            let existing = fs::read_to_string(&config_path).await.unwrap_or_default();
            if existing.is_empty() {
                new_toml
            } else {
                let new_table: toml::Table =
                    toml::from_str(&new_toml).context("Failed to round-trip serialized config")?;
                let mut doc: toml_edit::DocumentMut = existing
                    .parse()
                    .context("Failed to parse existing config for comment preservation")?;
                crate::migration::sync_table(doc.as_table_mut(), &new_table);
                doc.to_string()
            }
        } else {
            new_toml
        };

        let parent_dir = config_path
            .parent()
            .context("Config path must have a parent directory")?;

        fs::create_dir_all(parent_dir).await.with_context(|| {
            format!(
                "Failed to create config directory: {}",
                parent_dir.display()
            )
        })?;

        let file_name = config_path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("config.toml");
        let temp_path = parent_dir.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
        let backup_path = parent_dir.join(format!("{file_name}.bak"));

        let mut temp_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to create temporary config file: {}",
                    temp_path.display()
                )
            })?;
        temp_file
            .write_all(toml_str.as_bytes())
            .await
            .context("Failed to write temporary config contents")?;
        temp_file
            .sync_all()
            .await
            .context("Failed to fsync temporary config file")?;
        drop(temp_file);

        let had_existing_config = config_path.exists();
        if had_existing_config {
            fs::copy(&config_path, &backup_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to create config backup before atomic replace: {}",
                        backup_path.display()
                    )
                })?;
        }

        if let Err(e) = fs::rename(&temp_path, &config_path).await {
            let _ = fs::remove_file(&temp_path).await;
            if had_existing_config && backup_path.exists() {
                fs::copy(&backup_path, &config_path)
                    .await
                    .context("Failed to restore config backup")?;
            }
            anyhow::bail!("Failed to atomically replace config file: {e}");
        }

        #[cfg(unix)]
        {
            use std::{fs::Permissions, os::unix::fs::PermissionsExt};
            if let Err(err) = fs::set_permissions(&config_path, Permissions::from_mode(0o600)).await
            {
                tracing::warn!(
                    "Failed to harden config permissions to 0600 at {}: {}",
                    config_path.display(),
                    err
                );
            }
        }

        sync_directory(parent_dir).await?;

        if had_existing_config {
            let _ = fs::remove_file(&backup_path).await;
        }

        Ok(())
    }
}

#[allow(clippy::unused_async)] // async needed on unix for tokio File I/O; no-op on other platforms
async fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let dir = File::open(path)
            .await
            .with_context(|| format!("Failed to open directory for fsync: {}", path.display()))?;
        dir.sync_all()
            .await
            .with_context(|| format!("Failed to fsync directory metadata: {}", path.display()))?;
        Ok(())
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .with_context(|| format!("Failed to open directory for fsync: {}", path.display()))?;
        // FlushFileBuffers on directory handles returns ERROR_ACCESS_DENIED on
        // Windows (OS Error 5). This is expected — NTFS does not support
        // flushing directory metadata the same way Unix does. The individual
        // files have already been synced, so it is safe to ignore this error.
        if let Err(e) = dir.sync_all() {
            if e.raw_os_error() == Some(5) {
                tracing::trace!(
                    "Ignoring expected ACCESS_DENIED when fsyncing directory on Windows: {}",
                    path.display()
                );
            } else {
                return Err(e).with_context(|| {
                    format!("Failed to fsync directory metadata: {}", path.display())
                });
            }
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

// ── SOP engine configuration ───────────────────────────────────

/// Standard Operating Procedures engine configuration (`[sop]`).
///
/// The `default_execution_mode` field uses the `SopExecutionMode` type from
/// `sop::types` (re-exported via `sop::SopExecutionMode`). To avoid circular
/// module references, config stores it using the same enum definition.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "sop"]
pub struct SopConfig {
    /// Directory containing SOP definitions (subdirs with SOP.toml + SOP.md).
    /// Falls back to `<workspace>/sops` when omitted.
    #[serde(default)]
    pub sops_dir: Option<String>,

    /// Default execution mode for SOPs that omit `execution_mode`.
    /// Values: `auto`, `supervised` (default), `step_by_step`,
    /// `priority_based`, `deterministic`.
    #[serde(default = "default_sop_execution_mode")]
    pub default_execution_mode: String,

    /// Maximum total concurrent SOP runs across all SOPs.
    #[serde(default = "default_sop_max_concurrent_total")]
    pub max_concurrent_total: usize,

    /// Approval timeout in seconds. When a run waits for approval longer than
    /// this, Critical/High-priority SOPs auto-approve; others stay waiting.
    /// Set to 0 to disable timeout.
    #[serde(default = "default_sop_approval_timeout_secs")]
    pub approval_timeout_secs: u64,

    /// Maximum number of finished runs kept in memory for status queries.
    /// Oldest runs are evicted when over capacity. 0 = unlimited.
    #[serde(default = "default_sop_max_finished_runs")]
    pub max_finished_runs: usize,
}

fn default_sop_execution_mode() -> String {
    "supervised".to_string()
}

fn default_sop_max_concurrent_total() -> usize {
    4
}

fn default_sop_approval_timeout_secs() -> u64 {
    300
}

fn default_sop_max_finished_runs() -> usize {
    100
}

impl Default for SopConfig {
    fn default() -> Self {
        Self {
            sops_dir: None,
            default_execution_mode: default_sop_execution_mode(),
            max_concurrent_total: default_sop_max_concurrent_total(),
            approval_timeout_secs: default_sop_approval_timeout_secs(),
            max_finished_runs: default_sop_max_finished_runs(),
        }
    }
}

// ── HasPropKind impls for config enums ──
// Scalars (bool, String, integers, floats) are covered by impl_prop_kind! in traits.rs.
// Config enums serialize as TOML strings and are classified as PropKind::Enum.
macro_rules! impl_enum_prop_kind {
    ($($ty:ty),+ $(,)?) => {
        $(impl HasPropKind for $ty { const PROP_KIND: PropKind = PropKind::Enum; })+
    };
}
impl_enum_prop_kind!(
    SwarmStrategy,
    HardwareTransport,
    McpTransport,
    ToolFilterGroupMode,
    SkillsPromptInjectionMode,
    FirecrawlMode,
    ProxyScope,
    SearchMode,
    CronScheduleDecl,
    StreamMode,
    WhatsAppWebMode,
    WhatsAppChatPolicy,
    LineDmPolicy,
    LineGroupPolicy,
    LarkReceiveMode,
    OtpMethod,
    SandboxBackend,
    AutonomyLevel,
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::TempDir;
    use tokio::sync::{Mutex, MutexGuard};
    use tokio::test;

    // ── Tilde expansion ───────────────────────────────────────

    #[test]
    async fn expand_tilde_path_handles_absolute_path() {
        let path = expand_tilde_path("/absolute/path");
        assert_eq!(path, PathBuf::from("/absolute/path"));
    }

    #[test]
    async fn expand_tilde_path_handles_relative_path() {
        let path = expand_tilde_path("relative/path");
        assert_eq!(path, PathBuf::from("relative/path"));
    }

    #[test]
    async fn expand_tilde_path_expands_tilde_when_home_set() {
        // This test verifies that tilde expansion works when HOME is set.
        // In normal environments, HOME is set, so ~ should expand.
        let path = expand_tilde_path("~/.zeroclaw");
        // The path should not literally start with '~' if HOME is set
        // (it should be expanded to the actual home directory)
        if std::env::var("HOME").is_ok() {
            assert!(
                !path.to_string_lossy().starts_with('~'),
                "Tilde should be expanded when HOME is set"
            );
        }
    }

    // ── Defaults ─────────────────────────────────────────────

    fn has_test_table(raw: &str, table: &str) -> bool {
        let exact = format!("[{table}]");
        let nested = format!("[{table}.");
        raw.lines()
            .map(str::trim)
            .any(|line| line == exact || line.starts_with(&nested))
    }

    fn parse_test_config(raw: &str) -> Config {
        let mut merged = raw.trim().to_string();
        for table in [
            "data_retention",
            "cloud_ops",
            "conversational_ai",
            "security",
            "security_ops",
        ] {
            if has_test_table(&merged, table) {
                continue;
            }
            if !merged.is_empty() {
                merged.push_str("\n\n");
            }
            merged.push('[');
            merged.push_str(table);
            merged.push(']');
        }
        merged.push('\n');
        // Deserialize through V1Compat to handle legacy top-level fields.
        let compat: crate::migration::V1Compat = toml::from_str(&merged).unwrap();
        let mut config = compat.into_config();
        config.autonomy.ensure_default_auto_approve();
        config
    }

    #[test]
    async fn http_request_config_default_has_correct_values() {
        let cfg = HttpRequestConfig::default();
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_response_size, 1_000_000);
        assert!(cfg.enabled);
        assert_eq!(cfg.allowed_domains, vec!["*".to_string()]);
    }

    #[test]
    async fn config_default_has_sane_values() {
        let c = Config::default();
        // V2: no fallback provider by default — set during onboarding.
        assert!(c.providers.fallback.is_none());
        assert!(c.providers.fallback_provider().is_none());
        assert!(!c.skills.open_skills_enabled);
        assert!(!c.skills.allow_scripts);
        assert_eq!(
            c.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Full
        );
        assert!(c.workspace_dir.to_string_lossy().contains("workspace"));
        assert!(c.config_path.to_string_lossy().contains("config.toml"));
    }

    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<StdMutex<Vec<u8>>>);

    struct SharedLogWriter(Arc<StdMutex<Vec<u8>>>);

    impl SharedLogBuffer {
        fn captured(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedLogBuffer {
        type Writer = SharedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedLogWriter(self.0.clone())
        }
    }

    impl io::Write for SharedLogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    async fn config_dir_creation_error_mentions_openrc_and_path() {
        let msg = config_dir_creation_error(Path::new("/etc/zeroclaw"));
        assert!(msg.contains("/etc/zeroclaw"));
        assert!(msg.contains("OpenRC"));
        assert!(msg.contains("zeroclaw"));
    }

    #[test]
    async fn config_schema_export_contains_expected_contract_shape() {
        #[cfg(feature = "schema-export")]
        let schema = schemars::schema_for!(Config);
        let schema_json = serde_json::to_value(&schema).expect("schema should serialize to json");

        assert_eq!(
            schema_json
                .get("$schema")
                .and_then(serde_json::Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );

        let properties = schema_json
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("schema should expose top-level properties");

        assert!(properties.contains_key("providers"));
        assert!(properties.contains_key("skills"));
        assert!(properties.contains_key("gateway"));
        assert!(properties.contains_key("channels"));
        assert!(!properties.contains_key("workspace_dir"));
        assert!(!properties.contains_key("config_path"));
        // These fields are now #[serde(skip)] cache fields, not in schema.
        assert!(!properties.contains_key("default_provider"));
        assert!(!properties.contains_key("api_key"));
        assert!(!properties.contains_key("default_model"));

        assert!(
            schema_json
                .get("$defs")
                .and_then(serde_json::Value::as_object)
                .is_some(),
            "schema should include reusable type definitions"
        );
    }

    #[cfg(unix)]
    #[test]
    async fn save_sets_config_permissions_on_new_file() {
        let temp = TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");

        let config = Config {
            config_path: config_path.clone(),
            workspace_dir,
            ..Default::default()
        };

        config.save().await.expect("save config");

        let mode = std::fs::metadata(&config_path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    async fn observability_config_default() {
        let o = ObservabilityConfig::default();
        assert_eq!(o.backend, "none");
        assert_eq!(o.runtime_trace_mode, "none");
        assert_eq!(o.runtime_trace_path, "state/runtime-trace.jsonl");
        assert_eq!(o.runtime_trace_max_entries, 200);
    }

    #[test]
    async fn autonomy_config_default() {
        let a = AutonomyConfig::default();
        assert_eq!(a.level, AutonomyLevel::Supervised);
        assert!(a.workspace_only);
        assert!(a.allowed_commands.contains(&"git".to_string()));
        assert!(a.allowed_commands.contains(&"cargo".to_string()));
        assert!(a.forbidden_paths.contains(&"/etc".to_string()));
        assert_eq!(a.max_actions_per_hour, 20);
        assert_eq!(a.max_cost_per_day_cents, 500);
        assert!(a.require_approval_for_medium_risk);
        assert!(a.block_high_risk_commands);
        assert!(a.shell_env_passthrough.is_empty());
    }

    #[test]
    async fn runtime_config_default() {
        let r = RuntimeConfig::default();
        assert_eq!(r.kind, "native");
        assert_eq!(r.docker.image, "alpine:3.20");
        assert_eq!(r.docker.network, "none");
        assert_eq!(r.docker.memory_limit_mb, Some(512));
        assert_eq!(r.docker.cpu_limit, Some(1.0));
        assert!(r.docker.read_only_rootfs);
        assert!(r.docker.mount_workspace);
    }

    #[test]
    async fn heartbeat_config_default() {
        let h = HeartbeatConfig::default();
        assert!(h.enabled);
        assert_eq!(h.interval_minutes, 30);
        assert!(h.message.is_none());
        assert!(h.target.is_none());
        assert!(h.to.is_none());
    }

    #[test]
    async fn heartbeat_config_parses_delivery_aliases() {
        let raw = r#"
enabled = true
interval_minutes = 10
message = "Ping"
channel = "telegram"
recipient = "42"
"#;
        let parsed: HeartbeatConfig = toml::from_str(raw).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.interval_minutes, 10);
        assert_eq!(parsed.message.as_deref(), Some("Ping"));
        assert_eq!(parsed.target.as_deref(), Some("telegram"));
        assert_eq!(parsed.to.as_deref(), Some("42"));
    }

    #[test]
    async fn cron_config_default() {
        let c = CronConfig::default();
        assert!(c.enabled);
        assert_eq!(c.max_run_history, 50);
    }

    #[test]
    async fn cron_config_serde_roundtrip() {
        let c = CronConfig {
            enabled: false,
            catch_up_on_startup: false,
            max_run_history: 100,
            jobs: Vec::new(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: CronConfig = serde_json::from_str(&json).unwrap();
        assert!(!parsed.enabled);
        assert!(!parsed.catch_up_on_startup);
        assert_eq!(parsed.max_run_history, 100);
    }

    #[test]
    async fn config_defaults_cron_when_section_missing() {
        let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;

        let parsed = parse_test_config(toml_str);
        assert!(parsed.cron.enabled);
        assert!(parsed.cron.catch_up_on_startup);
        assert_eq!(parsed.cron.max_run_history, 50);
    }

    #[test]
    async fn memory_config_default_hygiene_settings() {
        let m = MemoryConfig::default();
        assert_eq!(m.backend, "sqlite");
        assert!(m.auto_save);
        assert!(m.hygiene_enabled);
        assert_eq!(m.archive_after_days, 7);
        assert_eq!(m.purge_after_days, 30);
        assert_eq!(m.conversation_retention_days, 30);
        assert!(m.sqlite_open_timeout_secs.is_none());
        assert_eq!(m.search_mode, SearchMode::Hybrid);
    }

    #[test]
    async fn search_mode_config_deserialization() {
        let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "bm25"
"#;
        let parsed = parse_test_config(toml_str);
        assert_eq!(parsed.memory.search_mode, SearchMode::Bm25);

        let toml_str_embedding = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "embedding"
"#;
        let parsed = parse_test_config(toml_str_embedding);
        assert_eq!(parsed.memory.search_mode, SearchMode::Embedding);

        let toml_str_hybrid = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
search_mode = "hybrid"
"#;
        let parsed = parse_test_config(toml_str_hybrid);
        assert_eq!(parsed.memory.search_mode, SearchMode::Hybrid);
    }

    #[test]
    async fn search_mode_defaults_to_hybrid_when_omitted() {
        let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7

[memory]
backend = "sqlite"
auto_save = true
"#;
        let parsed = parse_test_config(toml_str);
        assert_eq!(parsed.memory.search_mode, SearchMode::Hybrid);
    }

    #[test]
    async fn search_mode_serde_roundtrip() {
        let json_bm25 = serde_json::to_string(&SearchMode::Bm25).unwrap();
        assert_eq!(json_bm25, "\"bm25\"");
        let parsed: SearchMode = serde_json::from_str(&json_bm25).unwrap();
        assert_eq!(parsed, SearchMode::Bm25);

        let json_embedding = serde_json::to_string(&SearchMode::Embedding).unwrap();
        assert_eq!(json_embedding, "\"embedding\"");
        let parsed: SearchMode = serde_json::from_str(&json_embedding).unwrap();
        assert_eq!(parsed, SearchMode::Embedding);

        let json_hybrid = serde_json::to_string(&SearchMode::Hybrid).unwrap();
        assert_eq!(json_hybrid, "\"hybrid\"");
        let parsed: SearchMode = serde_json::from_str(&json_hybrid).unwrap();
        assert_eq!(parsed, SearchMode::Hybrid);
    }

    #[test]
    async fn storage_provider_config_defaults() {
        let storage = StorageConfig::default();
        assert!(storage.provider.config.provider.is_empty());
        assert!(storage.provider.config.db_url.is_none());
        assert_eq!(storage.provider.config.schema, "public");
        assert_eq!(storage.provider.config.table, "memories");
        assert!(storage.provider.config.connect_timeout_secs.is_none());
    }

    #[test]
    async fn channels_default() {
        let c = ChannelsConfig::default();
        assert!(c.cli);
        assert!(c.telegram.is_none());
        assert!(c.discord.is_none());
        assert!(!c.show_tool_calls);
    }

    // ── Serde round-trip ─────────────────────────────────────

    #[test]
    async fn config_toml_roundtrip() {
        let config = Config {
            schema_version: crate::migration::CURRENT_SCHEMA_VERSION,
            providers: crate::providers::ProvidersConfig {
                fallback: Some("openrouter".into()),
                models: {
                    let mut m = HashMap::new();
                    m.insert(
                        "openrouter".into(),
                        ModelProviderConfig {
                            api_key: Some("sk-test-key".into()),
                            model: Some("gpt-4o".into()),
                            temperature: Some(0.5),
                            timeout_secs: Some(120),
                            ..Default::default()
                        },
                    );
                    m
                },
                model_routes: Vec::new(),
                embedding_routes: Vec::new(),
            },
            workspace_dir: PathBuf::from("/tmp/test/workspace"),
            config_path: PathBuf::from("/tmp/test/config.toml"),
            observability: ObservabilityConfig {
                backend: "log".into(),
                ..ObservabilityConfig::default()
            },
            autonomy: AutonomyConfig {
                level: AutonomyLevel::Full,
                workspace_only: false,
                allowed_commands: vec!["docker".into()],
                forbidden_paths: vec!["/secret".into()],
                max_actions_per_hour: 50,
                max_cost_per_day_cents: 1000,
                require_approval_for_medium_risk: false,
                block_high_risk_commands: true,
                shell_env_passthrough: vec!["DATABASE_URL".into()],
                auto_approve: vec!["file_read".into()],
                always_ask: vec![],
                allowed_roots: vec![],
                non_cli_excluded_tools: vec![],
                shell_timeout_secs: default_shell_timeout_secs(),
            },
            trust: crate::scattered_types::TrustConfig::default(),
            backup: BackupConfig::default(),
            data_retention: DataRetentionConfig::default(),
            cloud_ops: CloudOpsConfig::default(),
            conversational_ai: ConversationalAiConfig::default(),
            security: SecurityConfig::default(),
            security_ops: SecurityOpsConfig::default(),
            runtime: RuntimeConfig {
                kind: "docker".into(),
                ..RuntimeConfig::default()
            },
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            skills: SkillsConfig::default(),
            pipeline: PipelineConfig::default(),
            query_classification: QueryClassificationConfig::default(),
            heartbeat: HeartbeatConfig {
                enabled: true,
                interval_minutes: 15,
                two_phase: true,
                message: Some("Check London time".into()),
                target: Some("telegram".into()),
                to: Some("123456".into()),
                ..HeartbeatConfig::default()
            },
            cron: CronConfig::default(),
            channels: ChannelsConfig {
                cli: true,
                telegram: Some(TelegramConfig {
                    enabled: true,
                    bot_token: "123:ABC".into(),
                    allowed_users: vec!["user1".into()],
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: default_draft_update_interval_ms(),
                    interrupt_on_new_message: false,
                    mention_only: false,
                    ack_reactions: None,
                    proxy_url: None,
                    approval_timeout_secs: default_telegram_approval_timeout_secs(),
                }),
                discord: None,
                discord_history: None,
                slack: None,
                mattermost: None,
                webhook: None,
                imessage: None,
                matrix: None,
                signal: None,
                whatsapp: None,
                linq: None,
                wati: None,
                nextcloud_talk: None,
                email: None,
                gmail_push: None,
                irc: None,
                lark: None,
                line: None,
                feishu: None,
                dingtalk: None,
                wecom: None,
                qq: None,
                twitter: None,
                mochat: None,
                #[cfg(feature = "channel-nostr")]
                nostr: None,
                clawdtalk: None,
                reddit: None,
                bluesky: None,
                voice_call: None,
                #[cfg(feature = "one2x")]
                web: None,
                #[cfg(feature = "voice-wake")]
                voice_wake: None,
                mqtt: None,
                message_timeout_secs: 300,
                ack_reactions: true,
                show_tool_calls: true,
                session_persistence: true,
                session_backend: default_session_backend(),
                session_ttl_hours: 0,
                debounce_ms: 0,
            },
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            microsoft365: Microsoft365Config::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            browser_delegate: crate::scattered_types::BrowserDelegateConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            media_pipeline: MediaPipelineConfig::default(),
            web_fetch: WebFetchConfig::default(),
            link_enricher: LinkEnricherConfig::default(),
            text_browser: TextBrowserConfig::default(),
            web_search: WebSearchConfig::default(),
            project_intel: ProjectIntelConfig::default(),
            google_workspace: GoogleWorkspaceConfig::default(),
            proxy: ProxyConfig::default(),
            agent: AgentConfig::default(),
            pacing: PacingConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            peripherals: PeripheralsConfig::default(),
            delegate: DelegateToolConfig::default(),
            agents: HashMap::new(),
            swarms: HashMap::new(),
            hooks: HooksConfig::default(),
            hardware: HardwareConfig::default(),
            transcription: TranscriptionConfig::default(),
            tts: TtsConfig::default(),
            mcp: McpConfig::default(),
            nodes: NodesConfig::default(),
            workspace: WorkspaceConfig::default(),
            notion: NotionConfig::default(),
            jira: JiraConfig::default(),
            node_transport: NodeTransportConfig::default(),
            knowledge: KnowledgeConfig::default(),
            linkedin: LinkedInConfig::default(),
            image_gen: ImageGenConfig::default(),
            plugins: PluginsConfig::default(),
            locale: None,
            verifiable_intent: VerifiableIntentConfig::default(),
            claude_code: ClaudeCodeConfig::default(),
            claude_code_runner: ClaudeCodeRunnerConfig::default(),
            codex_cli: CodexCliConfig::default(),
            gemini_cli: GeminiCliConfig::default(),
            opencode_cli: OpenCodeCliConfig::default(),
            sop: SopConfig::default(),
            shell_tool: ShellToolConfig::default(),
        };
        // Provider fields are now resolved directly — no cache needed.

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed = parse_test_config(&toml_str);

        assert_eq!(parsed.providers.fallback, config.providers.fallback);
        assert_eq!(parsed.observability.backend, "log");
        assert_eq!(parsed.observability.runtime_trace_mode, "none");
        assert_eq!(parsed.autonomy.level, AutonomyLevel::Full);
        assert!(!parsed.autonomy.workspace_only);
        assert_eq!(parsed.runtime.kind, "docker");
        assert!(parsed.heartbeat.enabled);
        assert_eq!(parsed.heartbeat.interval_minutes, 15);
        assert_eq!(
            parsed.heartbeat.message.as_deref(),
            Some("Check London time")
        );
        assert_eq!(parsed.heartbeat.target.as_deref(), Some("telegram"));
        assert_eq!(parsed.heartbeat.to.as_deref(), Some("123456"));
        assert!(parsed.channels.telegram.is_some());
        assert_eq!(parsed.channels.telegram.unwrap().bot_token, "123:ABC");
    }

    #[test]
    async fn config_minimal_toml_uses_defaults() {
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(minimal);
        assert!(
            parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref())
                .is_none()
        );
        assert_eq!(parsed.observability.backend, "none");
        assert_eq!(parsed.observability.runtime_trace_mode, "none");
        assert_eq!(parsed.autonomy.level, AutonomyLevel::Supervised);
        assert_eq!(parsed.runtime.kind, "native");
        assert!(parsed.heartbeat.enabled);
        assert!(parsed.channels.cli);
        assert!(parsed.memory.hygiene_enabled);
        assert_eq!(parsed.memory.archive_after_days, 7);
        assert_eq!(parsed.memory.purge_after_days, 30);
        assert_eq!(parsed.memory.conversation_retention_days, 30);
        // Temperature migrated to the fallback provider entry
        assert!(
            (parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.temperature)
                .unwrap_or(0.7)
                - 0.7)
                .abs()
                < f64::EPSILON
        );
        assert_eq!(
            parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.timeout_secs)
                .unwrap_or(120),
            DEFAULT_DELEGATE_TIMEOUT_SECS
        );
    }

    /// Regression test for #4171: the `[autonomy]` section must not be
    /// silently dropped when parsing config TOML.
    #[test]
    async fn autonomy_section_is_not_silently_ignored() {
        let raw = r#"
default_temperature = 0.7

[autonomy]
level = "full"
max_actions_per_hour = 99
auto_approve = ["file_read", "memory_recall", "http_request"]
"#;
        let parsed = parse_test_config(raw);
        assert_eq!(
            parsed.autonomy.level,
            AutonomyLevel::Full,
            "autonomy.level must be parsed from config (was silently defaulting to Supervised)"
        );
        assert_eq!(
            parsed.autonomy.max_actions_per_hour, 99,
            "autonomy.max_actions_per_hour must be parsed from config"
        );
        assert!(
            parsed
                .autonomy
                .auto_approve
                .contains(&"http_request".to_string()),
            "autonomy.auto_approve must include http_request from config"
        );
    }

    /// Regression test for #4247: when a user provides a custom auto_approve
    /// list, the built-in defaults must still be present.
    #[test]
    async fn auto_approve_merges_user_entries_with_defaults() {
        let raw = r#"
default_temperature = 0.7

[autonomy]
auto_approve = ["my_custom_tool", "another_tool"]
"#;
        let parsed = parse_test_config(raw);
        // User entries are preserved
        assert!(
            parsed
                .autonomy
                .auto_approve
                .contains(&"my_custom_tool".to_string()),
            "user-supplied tool must remain in auto_approve"
        );
        assert!(
            parsed
                .autonomy
                .auto_approve
                .contains(&"another_tool".to_string()),
            "user-supplied tool must remain in auto_approve"
        );
        // Defaults are merged in
        for default_tool in &[
            "file_read",
            "memory_recall",
            "weather",
            "calculator",
            "web_fetch",
        ] {
            assert!(
                parsed
                    .autonomy
                    .auto_approve
                    .contains(&String::from(*default_tool)),
                "default tool '{default_tool}' must be present in auto_approve even when user provides custom list"
            );
        }
    }

    /// Regression test: empty auto_approve still gets defaults merged.
    #[test]
    async fn auto_approve_empty_list_gets_defaults() {
        let raw = r#"
default_temperature = 0.7

[autonomy]
auto_approve = []
"#;
        let parsed = parse_test_config(raw);
        let defaults = default_auto_approve();
        for tool in &defaults {
            assert!(
                parsed.autonomy.auto_approve.contains(tool),
                "default tool '{tool}' must be present even when user sets auto_approve = []"
            );
        }
    }

    /// When no autonomy section is provided, defaults are applied normally.
    #[test]
    async fn auto_approve_defaults_when_no_autonomy_section() {
        let raw = r#"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(raw);
        let defaults = default_auto_approve();
        for tool in &defaults {
            assert!(
                parsed.autonomy.auto_approve.contains(tool),
                "default tool '{tool}' must be present when no [autonomy] section"
            );
        }
    }

    /// Duplicates are not introduced when ensure_default_auto_approve runs
    /// on a list that already contains the defaults.
    #[test]
    async fn auto_approve_no_duplicates() {
        let raw = r#"
default_temperature = 0.7

[autonomy]
auto_approve = ["weather", "file_read"]
"#;
        let parsed = parse_test_config(raw);
        let weather_count = parsed
            .autonomy
            .auto_approve
            .iter()
            .filter(|t| *t == "weather")
            .count();
        assert_eq!(weather_count, 1, "weather must not be duplicated");
        let file_read_count = parsed
            .autonomy
            .auto_approve
            .iter()
            .filter(|t| *t == "file_read")
            .count();
        assert_eq!(file_read_count, 1, "file_read must not be duplicated");
    }

    #[test]
    async fn provider_timeout_secs_parses_from_toml() {
        let raw = r#"
default_temperature = 0.7
provider_timeout_secs = 300
"#;
        let parsed = parse_test_config(raw);
        assert_eq!(
            parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.timeout_secs)
                .unwrap_or(120),
            300
        );
    }

    #[test]
    async fn parse_extra_headers_env_basic() {
        let headers = parse_extra_headers_env("User-Agent:MyApp/1.0,X-Title:zeroclaw");
        assert_eq!(headers.len(), 2);
        assert_eq!(
            headers[0],
            ("User-Agent".to_string(), "MyApp/1.0".to_string())
        );
        assert_eq!(headers[1], ("X-Title".to_string(), "zeroclaw".to_string()));
    }

    #[test]
    async fn parse_extra_headers_env_with_url_value() {
        let headers =
            parse_extra_headers_env("HTTP-Referer:https://github.com/zeroclaw-labs/zeroclaw");
        assert_eq!(headers.len(), 1);
        // Only splits on first colon, preserving URL colons in value
        assert_eq!(headers[0].0, "HTTP-Referer");
        assert_eq!(headers[0].1, "https://github.com/zeroclaw-labs/zeroclaw");
    }

    #[test]
    async fn parse_extra_headers_env_empty_string() {
        let headers = parse_extra_headers_env("");
        assert!(headers.is_empty());
    }

    #[test]
    async fn parse_extra_headers_env_whitespace_trimming() {
        let headers = parse_extra_headers_env("  X-Title : zeroclaw , User-Agent : cli/1.0 ");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("X-Title".to_string(), "zeroclaw".to_string()));
        assert_eq!(
            headers[1],
            ("User-Agent".to_string(), "cli/1.0".to_string())
        );
    }

    #[test]
    async fn parse_extra_headers_env_skips_malformed() {
        let headers = parse_extra_headers_env("X-Valid:value,no-colon-here,Another:ok");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("X-Valid".to_string(), "value".to_string()));
        assert_eq!(headers[1], ("Another".to_string(), "ok".to_string()));
    }

    #[test]
    async fn parse_extra_headers_env_skips_empty_key() {
        let headers = parse_extra_headers_env(":value,X-Valid:ok");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0], ("X-Valid".to_string(), "ok".to_string()));
    }

    #[test]
    async fn parse_extra_headers_env_allows_empty_value() {
        let headers = parse_extra_headers_env("X-Empty:");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0], ("X-Empty".to_string(), String::new()));
    }

    #[test]
    async fn parse_extra_headers_env_trailing_comma() {
        let headers = parse_extra_headers_env("X-Title:zeroclaw,");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0], ("X-Title".to_string(), "zeroclaw".to_string()));
    }

    #[test]
    async fn extra_headers_parses_from_toml() {
        let raw = r#"
default_temperature = 0.7

[extra_headers]
User-Agent = "MyApp/1.0"
X-Title = "zeroclaw"
"#;
        let parsed = parse_test_config(raw);
        let headers = &parsed
            .providers
            .fallback_provider()
            .expect("fallback provider")
            .extra_headers;
        assert_eq!(headers.len(), 2);
        assert_eq!(headers.get("User-Agent").unwrap(), "MyApp/1.0");
        assert_eq!(headers.get("X-Title").unwrap(), "zeroclaw");
    }

    #[test]
    async fn extra_headers_defaults_to_empty() {
        let raw = r#"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(raw);
        assert!(
            parsed
                .providers
                .fallback_provider()
                .map(|e| e.extra_headers.is_empty())
                .unwrap_or(true)
        );
    }

    #[test]
    async fn storage_provider_dburl_alias_deserializes() {
        let raw = r#"
default_temperature = 0.7

[storage.provider.config]
provider = "qdrant"
dbURL = "http://localhost:6333"
schema = "public"
table = "memories"
connect_timeout_secs = 12
"#;

        let parsed = parse_test_config(raw);
        assert_eq!(parsed.storage.provider.config.provider, "qdrant");
        assert_eq!(
            parsed.storage.provider.config.db_url.as_deref(),
            Some("http://localhost:6333")
        );
        assert_eq!(parsed.storage.provider.config.schema, "public");
        assert_eq!(parsed.storage.provider.config.table, "memories");
        assert_eq!(
            parsed.storage.provider.config.connect_timeout_secs,
            Some(12)
        );
    }

    #[test]
    async fn runtime_reasoning_enabled_deserializes() {
        let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_enabled = false
"#;

        let parsed = parse_test_config(raw);
        assert_eq!(parsed.runtime.reasoning_enabled, Some(false));
    }

    #[test]
    async fn runtime_reasoning_effort_deserializes() {
        let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_effort = "HIGH"
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.runtime.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    async fn runtime_reasoning_effort_rejects_invalid_values() {
        let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_effort = "turbo"
"#;

        let error = toml::from_str::<Config>(raw).expect_err("invalid value should fail");
        assert!(error.to_string().contains("reasoning_effort"));
    }

    #[test]
    async fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert!(cfg.compact_context);
        assert_eq!(cfg.max_tool_iterations, 10);
        assert_eq!(cfg.max_history_messages, 50);
        assert!(!cfg.parallel_tools);
        assert_eq!(cfg.tool_dispatcher, "auto");
    }

    #[test]
    async fn agent_config_deserializes() {
        let raw = r#"
default_temperature = 0.7
[agent]
compact_context = true
max_tool_iterations = 20
max_history_messages = 80
parallel_tools = true
tool_dispatcher = "xml"
"#;
        let parsed = parse_test_config(raw);
        assert!(parsed.agent.compact_context);
        assert_eq!(parsed.agent.max_tool_iterations, 20);
        assert_eq!(parsed.agent.max_history_messages, 80);
        assert!(parsed.agent.parallel_tools);
        assert_eq!(parsed.agent.tool_dispatcher, "xml");
    }

    #[test]
    async fn pacing_config_defaults_are_all_none_or_empty() {
        let cfg = PacingConfig::default();
        assert!(cfg.step_timeout_secs.is_none());
        assert!(cfg.loop_detection_min_elapsed_secs.is_none());
        assert!(cfg.loop_ignore_tools.is_empty());
        assert!(cfg.message_timeout_scale_max.is_none());
    }

    #[test]
    async fn pacing_config_deserializes_from_toml() {
        let raw = r#"
default_temperature = 0.7
[pacing]
step_timeout_secs = 120
loop_detection_min_elapsed_secs = 60
loop_ignore_tools = ["browser_screenshot", "browser_navigate"]
message_timeout_scale_max = 8
"#;
        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.pacing.step_timeout_secs, Some(120));
        assert_eq!(parsed.pacing.loop_detection_min_elapsed_secs, Some(60));
        assert_eq!(
            parsed.pacing.loop_ignore_tools,
            vec!["browser_screenshot", "browser_navigate"]
        );
        assert_eq!(parsed.pacing.message_timeout_scale_max, Some(8));
    }

    #[test]
    async fn pacing_config_absent_preserves_defaults() {
        let raw = r#"
default_temperature = 0.7
"#;
        let parsed: Config = toml::from_str(raw).unwrap();
        assert!(parsed.pacing.step_timeout_secs.is_none());
        assert!(parsed.pacing.loop_detection_min_elapsed_secs.is_none());
        assert!(parsed.pacing.loop_ignore_tools.is_empty());
        assert!(parsed.pacing.message_timeout_scale_max.is_none());
    }

    #[tokio::test]
    async fn sync_directory_handles_existing_directory() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_sync_directory_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        sync_directory(&dir).await.unwrap();

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_and_load_tmpdir() {
        let dir = std::env::temp_dir().join("zeroclaw_test_config");
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let config_path = dir.join("config.toml");
        let mut providers = crate::providers::ProvidersConfig {
            fallback: Some("openrouter".into()),
            ..Default::default()
        };
        providers.models.insert(
            "openrouter".into(),
            ModelProviderConfig {
                api_key: Some("sk-roundtrip".into()),
                model: Some("test-model".into()),
                temperature: Some(0.9),
                timeout_secs: Some(120),
                ..Default::default()
            },
        );
        let config = Config {
            schema_version: crate::migration::CURRENT_SCHEMA_VERSION,
            providers,
            workspace_dir: dir.join("workspace"),
            config_path: config_path.clone(),
            observability: ObservabilityConfig::default(),
            autonomy: AutonomyConfig::default(),
            trust: crate::scattered_types::TrustConfig::default(),
            backup: BackupConfig::default(),
            data_retention: DataRetentionConfig::default(),
            cloud_ops: CloudOpsConfig::default(),
            conversational_ai: ConversationalAiConfig::default(),
            security: SecurityConfig::default(),
            security_ops: SecurityOpsConfig::default(),
            runtime: RuntimeConfig::default(),
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            skills: SkillsConfig::default(),
            pipeline: PipelineConfig::default(),
            query_classification: QueryClassificationConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            cron: CronConfig::default(),
            channels: ChannelsConfig::default(),
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            microsoft365: Microsoft365Config::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            browser_delegate: crate::scattered_types::BrowserDelegateConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            media_pipeline: MediaPipelineConfig::default(),
            web_fetch: WebFetchConfig::default(),
            link_enricher: LinkEnricherConfig::default(),
            text_browser: TextBrowserConfig::default(),
            web_search: WebSearchConfig::default(),
            project_intel: ProjectIntelConfig::default(),
            google_workspace: GoogleWorkspaceConfig::default(),
            proxy: ProxyConfig::default(),
            agent: AgentConfig::default(),
            pacing: PacingConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            peripherals: PeripheralsConfig::default(),
            delegate: DelegateToolConfig::default(),
            agents: HashMap::new(),
            swarms: HashMap::new(),
            hooks: HooksConfig::default(),
            hardware: HardwareConfig::default(),
            transcription: TranscriptionConfig::default(),
            tts: TtsConfig::default(),
            mcp: McpConfig::default(),
            nodes: NodesConfig::default(),
            workspace: WorkspaceConfig::default(),
            notion: NotionConfig::default(),
            jira: JiraConfig::default(),
            node_transport: NodeTransportConfig::default(),
            knowledge: KnowledgeConfig::default(),
            linkedin: LinkedInConfig::default(),
            image_gen: ImageGenConfig::default(),
            plugins: PluginsConfig::default(),
            locale: None,
            verifiable_intent: VerifiableIntentConfig::default(),
            claude_code: ClaudeCodeConfig::default(),
            claude_code_runner: ClaudeCodeRunnerConfig::default(),
            codex_cli: CodexCliConfig::default(),
            gemini_cli: GeminiCliConfig::default(),
            opencode_cli: OpenCodeCliConfig::default(),
            sop: SopConfig::default(),
            shell_tool: ShellToolConfig::default(),
        };

        // Provider fields are now resolved directly — no cache needed.
        config.save().await.unwrap();
        assert!(config_path.exists());

        let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
        let compat: crate::migration::V1Compat = toml::from_str(&contents).unwrap();
        let loaded = compat.into_config();
        let entry = &loaded.providers.models["openrouter"];
        assert!(
            entry
                .api_key
                .as_deref()
                .is_some_and(crate::secrets::SecretStore::is_encrypted)
        );
        let store = crate::secrets::SecretStore::new(&dir, true);
        let decrypted = store.decrypt(entry.api_key.as_deref().unwrap()).unwrap();
        assert_eq!(decrypted, "sk-roundtrip");
        assert_eq!(entry.model.as_deref(), Some("test-model"));
        assert!(
            entry
                .temperature
                .is_some_and(|t| (t - 0.9).abs() < f64::EPSILON)
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_encrypts_nested_credentials() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_nested_credentials_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        let mut config = Config {
            workspace_dir: dir.join("workspace"),
            config_path: dir.join("config.toml"),
            ..Default::default()
        };
        config.providers.fallback = Some("default".into());
        config.providers.models.insert(
            "default".into(),
            ModelProviderConfig {
                api_key: Some("root-credential".into()),
                ..Default::default()
            },
        );
        // Provider fields are now resolved directly — no cache needed.
        config.composio.api_key = Some("composio-credential".into());
        config.browser.computer_use.api_key = Some("browser-credential".into());
        config.web_search.brave_api_key = Some("brave-credential".into());
        config.storage.provider.config.db_url = Some("postgres://user:pw@host/db".into());
        config.channels.feishu = Some(FeishuConfig {
            enabled: true,
            app_id: "cli_feishu_123".into(),
            app_secret: "feishu-secret".into(),
            encrypt_key: Some("feishu-encrypt".into()),
            verification_token: Some("feishu-verify".into()),
            allowed_users: vec!["*".into()],
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        });

        config.agents.insert(
            "worker".into(),
            DelegateAgentConfig {
                provider: "openrouter".into(),
                model: "model-test".into(),
                system_prompt: None,
                api_key: Some("agent-credential".into()),
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
                timeout_secs: None,
                agentic_timeout_secs: None,
                skills_directory: None,
                memory_namespace: None,
            },
        );

        config.save().await.unwrap();

        let contents = tokio::fs::read_to_string(config.config_path.clone())
            .await
            .unwrap();
        let stored: Config = toml::from_str::<crate::migration::V1Compat>(&contents)
            .unwrap()
            .into_config();
        let store = crate::secrets::SecretStore::new(&dir, true);

        let root_encrypted = stored
            .providers
            .models
            .get("default")
            .and_then(|m| m.api_key.as_deref())
            .unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(root_encrypted));
        assert_eq!(store.decrypt(root_encrypted).unwrap(), "root-credential");

        let composio_encrypted = stored.composio.api_key.as_deref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(
            composio_encrypted
        ));
        assert_eq!(
            store.decrypt(composio_encrypted).unwrap(),
            "composio-credential"
        );

        let browser_encrypted = stored.browser.computer_use.api_key.as_deref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(browser_encrypted));
        assert_eq!(
            store.decrypt(browser_encrypted).unwrap(),
            "browser-credential"
        );

        let web_search_encrypted = stored.web_search.brave_api_key.as_deref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(
            web_search_encrypted
        ));
        assert_eq!(
            store.decrypt(web_search_encrypted).unwrap(),
            "brave-credential"
        );

        let worker = stored.agents.get("worker").unwrap();
        let worker_encrypted = worker.api_key.as_deref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(worker_encrypted));
        assert_eq!(store.decrypt(worker_encrypted).unwrap(), "agent-credential");

        let storage_db_url = stored.storage.provider.config.db_url.as_deref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(storage_db_url));
        assert_eq!(
            store.decrypt(storage_db_url).unwrap(),
            "postgres://user:pw@host/db"
        );

        let feishu = stored.channels.feishu.as_ref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(
            &feishu.app_secret
        ));
        assert_eq!(store.decrypt(&feishu.app_secret).unwrap(), "feishu-secret");
        assert!(
            feishu
                .encrypt_key
                .as_deref()
                .is_some_and(crate::secrets::SecretStore::is_encrypted)
        );
        assert_eq!(
            store
                .decrypt(feishu.encrypt_key.as_deref().unwrap())
                .unwrap(),
            "feishu-encrypt"
        );
        assert!(
            feishu
                .verification_token
                .as_deref()
                .is_some_and(crate::secrets::SecretStore::is_encrypted)
        );
        assert_eq!(
            store
                .decrypt(feishu.verification_token.as_deref().unwrap())
                .unwrap(),
            "feishu-verify"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_atomic_cleanup() {
        let dir =
            std::env::temp_dir().join(format!("zeroclaw_test_config_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).await.unwrap();

        let config_path = dir.join("config.toml");
        let mut config = Config {
            workspace_dir: dir.join("workspace"),
            config_path: config_path.clone(),
            ..Default::default()
        };
        config.providers.fallback = Some("test".into());
        config.providers.models.insert(
            "test".into(),
            ModelProviderConfig {
                model: Some("model-a".into()),
                ..Default::default()
            },
        );
        config.save().await.unwrap();
        assert!(config_path.exists());

        config.providers.models.get_mut("test").unwrap().model = Some("model-b".into());
        config.save().await.unwrap();

        let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(contents.contains("model-b"));

        let mut names: Vec<String> = Vec::new();
        let mut read_dir = fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = read_dir.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert!(!names.iter().any(|name| name.contains(".tmp-")));
        assert!(!names.iter().any(|name| name.ends_with(".bak")));

        let _ = fs::remove_dir_all(&dir).await;
    }

    // ── Telegram / Discord config ────────────────────────────

    #[test]
    async fn telegram_config_serde() {
        let tc = TelegramConfig {
            enabled: true,
            bot_token: "123:XYZ".into(),
            allowed_users: vec!["alice".into(), "bob".into()],
            stream_mode: StreamMode::Partial,
            draft_update_interval_ms: 500,
            interrupt_on_new_message: true,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        };
        let json = serde_json::to_string(&tc).unwrap();
        let parsed: TelegramConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bot_token, "123:XYZ");
        assert_eq!(parsed.allowed_users.len(), 2);
        assert_eq!(parsed.stream_mode, StreamMode::Partial);
        assert_eq!(parsed.draft_update_interval_ms, 500);
        assert!(parsed.interrupt_on_new_message);
    }

    #[test]
    async fn telegram_config_defaults_stream_off() {
        let json = r#"{"bot_token":"tok","allowed_users":[]}"#;
        let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.stream_mode, StreamMode::Off);
        assert_eq!(parsed.draft_update_interval_ms, 1000);
        assert!(!parsed.interrupt_on_new_message);
    }

    #[test]
    async fn discord_config_serde() {
        let dc = DiscordConfig {
            enabled: true,
            bot_token: "discord-token".into(),
            guild_id: Some("12345".into()),
            allowed_users: vec![],
            listen_to_bots: false,
            interrupt_on_new_message: false,
            mention_only: false,
            proxy_url: None,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            multi_message_delay_ms: 800,
            stall_timeout_secs: 0,
        };
        let json = serde_json::to_string(&dc).unwrap();
        let parsed: DiscordConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bot_token, "discord-token");
        assert_eq!(parsed.guild_id.as_deref(), Some("12345"));
    }

    #[test]
    async fn discord_config_optional_guild() {
        let dc = DiscordConfig {
            enabled: true,
            bot_token: "tok".into(),
            guild_id: None,
            allowed_users: vec![],
            listen_to_bots: false,
            interrupt_on_new_message: false,
            mention_only: false,
            proxy_url: None,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            multi_message_delay_ms: 800,
            stall_timeout_secs: 0,
        };
        let json = serde_json::to_string(&dc).unwrap();
        let parsed: DiscordConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.guild_id.is_none());
    }

    // ── iMessage / Matrix config ────────────────────────────

    #[test]
    async fn imessage_config_serde() {
        let ic = IMessageConfig {
            enabled: true,
            allowed_contacts: vec!["+1234567890".into(), "user@icloud.com".into()],
        };
        let json = serde_json::to_string(&ic).unwrap();
        let parsed: IMessageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.allowed_contacts.len(), 2);
        assert_eq!(parsed.allowed_contacts[0], "+1234567890");
    }

    #[test]
    async fn imessage_config_empty_contacts() {
        let ic = IMessageConfig {
            enabled: true,
            allowed_contacts: vec![],
        };
        let json = serde_json::to_string(&ic).unwrap();
        let parsed: IMessageConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.allowed_contacts.is_empty());
    }

    #[test]
    async fn imessage_config_wildcard() {
        let ic = IMessageConfig {
            enabled: true,
            allowed_contacts: vec!["*".into()],
        };
        let toml_str = toml::to_string(&ic).unwrap();
        let parsed: IMessageConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.allowed_contacts, vec!["*"]);
    }

    #[test]
    async fn matrix_config_serde() {
        let mc = MatrixConfig {
            enabled: true,
            homeserver: "https://matrix.org".into(),
            access_token: "syt_token_abc".into(),
            user_id: Some("@bot:matrix.org".into()),
            device_id: Some("DEVICE123".into()),
            allowed_users: vec!["@user:matrix.org".into()],
            allowed_rooms: vec!["!room123:matrix.org".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        let json = serde_json::to_string(&mc).unwrap();
        let parsed: MatrixConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.homeserver, "https://matrix.org");
        assert_eq!(parsed.access_token, "syt_token_abc");
        assert_eq!(parsed.user_id.as_deref(), Some("@bot:matrix.org"));
        assert_eq!(parsed.device_id.as_deref(), Some("DEVICE123"));
        assert_eq!(
            parsed.allowed_rooms.first().map(|s| s.as_str()),
            Some("!room123:matrix.org")
        );
        assert_eq!(parsed.allowed_users.len(), 1);
    }

    #[test]
    async fn matrix_config_toml_roundtrip() {
        let mc = MatrixConfig {
            enabled: true,
            homeserver: "https://synapse.local:8448".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec!["@admin:synapse.local".into(), "*".into()],
            allowed_rooms: vec!["!abc:synapse.local".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        let toml_str = toml::to_string(&mc).unwrap();
        let parsed: MatrixConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.homeserver, "https://synapse.local:8448");
        assert_eq!(parsed.allowed_users.len(), 2);
    }

    #[test]
    async fn matrix_config_backward_compatible_without_session_hints() {
        // room_id in TOML is now migrated by prepare_table at the top level;
        // a bare MatrixConfig parse just ignores unknown keys.
        let toml = r#"
homeserver = "https://matrix.org"
access_token = "tok"
allowed_users = ["@ops:matrix.org"]
allowed_rooms = ["!ops:matrix.org"]
"#;

        let parsed: MatrixConfig = toml::from_str(toml).unwrap();
        assert_eq!(parsed.homeserver, "https://matrix.org");
        assert!(parsed.user_id.is_none());
        assert!(parsed.device_id.is_none());
        assert_eq!(parsed.allowed_rooms, vec!["!ops:matrix.org"]);
    }

    #[test]
    async fn signal_config_serde() {
        let sc = SignalConfig {
            enabled: true,
            http_url: "http://127.0.0.1:8686".into(),
            account: "+1234567890".into(),
            group_id: Some("group123".into()),
            allowed_from: vec!["+1111111111".into()],
            ignore_attachments: true,
            ignore_stories: false,
            proxy_url: None,
        };
        let json = serde_json::to_string(&sc).unwrap();
        let parsed: SignalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.http_url, "http://127.0.0.1:8686");
        assert_eq!(parsed.account, "+1234567890");
        assert_eq!(parsed.group_id.as_deref(), Some("group123"));
        assert_eq!(parsed.allowed_from.len(), 1);
        assert!(parsed.ignore_attachments);
        assert!(!parsed.ignore_stories);
    }

    #[test]
    async fn signal_config_toml_roundtrip() {
        let sc = SignalConfig {
            enabled: true,
            http_url: "http://localhost:8080".into(),
            account: "+9876543210".into(),
            group_id: None,
            allowed_from: vec!["*".into()],
            ignore_attachments: false,
            ignore_stories: true,
            proxy_url: None,
        };
        let toml_str = toml::to_string(&sc).unwrap();
        let parsed: SignalConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.http_url, "http://localhost:8080");
        assert_eq!(parsed.account, "+9876543210");
        assert!(parsed.group_id.is_none());
        assert!(parsed.ignore_stories);
    }

    #[test]
    async fn signal_config_defaults() {
        let json = r#"{"http_url":"http://127.0.0.1:8686","account":"+1234567890"}"#;
        let parsed: SignalConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.group_id.is_none());
        assert!(parsed.allowed_from.is_empty());
        assert!(!parsed.ignore_attachments);
        assert!(!parsed.ignore_stories);
    }

    #[test]
    async fn channels_with_imessage_and_matrix() {
        let c = ChannelsConfig {
            cli: true,
            telegram: None,
            discord: None,
            discord_history: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: Some(IMessageConfig {
                enabled: true,
                allowed_contacts: vec!["+1".into()],
            }),
            matrix: Some(MatrixConfig {
                enabled: true,
                homeserver: "https://m.org".into(),
                access_token: "tok".into(),
                user_id: None,
                device_id: None,
                allowed_users: vec!["@u:m".into()],
                allowed_rooms: vec!["!r:m".into()],
                interrupt_on_new_message: false,
                stream_mode: StreamMode::default(),
                draft_update_interval_ms: 1500,
                multi_message_delay_ms: 800,
                recovery_key: None,
                mention_only: false,
                password: None,
            }),
            signal: None,
            whatsapp: None,
            linq: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            gmail_push: None,
            irc: None,
            lark: None,
            line: None,
            feishu: None,
            dingtalk: None,
            wecom: None,
            qq: None,
            twitter: None,
            mochat: None,
            #[cfg(feature = "channel-nostr")]
            nostr: None,
            clawdtalk: None,
            reddit: None,
            bluesky: None,
            voice_call: None,
            #[cfg(feature = "one2x")]
            web: None,
            #[cfg(feature = "voice-wake")]
            voice_wake: None,
            mqtt: None,
            message_timeout_secs: 300,
            ack_reactions: true,
            show_tool_calls: true,
            session_persistence: true,
            session_backend: default_session_backend(),
            session_ttl_hours: 0,
            debounce_ms: 0,
        };
        let toml_str = toml::to_string_pretty(&c).unwrap();
        let parsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.imessage.is_some());
        assert!(parsed.matrix.is_some());
        assert_eq!(parsed.imessage.unwrap().allowed_contacts, vec!["+1"]);
        assert_eq!(parsed.matrix.unwrap().homeserver, "https://m.org");
    }

    #[test]
    async fn channels_default_has_no_imessage_matrix() {
        let c = ChannelsConfig::default();
        assert!(c.imessage.is_none());
        assert!(c.matrix.is_none());
    }

    // ── Edge cases: serde(default) for allowed_users ─────────

    #[test]
    async fn discord_config_deserializes_without_allowed_users() {
        // Old configs won't have allowed_users — serde(default) should fill vec![]
        let json = r#"{"bot_token":"tok","guild_id":"123"}"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.allowed_users.is_empty());
    }

    #[test]
    async fn discord_config_deserializes_with_allowed_users() {
        let json = r#"{"bot_token":"tok","guild_id":"123","allowed_users":["111","222"]}"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.allowed_users, vec!["111", "222"]);
    }

    #[test]
    async fn slack_config_deserializes_without_allowed_users() {
        let json = r#"{"bot_token":"xoxb-tok"}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.channel_ids.is_empty());
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn slack_config_deserializes_with_allowed_users() {
        let json = r#"{"bot_token":"xoxb-tok","allowed_users":["U111"]}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.channel_ids.is_empty());
        assert_eq!(parsed.allowed_users, vec!["U111"]);
        assert!(!parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn slack_config_deserializes_with_channel_ids() {
        let json = r#"{"bot_token":"xoxb-tok","channel_ids":["C111","D222"]}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.channel_ids, vec!["C111", "D222"]);
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn slack_config_deserializes_with_mention_only() {
        let json = r#"{"bot_token":"xoxb-tok","mention_only":true}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.mention_only);
        assert!(!parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
    }

    #[test]
    async fn slack_config_deserializes_interrupt_on_new_message() {
        let json = r#"{"bot_token":"xoxb-tok","interrupt_on_new_message":true}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn slack_config_deserializes_thread_replies() {
        let json = r#"{"bot_token":"xoxb-tok","thread_replies":false}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.thread_replies, Some(false));
        assert!(!parsed.interrupt_on_new_message);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn discord_config_default_interrupt_on_new_message_is_false() {
        let json = r#"{"bot_token":"tok"}"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert!(!parsed.interrupt_on_new_message);
    }

    #[test]
    async fn discord_config_deserializes_interrupt_on_new_message_true() {
        let json = r#"{"bot_token":"tok","interrupt_on_new_message":true}"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.interrupt_on_new_message);
    }

    #[test]
    async fn discord_config_toml_backward_compat() {
        let toml_str = r#"
bot_token = "tok"
guild_id = "123"
"#;
        let parsed: DiscordConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.allowed_users.is_empty());
        assert_eq!(parsed.bot_token, "tok");
    }

    #[test]
    async fn slack_config_toml_with_channel_ids() {
        let toml_str = r#"
bot_token = "xoxb-tok"
channel_ids = ["C123", "D456"]
"#;
        let parsed: SlackConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(parsed.channel_ids, vec!["C123", "D456"]);
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.interrupt_on_new_message);
        assert_eq!(parsed.thread_replies, None);
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn slack_config_toml_without_channel_ids_defaults_empty() {
        let toml_str = r#"
bot_token = "xoxb-tok"
"#;
        let parsed: SlackConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.channel_ids.is_empty());
    }

    #[test]
    async fn mattermost_config_default_interrupt_on_new_message_is_false() {
        let json = r#"{"url":"https://mm.example.com","bot_token":"tok"}"#;
        let parsed: MattermostConfig = serde_json::from_str(json).unwrap();
        assert!(!parsed.interrupt_on_new_message);
    }

    #[test]
    async fn mattermost_config_deserializes_interrupt_on_new_message_true() {
        let json =
            r#"{"url":"https://mm.example.com","bot_token":"tok","interrupt_on_new_message":true}"#;
        let parsed: MattermostConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.interrupt_on_new_message);
    }

    #[test]
    async fn webhook_config_with_secret() {
        let json = r#"{"port":8080,"secret":"my-secret-key"}"#;
        let parsed: WebhookConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.secret.as_deref(), Some("my-secret-key"));
    }

    #[test]
    async fn webhook_config_without_secret() {
        let json = r#"{"port":8080}"#;
        let parsed: WebhookConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.secret.is_none());
        assert_eq!(parsed.port, 8080);
    }

    // ── WhatsApp config ──────────────────────────────────────

    #[test]
    async fn whatsapp_config_serde() {
        let wc = WhatsAppConfig {
            enabled: true,
            access_token: Some("EAABx...".into()),
            phone_number_id: Some("123456789".into()),
            verify_token: Some("my-verify-token".into()),
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1234567890".into(), "+9876543210".into()],
            mention_only: false,
            mode: WhatsAppWebMode::default(),
            dm_policy: WhatsAppChatPolicy::default(),
            group_policy: WhatsAppChatPolicy::default(),
            self_chat_mode: false,
            dm_mention_patterns: vec![],
            group_mention_patterns: vec![],
            proxy_url: None,
        };
        let json = serde_json::to_string(&wc).unwrap();
        let parsed: WhatsAppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_token, Some("EAABx...".into()));
        assert_eq!(parsed.phone_number_id, Some("123456789".into()));
        assert_eq!(parsed.verify_token, Some("my-verify-token".into()));
        assert_eq!(parsed.allowed_numbers.len(), 2);
    }

    #[test]
    async fn whatsapp_config_toml_roundtrip() {
        let wc = WhatsAppConfig {
            enabled: true,
            access_token: Some("tok".into()),
            phone_number_id: Some("12345".into()),
            verify_token: Some("verify".into()),
            app_secret: Some("secret123".into()),
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1".into()],
            mention_only: false,
            mode: WhatsAppWebMode::default(),
            dm_policy: WhatsAppChatPolicy::default(),
            group_policy: WhatsAppChatPolicy::default(),
            self_chat_mode: false,
            dm_mention_patterns: vec![],
            group_mention_patterns: vec![],
            proxy_url: None,
        };
        let toml_str = toml::to_string(&wc).unwrap();
        let parsed: WhatsAppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.phone_number_id, Some("12345".into()));
        assert_eq!(parsed.allowed_numbers, vec!["+1"]);
    }

    #[test]
    async fn whatsapp_config_deserializes_without_allowed_numbers() {
        let json = r#"{"access_token":"tok","phone_number_id":"123","verify_token":"ver"}"#;
        let parsed: WhatsAppConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.allowed_numbers.is_empty());
    }

    #[test]
    async fn whatsapp_config_wildcard_allowed() {
        let wc = WhatsAppConfig {
            enabled: true,
            access_token: Some("tok".into()),
            phone_number_id: Some("123".into()),
            verify_token: Some("ver".into()),
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["*".into()],
            mention_only: false,
            mode: WhatsAppWebMode::default(),
            dm_policy: WhatsAppChatPolicy::default(),
            group_policy: WhatsAppChatPolicy::default(),
            self_chat_mode: false,
            dm_mention_patterns: vec![],
            group_mention_patterns: vec![],
            proxy_url: None,
        };
        let toml_str = toml::to_string(&wc).unwrap();
        let parsed: WhatsAppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.allowed_numbers, vec!["*"]);
    }

    #[test]
    async fn whatsapp_config_backend_type_cloud_precedence_when_ambiguous() {
        let wc = WhatsAppConfig {
            enabled: true,
            access_token: Some("tok".into()),
            phone_number_id: Some("123".into()),
            verify_token: Some("ver".into()),
            app_secret: None,
            session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1".into()],
            mention_only: false,
            mode: WhatsAppWebMode::default(),
            dm_policy: WhatsAppChatPolicy::default(),
            group_policy: WhatsAppChatPolicy::default(),
            self_chat_mode: false,
            dm_mention_patterns: vec![],
            group_mention_patterns: vec![],
            proxy_url: None,
        };
        assert!(wc.is_ambiguous_config());
        assert_eq!(wc.backend_type(), "cloud");
    }

    #[test]
    async fn whatsapp_config_backend_type_web() {
        let wc = WhatsAppConfig {
            enabled: true,
            access_token: None,
            phone_number_id: None,
            verify_token: None,
            app_secret: None,
            session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec![],
            mention_only: false,
            mode: WhatsAppWebMode::default(),
            dm_policy: WhatsAppChatPolicy::default(),
            group_policy: WhatsAppChatPolicy::default(),
            self_chat_mode: false,
            dm_mention_patterns: vec![],
            group_mention_patterns: vec![],
            proxy_url: None,
        };
        assert!(!wc.is_ambiguous_config());
        assert_eq!(wc.backend_type(), "web");
    }

    #[test]
    async fn channels_with_whatsapp() {
        let c = ChannelsConfig {
            cli: true,
            telegram: None,
            discord: None,
            discord_history: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: None,
            matrix: None,
            signal: None,
            whatsapp: Some(WhatsAppConfig {
                enabled: true,
                access_token: Some("tok".into()),
                phone_number_id: Some("123".into()),
                verify_token: Some("ver".into()),
                app_secret: None,
                session_path: None,
                pair_phone: None,
                pair_code: None,
                allowed_numbers: vec!["+1".into()],
                mention_only: false,
                mode: WhatsAppWebMode::default(),
                dm_policy: WhatsAppChatPolicy::default(),
                group_policy: WhatsAppChatPolicy::default(),
                self_chat_mode: false,
                dm_mention_patterns: vec![],
                group_mention_patterns: vec![],
                proxy_url: None,
            }),
            linq: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            gmail_push: None,
            irc: None,
            lark: None,
            line: None,
            feishu: None,
            dingtalk: None,
            wecom: None,
            qq: None,
            twitter: None,
            mochat: None,
            #[cfg(feature = "channel-nostr")]
            nostr: None,
            clawdtalk: None,
            reddit: None,
            bluesky: None,
            voice_call: None,
            #[cfg(feature = "one2x")]
            web: None,
            #[cfg(feature = "voice-wake")]
            voice_wake: None,
            mqtt: None,
            message_timeout_secs: 300,
            ack_reactions: true,
            show_tool_calls: true,
            session_persistence: true,
            session_backend: default_session_backend(),
            session_ttl_hours: 0,
            debounce_ms: 0,
        };
        let toml_str = toml::to_string_pretty(&c).unwrap();
        let parsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.whatsapp.is_some());
        let wa = parsed.whatsapp.unwrap();
        assert_eq!(wa.phone_number_id, Some("123".into()));
        assert_eq!(wa.allowed_numbers, vec!["+1"]);
    }

    #[test]
    async fn channels_default_has_no_whatsapp() {
        let c = ChannelsConfig::default();
        assert!(c.whatsapp.is_none());
    }

    #[test]
    async fn channels_default_has_no_nextcloud_talk() {
        let c = ChannelsConfig::default();
        assert!(c.nextcloud_talk.is_none());
    }

    // ══════════════════════════════════════════════════════════
    // SECURITY CHECKLIST TESTS — Gateway config
    // ══════════════════════════════════════════════════════════

    #[test]
    async fn checklist_gateway_default_requires_pairing() {
        let g = GatewayConfig::default();
        assert!(g.require_pairing, "Pairing must be required by default");
    }

    #[test]
    async fn checklist_gateway_default_blocks_public_bind() {
        let g = GatewayConfig::default();
        assert!(
            !g.allow_public_bind,
            "Public bind must be blocked by default"
        );
    }

    #[test]
    async fn checklist_gateway_default_no_tokens() {
        let g = GatewayConfig::default();
        assert!(
            g.paired_tokens.is_empty(),
            "No pre-paired tokens by default"
        );
        assert_eq!(g.pair_rate_limit_per_minute, 10);
        assert_eq!(g.webhook_rate_limit_per_minute, 60);
        assert!(!g.trust_forwarded_headers);
        assert_eq!(g.rate_limit_max_keys, 10_000);
        assert_eq!(g.idempotency_ttl_secs, 300);
        assert_eq!(g.idempotency_max_keys, 10_000);
    }

    #[test]
    async fn checklist_gateway_cli_default_host_is_localhost() {
        // The CLI default for --host is 127.0.0.1 (checked in main.rs)
        // Here we verify the config default matches
        let c = Config::default();
        assert!(
            c.gateway.require_pairing,
            "Config default must require pairing"
        );
        assert!(
            !c.gateway.allow_public_bind,
            "Config default must block public bind"
        );
    }

    #[test]
    async fn checklist_gateway_serde_roundtrip() {
        let g = GatewayConfig {
            port: 42617,
            host: "127.0.0.1".into(),
            require_pairing: true,
            allow_public_bind: false,
            paired_tokens: vec!["zc_test_token".into()],
            pair_rate_limit_per_minute: 12,
            webhook_rate_limit_per_minute: 80,
            trust_forwarded_headers: true,
            path_prefix: Some("/zeroclaw".into()),
            rate_limit_max_keys: 2048,
            idempotency_ttl_secs: 600,
            idempotency_max_keys: 4096,
            session_persistence: true,
            session_ttl_hours: 0,
            pairing_dashboard: PairingDashboardConfig::default(),
            web_dist_dir: None,
            tls: None,
        };
        let toml_str = toml::to_string(&g).unwrap();
        let parsed: GatewayConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.require_pairing);
        assert!(parsed.session_persistence);
        assert_eq!(parsed.session_ttl_hours, 0);
        assert!(!parsed.allow_public_bind);
        assert_eq!(parsed.paired_tokens, vec!["zc_test_token"]);
        assert_eq!(parsed.pair_rate_limit_per_minute, 12);
        assert_eq!(parsed.webhook_rate_limit_per_minute, 80);
        assert!(parsed.trust_forwarded_headers);
        assert_eq!(parsed.path_prefix.as_deref(), Some("/zeroclaw"));
        assert_eq!(parsed.rate_limit_max_keys, 2048);
        assert_eq!(parsed.idempotency_ttl_secs, 600);
        assert_eq!(parsed.idempotency_max_keys, 4096);
    }

    #[test]
    async fn checklist_gateway_backward_compat_no_gateway_section() {
        // Old configs without [gateway] should get secure defaults
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(minimal);
        assert!(
            parsed.gateway.require_pairing,
            "Missing [gateway] must default to require_pairing=true"
        );
        assert!(
            !parsed.gateway.allow_public_bind,
            "Missing [gateway] must default to allow_public_bind=false"
        );
    }

    #[test]
    async fn checklist_autonomy_default_is_workspace_scoped() {
        let a = AutonomyConfig::default();
        assert!(a.workspace_only, "Default autonomy must be workspace_only");
        assert!(
            a.forbidden_paths.contains(&"/etc".to_string()),
            "Must block /etc"
        );
        assert!(
            a.forbidden_paths.contains(&"/proc".to_string()),
            "Must block /proc"
        );
        assert!(
            a.forbidden_paths.contains(&"~/.ssh".to_string()),
            "Must block ~/.ssh"
        );
    }

    // ══════════════════════════════════════════════════════════
    // COMPOSIO CONFIG TESTS
    // ══════════════════════════════════════════════════════════

    #[test]
    async fn composio_config_default_disabled() {
        let c = ComposioConfig::default();
        assert!(!c.enabled, "Composio must be disabled by default");
        assert!(c.api_key.is_none(), "No API key by default");
        assert_eq!(c.entity_id, "default");
    }

    #[test]
    async fn composio_config_serde_roundtrip() {
        let c = ComposioConfig {
            enabled: true,
            api_key: Some("comp-key-123".into()),
            entity_id: "user42".into(),
        };
        let toml_str = toml::to_string(&c).unwrap();
        let parsed: ComposioConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.api_key.as_deref(), Some("comp-key-123"));
        assert_eq!(parsed.entity_id, "user42");
    }

    #[test]
    async fn composio_config_backward_compat_missing_section() {
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(minimal);
        assert!(
            !parsed.composio.enabled,
            "Missing [composio] must default to disabled"
        );
        assert!(parsed.composio.api_key.is_none());
    }

    #[test]
    async fn composio_config_partial_toml() {
        let toml_str = r"
enabled = true
";
        let parsed: ComposioConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.api_key.is_none());
        assert_eq!(parsed.entity_id, "default");
    }

    #[test]
    async fn composio_config_enable_alias_supported() {
        let toml_str = r"
enable = true
";
        let parsed: ComposioConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.enabled);
        assert!(parsed.api_key.is_none());
        assert_eq!(parsed.entity_id, "default");
    }

    // ══════════════════════════════════════════════════════════
    // SECRETS CONFIG TESTS
    // ══════════════════════════════════════════════════════════

    #[test]
    async fn secrets_config_default_encrypts() {
        let s = SecretsConfig::default();
        assert!(s.encrypt, "Encryption must be enabled by default");
    }

    #[test]
    async fn secrets_config_serde_roundtrip() {
        let s = SecretsConfig { encrypt: false };
        let toml_str = toml::to_string(&s).unwrap();
        let parsed: SecretsConfig = toml::from_str(&toml_str).unwrap();
        assert!(!parsed.encrypt);
    }

    #[test]
    async fn secrets_config_backward_compat_missing_section() {
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(minimal);
        assert!(
            parsed.secrets.encrypt,
            "Missing [secrets] must default to encrypt=true"
        );
    }

    #[test]
    async fn config_default_has_composio_and_secrets() {
        let c = Config::default();
        assert!(!c.composio.enabled);
        assert!(c.composio.api_key.is_none());
        assert!(c.secrets.encrypt);
        assert!(c.browser.enabled);
        assert_eq!(c.browser.allowed_domains, vec!["*".to_string()]);
    }

    #[test]
    async fn browser_config_default_enabled() {
        let b = BrowserConfig::default();
        assert!(b.enabled);
        assert_eq!(b.allowed_domains, vec!["*".to_string()]);
        assert_eq!(b.backend, "agent_browser");
        assert!(b.native_headless);
        assert_eq!(b.native_webdriver_url, "http://127.0.0.1:9515");
        assert!(b.native_chrome_path.is_none());
        assert_eq!(b.computer_use.endpoint, "http://127.0.0.1:8787/v1/actions");
        assert_eq!(b.computer_use.timeout_ms, 15_000);
        assert!(!b.computer_use.allow_remote_endpoint);
        assert!(b.computer_use.window_allowlist.is_empty());
        assert!(b.computer_use.max_coordinate_x.is_none());
        assert!(b.computer_use.max_coordinate_y.is_none());
    }

    #[test]
    async fn browser_config_serde_roundtrip() {
        let b = BrowserConfig {
            enabled: true,
            allowed_domains: vec!["example.com".into(), "docs.example.com".into()],
            session_name: None,
            backend: "auto".into(),
            native_headless: false,
            native_webdriver_url: "http://localhost:4444".into(),
            native_chrome_path: Some("/usr/bin/chromium".into()),
            computer_use: BrowserComputerUseConfig {
                endpoint: "https://computer-use.example.com/v1/actions".into(),
                api_key: Some("test-token".into()),
                timeout_ms: 8_000,
                allow_remote_endpoint: true,
                window_allowlist: vec!["Chrome".into(), "Visual Studio Code".into()],
                max_coordinate_x: Some(3840),
                max_coordinate_y: Some(2160),
            },
        };
        let toml_str = toml::to_string(&b).unwrap();
        let parsed: BrowserConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.allowed_domains.len(), 2);
        assert_eq!(parsed.allowed_domains[0], "example.com");
        assert_eq!(parsed.backend, "auto");
        assert!(!parsed.native_headless);
        assert_eq!(parsed.native_webdriver_url, "http://localhost:4444");
        assert_eq!(
            parsed.native_chrome_path.as_deref(),
            Some("/usr/bin/chromium")
        );
        assert_eq!(
            parsed.computer_use.endpoint,
            "https://computer-use.example.com/v1/actions"
        );
        assert_eq!(parsed.computer_use.api_key.as_deref(), Some("test-token"));
        assert_eq!(parsed.computer_use.timeout_ms, 8_000);
        assert!(parsed.computer_use.allow_remote_endpoint);
        assert_eq!(parsed.computer_use.window_allowlist.len(), 2);
        assert_eq!(parsed.computer_use.max_coordinate_x, Some(3840));
        assert_eq!(parsed.computer_use.max_coordinate_y, Some(2160));
    }

    #[test]
    async fn browser_config_backward_compat_missing_section() {
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed = parse_test_config(minimal);
        assert!(parsed.browser.enabled);
        assert_eq!(parsed.browser.allowed_domains, vec!["*".to_string()]);
    }

    // ── Environment variable overrides (Docker support) ─────────

    async fn env_override_lock() -> MutexGuard<'static, ()> {
        static ENV_OVERRIDE_TEST_LOCK: Mutex<()> = Mutex::const_new(());
        ENV_OVERRIDE_TEST_LOCK.lock().await
    }

    fn clear_proxy_env_test_vars() {
        for key in [
            "ZEROCLAW_PROXY_ENABLED",
            "ZEROCLAW_HTTP_PROXY",
            "ZEROCLAW_HTTPS_PROXY",
            "ZEROCLAW_ALL_PROXY",
            "ZEROCLAW_NO_PROXY",
            "ZEROCLAW_PROXY_SCOPE",
            "ZEROCLAW_PROXY_SERVICES",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    async fn env_override_api_key() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_ref())
                .is_none()
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_API_KEY", "sk-test-env-key") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("sk-test-env-key")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_API_KEY") };
    }

    #[test]
    async fn env_override_api_key_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_API_KEY") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("API_KEY", "sk-fallback-key") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("sk-fallback-key")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("API_KEY") };
    }

    #[test]
    async fn env_override_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROVIDER", "anthropic") };
        config.apply_env_overrides();
        assert_eq!(config.providers.fallback.as_deref(), Some("anthropic"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
    }

    #[test]
    async fn env_override_model_provider_alias() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_MODEL_PROVIDER", "openai-codex") };
        config.apply_env_overrides();
        assert_eq!(config.providers.fallback.as_deref(), Some("openai-codex"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_MODEL_PROVIDER") };
    }

    #[test]
    async fn toml_supports_model_provider_and_model_alias_fields() {
        let raw = r#"
default_temperature = 0.7
model_provider = "sub2api"
model = "gpt-5.3-codex"

[model_providers.sub2api]
name = "sub2api"
base_url = "https://api.tonsof.blue/v1"
wire_api = "responses"
requires_openai_auth = true
"#;

        let parsed = parse_test_config(raw);
        assert_eq!(parsed.providers.fallback.as_deref(), Some("sub2api"));
        assert_eq!(
            parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-5.3-codex")
        );
        let profile = parsed
            .providers
            .models
            .get("sub2api")
            .expect("profile should exist");
        assert_eq!(profile.wire_api.as_deref(), Some("responses"));
        assert!(profile.requires_openai_auth);
    }

    #[test]
    async fn env_override_open_skills_enabled_and_dir() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert!(!config.skills.open_skills_enabled);
        assert!(config.skills.open_skills_dir.is_none());
        assert_eq!(
            config.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Full
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_OPEN_SKILLS_ENABLED", "true") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_OPEN_SKILLS_DIR", "/tmp/open-skills") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS", "yes") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_SKILLS_PROMPT_MODE", "compact") };
        config.apply_env_overrides();

        assert!(config.skills.open_skills_enabled);
        assert!(config.skills.allow_scripts);
        assert_eq!(
            config.skills.open_skills_dir.as_deref(),
            Some("/tmp/open-skills")
        );
        assert_eq!(
            config.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Compact
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_OPEN_SKILLS_ENABLED") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_OPEN_SKILLS_DIR") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_SKILLS_PROMPT_MODE") };
    }

    #[test]
    async fn env_override_open_skills_enabled_invalid_value_keeps_existing_value() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.skills.open_skills_enabled = true;
        config.skills.allow_scripts = true;
        config.skills.prompt_injection_mode = SkillsPromptInjectionMode::Compact;

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_OPEN_SKILLS_ENABLED", "maybe") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS", "maybe") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_SKILLS_PROMPT_MODE", "invalid") };
        config.apply_env_overrides();

        assert!(config.skills.open_skills_enabled);
        assert!(config.skills.allow_scripts);
        assert_eq!(
            config.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Compact
        );
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_OPEN_SKILLS_ENABLED") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_SKILLS_PROMPT_MODE") };
    }

    #[test]
    async fn env_override_provider_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("PROVIDER", "openai") };
        config.apply_env_overrides();
        assert_eq!(config.providers.fallback.as_deref(), Some("openai"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("PROVIDER") };
    }

    #[test]
    async fn env_override_provider_fallback_does_not_replace_non_default_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("custom:https://proxy.example.com/v1".to_string());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("PROVIDER", "openrouter") };
        config.apply_env_overrides();
        assert_eq!(
            config.providers.fallback.as_deref(),
            Some("custom:https://proxy.example.com/v1")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("PROVIDER") };
    }

    #[test]
    async fn env_override_zero_claw_provider_overrides_non_default_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("custom:https://proxy.example.com/v1".to_string());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROVIDER", "openrouter") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("PROVIDER", "anthropic") };
        config.apply_env_overrides();
        assert_eq!(config.providers.fallback.as_deref(), Some("openrouter"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("PROVIDER") };
    }

    #[test]
    async fn env_override_glm_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("glm-cn".to_string());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("GLM_API_KEY", "glm-regional-key") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("glm-regional-key")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("GLM_API_KEY") };
    }

    #[test]
    async fn env_override_zai_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("zai-cn".to_string());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZAI_API_KEY", "zai-regional-key") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("zai-regional-key")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZAI_API_KEY") };
    }

    #[test]
    async fn env_override_model() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_MODEL", "gpt-4o") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-4o")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_MODEL") };
    }

    #[test]
    async fn model_provider_profile_maps_to_custom_endpoint() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("sub2api".to_string());
        config.providers.models.insert(
            "sub2api".to_string(),
            ModelProviderConfig {
                name: Some("sub2api".to_string()),
                base_url: Some("https://api.tonsof.blue/v1".to_string()),
                wire_api: None,
                requires_openai_auth: false,
                azure_openai_resource: None,
                azure_openai_deployment: None,
                azure_openai_api_version: None,
                api_path: None,
                max_tokens: None,
                ..Default::default()
            },
        );

        config.apply_env_overrides();
        assert_eq!(
            config.providers.fallback.as_deref(),
            Some("custom:https://api.tonsof.blue/v1")
        );
        // The original entry is still stored under its config key.
        assert_eq!(
            config
                .providers
                .models
                .get("sub2api")
                .and_then(|e| e.base_url.as_deref()),
            Some("https://api.tonsof.blue/v1")
        );
    }

    #[test]
    async fn model_provider_profile_responses_uses_openai_codex_and_openai_key() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("sub2api".to_string());
        config.providers.models.insert(
            "sub2api".to_string(),
            ModelProviderConfig {
                name: Some("sub2api".to_string()),
                base_url: Some("https://api.tonsof.blue".to_string()),
                wire_api: Some("responses".to_string()),
                requires_openai_auth: true,
                azure_openai_resource: None,
                azure_openai_deployment: None,
                azure_openai_api_version: None,
                api_path: None,
                max_tokens: None,
                ..Default::default()
            },
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-test-codex-key") };
        config.apply_env_overrides();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("OPENAI_API_KEY") };

        assert_eq!(config.providers.fallback.as_deref(), Some("openai-codex"));
        // The original entry is still stored under its config key.
        let entry = config
            .providers
            .models
            .get("sub2api")
            .expect("sub2api entry");
        assert_eq!(entry.base_url.as_deref(), Some("https://api.tonsof.blue"));
        assert_eq!(entry.api_key.as_deref(), Some("sk-test-codex-key"));
    }

    #[test]
    async fn save_repairs_bare_config_filename_using_runtime_resolution() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let workspace_dir = temp_home.join("workspace");
        let resolved_config_path = temp_home.join(".zeroclaw").join("config.toml");

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

        let mut config = Config {
            workspace_dir,
            config_path: PathBuf::from("config.toml"),
            ..Default::default()
        };
        config.providers.fallback = Some("default".into());
        config.providers.models.insert(
            "default".into(),
            ModelProviderConfig {
                temperature: Some(0.5),
                ..Default::default()
            },
        );
        // Provider fields are now resolved directly — no cache needed.
        config.save().await.unwrap();

        assert!(resolved_config_path.exists());
        let saved = tokio::fs::read_to_string(&resolved_config_path)
            .await
            .unwrap();
        let parsed = parse_test_config(&saved);
        assert!(
            (parsed
                .providers
                .fallback_provider()
                .and_then(|e| e.temperature)
                .unwrap_or(0.7)
                - 0.5)
                .abs()
                < f64::EPSILON
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = tokio::fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn validate_ollama_cloud_model_requires_remote_api_url() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("ollama".to_string());
        config.providers.models.insert(
            "ollama".to_string(),
            ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                base_url: None,
                api_key: Some("ollama-key".to_string()),
                ..Default::default()
            },
        );

        let error = config.validate().expect_err("expected validation to fail");
        assert!(error.to_string().contains(
            "default_model uses ':cloud' with provider 'ollama', but api_url is local or unset"
        ));
    }

    #[test]
    async fn validate_ollama_cloud_model_accepts_remote_endpoint_and_env_key() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("ollama".to_string());
        config.providers.models.insert(
            "ollama".to_string(),
            ModelProviderConfig {
                model: Some("glm-5:cloud".to_string()),
                base_url: Some("https://ollama.com/api".to_string()),
                api_key: None,
                ..Default::default()
            },
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("OLLAMA_API_KEY", "ollama-env-key") };
        let result = config.validate();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("OLLAMA_API_KEY") };

        assert!(result.is_ok(), "expected validation to pass: {result:?}");
    }

    #[test]
    async fn validate_rejects_unknown_model_provider_wire_api() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.providers.fallback = Some("sub2api".to_string());
        config.providers.models.insert(
            "sub2api".to_string(),
            ModelProviderConfig {
                name: Some("sub2api".to_string()),
                base_url: Some("https://api.tonsof.blue/v1".to_string()),
                wire_api: Some("ws".to_string()),
                requires_openai_auth: false,
                azure_openai_resource: None,
                azure_openai_deployment: None,
                azure_openai_api_version: None,
                api_path: None,
                max_tokens: None,
                ..Default::default()
            },
        );

        let error = config.validate().expect_err("expected validation failure");
        assert!(
            error
                .to_string()
                .contains("wire_api must be one of: responses, chat_completions")
        );
    }

    #[test]
    async fn env_override_model_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_MODEL") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("MODEL", "anthropic/claude-3.5-sonnet") };
        config.apply_env_overrides();
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("anthropic/claude-3.5-sonnet")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("MODEL") };
    }

    #[test]
    async fn env_override_workspace() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", "/custom/workspace") };
        config.apply_env_overrides();
        assert_eq!(config.workspace_dir, PathBuf::from("/custom/workspace"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
    }

    #[test]
    async fn resolve_runtime_config_dirs_uses_env_workspace_first() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let workspace_dir = default_config_dir.join("profile-a");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };
        let (config_dir, resolved_workspace_dir, source) =
            resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
                .await
                .unwrap();

        assert_eq!(source, ConfigResolutionSource::EnvWorkspace);
        assert_eq!(config_dir, workspace_dir);
        assert_eq!(resolved_workspace_dir, workspace_dir.join("workspace"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_uses_env_config_dir_first() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let explicit_config_dir = default_config_dir.join("explicit-config");
        let marker_config_dir = default_config_dir.join("profiles").join("alpha");
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        fs::create_dir_all(&default_config_dir).await.unwrap();
        let state = ActiveWorkspaceState {
            config_dir: marker_config_dir.to_string_lossy().into_owned(),
        };
        fs::write(&state_path, toml::to_string(&state).unwrap())
            .await
            .unwrap();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", &explicit_config_dir) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };

        let (config_dir, resolved_workspace_dir, source) =
            resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
                .await
                .unwrap();

        assert_eq!(source, ConfigResolutionSource::EnvConfigDir);
        assert_eq!(config_dir, explicit_config_dir);
        assert_eq!(
            resolved_workspace_dir,
            explicit_config_dir.join("workspace")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_CONFIG_DIR") };
        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_uses_active_workspace_marker() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let marker_config_dir = default_config_dir.join("profiles").join("alpha");
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        fs::create_dir_all(&default_config_dir).await.unwrap();
        let state = ActiveWorkspaceState {
            config_dir: marker_config_dir.to_string_lossy().into_owned(),
        };
        fs::write(&state_path, toml::to_string(&state).unwrap())
            .await
            .unwrap();

        let (config_dir, resolved_workspace_dir, source) =
            resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
                .await
                .unwrap();

        assert_eq!(source, ConfigResolutionSource::ActiveWorkspaceMarker);
        assert_eq!(config_dir, marker_config_dir);
        assert_eq!(resolved_workspace_dir, marker_config_dir.join("workspace"));

        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_falls_back_to_default_layout() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        let (config_dir, resolved_workspace_dir, source) =
            resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
                .await
                .unwrap();

        assert_eq!(source, ConfigResolutionSource::DefaultConfigDir);
        assert_eq!(config_dir, default_config_dir);
        assert_eq!(resolved_workspace_dir, default_workspace_dir);

        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn load_or_init_workspace_override_uses_workspace_root_for_config() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let workspace_dir = temp_home.join("profile-a");

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir.join("workspace"));
        assert_eq!(config.config_path, workspace_dir.join("config.toml"));
        assert!(workspace_dir.join("config.toml").exists());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_workspace_suffix_uses_legacy_config_layout() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let workspace_dir = temp_home.join("workspace");
        let legacy_config_path = temp_home.join(".zeroclaw").join("config.toml");

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir);
        assert_eq!(config.config_path, legacy_config_path);
        assert!(config.config_path.exists());

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_workspace_override_keeps_existing_legacy_config() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let workspace_dir = temp_home.join("custom-workspace");
        let legacy_config_dir = temp_home.join(".zeroclaw");
        let legacy_config_path = legacy_config_dir.join("config.toml");

        fs::create_dir_all(&legacy_config_dir).await.unwrap();
        fs::write(
            &legacy_config_path,
            r#"default_temperature = 0.7
default_model = "legacy-model"
"#,
        )
        .await
        .unwrap();

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir);
        assert_eq!(config.config_path, legacy_config_path);
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("legacy-model")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_decrypts_feishu_channel_secrets() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let config_dir = temp_home.join(".zeroclaw");
        let config_path = config_dir.join("config.toml");

        fs::create_dir_all(&config_dir).await.unwrap();

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };

        let mut config = Config {
            config_path: config_path.clone(),
            workspace_dir: config_dir.join("workspace"),
            ..Default::default()
        };
        config.secrets.encrypt = true;
        config.channels.feishu = Some(FeishuConfig {
            enabled: true,
            app_id: "cli_feishu_123".into(),
            app_secret: "feishu-secret".into(),
            encrypt_key: Some("feishu-encrypt".into()),
            verification_token: Some("feishu-verify".into()),
            allowed_users: vec!["*".into()],
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        });
        config.save().await.unwrap();

        let loaded = Box::pin(Config::load_or_init()).await.unwrap();
        let feishu = loaded.channels.feishu.as_ref().unwrap();
        assert_eq!(feishu.app_secret, "feishu-secret");
        assert_eq!(feishu.encrypt_key.as_deref(), Some("feishu-encrypt"));
        assert_eq!(feishu.verification_token.as_deref(), Some("feishu-verify"));

        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_uses_persisted_active_workspace_marker() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let temp_default_dir = temp_home.join(".zeroclaw");
        let custom_config_dir = temp_home.join("profiles").join("agent-alpha");

        fs::create_dir_all(&custom_config_dir).await.unwrap();
        // Pre-create the default dir so is_temp_directory() can canonicalize
        // the path on macOS (where /var → /private/var symlink requires
        // the directory to exist for canonicalize to resolve correctly).
        fs::create_dir_all(&temp_default_dir).await.unwrap();
        fs::write(
            custom_config_dir.join("config.toml"),
            "default_temperature = 0.7\ndefault_model = \"persisted-profile\"\n",
        )
        .await
        .unwrap();

        // Write the marker using the explicit default dir (no HOME manipulation
        // needed for the persist call itself).
        persist_active_workspace_config_dir_in(&custom_config_dir, &temp_default_dir)
            .await
            .unwrap();

        // Config::load_or_init still reads HOME to find the marker, so we
        // must override HOME here. The persist above already wrote to the
        // correct temp location, so no stale marker can leak.
        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        assert_eq!(config.config_path, custom_config_dir.join("config.toml"));
        assert_eq!(config.workspace_dir, custom_config_dir.join("workspace"));
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("persisted-profile")
        );

        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_env_workspace_override_takes_priority_over_marker() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let temp_default_dir = temp_home.join(".zeroclaw");
        let marker_config_dir = temp_home.join("profiles").join("persisted-profile");
        let env_workspace_dir = temp_home.join("env-workspace");

        fs::create_dir_all(&marker_config_dir).await.unwrap();
        fs::write(
            marker_config_dir.join("config.toml"),
            "default_temperature = 0.7\ndefault_model = \"marker-model\"\n",
        )
        .await
        .unwrap();

        // Write marker via explicit default dir, then set HOME for load_or_init.
        persist_active_workspace_config_dir_in(&marker_config_dir, &temp_default_dir)
            .await
            .unwrap();

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &env_workspace_dir) };

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        assert_eq!(config.workspace_dir, env_workspace_dir.join("workspace"));
        assert_eq!(config.config_path, env_workspace_dir.join("config.toml"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn persist_active_workspace_marker_is_cleared_for_default_config_dir() {
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let default_config_dir = temp_home.join(".zeroclaw");
        let custom_config_dir = temp_home.join("profiles").join("custom-profile");
        let marker_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        // Use the _in variant directly -- no HOME manipulation needed since
        // this test only exercises persist/clear logic, not Config::load_or_init.
        persist_active_workspace_config_dir_in(&custom_config_dir, &default_config_dir)
            .await
            .unwrap();
        assert!(marker_path.exists());

        persist_active_workspace_config_dir_in(&default_config_dir, &default_config_dir)
            .await
            .unwrap();
        assert!(!marker_path.exists());

        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    #[allow(clippy::large_futures)]
    async fn load_or_init_logs_existing_config_as_initialized() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let workspace_dir = temp_home.join("profile-a");
        let config_path = workspace_dir.join("config.toml");

        fs::create_dir_all(&workspace_dir).await.unwrap();
        fs::write(
            &config_path,
            r#"default_temperature = 0.7
default_model = "persisted-profile"
"#,
        )
        .await
        .unwrap();

        let original_home = std::env::var("HOME").ok();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOME", &temp_home) };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir) };

        let capture = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_target(false)
            .with_writer(capture.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let guard = tracing::dispatcher::set_default(&dispatch);

        let config = Box::pin(Config::load_or_init()).await.unwrap();

        drop(guard);
        let logs = capture.captured();

        assert_eq!(config.workspace_dir, workspace_dir.join("workspace"));
        assert_eq!(config.config_path, config_path);
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("persisted-profile")
        );
        assert!(logs.contains("Config loaded"), "{logs}");
        assert!(logs.contains("initialized=true"), "{logs}");
        assert!(!logs.contains("initialized=false"), "{logs}");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_WORKSPACE") };
        if let Some(home) = original_home {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var("HOME", home) };
        } else {
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var("HOME") };
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn env_override_empty_values_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_provider = config.providers.fallback.clone();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROVIDER", "") };
        config.apply_env_overrides();
        assert_eq!(config.providers.fallback, original_provider);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_PROVIDER") };
    }

    #[test]
    async fn env_override_gateway_port() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.gateway.port, 42617);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_GATEWAY_PORT", "8080") };
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, 8080);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_GATEWAY_PORT") };
    }

    #[test]
    async fn env_override_port_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_GATEWAY_PORT") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("PORT", "9000") };
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, 9000);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("PORT") };
    }

    #[test]
    async fn env_override_gateway_host() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.gateway.host, "127.0.0.1");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_GATEWAY_HOST", "0.0.0.0") };
        config.apply_env_overrides();
        assert_eq!(config.gateway.host, "0.0.0.0");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_GATEWAY_HOST") };
    }

    #[test]
    async fn env_override_host_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_GATEWAY_HOST") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("HOST", "0.0.0.0") };
        config.apply_env_overrides();
        assert_eq!(config.gateway.host, "0.0.0.0");

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("HOST") };
    }

    #[test]
    async fn env_override_require_pairing() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert!(config.gateway.require_pairing);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REQUIRE_PAIRING", "false") };
        config.apply_env_overrides();
        assert!(!config.gateway.require_pairing);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REQUIRE_PAIRING", "true") };
        config.apply_env_overrides();
        assert!(config.gateway.require_pairing);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_REQUIRE_PAIRING") };
    }

    #[test]
    async fn env_override_temperature() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_TEMPERATURE", "0.5") };
        config.apply_env_overrides();
        assert!(
            (config
                .providers
                .fallback_provider()
                .and_then(|e| e.temperature)
                .unwrap_or(0.7)
                - 0.5)
                .abs()
                < f64::EPSILON
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_TEMPERATURE") };
    }

    #[test]
    async fn env_override_temperature_out_of_range_ignored() {
        let _env_guard = env_override_lock().await;
        // Clean up any leftover env vars from other tests
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_TEMPERATURE") };

        let mut config = Config::default();
        let original_temp = config
            .providers
            .fallback_provider()
            .and_then(|e| e.temperature)
            .unwrap_or(0.7);

        // Temperature > 2.0 should be ignored
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_TEMPERATURE", "3.0") };
        config.apply_env_overrides();
        assert!(
            (config
                .providers
                .fallback_provider()
                .and_then(|e| e.temperature)
                .unwrap_or(0.7)
                - original_temp)
                .abs()
                < f64::EPSILON,
            "Temperature 3.0 should be ignored (out of range)"
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_TEMPERATURE") };
    }

    #[test]
    async fn validate_rejects_out_of_range_temperature() {
        let mut config = Config::default();
        config.providers.fallback = Some("test".into());
        config.providers.models.insert(
            "test".into(),
            ModelProviderConfig {
                name: Some("test-provider".into()),
                temperature: Some(99.0),
                ..Default::default()
            },
        );
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("temperature"),
            "expected temperature validation error, got: {err}"
        );
    }

    #[test]
    async fn validate_rejects_negative_temperature() {
        let mut config = Config::default();
        config.providers.fallback = Some("test".into());
        config.providers.models.insert(
            "test".into(),
            ModelProviderConfig {
                name: Some("test-provider".into()),
                temperature: Some(-0.5),
                ..Default::default()
            },
        );
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("temperature"),
            "expected temperature validation error, got: {err}"
        );
    }

    #[test]
    async fn validate_accepts_valid_temperature() {
        let mut config = Config::default();
        config.providers.fallback = Some("test".into());
        config.providers.models.insert(
            "test".into(),
            ModelProviderConfig {
                name: Some("test-provider".into()),
                temperature: Some(0.7),
                ..Default::default()
            },
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    async fn env_override_reasoning_enabled() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.runtime.reasoning_enabled, None);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REASONING_ENABLED", "false") };
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(false));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REASONING_ENABLED", "true") };
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(true));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_REASONING_ENABLED") };
    }

    #[test]
    async fn env_override_reasoning_invalid_value_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.runtime.reasoning_enabled = Some(false);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REASONING_ENABLED", "maybe") };
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(false));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_REASONING_ENABLED") };
    }

    #[test]
    async fn env_override_reasoning_effort() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.runtime.reasoning_effort, None);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_REASONING_EFFORT", "HIGH") };
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_effort.as_deref(), Some("high"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_REASONING_EFFORT") };
    }

    #[test]
    async fn env_override_reasoning_effort_legacy_codex_env() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_CODEX_REASONING_EFFORT", "minimal") };
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_effort.as_deref(), Some("minimal"));

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_CODEX_REASONING_EFFORT") };
    }

    #[test]
    async fn env_override_invalid_port_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_port = config.gateway.port;

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("PORT", "not_a_number") };
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, original_port);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("PORT") };
    }

    #[test]
    async fn env_override_web_search_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_ENABLED", "false") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_PROVIDER", "brave") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_MAX_RESULTS", "7") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_TIMEOUT_SECS", "20") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("BRAVE_API_KEY", "brave-test-key") };

        config.apply_env_overrides();

        assert!(!config.web_search.enabled);
        assert_eq!(config.web_search.provider, "brave");
        assert_eq!(config.web_search.max_results, 7);
        assert_eq!(config.web_search.timeout_secs, 20);
        assert_eq!(
            config.web_search.brave_api_key.as_deref(),
            Some("brave-test-key")
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_ENABLED") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_MAX_RESULTS") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_TIMEOUT_SECS") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("BRAVE_API_KEY") };
    }

    #[test]
    async fn env_override_web_search_invalid_values_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_max_results = config.web_search.max_results;
        let original_timeout = config.web_search.timeout_secs;

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_MAX_RESULTS", "99") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("WEB_SEARCH_TIMEOUT_SECS", "0") };

        config.apply_env_overrides();

        assert_eq!(config.web_search.max_results, original_max_results);
        assert_eq!(config.web_search.timeout_secs, original_timeout);

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_MAX_RESULTS") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("WEB_SEARCH_TIMEOUT_SECS") };
    }

    #[test]
    async fn env_override_storage_provider_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_STORAGE_PROVIDER", "qdrant") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_STORAGE_DB_URL", "http://localhost:6333") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS", "15") };

        config.apply_env_overrides();

        assert_eq!(config.storage.provider.config.provider, "qdrant");
        assert_eq!(
            config.storage.provider.config.db_url.as_deref(),
            Some("http://localhost:6333")
        );
        assert_eq!(
            config.storage.provider.config.connect_timeout_secs,
            Some(15)
        );

        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_STORAGE_PROVIDER") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_STORAGE_DB_URL") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS") };
    }

    #[test]
    async fn proxy_config_scope_services_requires_entries_when_enabled() {
        let proxy = ProxyConfig {
            enabled: true,
            http_proxy: Some("http://127.0.0.1:7890".into()),
            https_proxy: None,
            all_proxy: None,
            no_proxy: Vec::new(),
            scope: ProxyScope::Services,
            services: Vec::new(),
        };

        let error = proxy.validate().unwrap_err().to_string();
        assert!(error.contains("proxy.scope='services'"));
    }

    #[test]
    async fn env_override_proxy_scope_services() {
        let _env_guard = env_override_lock().await;
        clear_proxy_env_test_vars();

        let mut config = Config::default();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROXY_ENABLED", "true") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_HTTP_PROXY", "http://127.0.0.1:7890") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe {
            std::env::set_var(
                "ZEROCLAW_PROXY_SERVICES",
                "provider.openai, tool.http_request",
            );
        }
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROXY_SCOPE", "services") };

        config.apply_env_overrides();

        assert!(config.proxy.enabled);
        assert_eq!(config.proxy.scope, ProxyScope::Services);
        assert_eq!(
            config.proxy.http_proxy.as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert!(config.proxy.should_apply_to_service("provider.openai"));
        assert!(config.proxy.should_apply_to_service("tool.http_request"));
        assert!(!config.proxy.should_apply_to_service("provider.anthropic"));

        clear_proxy_env_test_vars();
    }

    #[test]
    async fn env_override_proxy_scope_environment_applies_process_env() {
        let _env_guard = env_override_lock().await;
        clear_proxy_env_test_vars();

        let mut config = Config::default();
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROXY_ENABLED", "true") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_PROXY_SCOPE", "environment") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_HTTP_PROXY", "http://127.0.0.1:7890") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_HTTPS_PROXY", "http://127.0.0.1:7891") };
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::set_var("ZEROCLAW_NO_PROXY", "localhost,127.0.0.1") };

        config.apply_env_overrides();

        assert_eq!(config.proxy.scope, ProxyScope::Environment);
        assert_eq!(
            std::env::var("HTTP_PROXY").ok().as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            std::env::var("HTTPS_PROXY").ok().as_deref(),
            Some("http://127.0.0.1:7891")
        );
        assert!(
            std::env::var("NO_PROXY")
                .ok()
                .is_some_and(|value| value.contains("localhost"))
        );

        clear_proxy_env_test_vars();
    }

    #[test]
    async fn google_workspace_allowed_operations_require_methods() {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "gmail".into(),
            resource: "users".into(),
            sub_resource: Some("drafts".into()),
            methods: Vec::new(),
        }];

        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("google_workspace.allowed_operations[0].methods"));
    }

    #[test]
    async fn google_workspace_allowed_operations_reject_duplicate_service_resource_sub_resource_entries()
     {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("drafts".into()),
                methods: vec!["create".into()],
            },
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("drafts".into()),
                methods: vec!["update".into()],
            },
        ];

        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("duplicate service/resource/sub_resource entry"));
    }

    #[test]
    async fn google_workspace_allowed_operations_allow_same_resource_different_sub_resource() {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("messages".into()),
                methods: vec!["list".into(), "get".into()],
            },
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("drafts".into()),
                methods: vec!["create".into(), "update".into()],
            },
        ];

        assert!(config.validate().is_ok());
    }

    #[test]
    async fn google_workspace_allowed_operations_reject_duplicate_methods_within_entry() {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "gmail".into(),
            resource: "users".into(),
            sub_resource: Some("drafts".into()),
            methods: vec!["create".into(), "create".into()],
        }];

        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("duplicate entry"),
            "expected duplicate entry error, got: {err}"
        );
    }

    #[test]
    async fn google_workspace_allowed_operations_accept_valid_entries() {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("messages".into()),
                methods: vec!["list".into(), "get".into()],
            },
            GoogleWorkspaceAllowedOperation {
                service: "drive".into(),
                resource: "files".into(),
                sub_resource: None,
                methods: vec!["list".into(), "get".into()],
            },
        ];

        assert!(config.validate().is_ok());
    }

    #[test]
    async fn google_workspace_allowed_operations_reject_invalid_sub_resource_characters() {
        let mut config = Config::default();
        config.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "gmail".into(),
            resource: "users".into(),
            sub_resource: Some("bad resource!".into()),
            methods: vec!["list".into()],
        }];

        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("sub_resource contains invalid characters"));
    }

    fn runtime_proxy_cache_contains(cache_key: &str) -> bool {
        match runtime_proxy_client_cache().read() {
            Ok(guard) => guard.contains_key(cache_key),
            Err(poisoned) => poisoned.into_inner().contains_key(cache_key),
        }
    }

    #[test]
    async fn runtime_proxy_client_cache_reuses_default_profile_key() {
        let service_key = format!(
            "provider.cache_test.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        );
        let cache_key = runtime_proxy_cache_key(&service_key, None, None);

        clear_runtime_proxy_client_cache();
        assert!(!runtime_proxy_cache_contains(&cache_key));

        let _ = build_runtime_proxy_client(&service_key);
        assert!(runtime_proxy_cache_contains(&cache_key));

        let _ = build_runtime_proxy_client(&service_key);
        assert!(runtime_proxy_cache_contains(&cache_key));
    }

    #[test]
    async fn set_runtime_proxy_config_clears_runtime_proxy_client_cache() {
        let service_key = format!(
            "provider.cache_timeout_test.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        );
        let cache_key = runtime_proxy_cache_key(&service_key, Some(30), Some(5));

        clear_runtime_proxy_client_cache();
        let _ = build_runtime_proxy_client_with_timeouts(&service_key, 30, 5);
        assert!(runtime_proxy_cache_contains(&cache_key));

        set_runtime_proxy_config(ProxyConfig::default());
        assert!(!runtime_proxy_cache_contains(&cache_key));
    }

    #[test]
    async fn gateway_config_default_values() {
        let g = GatewayConfig::default();
        assert_eq!(g.port, 42617);
        assert_eq!(g.host, "127.0.0.1");
        assert!(g.require_pairing);
        assert!(!g.allow_public_bind);
        assert!(g.paired_tokens.is_empty());
        assert!(!g.trust_forwarded_headers);
        assert_eq!(g.rate_limit_max_keys, 10_000);
        assert_eq!(g.idempotency_max_keys, 10_000);
    }

    // ── Peripherals config ───────────────────────────────────────

    #[test]
    async fn peripherals_config_default_disabled() {
        let p = PeripheralsConfig::default();
        assert!(!p.enabled);
        assert!(p.boards.is_empty());
    }

    #[test]
    async fn peripheral_board_config_defaults() {
        let b = PeripheralBoardConfig::default();
        assert!(b.board.is_empty());
        assert_eq!(b.transport, "serial");
        assert!(b.path.is_none());
        assert_eq!(b.baud, 115_200);
    }

    #[test]
    async fn peripherals_config_toml_roundtrip() {
        let p = PeripheralsConfig {
            enabled: true,
            boards: vec![PeripheralBoardConfig {
                board: "nucleo-f401re".into(),
                transport: "serial".into(),
                path: Some("/dev/ttyACM0".into()),
                baud: 115_200,
            }],
            datasheet_dir: None,
        };
        let toml_str = toml::to_string(&p).unwrap();
        let parsed: PeripheralsConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.boards.len(), 1);
        assert_eq!(parsed.boards[0].board, "nucleo-f401re");
        assert_eq!(parsed.boards[0].path.as_deref(), Some("/dev/ttyACM0"));
    }

    #[test]
    async fn lark_config_serde() {
        let lc = LarkConfig {
            enabled: true,
            app_id: "cli_123456".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["user_123".into(), "user_456".into()],
            mention_only: false,
            use_feishu: true,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        };
        let json = serde_json::to_string(&lc).unwrap();
        let parsed: LarkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.app_id, "cli_123456");
        assert_eq!(parsed.app_secret, "secret_abc");
        assert_eq!(parsed.encrypt_key.as_deref(), Some("encrypt_key"));
        assert_eq!(parsed.verification_token.as_deref(), Some("verify_token"));
        assert_eq!(parsed.allowed_users.len(), 2);
        assert!(parsed.use_feishu);
    }

    #[test]
    async fn lark_config_toml_roundtrip() {
        let lc = LarkConfig {
            enabled: true,
            app_id: "cli_123456".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["*".into()],
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
        };
        let toml_str = toml::to_string(&lc).unwrap();
        let parsed: LarkConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.app_id, "cli_123456");
        assert_eq!(parsed.app_secret, "secret_abc");
        assert!(!parsed.use_feishu);
    }

    #[test]
    async fn lark_config_deserializes_without_optional_fields() {
        let json = r#"{"app_id":"cli_123","app_secret":"secret"}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.encrypt_key.is_none());
        assert!(parsed.verification_token.is_none());
        assert!(parsed.allowed_users.is_empty());
        assert!(!parsed.mention_only);
        assert!(!parsed.use_feishu);
    }

    #[test]
    async fn lark_config_defaults_to_lark_endpoint() {
        let json = r#"{"app_id":"cli_123","app_secret":"secret"}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert!(
            !parsed.use_feishu,
            "use_feishu should default to false (Lark)"
        );
    }

    #[test]
    async fn lark_config_with_wildcard_allowed_users() {
        let json = r#"{"app_id":"cli_123","app_secret":"secret","allowed_users":["*"]}"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.allowed_users, vec!["*"]);
    }

    #[test]
    async fn feishu_config_serde() {
        let fc = FeishuConfig {
            enabled: true,
            app_id: "cli_feishu_123".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["user_123".into(), "user_456".into()],
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        };
        let json = serde_json::to_string(&fc).unwrap();
        let parsed: FeishuConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.app_id, "cli_feishu_123");
        assert_eq!(parsed.app_secret, "secret_abc");
        assert_eq!(parsed.encrypt_key.as_deref(), Some("encrypt_key"));
        assert_eq!(parsed.verification_token.as_deref(), Some("verify_token"));
        assert_eq!(parsed.allowed_users.len(), 2);
    }

    #[test]
    async fn feishu_config_toml_roundtrip() {
        let fc = FeishuConfig {
            enabled: true,
            app_id: "cli_feishu_123".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["*".into()],
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            proxy_url: None,
        };
        let toml_str = toml::to_string(&fc).unwrap();
        let parsed: FeishuConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.app_id, "cli_feishu_123");
        assert_eq!(parsed.app_secret, "secret_abc");
        assert_eq!(parsed.receive_mode, LarkReceiveMode::Webhook);
        assert_eq!(parsed.port, Some(9898));
    }

    #[test]
    async fn feishu_config_deserializes_without_optional_fields() {
        let json = r#"{"app_id":"cli_123","app_secret":"secret"}"#;
        let parsed: FeishuConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.encrypt_key.is_none());
        assert!(parsed.verification_token.is_none());
        assert!(parsed.allowed_users.is_empty());
        assert_eq!(parsed.receive_mode, LarkReceiveMode::Websocket);
        assert!(parsed.port.is_none());
    }

    // ── LINE ──────────────────────────────────────────────────

    #[test]
    async fn line_config_toml_roundtrip() {
        // Full [channels.line] TOML block — covers every user-facing field.
        //
        // channel_access_token and channel_secret can be omitted here and
        // supplied via LINE_CHANNEL_ACCESS_TOKEN / LINE_CHANNEL_SECRET env vars
        // instead; both fields default to "" when absent.
        let toml = r#"
[channels_config.line]
enabled = true
channel_access_token = "ChannelAccessToken=="
channel_secret = "abc123secret"
dm_policy = "pairing"
group_policy = "mention"
allowed_users = []
webhook_port = 8443
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ln = config.channels.line.as_ref().unwrap();
        assert!(ln.enabled);
        assert_eq!(ln.channel_access_token, "ChannelAccessToken==");
        assert_eq!(ln.channel_secret, "abc123secret");
        assert_eq!(ln.dm_policy, LineDmPolicy::Pairing);
        assert_eq!(ln.group_policy, LineGroupPolicy::Mention);
        assert_eq!(ln.webhook_port, 8443);
        assert!(ln.proxy_url.is_none());
    }

    #[test]
    async fn line_config_defaults() {
        // Minimal config — only the required secret fields are provided.
        // All optional fields should resolve to documented defaults.
        let toml = r#"
[channels_config.line]
channel_access_token = "tok"
channel_secret = "sec"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ln = config.channels.line.as_ref().unwrap();
        assert!(!ln.enabled, "enabled should default to false");
        assert_eq!(
            ln.dm_policy,
            LineDmPolicy::Pairing,
            "dm_policy default is pairing"
        );
        assert_eq!(
            ln.group_policy,
            LineGroupPolicy::Mention,
            "group_policy default is mention"
        );
        assert_eq!(ln.webhook_port, 8443, "webhook_port default is 8443");
        assert!(ln.allowed_users.is_empty());
        assert!(ln.proxy_url.is_none());
    }

    #[test]
    async fn line_config_allowlist_policy() {
        // dm_policy = allowlist with an explicit user ID list.
        let toml = r#"
[channels_config.line]
channel_access_token = "tok"
channel_secret = "sec"
dm_policy = "allowlist"
allowed_users = ["Uabc123", "Udef456"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ln = config.channels.line.as_ref().unwrap();
        assert_eq!(ln.dm_policy, LineDmPolicy::Allowlist);
        assert_eq!(ln.allowed_users, vec!["Uabc123", "Udef456"]);
    }

    #[test]
    async fn line_config_open_policies() {
        // dm_policy = open + group_policy = open — most permissive combination.
        let toml = r#"
[channels_config.line]
channel_access_token = "tok"
channel_secret = "sec"
dm_policy = "open"
group_policy = "open"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ln = config.channels.line.as_ref().unwrap();
        assert_eq!(ln.dm_policy, LineDmPolicy::Open);
        assert_eq!(ln.group_policy, LineGroupPolicy::Open);
    }

    #[test]
    async fn line_config_group_disabled() {
        // group_policy = disabled — bot ignores all group/room messages.
        let toml = r#"
[channels_config.line]
channel_access_token = "tok"
channel_secret = "sec"
group_policy = "disabled"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let ln = config.channels.line.as_ref().unwrap();
        assert_eq!(ln.group_policy, LineGroupPolicy::Disabled);
    }

    #[test]
    async fn nextcloud_talk_config_serde() {
        let nc = NextcloudTalkConfig {
            enabled: true,
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: Some("webhook-secret".into()),
            allowed_users: vec!["user_a".into(), "*".into()],
            proxy_url: None,
            bot_name: None,
        };

        let json = serde_json::to_string(&nc).unwrap();
        let parsed: NextcloudTalkConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.base_url, "https://cloud.example.com");
        assert_eq!(parsed.app_token, "app-token");
        assert_eq!(parsed.webhook_secret.as_deref(), Some("webhook-secret"));
        assert_eq!(parsed.allowed_users, vec!["user_a", "*"]);
    }

    #[test]
    async fn nextcloud_talk_config_defaults_optional_fields() {
        let json = r#"{"base_url":"https://cloud.example.com","app_token":"app-token"}"#;
        let parsed: NextcloudTalkConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.webhook_secret.is_none());
        assert!(parsed.allowed_users.is_empty());
    }

    // ── Config file permission hardening (Unix only) ───────────────

    #[cfg(unix)]
    #[test]
    async fn new_config_file_has_restricted_permissions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Create a config and save it
        let config = Config {
            config_path: config_path.clone(),
            ..Default::default()
        };
        config.save().await.unwrap();

        let meta = fs::metadata(&config_path).await.unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "New config file should be owner-only (0600), got {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    async fn save_restricts_existing_world_readable_config_to_owner_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        let mut config = Config {
            config_path: config_path.clone(),
            ..Default::default()
        };
        config.save().await.unwrap();

        // Simulate the regression state observed in issue #1345.
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let loose_mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            loose_mode, 0o644,
            "test setup requires world-readable config"
        );

        if let Some(entry) = config.providers.fallback_provider_mut() {
            entry.temperature = Some(0.6);
        }
        config.save().await.unwrap();

        let hardened_mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            hardened_mode, 0o600,
            "Saving config should restore owner-only permissions (0600)"
        );
    }

    #[cfg(unix)]
    #[test]
    async fn world_readable_config_is_detectable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let config_path = tmp.path().join("config.toml");

        // Create a config file with intentionally loose permissions
        std::fs::write(&config_path, "# test config").unwrap();
        std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let meta = std::fs::metadata(&config_path).unwrap();
        let mode = meta.permissions().mode();
        assert!(
            mode & 0o004 != 0,
            "Test setup: file should be world-readable (mode {mode:o})"
        );
    }

    #[test]
    async fn transcription_config_defaults() {
        let tc = TranscriptionConfig::default();
        assert!(!tc.enabled);
        assert!(tc.api_url.contains("groq.com"));
        assert_eq!(tc.model, "whisper-large-v3-turbo");
        assert!(tc.language.is_none());
        assert_eq!(tc.max_duration_secs, 120);
        assert!(!tc.transcribe_non_ptt_audio);
    }

    #[test]
    async fn config_roundtrip_with_transcription() {
        let mut config = Config::default();
        config.transcription.enabled = true;
        config.transcription.language = Some("en".into());

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed = parse_test_config(&toml_str);

        assert!(parsed.transcription.enabled);
        assert_eq!(parsed.transcription.language.as_deref(), Some("en"));
        assert_eq!(parsed.transcription.model, "whisper-large-v3-turbo");
    }

    #[test]
    async fn config_without_transcription_uses_defaults() {
        let toml_str = r#"
            default_provider = "openrouter"
            default_model = "test-model"
            default_temperature = 0.7
        "#;
        let parsed = parse_test_config(toml_str);
        assert!(!parsed.transcription.enabled);
        assert_eq!(parsed.transcription.max_duration_secs, 120);
    }

    #[test]
    async fn security_defaults_are_backward_compatible() {
        let parsed = parse_test_config(
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7
"#,
        );

        assert!(!parsed.security.otp.enabled);
        assert_eq!(parsed.security.otp.method, OtpMethod::Totp);
        assert!(!parsed.security.estop.enabled);
        assert!(parsed.security.estop.require_otp_to_resume);
    }

    #[test]
    async fn security_toml_parses_otp_and_estop_sections() {
        let parsed = parse_test_config(
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7

[security.otp]
enabled = true
method = "totp"
token_ttl_secs = 30
cache_valid_secs = 120
gated_actions = ["shell", "browser_open"]
gated_domains = ["*.chase.com", "accounts.google.com"]
gated_domain_categories = ["banking"]

[security.estop]
enabled = true
state_file = "~/.zeroclaw/estop-state.json"
require_otp_to_resume = true
"#,
        );

        assert!(parsed.security.otp.enabled);
        assert!(parsed.security.estop.enabled);
        assert_eq!(parsed.security.otp.gated_actions.len(), 2);
        assert_eq!(parsed.security.otp.gated_domains.len(), 2);
        parsed.validate().unwrap();
    }

    #[test]
    async fn security_validation_rejects_invalid_domain_glob() {
        let mut config = Config::default();
        config.security.otp.gated_domains = vec!["bad domain.com".into()];

        let err = config.validate().expect_err("expected invalid domain glob");
        assert!(err.to_string().contains("gated_domains"));
    }

    #[test]
    async fn validate_accepts_local_whisper_as_transcription_default_provider() {
        let mut config = Config::default();
        config.transcription.default_provider = "local_whisper".to_string();

        config.validate().expect(
            "local_whisper must be accepted by the transcription.default_provider allowlist",
        );
    }

    #[test]
    async fn validate_rejects_unknown_transcription_default_provider() {
        let mut config = Config::default();
        config.transcription.default_provider = "unknown_stt".to_string();

        let err = config
            .validate()
            .expect_err("expected validation to reject unknown transcription provider");
        assert!(
            err.to_string().contains("transcription.default_provider"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn channel_secret_telegram_bot_token_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_tg_bot_token_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        let plaintext_token = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

        let mut config = Config {
            workspace_dir: dir.join("workspace"),
            config_path: dir.join("config.toml"),
            ..Default::default()
        };
        config.channels.telegram = Some(TelegramConfig {
            enabled: true,
            bot_token: plaintext_token.into(),
            allowed_users: vec!["user1".into()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: default_draft_update_interval_ms(),
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: default_telegram_approval_timeout_secs(),
        });

        // Save (triggers encryption)
        config.save().await.unwrap();

        // Read raw TOML and verify plaintext token is NOT present
        let raw_toml = tokio::fs::read_to_string(&config.config_path)
            .await
            .unwrap();
        assert!(
            !raw_toml.contains(plaintext_token),
            "Saved TOML must not contain the plaintext bot_token"
        );

        // Parse stored TOML and verify the value is encrypted
        let stored: Config = toml::from_str(&raw_toml).unwrap();
        let stored_token = &stored.channels.telegram.as_ref().unwrap().bot_token;
        assert!(
            crate::secrets::SecretStore::is_encrypted(stored_token),
            "Stored bot_token must be marked as encrypted"
        );

        // Decrypt and verify it matches the original plaintext
        let store = crate::secrets::SecretStore::new(&dir, true);
        assert_eq!(store.decrypt(stored_token).unwrap(), plaintext_token);

        // Simulate a full load: deserialize then decrypt (mirrors load_or_init logic)
        let mut loaded: Config = toml::from_str(&raw_toml).unwrap();
        loaded.config_path = dir.join("config.toml");
        let load_store = crate::secrets::SecretStore::new(&dir, loaded.secrets.encrypt);
        loaded.decrypt_secrets(&load_store).unwrap();
        assert_eq!(
            loaded.channels.telegram.as_ref().unwrap().bot_token,
            plaintext_token,
            "Loaded bot_token must match the original plaintext after decryption"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[test]
    async fn security_validation_rejects_unknown_domain_category() {
        let mut config = Config::default();
        config.security.otp.gated_domain_categories = vec!["not_real".into()];

        let err = config
            .validate()
            .expect_err("expected unknown domain category");
        assert!(err.to_string().contains("gated_domain_categories"));
    }

    #[test]
    async fn security_validation_rejects_zero_token_ttl() {
        let mut config = Config::default();
        config.security.otp.token_ttl_secs = 0;

        let err = config
            .validate()
            .expect_err("expected ttl validation failure");
        assert!(err.to_string().contains("token_ttl_secs"));
    }

    // ── MCP config validation ─────────────────────────────────────────────

    fn stdio_server(name: &str, command: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Stdio,
            command: command.to_string(),
            ..Default::default()
        }
    }

    fn http_server(name: &str, url: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Http,
            url: Some(url.to_string()),
            ..Default::default()
        }
    }

    fn sse_server(name: &str, url: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpTransport::Sse,
            url: Some(url.to_string()),
            ..Default::default()
        }
    }

    #[test]
    async fn validate_mcp_config_empty_servers_ok() {
        let cfg = McpConfig::default();
        assert!(validate_mcp_config(&cfg).is_ok());
    }

    #[test]
    async fn validate_mcp_config_valid_stdio_ok() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![stdio_server("fs", "/usr/bin/mcp-fs")],
            ..Default::default()
        };
        assert!(validate_mcp_config(&cfg).is_ok());
    }

    #[test]
    async fn validate_mcp_config_valid_http_ok() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![http_server("svc", "http://localhost:8080/mcp")],
            ..Default::default()
        };
        assert!(validate_mcp_config(&cfg).is_ok());
    }

    #[test]
    async fn validate_mcp_config_valid_sse_ok() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![sse_server("svc", "https://example.com/events")],
            ..Default::default()
        };
        assert!(validate_mcp_config(&cfg).is_ok());
    }

    #[test]
    async fn validate_mcp_config_rejects_empty_name() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![stdio_server("", "/usr/bin/tool")],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("empty name should fail");
        assert!(
            err.to_string().contains("name must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    async fn validate_mcp_config_rejects_whitespace_name() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![stdio_server("   ", "/usr/bin/tool")],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("whitespace name should fail");
        assert!(
            err.to_string().contains("name must not be empty"),
            "got: {err}"
        );
    }

    #[test]
    async fn validate_mcp_config_rejects_duplicate_names() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![
                stdio_server("fs", "/usr/bin/mcp-a"),
                stdio_server("fs", "/usr/bin/mcp-b"),
            ],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("duplicate name should fail");
        assert!(err.to_string().contains("duplicate name"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_rejects_zero_timeout() {
        let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
        server.tool_timeout_secs = Some(0);
        let cfg = McpConfig {
            enabled: true,
            servers: vec![server],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("zero timeout should fail");
        assert!(err.to_string().contains("greater than 0"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_rejects_timeout_exceeding_max() {
        let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
        server.tool_timeout_secs = Some(MCP_MAX_TOOL_TIMEOUT_SECS + 1);
        let cfg = McpConfig {
            enabled: true,
            servers: vec![server],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("oversized timeout should fail");
        assert!(err.to_string().contains("exceeds max"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_allows_max_timeout_exactly() {
        let mut server = stdio_server("fs", "/usr/bin/mcp-fs");
        server.tool_timeout_secs = Some(MCP_MAX_TOOL_TIMEOUT_SECS);
        let cfg = McpConfig {
            enabled: true,
            servers: vec![server],
            ..Default::default()
        };
        assert!(validate_mcp_config(&cfg).is_ok());
    }

    #[test]
    async fn validate_mcp_config_rejects_stdio_with_empty_command() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![stdio_server("fs", "")],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("empty command should fail");
        assert!(
            err.to_string().contains("requires non-empty command"),
            "got: {err}"
        );
    }

    #[test]
    async fn validate_mcp_config_rejects_http_without_url() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                name: "svc".to_string(),
                transport: McpTransport::Http,
                url: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("http without url should fail");
        assert!(err.to_string().contains("requires url"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_rejects_sse_without_url() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![McpServerConfig {
                name: "svc".to_string(),
                transport: McpTransport::Sse,
                url: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("sse without url should fail");
        assert!(err.to_string().contains("requires url"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_rejects_non_http_scheme() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![http_server("svc", "ftp://example.com/mcp")],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("non-http scheme should fail");
        assert!(err.to_string().contains("http/https"), "got: {err}");
    }

    #[test]
    async fn validate_mcp_config_rejects_invalid_url() {
        let cfg = McpConfig {
            enabled: true,
            servers: vec![http_server("svc", "not a url at all !!!")],
            ..Default::default()
        };
        let err = validate_mcp_config(&cfg).expect_err("invalid url should fail");
        assert!(err.to_string().contains("valid URL"), "got: {err}");
    }

    #[test]
    async fn mcp_config_default_disabled_with_empty_servers() {
        let cfg = McpConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.servers.is_empty());
    }

    #[test]
    async fn mcp_transport_serde_roundtrip_lowercase() {
        let cases = [
            (McpTransport::Stdio, "\"stdio\""),
            (McpTransport::Http, "\"http\""),
            (McpTransport::Sse, "\"sse\""),
        ];
        for (variant, expected_json) in &cases {
            let serialized = serde_json::to_string(variant).expect("serialize");
            assert_eq!(&serialized, expected_json, "variant: {variant:?}");
            let deserialized: McpTransport =
                serde_json::from_str(expected_json).expect("deserialize");
            assert_eq!(&deserialized, variant);
        }
    }

    #[test]
    async fn swarm_strategy_roundtrip() {
        let cases = vec![
            (SwarmStrategy::Sequential, "\"sequential\""),
            (SwarmStrategy::Parallel, "\"parallel\""),
            (SwarmStrategy::Router, "\"router\""),
        ];
        for (variant, expected_json) in &cases {
            let serialized = serde_json::to_string(variant).expect("serialize");
            assert_eq!(&serialized, expected_json, "variant: {variant:?}");
            let deserialized: SwarmStrategy =
                serde_json::from_str(expected_json).expect("deserialize");
            assert_eq!(&deserialized, variant);
        }
    }

    #[test]
    async fn swarm_config_deserializes_with_defaults() {
        let toml_str = r#"
            agents = ["researcher", "writer"]
            strategy = "sequential"
        "#;
        let config: SwarmConfig = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(config.agents, vec!["researcher", "writer"]);
        assert_eq!(config.strategy, SwarmStrategy::Sequential);
        assert!(config.router_prompt.is_none());
        assert!(config.description.is_none());
        assert_eq!(config.timeout_secs, 300);
    }

    #[test]
    async fn swarm_config_deserializes_full() {
        let toml_str = r#"
            agents = ["a", "b", "c"]
            strategy = "router"
            router_prompt = "Pick the best."
            description = "Multi-agent router"
            timeout_secs = 120
        "#;
        let config: SwarmConfig = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.strategy, SwarmStrategy::Router);
        assert_eq!(config.router_prompt.as_deref(), Some("Pick the best."));
        assert_eq!(config.description.as_deref(), Some("Multi-agent router"));
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    async fn config_with_swarms_section_deserializes() {
        let toml_str = r#"
            [agents.researcher]
            provider = "ollama"
            model = "llama3"

            [agents.writer]
            provider = "openrouter"
            model = "claude-sonnet"

            [swarms.pipeline]
            agents = ["researcher", "writer"]
            strategy = "sequential"
        "#;
        let config = parse_test_config(toml_str);
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.swarms.len(), 1);
        assert!(config.swarms.contains_key("pipeline"));
    }

    #[tokio::test]
    async fn nevis_client_secret_encrypt_decrypt_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_nevis_secret_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        let plaintext_secret = "nevis-test-client-secret-value";

        let mut config = Config {
            workspace_dir: dir.join("workspace"),
            config_path: dir.join("config.toml"),
            ..Default::default()
        };
        config.security.nevis.client_secret = Some(plaintext_secret.into());

        // Save (triggers encryption)
        config.save().await.unwrap();

        // Read raw TOML and verify plaintext secret is NOT present
        let raw_toml = tokio::fs::read_to_string(&config.config_path)
            .await
            .unwrap();
        assert!(
            !raw_toml.contains(plaintext_secret),
            "Saved TOML must not contain the plaintext client_secret"
        );

        // Parse stored TOML and verify the value is encrypted
        let stored: Config = toml::from_str(&raw_toml).unwrap();
        let stored_secret = stored.security.nevis.client_secret.as_ref().unwrap();
        assert!(
            crate::secrets::SecretStore::is_encrypted(stored_secret),
            "Stored client_secret must be marked as encrypted"
        );

        // Decrypt and verify it matches the original plaintext
        let store = crate::secrets::SecretStore::new(&dir, true);
        assert_eq!(store.decrypt(stored_secret).unwrap(), plaintext_secret);

        // Simulate a full load: deserialize then decrypt (mirrors load_or_init logic)
        let mut loaded: Config = toml::from_str(&raw_toml).unwrap();
        loaded.config_path = dir.join("config.toml");
        let load_store = crate::secrets::SecretStore::new(&dir, loaded.secrets.encrypt);
        loaded.decrypt_secrets(&load_store).unwrap();
        assert_eq!(
            loaded.security.nevis.client_secret.as_deref().unwrap(),
            plaintext_secret,
            "Loaded client_secret must match the original plaintext after decryption"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    // ══════════════════════════════════════════════════════════
    // Nevis config validation tests
    // ══════════════════════════════════════════════════════════

    #[test]
    async fn nevis_config_validate_disabled_accepts_empty_fields() {
        let cfg = NevisConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    async fn nevis_config_validate_rejects_empty_instance_url() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: String::new(),
            client_id: "test-client".into(),
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("instance_url"));
    }

    #[test]
    async fn nevis_config_validate_rejects_empty_client_id() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            client_id: String::new(),
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("client_id"));
    }

    #[test]
    async fn nevis_config_validate_rejects_empty_realm() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            client_id: "test-client".into(),
            realm: String::new(),
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("realm"));
    }

    #[test]
    async fn nevis_config_validate_rejects_local_without_jwks() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            client_id: "test-client".into(),
            token_validation: "local".into(),
            jwks_url: None,
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("jwks_url"));
    }

    #[test]
    async fn nevis_config_validate_rejects_zero_session_timeout() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            client_id: "test-client".into(),
            token_validation: "remote".into(),
            session_timeout_secs: 0,
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("session_timeout_secs"));
    }

    #[test]
    async fn nevis_config_validate_accepts_valid_enabled_config() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            realm: "master".into(),
            client_id: "test-client".into(),
            token_validation: "remote".into(),
            session_timeout_secs: 3600,
            ..NevisConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    async fn nevis_config_validate_rejects_invalid_token_validation() {
        let cfg = NevisConfig {
            enabled: true,
            instance_url: "https://nevis.example.com".into(),
            realm: "master".into(),
            client_id: "test-client".into(),
            token_validation: "invalid_mode".into(),
            session_timeout_secs: 3600,
            ..NevisConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(
            err.contains("invalid value 'invalid_mode'"),
            "Expected invalid token_validation error, got: {err}"
        );
    }

    #[test]
    async fn nevis_config_debug_redacts_client_secret() {
        let cfg = NevisConfig {
            client_secret: Some("super-secret".into()),
            ..NevisConfig::default()
        };
        let debug_output = format!("{:?}", cfg);
        assert!(
            !debug_output.contains("super-secret"),
            "Debug output must not contain the raw client_secret"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "Debug output must show [REDACTED] for client_secret"
        );
    }

    #[test]
    async fn telegram_config_ack_reactions_false_deserializes() {
        let toml_str = r#"
            bot_token = "123:ABC"
            allowed_users = ["alice"]
            ack_reactions = false
        "#;
        let cfg: TelegramConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ack_reactions, Some(false));
    }

    #[test]
    async fn telegram_config_ack_reactions_true_deserializes() {
        let toml_str = r#"
            bot_token = "123:ABC"
            allowed_users = ["alice"]
            ack_reactions = true
        "#;
        let cfg: TelegramConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ack_reactions, Some(true));
    }

    #[test]
    async fn telegram_config_ack_reactions_missing_defaults_to_none() {
        let toml_str = r#"
            bot_token = "123:ABC"
            allowed_users = ["alice"]
        "#;
        let cfg: TelegramConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ack_reactions, None);
    }

    #[test]
    async fn telegram_config_ack_reactions_channel_overrides_top_level() {
        let tg_toml = r#"
            bot_token = "123:ABC"
            allowed_users = ["alice"]
            ack_reactions = false
        "#;
        let tg: TelegramConfig = toml::from_str(tg_toml).unwrap();
        let top_level_ack = true;
        let effective = tg.ack_reactions.unwrap_or(top_level_ack);
        assert!(
            !effective,
            "channel-level false must override top-level true"
        );
    }

    #[test]
    async fn telegram_config_ack_reactions_falls_back_to_top_level() {
        let tg_toml = r#"
            bot_token = "123:ABC"
            allowed_users = ["alice"]
        "#;
        let tg: TelegramConfig = toml::from_str(tg_toml).unwrap();
        let top_level_ack = false;
        let effective = tg.ack_reactions.unwrap_or(top_level_ack);
        assert!(
            !effective,
            "must fall back to top-level false when channel omits field"
        );
    }

    #[test]
    async fn google_workspace_allowed_operations_deserialize_from_toml() {
        let toml_str = r#"
            enabled = true

            [[allowed_operations]]
            service = "gmail"
            resource = "users"
            sub_resource = "drafts"
            methods = ["create", "update"]
        "#;

        let cfg: GoogleWorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.allowed_operations.len(), 1);
        assert_eq!(cfg.allowed_operations[0].service, "gmail");
        assert_eq!(cfg.allowed_operations[0].resource, "users");
        assert_eq!(
            cfg.allowed_operations[0].sub_resource.as_deref(),
            Some("drafts")
        );
        assert_eq!(
            cfg.allowed_operations[0].methods,
            vec!["create".to_string(), "update".to_string()]
        );
    }

    #[test]
    async fn google_workspace_allowed_operations_deserialize_without_sub_resource() {
        let toml_str = r#"
            enabled = true

            [[allowed_operations]]
            service = "drive"
            resource = "files"
            methods = ["list", "get"]
        "#;

        let cfg: GoogleWorkspaceConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.allowed_operations[0].sub_resource, None);
    }

    #[test]
    async fn config_validate_accepts_google_workspace_allowed_operations() {
        let mut cfg = Config::default();
        cfg.google_workspace.enabled = true;
        cfg.google_workspace.allowed_services = vec!["gmail".into()];
        cfg.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "gmail".into(),
            resource: "users".into(),
            sub_resource: Some("drafts".into()),
            methods: vec!["create".into(), "update".into()],
        }];

        cfg.validate().unwrap();
    }

    #[test]
    async fn config_validate_rejects_duplicate_google_workspace_allowed_operations() {
        let mut cfg = Config::default();
        cfg.google_workspace.enabled = true;
        cfg.google_workspace.allowed_services = vec!["gmail".into()];
        cfg.google_workspace.allowed_operations = vec![
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("drafts".into()),
                methods: vec!["create".into()],
            },
            GoogleWorkspaceAllowedOperation {
                service: "gmail".into(),
                resource: "users".into(),
                sub_resource: Some("drafts".into()),
                methods: vec!["update".into()],
            },
        ];

        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("duplicate service/resource/sub_resource entry"));
    }

    #[test]
    async fn config_validate_rejects_operation_service_not_in_allowed_services() {
        let mut cfg = Config::default();
        cfg.google_workspace.enabled = true;
        cfg.google_workspace.allowed_services = vec!["gmail".into()];
        cfg.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "drive".into(), // drive is not in allowed_services
            resource: "files".into(),
            sub_resource: None,
            methods: vec!["list".into()],
        }];

        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("not in the effective allowed_services"),
            "expected not-in-allowed_services error, got: {err}"
        );
    }

    #[test]
    async fn config_validate_accepts_default_service_when_allowed_services_empty() {
        // When allowed_services is empty the validator uses DEFAULT_GWS_SERVICES.
        // A known default service must pass.
        let mut cfg = Config::default();
        cfg.google_workspace.enabled = true;
        // allowed_services deliberately left empty (falls back to defaults)
        cfg.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "drive".into(),
            resource: "files".into(),
            sub_resource: None,
            methods: vec!["list".into()],
        }];

        assert!(cfg.validate().is_ok());
    }

    #[test]
    async fn config_validate_rejects_unknown_service_when_allowed_services_empty() {
        // Even with allowed_services empty (using defaults), an operation whose
        // service is not in DEFAULT_GWS_SERVICES must fail validation — not silently
        // pass through to be rejected at runtime.
        let mut cfg = Config::default();
        cfg.google_workspace.enabled = true;
        // allowed_services deliberately left empty
        cfg.google_workspace.allowed_operations = vec![GoogleWorkspaceAllowedOperation {
            service: "not_a_real_service".into(),
            resource: "files".into(),
            sub_resource: None,
            methods: vec!["list".into()],
        }];

        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("not in the effective allowed_services"),
            "expected effective-allowed_services error, got: {err}"
        );
    }

    // ── Bootstrap files ─────────────────────────────────────

    #[tokio::test]
    async fn ensure_bootstrap_files_creates_missing_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("workspace");
        let _: () = tokio::fs::create_dir_all(&ws).await.unwrap();

        ensure_bootstrap_files(&ws).await.unwrap();

        let soul: String = tokio::fs::read_to_string(ws.join("SOUL.md")).await.unwrap();
        let identity: String = tokio::fs::read_to_string(ws.join("IDENTITY.md"))
            .await
            .unwrap();
        assert!(soul.contains("SOUL.md"));
        assert!(identity.contains("IDENTITY.md"));
    }

    #[tokio::test]
    async fn ensure_bootstrap_files_does_not_overwrite_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = tmp.path().join("workspace");
        let _: () = tokio::fs::create_dir_all(&ws).await.unwrap();

        let custom = "# My custom SOUL";
        let _: () = tokio::fs::write(ws.join("SOUL.md"), custom).await.unwrap();

        ensure_bootstrap_files(&ws).await.unwrap();

        let soul: String = tokio::fs::read_to_string(ws.join("SOUL.md")).await.unwrap();
        assert_eq!(
            soul, custom,
            "ensure_bootstrap_files must not overwrite existing files"
        );

        // IDENTITY.md should still be created since it was missing
        let identity: String = tokio::fs::read_to_string(ws.join("IDENTITY.md"))
            .await
            .unwrap();
        assert!(identity.contains("IDENTITY.md"));
    }

    // ── PacingConfig serde defaults ─────────────────────────────

    #[test]
    async fn pacing_config_serde_defaults_match_manual_default() {
        // Deserialise an empty TOML table and verify the loop-detection
        // fields receive the same defaults as `PacingConfig::default()`.
        let from_toml: PacingConfig = toml::from_str("").unwrap();
        let manual = PacingConfig::default();

        assert_eq!(
            from_toml.loop_detection_enabled,
            manual.loop_detection_enabled
        );
        assert_eq!(
            from_toml.loop_detection_window_size,
            manual.loop_detection_window_size
        );
        assert_eq!(
            from_toml.loop_detection_max_repeats,
            manual.loop_detection_max_repeats
        );

        // Verify concrete values so a silent change to the defaults is caught.
        assert!(from_toml.loop_detection_enabled, "default should be true");
        assert_eq!(from_toml.loop_detection_window_size, 20);
        assert_eq!(from_toml.loop_detection_max_repeats, 3);
    }

    // ── Docker baked config template ────────────────────────────

    /// The TOML template baked into Docker images (Dockerfile + Dockerfile.debian).
    /// Kept here so changes to the Dockerfiles can be validated by `cargo test`.
    const DOCKER_CONFIG_TEMPLATE: &str = r#"
workspace_dir = "/zeroclaw-data/workspace"
config_path = "/zeroclaw-data/.zeroclaw/config.toml"
api_key = ""
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4-20250514"
default_temperature = 0.7

[gateway]
port = 42617
host = "[::]"
allow_public_bind = true

[autonomy]
level = "supervised"
auto_approve = ["file_read", "file_write", "file_edit", "memory_recall", "memory_store", "web_search_tool", "web_fetch", "calculator", "glob_search", "content_search", "image_info", "weather", "git_operations"]
"#;

    #[test]
    async fn docker_config_template_is_parseable() {
        let cfg: Config = toml::from_str(DOCKER_CONFIG_TEMPLATE)
            .expect("Docker baked config.toml must be valid TOML that deserialises into Config");

        // The [autonomy] section must be present and contain the expected tools.
        let auto = &cfg.autonomy.auto_approve;
        for tool in &[
            "file_read",
            "file_write",
            "file_edit",
            "memory_recall",
            "memory_store",
            "web_search_tool",
            "web_fetch",
            "calculator",
            "glob_search",
            "content_search",
            "image_info",
            "weather",
            "git_operations",
        ] {
            assert!(
                auto.iter().any(|t| t == tool),
                "Docker config auto_approve missing expected tool: {tool}"
            );
        }
    }

    #[test]
    async fn cost_enforcement_config_defaults() {
        let config = CostEnforcementConfig::default();
        assert_eq!(config.mode, "warn");
        assert_eq!(config.route_down_model, None);
        assert_eq!(config.reserve_percent, 10);
    }

    #[test]
    async fn cost_config_includes_enforcement() {
        let config = CostConfig::default();
        assert_eq!(config.enforcement.mode, "warn");
        assert_eq!(config.enforcement.reserve_percent, 10);
    }

    // ── Configurable macro tests ──

    #[test]
    async fn matrix_secret_fields_discovered() {
        let mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        let fields = mx.secret_fields();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "channels.matrix.access-token");
        assert_eq!(fields[0].category, "Channels");
        assert!(fields[0].is_set);
        assert_eq!(fields[1].name, "channels.matrix.recovery-key");
        assert!(!fields[1].is_set);
        assert_eq!(fields[2].name, "channels.matrix.password");
        assert!(!fields[2].is_set);
    }

    #[test]
    async fn matrix_secret_fields_empty_not_set() {
        let mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: String::new(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        let fields = mx.secret_fields();
        assert!(!fields[0].is_set);
    }

    #[test]
    async fn set_secret_updates_field() {
        let mut mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "old".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        mx.set_secret("channels.matrix.access-token", "new-token".into())
            .unwrap();
        assert_eq!(mx.access_token, "new-token");
    }

    #[test]
    async fn set_secret_unknown_name_fails() {
        let mut mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };
        assert!(
            mx.set_secret("channels.matrix.nonexistent", "val".into())
                .is_err()
        );
    }

    #[test]
    async fn config_tree_traversal_discovers_nested_secrets() {
        let mut config = Config::default();
        // Set api_key on fallback provider
        if let Some(name) = config.providers.fallback.clone() {
            if let Some(entry) = config.providers.models.get_mut(&name) {
                entry.api_key = Some("test-key".into());
            }
        } else {
            config.providers.fallback = Some("default".into());
            config.providers.models.insert(
                "default".into(),
                ModelProviderConfig {
                    api_key: Some("test-key".into()),
                    ..Default::default()
                },
            );
        }
        config.channels.matrix = Some(MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "mx-tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        });

        let fields = config.secret_fields();
        let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
        assert!(names.contains(&"channels.matrix.access-token"));
        assert!(names.contains(&"channels.matrix.recovery-key"));
    }

    #[test]
    async fn config_set_secret_dispatches_to_child() {
        let mut config = Config::default();
        config.channels.matrix = Some(MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "old".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        });

        config
            .set_secret("channels.matrix.access-token", "new".into())
            .unwrap();
        assert_eq!(config.channels.matrix.as_ref().unwrap().access_token, "new");
    }

    #[test]
    async fn config_set_secret_dispatches_to_matrix_child() {
        let mut config = Config::default();
        config.channels.matrix = Some(MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "old".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            mention_only: false,
            recovery_key: None,
            password: None,
        });
        config
            .set_secret("channels.matrix.access-token", "sk-test".into())
            .unwrap();
        assert_eq!(
            config.channels.matrix.as_ref().unwrap().access_token,
            "sk-test"
        );
    }

    #[test]
    async fn config_set_secret_unknown_fails() {
        let mut config = Config::default();
        assert!(
            config
                .set_secret("nonexistent.field", "val".into())
                .is_err()
        );
    }

    #[test]
    async fn encrypt_decrypt_roundtrip_via_macro() {
        let dir = TempDir::new().unwrap();
        let store = crate::secrets::SecretStore::new(dir.path(), true);

        let mut mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "plaintext-token".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };

        // Encrypt
        mx.encrypt_secrets(&store).unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(&mx.access_token));
        assert_ne!(mx.access_token, "plaintext-token");

        // Decrypt
        mx.decrypt_secrets(&store).unwrap();
        assert_eq!(mx.access_token, "plaintext-token");
    }

    #[test]
    async fn encrypt_skips_already_encrypted() {
        let dir = TempDir::new().unwrap();
        let store = crate::secrets::SecretStore::new(dir.path(), true);

        let mut mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "plaintext-token".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };

        mx.encrypt_secrets(&store).unwrap();
        let first_encrypted = mx.access_token.clone();

        // Encrypt again — should be idempotent
        mx.encrypt_secrets(&store).unwrap();
        assert_eq!(mx.access_token, first_encrypted);
    }

    #[test]
    async fn encrypt_no_op_on_disabled_store() {
        let dir = TempDir::new().unwrap();
        let store = crate::secrets::SecretStore::new(dir.path(), false);

        let mut mx = MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "plaintext-token".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        };

        mx.encrypt_secrets(&store).unwrap();
        // With encryption disabled, value should stay plaintext
        assert_eq!(mx.access_token, "plaintext-token");
    }

    // ── Property method tests ──

    fn test_matrix_config() -> MatrixConfig {
        MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: Some("@bot:m.org".into()),
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            mention_only: false,
            password: None,
        }
    }

    #[test]
    async fn prop_fields_returns_typed_entries() {
        let mx = test_matrix_config();
        let fields = mx.prop_fields();
        let by_name: std::collections::HashMap<&str, &crate::traits::PropFieldInfo> =
            fields.iter().map(|f| (f.name, f)).collect();

        // Bool field
        let enabled = by_name["channels.matrix.enabled"];
        assert_eq!(enabled.type_hint, "bool");
        assert_eq!(enabled.display_value, "true");
        assert!(!enabled.is_secret);
        assert!(!enabled.is_enum());

        // String field
        let homeserver = by_name["channels.matrix.homeserver"];
        assert_eq!(homeserver.type_hint, "String");
        assert_eq!(homeserver.display_value, "https://m.org");

        // Option<String> — set
        let user_id = by_name["channels.matrix.user-id"];
        assert_eq!(user_id.type_hint, "Option<String>");
        assert_eq!(user_id.display_value, "@bot:m.org");

        // Option<String> — unset
        let device_id = by_name["channels.matrix.device-id"];
        assert_eq!(device_id.display_value, "<unset>");

        // u64 field
        let interval = by_name["channels.matrix.draft-update-interval-ms"];
        assert_eq!(interval.type_hint, "u64");
        assert_eq!(interval.display_value, "1500");

        // Enum field
        let stream = by_name["channels.matrix.stream-mode"];
        assert!(stream.is_enum());
        assert!(stream.enum_variants.is_some());

        // Secret field — masked
        let token = by_name["channels.matrix.access-token"];
        assert!(token.is_secret);
        assert_eq!(token.display_value, "****");

        // All fields have correct category
        for field in &fields {
            assert_eq!(field.category, "Channels");
        }
    }

    #[test]
    async fn get_prop_returns_values_by_path() {
        let mx = test_matrix_config();

        assert_eq!(
            mx.get_prop("channels.matrix.homeserver").unwrap(),
            "https://m.org"
        );
        assert_eq!(mx.get_prop("channels.matrix.enabled").unwrap(), "true");
        assert_eq!(
            mx.get_prop("channels.matrix.draft-update-interval-ms")
                .unwrap(),
            "1500"
        );
        assert_eq!(
            mx.get_prop("channels.matrix.user-id").unwrap(),
            "@bot:m.org"
        );
        assert_eq!(mx.get_prop("channels.matrix.device-id").unwrap(), "<unset>");
        // Secrets return masked value
        assert_eq!(
            mx.get_prop("channels.matrix.access-token").unwrap(),
            "**** (encrypted)"
        );
    }

    #[test]
    async fn get_prop_unknown_path_fails() {
        let mx = test_matrix_config();
        assert!(mx.get_prop("channels.matrix.nonexistent").is_err());
    }

    #[test]
    async fn set_prop_string() {
        let mut mx = test_matrix_config();
        mx.set_prop("channels.matrix.homeserver", "https://new.org")
            .unwrap();
        assert_eq!(mx.homeserver, "https://new.org");
    }

    #[test]
    async fn set_prop_bool() {
        let mut mx = test_matrix_config();
        mx.set_prop("channels.matrix.interrupt-on-new-message", "true")
            .unwrap();
        assert!(mx.interrupt_on_new_message);
    }

    #[test]
    async fn set_prop_bool_rejects_invalid() {
        let mut mx = test_matrix_config();
        let err = mx.set_prop("channels.matrix.enabled", "yes").unwrap_err();
        assert!(err.to_string().contains("bool"));
    }

    #[test]
    async fn set_prop_u64() {
        let mut mx = test_matrix_config();
        mx.set_prop("channels.matrix.draft-update-interval-ms", "3000")
            .unwrap();
        assert_eq!(mx.draft_update_interval_ms, 3000);
    }

    #[test]
    async fn set_prop_u64_rejects_invalid() {
        let mut mx = test_matrix_config();
        assert!(
            mx.set_prop("channels.matrix.draft-update-interval-ms", "abc")
                .is_err()
        );
    }

    #[test]
    async fn set_prop_option_string_set_and_clear() {
        let mut mx = test_matrix_config();
        mx.set_prop("channels.matrix.user-id", "@new:m.org")
            .unwrap();
        assert_eq!(mx.user_id.as_deref(), Some("@new:m.org"));

        // Empty string clears Option
        mx.set_prop("channels.matrix.user-id", "").unwrap();
        assert!(mx.user_id.is_none());
    }

    #[test]
    async fn set_prop_enum() {
        let mut mx = test_matrix_config();
        mx.set_prop("channels.matrix.stream-mode", "partial")
            .unwrap();
        assert_eq!(mx.stream_mode, StreamMode::Partial);

        mx.set_prop("channels.matrix.stream-mode", "multi_message")
            .unwrap();
        assert_eq!(mx.stream_mode, StreamMode::MultiMessage);
    }

    #[test]
    async fn set_prop_enum_rejects_invalid() {
        let mut mx = test_matrix_config();
        let err = mx
            .set_prop("channels.matrix.stream-mode", "invalid")
            .unwrap_err();
        assert!(err.to_string().contains("expected one of"));
    }

    #[test]
    async fn set_prop_unknown_path_fails() {
        let mut mx = test_matrix_config();
        assert!(mx.set_prop("channels.matrix.nonexistent", "val").is_err());
    }

    #[test]
    async fn prop_is_secret_static_check() {
        assert!(MatrixConfig::prop_is_secret("channels.matrix.access-token"));
        assert!(MatrixConfig::prop_is_secret("channels.matrix.recovery-key"));
        assert!(!MatrixConfig::prop_is_secret("channels.matrix.homeserver"));
        assert!(!MatrixConfig::prop_is_secret("channels.matrix.enabled"));
    }

    #[test]
    async fn enum_variants_callback_returns_values() {
        let mx = test_matrix_config();
        let fields = mx.prop_fields();
        let stream_field = fields
            .iter()
            .find(|f| f.name == "channels.matrix.stream-mode")
            .unwrap();
        let variants = (stream_field.enum_variants.unwrap())();
        assert!(variants.contains(&"off".to_string()));
        assert!(variants.contains(&"partial".to_string()));
        assert!(variants.contains(&"multi_message".to_string()));
    }

    #[test]
    async fn init_defaults_instantiates_none_sections() {
        let mut config = Config::default();
        assert!(config.channels.matrix.is_none());

        let initialized = config.init_defaults(Some("channels.matrix"));
        assert!(initialized.contains(&"channels.matrix"));
        assert!(config.channels.matrix.is_some());
    }

    #[test]
    async fn init_defaults_skips_already_set() {
        let mut config = Config::default();
        config.channels.matrix = Some(test_matrix_config());

        let initialized = config.init_defaults(Some("channels.matrix"));
        // Already set — should not re-initialize
        assert!(!initialized.contains(&"channels.matrix"));
        // Original value preserved
        assert_eq!(
            config.channels.matrix.as_ref().unwrap().homeserver,
            "https://m.org"
        );
    }

    #[test]
    async fn nested_get_set_prop_traverses_config_tree() {
        let mut config = Config::default();
        config.channels.matrix = Some(test_matrix_config());

        // get_prop traverses Config → ChannelsConfig → MatrixConfig
        assert_eq!(
            config.get_prop("channels.matrix.homeserver").unwrap(),
            "https://m.org"
        );

        // set_prop traverses the same path
        config
            .set_prop("channels.matrix.homeserver", "https://new.org")
            .unwrap();
        assert_eq!(
            config.channels.matrix.as_ref().unwrap().homeserver,
            "https://new.org"
        );
    }

    #[test]
    async fn hashmap_nested_encrypt_decrypt_traverses_values() {
        let dir = TempDir::new().unwrap();
        let store = crate::secrets::SecretStore::new(dir.path(), true);

        let mut config = Config::default();
        config.agents.insert(
            "test-agent".into(),
            DelegateAgentConfig {
                api_key: Some("secret-key".into()),
                ..Default::default()
            },
        );

        config.encrypt_secrets(&store).unwrap();
        let encrypted_key = config.agents["test-agent"].api_key.as_ref().unwrap();
        assert!(crate::secrets::SecretStore::is_encrypted(encrypted_key));

        config.decrypt_secrets(&store).unwrap();
        assert_eq!(
            config.agents["test-agent"].api_key.as_deref(),
            Some("secret-key")
        );
    }

    #[test]
    async fn vec_secret_encrypt_decrypt_traverses_elements() {
        let dir = TempDir::new().unwrap();
        let store = crate::secrets::SecretStore::new(dir.path(), true);

        let mut config = Config::default();
        config.gateway.paired_tokens = vec!["token-a".into(), "token-b".into()];

        config.encrypt_secrets(&store).unwrap();
        for token in &config.gateway.paired_tokens {
            assert!(crate::secrets::SecretStore::is_encrypted(token));
        }

        config.decrypt_secrets(&store).unwrap();
        assert_eq!(config.gateway.paired_tokens, vec!["token-a", "token-b"]);
    }

    /// Walk every property on a default Config: get_prop must succeed,
    /// and set_prop must round-trip for non-secret, non-enum scalar fields.
    #[test]
    async fn every_prop_is_gettable_and_settable() {
        let mut config = Config::default();
        // Initialize all Option<T> sections so their fields are reachable
        config.init_defaults(None);

        let fields = config.prop_fields();
        assert!(
            fields.len() > 50,
            "Expected 50+ props, got {} — macro may be skipping fields",
            fields.len()
        );

        for field in &fields {
            // get_prop must not panic or error
            let get_result = config.get_prop(field.name);
            assert!(
                get_result.is_ok(),
                "get_prop failed for '{}': {}",
                field.name,
                get_result.unwrap_err()
            );

            // set_prop: round-trip the display value back through set_prop.
            // Skip secrets (masked), enums (need valid variant), and <unset> Options.
            if field.is_secret || field.is_enum() || field.display_value == "<unset>" {
                continue;
            }

            let set_result = config.set_prop(field.name, &field.display_value);
            assert!(
                set_result.is_ok(),
                "set_prop failed for '{}' with value '{}': {}",
                field.name,
                field.display_value,
                set_result.unwrap_err()
            );

            // Value should survive the round-trip
            let after = config.get_prop(field.name).unwrap();
            assert_eq!(
                after, field.display_value,
                "round-trip mismatch for '{}': set '{}', got '{}'",
                field.name, field.display_value, after
            );
        }
    }

    /// Every enum field must have a working enum_variants callback, and
    /// set_prop must accept each variant it advertises.
    #[test]
    async fn every_enum_variant_is_settable() {
        let mut config = Config::default();
        config.init_defaults(None);

        for field in config.prop_fields() {
            if !field.is_enum() {
                continue;
            }
            let get_variants = field.enum_variants.unwrap_or_else(|| {
                panic!("enum field '{}' has no enum_variants callback", field.name)
            });
            let variants = get_variants();
            assert!(
                !variants.is_empty(),
                "enum field '{}' returned no variants",
                field.name
            );

            for variant in &variants {
                let result = config.set_prop(field.name, variant);
                assert!(
                    result.is_ok(),
                    "set_prop('{}', '{}') failed: {}",
                    field.name,
                    variant,
                    result.unwrap_err()
                );
            }
        }
    }

    #[test]
    async fn backfill_enabled_activates_channel_without_explicit_enabled() {
        let toml = r#"
[channels.matrix]
homeserver = "https://matrix.org"
access_token = "tok"
allowed_rooms = ["!r:m"]
allowed_users = ["@u:m"]
"#;
        let mut config: Config = toml::from_str(toml).unwrap();
        assert!(!config.channels.matrix.as_ref().unwrap().enabled);

        config.channels.backfill_enabled(toml);
        assert!(config.channels.matrix.as_ref().unwrap().enabled);
    }

    #[test]
    async fn backfill_enabled_respects_explicit_false() {
        let toml = r#"
[channels.matrix]
homeserver = "https://matrix.org"
access_token = "tok"
allowed_rooms = ["!r:m"]
allowed_users = ["@u:m"]
enabled = false
"#;
        let mut config: Config = toml::from_str(toml).unwrap();
        config.channels.backfill_enabled(toml);
        assert!(
            !config.channels.matrix.as_ref().unwrap().enabled,
            "explicit enabled=false must not be overwritten"
        );
    }

    #[test]
    async fn backfill_enabled_no_op_when_section_absent() {
        let toml = r#"
api_key = "sk-test"
"#;
        let mut config: Config = toml::from_str(toml).unwrap();
        config.channels.backfill_enabled(toml);
        assert!(config.channels.telegram.is_none());
    }

    #[test]
    async fn backfill_enabled_works_with_toml_comments() {
        let toml = r#"
# My matrix setup
[channels.matrix]
homeserver = "https://matrix.org"  # production server
access_token = "tok"
allowed_rooms = ["!r:m"]
allowed_users = ["@u:m"]
# enabled intentionally omitted
"#;
        let mut config: Config = toml::from_str(toml).unwrap();
        assert!(!config.channels.matrix.as_ref().unwrap().enabled);

        config.channels.backfill_enabled(toml);
        assert!(
            config.channels.matrix.as_ref().unwrap().enabled,
            "backfill should activate channel even when config has comments"
        );
    }

    // NOTE: SkillForgeConfig + SkillConfidenceConfig tests live in
    // `crates/zeroclaw-config/tests/skill_configs.rs` as integration tests.
    // The inline `tests` module here has a pre-existing compile issue (it
    // uses `tokio-stream/fs` features that are not enabled), so we cannot
    // add new tests to it until that is fixed. The standalone integration
    // test file compiles independently.
}
