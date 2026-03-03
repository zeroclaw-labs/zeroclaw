use crate::config::traits::ChannelConfig;
use crate::providers::{
    canonical_china_provider_name, is_glm_alias, is_qwen_oauth_alias, is_zai_alias,
};
use crate::security::{AutonomyLevel, DomainMatcher};
use anyhow::{Context, Result};
use directories::UserDirs;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
#[cfg(unix)]
use tokio::fs::File;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;

/// Default fallback model when none is configured. Uses a format compatible with
/// OpenRouter and other multi-provider gateways. For Anthropic direct API, this
/// model ID will be normalized by the provider layer.
pub const DEFAULT_MODEL_FALLBACK: &str = "anthropic/claude-sonnet-4.6";

fn canonical_provider_for_model_defaults(provider_name: &str) -> String {
    if let Some(canonical) = canonical_china_provider_name(provider_name) {
        return if canonical == "doubao" {
            "volcengine".to_string()
        } else {
            canonical.to_string()
        };
    }

    match provider_name {
        "grok" => "xai".to_string(),
        "together" => "together-ai".to_string(),
        "google" | "google-gemini" => "gemini".to_string(),
        "github-copilot" => "copilot".to_string(),
        "openai_codex" | "codex" => "openai-codex".to_string(),
        "kimi_coding" | "kimi_for_coding" => "kimi-code".to_string(),
        "nvidia-nim" | "build.nvidia.com" => "nvidia".to_string(),
        "aws-bedrock" => "bedrock".to_string(),
        "llama.cpp" => "llamacpp".to_string(),
        _ => provider_name.to_string(),
    }
}

/// Returns a provider-aware fallback model ID when `default_model` is missing.
pub fn default_model_fallback_for_provider(provider_name: Option<&str>) -> &'static str {
    let normalized_provider = provider_name
        .unwrap_or("openrouter")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");

    if normalized_provider == "qwen-coding-plan" {
        return "qwen3-coder-plus";
    }

    let canonical_provider = if is_qwen_oauth_alias(&normalized_provider) {
        "qwen-code".to_string()
    } else {
        canonical_provider_for_model_defaults(&normalized_provider)
    };

    match canonical_provider.as_str() {
        "anthropic" => "claude-sonnet-4-5-20250929",
        "openai" => "gpt-5.2",
        "openai-codex" => "gpt-5-codex",
        "venice" => "zai-org-glm-5",
        "groq" => "llama-3.3-70b-versatile",
        "mistral" => "mistral-large-latest",
        "deepseek" => "deepseek-chat",
        "xai" => "grok-4-1-fast-reasoning",
        "perplexity" => "sonar-pro",
        "fireworks" => "accounts/fireworks/models/llama-v3p3-70b-instruct",
        "novita" => "minimax/minimax-m2.5",
        "together-ai" => "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        "cohere" => "command-a-03-2025",
        "moonshot" => "kimi-k2.5",
        "stepfun" => "step-3.5-flash",
        "hunyuan" => "hunyuan-t1-latest",
        "glm" | "zai" => "glm-5",
        "minimax" => "MiniMax-M2.5",
        "qwen" => "qwen-plus",
        "volcengine" => "doubao-1-5-pro-32k-250115",
        "siliconflow" => "Pro/zai-org/GLM-4.7",
        "qwen-code" => "qwen3-coder-plus",
        "ollama" => "llama3.2",
        "llamacpp" => "ggml-org/gpt-oss-20b-GGUF",
        "sglang" | "vllm" | "osaurus" | "copilot" => "default",
        "gemini" => "gemini-2.5-pro",
        "kimi-code" => "kimi-for-coding",
        "bedrock" => "anthropic.claude-sonnet-4-5-20250929-v1:0",
        "nvidia" => "meta/llama-3.3-70b-instruct",
        _ => DEFAULT_MODEL_FALLBACK,
    }
}

/// Resolves the model ID used by runtime components.
/// Preference order:
/// 1) Explicit configured model (if non-empty)
/// 2) Provider-aware fallback
pub fn resolve_default_model_id(
    default_model: Option<&str>,
    provider_name: Option<&str>,
) -> String {
    if let Some(model) = default_model.map(str::trim).filter(|m| !m.is_empty()) {
        return model.to_string();
    }

    default_model_fallback_for_provider(provider_name).to_string()
}

const SUPPORTED_PROXY_SERVICE_KEYS: &[&str] = &[
    "provider.anthropic",
    "provider.compatible",
    "provider.copilot",
    "provider.gemini",
    "provider.glm",
    "provider.ollama",
    "provider.openai",
    "provider.openrouter",
    "channel.bluebubbles",
    "channel.dingtalk",
    "channel.discord",
    "channel.feishu",
    "channel.github",
    "channel.lark",
    "channel.matrix",
    "channel.mattermost",
    "channel.nextcloud_talk",
    "channel.napcat",
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
const DEFAULT_PROVIDER_NAME: &str = "openrouter";
const DEFAULT_MODEL_NAME: &str = "anthropic/claude-sonnet-4.6";

// ── Top-level config ──────────────────────────────────────────────

/// Protocol mode for `custom:` OpenAI-compatible providers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApiMode {
    /// Default behavior: `/chat/completions` first, optional `/responses`
    /// fallback when supported.
    OpenAiChatCompletions,
    /// Responses-first behavior: call `/responses` directly.
    OpenAiResponses,
}

impl ProviderApiMode {
    pub fn as_compatible_mode(self) -> crate::providers::compatible::CompatibleApiMode {
        match self {
            Self::OpenAiChatCompletions => {
                crate::providers::compatible::CompatibleApiMode::OpenAiChatCompletions
            }
            Self::OpenAiResponses => {
                crate::providers::compatible::CompatibleApiMode::OpenAiResponses
            }
        }
    }
}

/// Top-level ZeroClaw configuration, loaded from `config.toml`.
///
/// Resolution order: `ZEROCLAW_WORKSPACE` env → `active_workspace.toml` marker → `~/.zeroclaw/config.toml`.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct Config {
    /// Workspace directory - computed from home, not serialized
    #[serde(skip)]
    pub workspace_dir: PathBuf,
    /// Path to config.toml - computed from home, not serialized
    #[serde(skip)]
    pub config_path: PathBuf,
    /// API key for the selected provider. Always overridden by `ZEROCLAW_API_KEY` env var.
    /// `API_KEY` env var is only used as fallback when no config key is set.
    pub api_key: Option<String>,
    /// Base URL override for provider API (e.g. "http://10.0.0.1:11434" for remote Ollama)
    pub api_url: Option<String>,
    /// Default provider ID or alias (e.g. `"openrouter"`, `"ollama"`, `"anthropic"`). Default: `"openrouter"`.
    #[serde(alias = "model_provider")]
    pub default_provider: Option<String>,
    /// Optional API protocol mode for `custom:` providers.
    #[serde(default)]
    pub provider_api: Option<ProviderApiMode>,
    /// Default model routed through the selected provider (e.g. `"anthropic/claude-sonnet-4-6"`).
    #[serde(alias = "model")]
    pub default_model: Option<String>,
    /// Optional named provider profiles keyed by id (Codex app-server compatible layout).
    #[serde(default)]
    pub model_providers: HashMap<String, ModelProviderConfig>,
    /// Provider-specific behavior overrides (`[provider]`).
    #[serde(default)]
    pub provider: ProviderConfig,
    /// Default model temperature (0.0–2.0). Default: `0.7`.
    pub default_temperature: f64,

    /// Observability backend configuration (`[observability]`).
    #[serde(default)]
    pub observability: ObservabilityConfig,

    /// Autonomy and security policy configuration (`[autonomy]`).
    #[serde(default)]
    pub autonomy: AutonomyConfig,

    /// Security subsystem configuration (`[security]`).
    #[serde(default)]
    pub security: SecurityConfig,

    /// Runtime adapter configuration (`[runtime]`). Controls native vs Docker execution.
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Research phase configuration (`[research]`). Proactive information gathering.
    #[serde(default)]
    pub research: ResearchPhaseConfig,

    /// Reliability settings: retries, fallback providers, backoff (`[reliability]`).
    #[serde(default)]
    pub reliability: ReliabilityConfig,

    /// Scheduler configuration for periodic task execution (`[scheduler]`).
    #[serde(default)]
    pub scheduler: SchedulerConfig,

    /// Agent orchestration settings (`[agent]`).
    #[serde(default)]
    pub agent: AgentConfig,

    /// Skills loading and community repository behavior (`[skills]`).
    #[serde(default)]
    pub skills: SkillsConfig,

    /// Model routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default)]
    pub model_routes: Vec<ModelRouteConfig>,

    /// Embedding routing rules — route `hint:<name>` to specific provider+model combos.
    #[serde(default)]
    pub embedding_routes: Vec<EmbeddingRouteConfig>,

    /// Automatic query classification — maps user messages to model hints.
    #[serde(default)]
    pub query_classification: QueryClassificationConfig,

    /// Heartbeat configuration for periodic health pings (`[heartbeat]`).
    #[serde(default)]
    pub heartbeat: HeartbeatConfig,

    /// Cron job configuration (`[cron]`).
    #[serde(default)]
    pub cron: CronConfig,

    /// Goal loop configuration for autonomous long-term goal execution (`[goal_loop]`).
    #[serde(default)]
    pub goal_loop: GoalLoopConfig,

    /// Channel configurations: Telegram, Discord, Slack, etc. (`[channels_config]`).
    #[serde(default)]
    pub channels_config: ChannelsConfig,

    /// Memory backend configuration: sqlite, markdown, embeddings (`[memory]`).
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Persistent storage provider configuration (`[storage]`).
    #[serde(default)]
    pub storage: StorageConfig,

    /// Tunnel configuration for exposing the gateway publicly (`[tunnel]`).
    #[serde(default)]
    pub tunnel: TunnelConfig,

    /// Gateway server configuration: host, port, pairing, rate limits (`[gateway]`).
    #[serde(default)]
    pub gateway: GatewayConfig,

    /// Composio managed OAuth tools integration (`[composio]`).
    #[serde(default)]
    pub composio: ComposioConfig,

    /// Secrets encryption configuration (`[secrets]`).
    #[serde(default)]
    pub secrets: SecretsConfig,

    /// Browser automation configuration (`[browser]`).
    #[serde(default)]
    pub browser: BrowserConfig,

    /// HTTP request tool configuration (`[http_request]`).
    #[serde(default)]
    pub http_request: HttpRequestConfig,

    /// Multimodal (image) handling configuration (`[multimodal]`).
    #[serde(default)]
    pub multimodal: MultimodalConfig,

    /// Web fetch tool configuration (`[web_fetch]`).
    #[serde(default)]
    pub web_fetch: WebFetchConfig,

    /// Web search tool configuration (`[web_search]`).
    #[serde(default)]
    pub web_search: WebSearchConfig,

    /// Proxy configuration for outbound HTTP/HTTPS/SOCKS5 traffic (`[proxy]`).
    #[serde(default)]
    pub proxy: ProxyConfig,

    /// Identity format configuration: OpenClaw or AIEOS (`[identity]`).
    #[serde(default)]
    pub identity: IdentityConfig,

    /// Cost tracking and budget enforcement configuration (`[cost]`).
    #[serde(default)]
    pub cost: CostConfig,

    /// Economic agent survival tracking (`[economic]`).
    /// Tracks balance, token costs, work income, and survival status.
    #[serde(default)]
    pub economic: EconomicConfig,

    /// Peripheral board configuration for hardware integration (`[peripherals]`).
    #[serde(default)]
    pub peripherals: PeripheralsConfig,

    /// Delegate agent configurations for multi-agent workflows.
    #[serde(default)]
    pub agents: HashMap<String, DelegateAgentConfig>,

    /// Delegate coordination runtime configuration (`[coordination]`).
    #[serde(default)]
    pub coordination: CoordinationConfig,

    /// Hooks configuration (lifecycle hooks and built-in hook toggles).
    #[serde(default)]
    pub hooks: HooksConfig,

    /// Plugin system configuration (discovery, loading, per-plugin config).
    #[serde(default)]
    pub plugins: PluginsConfig,

    /// Hardware configuration (wizard-driven physical world setup).
    #[serde(default)]
    pub hardware: HardwareConfig,

    /// Voice transcription configuration (Whisper API via Groq).
    #[serde(default)]
    pub transcription: TranscriptionConfig,

    /// Inter-process agent communication (`[agents_ipc]`).
    #[serde(default)]
    pub agents_ipc: AgentsIpcConfig,

    /// External MCP server connections (`[mcp]`).
    #[serde(default, alias = "mcpServers")]
    pub mcp: McpConfig,

    /// Vision support override for the active provider/model.
    /// - `None` (default): use provider's built-in default
    /// - `Some(true)`: force vision support on (e.g. Ollama running llava)
    /// - `Some(false)`: force vision support off
    #[serde(default)]
    pub model_support_vision: Option<bool>,

    /// WASM plugin engine configuration (`[wasm]` section).
    #[serde(default)]
    pub wasm: WasmConfig,
}

/// Named provider profile definition compatible with Codex app-server style config.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ModelProviderConfig {
    /// Optional provider type/name override (e.g. "openai", "openai-codex", or custom profile id).
    #[serde(default)]
    pub name: Option<String>,
    /// Optional base URL for OpenAI-compatible endpoints.
    #[serde(default)]
    pub base_url: Option<String>,
    /// Provider protocol variant ("responses" or "chat_completions").
    #[serde(default)]
    pub wire_api: Option<String>,
    /// Optional profile-scoped default model.
    #[serde(default, alias = "model")]
    pub default_model: Option<String>,
    /// Optional profile-scoped API key.
    #[serde(default)]
    pub api_key: Option<String>,
    /// If true, load OpenAI auth material (OPENAI_API_KEY or ~/.codex/auth.json).
    #[serde(default)]
    pub requires_openai_auth: bool,
}

/// Provider behavior overrides (`[provider]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ProviderConfig {
    /// Optional reasoning level override for providers that support explicit levels
    /// (e.g. OpenAI Codex `/responses` reasoning effort).
    #[serde(default)]
    pub reasoning_level: Option<String>,
    /// Optional transport override for providers that support multiple transports.
    /// Supported values: "auto", "websocket", "sse".
    ///
    /// Resolution order:
    /// 1) `model_routes[].transport` (route-specific)
    /// 2) env overrides (`PROVIDER_TRANSPORT`, `ZEROCLAW_PROVIDER_TRANSPORT`, `ZEROCLAW_CODEX_TRANSPORT`)
    /// 3) `provider.transport`
    /// 4) runtime default (`auto`, WebSocket-first with SSE fallback for OpenAI Codex)
    ///
    /// Note: env overrides replace configured `provider.transport` when set.
    ///
    /// Existing configs that omit `provider.transport` remain valid and fall back to defaults.
    #[serde(default)]
    pub transport: Option<String>,
}

// ── Delegate Agents ──────────────────────────────────────────────

/// Configuration for a delegate sub-agent used by the `delegate` tool.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
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
}

fn default_max_depth() -> u32 {
    3
}

fn default_max_tool_iterations() -> usize {
    10
}

impl std::fmt::Debug for DelegateAgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegateAgentConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("system_prompt", &self.system_prompt)
            .field("api_key_configured", &self.api_key.is_some())
            .field("temperature", &self.temperature)
            .field("max_depth", &self.max_depth)
            .field("agentic", &self.agentic)
            .field("allowed_tools", &self.allowed_tools)
            .field("max_iterations", &self.max_iterations)
            .finish()
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let model_provider_ids: Vec<&str> =
            self.model_providers.keys().map(String::as_str).collect();
        let delegate_agent_ids: Vec<&str> = self.agents.keys().map(String::as_str).collect();
        let enabled_channel_count = [
            self.channels_config.telegram.is_some(),
            self.channels_config.discord.is_some(),
            self.channels_config.slack.is_some(),
            self.channels_config.mattermost.is_some(),
            self.channels_config.webhook.is_some(),
            self.channels_config.imessage.is_some(),
            self.channels_config.matrix.is_some(),
            self.channels_config.signal.is_some(),
            self.channels_config.whatsapp.is_some(),
            self.channels_config.linq.is_some(),
            self.channels_config.github.is_some(),
            self.channels_config.bluebubbles.is_some(),
            self.channels_config.wati.is_some(),
            self.channels_config.nextcloud_talk.is_some(),
            self.channels_config.email.is_some(),
            self.channels_config.irc.is_some(),
            self.channels_config.lark.is_some(),
            self.channels_config.feishu.is_some(),
            self.channels_config.dingtalk.is_some(),
            self.channels_config.napcat.is_some(),
            self.channels_config.qq.is_some(),
            self.channels_config.acp.is_some(),
            self.channels_config.nostr.is_some(),
            self.channels_config.clawdtalk.is_some(),
        ]
        .into_iter()
        .filter(|enabled| *enabled)
        .count();

        f.debug_struct("Config")
            .field("workspace_dir", &self.workspace_dir)
            .field("config_path", &self.config_path)
            .field("api_key_configured", &self.api_key.is_some())
            .field("api_url_configured", &self.api_url.is_some())
            .field("default_provider", &self.default_provider)
            .field("provider_api", &self.provider_api)
            .field("default_model", &self.default_model)
            .field("model_providers", &model_provider_ids)
            .field("default_temperature", &self.default_temperature)
            .field("model_routes_count", &self.model_routes.len())
            .field("embedding_routes_count", &self.embedding_routes.len())
            .field("delegate_agents", &delegate_agent_ids)
            .field("cli_channel_enabled", &self.channels_config.cli)
            .field("enabled_channels_count", &enabled_channel_count)
            .field("sensitive_sections", &"***REDACTED***")
            .finish_non_exhaustive()
    }
}

// ── Hardware Config (wizard-driven) ─────────────────────────────

/// Hardware transport mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// Voice transcription configuration (Whisper API via Groq).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TranscriptionConfig {
    /// Enable voice transcription for channels that support it.
    #[serde(default)]
    pub enabled: bool,
    /// API key used for transcription requests.
    ///
    /// If unset, runtime falls back to `GROQ_API_KEY` for backward compatibility.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whisper API endpoint URL.
    #[serde(default = "default_transcription_api_url")]
    pub api_url: String,
    /// Whisper model name.
    #[serde(default = "default_transcription_model")]
    pub model: String,
    /// Optional language hint (ISO-639-1, e.g. "en", "ru").
    #[serde(default)]
    pub language: Option<String>,
    /// Maximum voice duration in seconds (messages longer than this are skipped).
    #[serde(default = "default_transcription_max_duration_secs")]
    pub max_duration_secs: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: None,
            api_url: default_transcription_api_url(),
            model: default_transcription_model(),
            language: None,
            max_duration_secs: default_transcription_max_duration_secs(),
        }
    }
}

// ── MCP ─────────────────────────────────────────────────────────

/// Transport type for MCP server connections.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct McpConfig {
    /// Enable MCP tool loading.
    #[serde(default)]
    pub enabled: bool,
    /// Configured MCP servers.
    #[serde(default, alias = "mcpServers")]
    pub servers: Vec<McpServerConfig>,
}

// ── Agents IPC ──────────────────────────────────────────────────

fn default_agents_ipc_db_path() -> String {
    "~/.zeroclaw/agents.db".into()
}

fn default_agents_ipc_staleness_secs() -> u64 {
    300
}

/// Inter-process agent communication configuration (`[agents_ipc]` section).
///
/// When enabled, registers IPC tools that let independent ZeroClaw processes
/// on the same host discover each other and exchange messages via a shared
/// SQLite database. Disabled by default (zero overhead when off).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentsIpcConfig {
    /// Enable inter-process agent communication tools.
    #[serde(default)]
    pub enabled: bool,
    /// Path to shared SQLite database (all agents on this host share one file).
    #[serde(default = "default_agents_ipc_db_path")]
    pub db_path: String,
    /// Agents not seen within this window are considered offline (seconds).
    #[serde(default = "default_agents_ipc_staleness_secs")]
    pub staleness_secs: u64,
}

impl Default for AgentsIpcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            db_path: default_agents_ipc_db_path(),
            staleness_secs: default_agents_ipc_staleness_secs(),
        }
    }
}

fn default_coordination_enabled() -> bool {
    true
}

fn default_coordination_lead_agent() -> String {
    "delegate-lead".into()
}

fn default_coordination_max_inbox_messages_per_agent() -> usize {
    256
}

fn default_coordination_max_dead_letters() -> usize {
    256
}

fn default_coordination_max_context_entries() -> usize {
    512
}

fn default_coordination_max_seen_message_ids() -> usize {
    4096
}

/// Delegate coordination runtime configuration (`[coordination]` section).
///
/// Controls typed delegate message-bus integration used by `delegate` and
/// `delegate_coordination_status` tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CoordinationConfig {
    /// Enable delegate coordination tracing/runtime bus integration.
    #[serde(default = "default_coordination_enabled")]
    pub enabled: bool,
    /// Logical lead-agent identity used as coordinator sender/recipient.
    #[serde(default = "default_coordination_lead_agent")]
    pub lead_agent: String,
    /// Maximum retained inbox messages per registered agent.
    #[serde(default = "default_coordination_max_inbox_messages_per_agent")]
    pub max_inbox_messages_per_agent: usize,
    /// Maximum retained dead-letter entries.
    #[serde(default = "default_coordination_max_dead_letters")]
    pub max_dead_letters: usize,
    /// Maximum retained shared-context entries (`ContextPatch` state keys).
    #[serde(default = "default_coordination_max_context_entries")]
    pub max_context_entries: usize,
    /// Maximum retained dedupe window size for processed message IDs.
    #[serde(default = "default_coordination_max_seen_message_ids")]
    pub max_seen_message_ids: usize,
}

impl Default for CoordinationConfig {
    fn default() -> Self {
        Self {
            enabled: default_coordination_enabled(),
            lead_agent: default_coordination_lead_agent(),
            max_inbox_messages_per_agent: default_coordination_max_inbox_messages_per_agent(),
            max_dead_letters: default_coordination_max_dead_letters(),
            max_context_entries: default_coordination_max_context_entries(),
            max_seen_message_ids: default_coordination_max_seen_message_ids(),
        }
    }
}

/// Agent orchestration configuration (`[agent]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentConfig {
    /// When true: bootstrap_max_chars=6000, rag_chunk_limit=2. Use for 13B or smaller models.
    #[serde(default)]
    pub compact_context: bool,
    #[serde(default)]
    pub session: AgentSessionConfig,
    /// Maximum tool-call loop turns per user message. Default: `20`.
    /// Setting to `0` falls back to the safe default of `20`.
    #[serde(default = "default_agent_max_tool_iterations")]
    pub max_tool_iterations: usize,
    /// Maximum conversation history messages retained per session. Default: `50`.
    #[serde(default = "default_agent_max_history_messages")]
    pub max_history_messages: usize,
    /// Enable parallel tool execution within a single iteration. Default: `false`.
    #[serde(default)]
    pub parallel_tools: bool,
    /// Tool dispatch strategy (e.g. `"auto"`). Default: `"auto"`.
    #[serde(default = "default_agent_tool_dispatcher")]
    pub tool_dispatcher: String,
    /// Loop detection: no-progress repeat threshold.
    /// Triggers when the same tool+args produces identical output this many times.
    /// Set to `0` to disable. Default: `3`.
    #[serde(default = "default_loop_detection_no_progress_threshold")]
    pub loop_detection_no_progress_threshold: usize,
    /// Loop detection: ping-pong cycle threshold.
    /// Detects A→B→A→B alternating patterns with no progress.
    /// Value is number of full cycles (A-B = 1 cycle). Set to `0` to disable. Default: `2`.
    #[serde(default = "default_loop_detection_ping_pong_cycles")]
    pub loop_detection_ping_pong_cycles: usize,
    /// Loop detection: consecutive failure streak threshold.
    /// Triggers when the same tool fails this many times in a row.
    /// Set to `0` to disable. Default: `3`.
    #[serde(default = "default_loop_detection_failure_streak")]
    pub loop_detection_failure_streak: usize,
    /// Safety heartbeat injection interval inside `run_tool_call_loop`.
    /// Injects a security-constraint reminder every N tool iterations.
    /// Set to `0` to disable. Default: `5`.
    /// Compatibility/rollback: omit/remove this key to use default (`5`), or set
    /// to `0` for explicit disable.
    #[serde(default = "default_safety_heartbeat_interval")]
    pub safety_heartbeat_interval: usize,
    /// Safety heartbeat injection interval for interactive sessions.
    /// Injects a security-constraint reminder every N conversation turns.
    /// Set to `0` to disable. Default: `10`.
    /// Compatibility/rollback: omit/remove this key to use default (`10`), or
    /// set to `0` for explicit disable.
    #[serde(default = "default_safety_heartbeat_turn_interval")]
    pub safety_heartbeat_turn_interval: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AgentSessionBackend {
    Memory,
    Sqlite,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AgentSessionStrategy {
    PerSender,
    PerChannel,
    Main,
}

/// Session persistence configuration (`[agent.session]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentSessionConfig {
    /// Session backend to use. Options: "memory", "sqlite", "none".
    /// Default: "none" (no persistence).
    /// Set to "none" to disable session persistence entirely.
    #[serde(default = "default_agent_session_backend")]
    pub backend: AgentSessionBackend,

    /// Strategy for resolving session IDs. Options: "per-sender", "per-channel", "main".
    /// Default: "per-sender" (each user gets a unique session per channel).
    #[serde(default = "default_agent_session_strategy")]
    pub strategy: AgentSessionStrategy,

    /// Time-to-live for sessions in seconds.
    /// Default: 3600 (1 hour).
    #[serde(default = "default_agent_session_ttl_seconds")]
    pub ttl_seconds: u64,

    /// Maximum number of messages to retain per session.
    /// Default: 50.
    #[serde(default = "default_agent_session_max_messages")]
    pub max_messages: usize,
}

fn default_agent_max_tool_iterations() -> usize {
    20
}

fn default_agent_max_history_messages() -> usize {
    50
}

fn default_agent_tool_dispatcher() -> String {
    "auto".into()
}

fn default_agent_session_backend() -> AgentSessionBackend {
    AgentSessionBackend::None
}

fn default_agent_session_strategy() -> AgentSessionStrategy {
    AgentSessionStrategy::PerSender
}

fn default_agent_session_ttl_seconds() -> u64 {
    3600
}

fn default_agent_session_max_messages() -> usize {
    default_agent_max_history_messages()
}

fn default_loop_detection_no_progress_threshold() -> usize {
    3
}

fn default_loop_detection_ping_pong_cycles() -> usize {
    2
}

fn default_loop_detection_failure_streak() -> usize {
    3
}

fn default_safety_heartbeat_interval() -> usize {
    5
}

fn default_safety_heartbeat_turn_interval() -> usize {
    10
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            compact_context: true,
            session: AgentSessionConfig::default(),
            max_tool_iterations: default_agent_max_tool_iterations(),
            max_history_messages: default_agent_max_history_messages(),
            parallel_tools: false,
            tool_dispatcher: default_agent_tool_dispatcher(),
            loop_detection_no_progress_threshold: default_loop_detection_no_progress_threshold(),
            loop_detection_ping_pong_cycles: default_loop_detection_ping_pong_cycles(),
            loop_detection_failure_streak: default_loop_detection_failure_streak(),
            safety_heartbeat_interval: default_safety_heartbeat_interval(),
            safety_heartbeat_turn_interval: default_safety_heartbeat_turn_interval(),
        }
    }
}

impl Default for AgentSessionConfig {
    fn default() -> Self {
        Self {
            backend: default_agent_session_backend(),
            strategy: default_agent_session_strategy(),
            ttl_seconds: default_agent_session_ttl_seconds(),
            max_messages: default_agent_session_max_messages(),
        }
    }
}

/// Skills loading configuration (`[skills]` section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SkillsConfig {
    /// Enable loading and syncing the community open-skills repository.
    /// Default: `false` (opt-in).
    #[serde(default)]
    pub open_skills_enabled: bool,
    /// Optional path to a local open-skills repository.
    /// If unset, defaults to `$HOME/open-skills` when enabled.
    #[serde(default)]
    pub open_skills_dir: Option<String>,
    /// Optional allowlist of canonical directory roots for workspace skill symlink targets.
    /// Symlinked workspace skills are rejected unless their resolved targets are under one
    /// of these roots. Accepts absolute paths and `~/` home-relative paths.
    #[serde(default)]
    pub trusted_skill_roots: Vec<String>,
    /// Allow script-like files in skills (`.sh`, `.bash`, `.ps1`, shebang shell files).
    /// Default: `false` (secure by default).
    #[serde(default)]
    pub allow_scripts: bool,
    /// Controls how skills are injected into the system prompt.
    /// `full` preserves legacy behavior. `compact` keeps context small and loads skills on demand.
    #[serde(default)]
    pub prompt_injection_mode: SkillsPromptInjectionMode,
    /// Optional ClawhHub API token for authenticated skill downloads.
    /// Obtain from https://clawhub.ai after signing in.
    /// Set via config: `clawhub_token = "..."` under `[skills]`.
    #[serde(default)]
    pub clawhub_token: Option<String>,
}

/// WASM plugin engine configuration (`[wasm]` section).
///
/// Controls limits applied to every WASM tool invocation.
/// Requires the `wasm-tools` compile-time feature to have any effect.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WasmConfig {
    /// Enable loading WASM tools from installed skill packages.
    /// Default: `true` (auto-discovers plugins in the skills directory).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum linear memory per WASM invocation in MiB.
    /// Valid range: 1..=256. Default: `64`.
    #[serde(default = "default_wasm_memory_limit_mb")]
    pub memory_limit_mb: u64,
    /// CPU fuel budget per invocation (roughly one unit ≈ one WASM instruction).
    /// Default: 1_000_000_000.
    #[serde(default = "default_wasm_fuel_limit")]
    pub fuel_limit: u64,
    /// URL of the ZeroMarket (or compatible) registry used by `zeroclaw skill install`.
    /// Default: the public ZeroMarket registry.
    #[serde(default = "default_registry_url")]
    pub registry_url: String,
}

fn default_wasm_memory_limit_mb() -> u64 {
    64
}

fn default_wasm_fuel_limit() -> u64 {
    1_000_000_000
}

fn default_registry_url() -> String {
    "https://zeromarket.vercel.app/api".to_string()
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_limit_mb: default_wasm_memory_limit_mb(),
            fuel_limit: default_wasm_fuel_limit(),
            registry_url: default_registry_url(),
        }
    }
}

/// Multimodal (image) handling configuration (`[multimodal]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
        }
    }
}

// ── Identity (AIEOS / OpenClaw format) ──────────────────────────

/// Identity format configuration (`[identity]` section).
///
/// Supports `"openclaw"` (default) or `"aieos"` identity documents.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IdentityConfig {
    /// Identity format: "openclaw" (default) or "aieos"
    #[serde(default = "default_identity_format")]
    pub format: String,
    /// Additional workspace files injected for the OpenClaw identity format.
    ///
    /// Paths are resolved relative to the workspace root.
    #[serde(default)]
    pub extra_files: Vec<String>,
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
            extra_files: Vec::new(),
            aieos_path: None,
            aieos_inline: None,
        }
    }
}

// ── Cost tracking and budget enforcement ───────────────────────────

/// Cost tracking and budget enforcement configuration (`[cost]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CostConfig {
    /// Enable cost tracking (default: false)
    #[serde(default)]
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

    /// Runtime budget enforcement policy (`[cost.enforcement]`).
    #[serde(default)]
    pub enforcement: CostEnforcementConfig,
}

/// Budget enforcement behavior when projected spend approaches/exceeds limits.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CostEnforcementMode {
    /// Log warnings only; never block the request.
    Warn,
    /// Attempt one downgrade to a cheaper route/model, then block if still over budget.
    RouteDown,
    /// Block immediately when projected spend exceeds configured limits.
    Block,
}

fn default_cost_enforcement_mode() -> CostEnforcementMode {
    CostEnforcementMode::Warn
}

/// Runtime budget enforcement controls (`[cost.enforcement]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CostEnforcementConfig {
    /// Enforcement behavior. Default: `warn`.
    #[serde(default = "default_cost_enforcement_mode")]
    pub mode: CostEnforcementMode,
    /// Optional fallback model (or `hint:*`) when `mode = "route_down"`.
    #[serde(default = "default_route_down_model")]
    pub route_down_model: Option<String>,
    /// Extra reserve added to token/cost estimates (percentage, 0-100). Default: `10`.
    #[serde(default = "default_cost_reserve_percent")]
    pub reserve_percent: u8,
}

fn default_route_down_model() -> Option<String> {
    Some("hint:fast".to_string())
}

fn default_cost_reserve_percent() -> u8 {
    10
}

impl Default for CostEnforcementConfig {
    fn default() -> Self {
        Self {
            mode: default_cost_enforcement_mode(),
            route_down_model: default_route_down_model(),
            reserve_percent: default_cost_reserve_percent(),
        }
    }
}

/// Per-model pricing entry (USD per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

impl Default for CostConfig {
    fn default() -> Self {
        Self {
            enabled: false,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

// ── Economic Agent Config ─────────────────────────────────────────

/// Token pricing configuration for economic tracking.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EconomicTokenPricing {
    /// Price per million input tokens (USD)
    #[serde(default = "default_input_price")]
    pub input_price_per_million: f64,
    /// Price per million output tokens (USD)
    #[serde(default = "default_output_price")]
    pub output_price_per_million: f64,
}

fn default_input_price() -> f64 {
    3.0 // Claude Sonnet 4 input price
}

fn default_output_price() -> f64 {
    15.0 // Claude Sonnet 4 output price
}

impl Default for EconomicTokenPricing {
    fn default() -> Self {
        Self {
            input_price_per_million: default_input_price(),
            output_price_per_million: default_output_price(),
        }
    }
}

/// Economic agent survival tracking configuration (`[economic]` section).
///
/// Implements the ClawWork economic model for AI agents, tracking
/// balance, costs, income, and survival status.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EconomicConfig {
    /// Enable economic tracking (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Starting balance in USD (default: 1000.0)
    #[serde(default = "default_initial_balance")]
    pub initial_balance: f64,

    /// Token pricing configuration
    #[serde(default)]
    pub token_pricing: EconomicTokenPricing,

    /// Minimum evaluation score (0.0-1.0) to receive payment (default: 0.6)
    #[serde(default = "default_min_evaluation_threshold")]
    pub min_evaluation_threshold: f64,

    /// Data directory for economic state persistence (relative to workspace)
    #[serde(default)]
    pub data_path: Option<String>,
}

fn default_initial_balance() -> f64 {
    1000.0
}

fn default_min_evaluation_threshold() -> f64 {
    0.6
}

impl Default for EconomicConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            initial_balance: default_initial_balance(),
            token_pricing: EconomicTokenPricing::default(),
            min_evaluation_threshold: default_min_evaluation_threshold(),
            data_path: None,
        }
    }
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

    /// Maximum distinct client keys tracked by gateway rate limiter maps.
    #[serde(default = "default_gateway_rate_limit_max_keys")]
    pub rate_limit_max_keys: usize,

    /// TTL for webhook idempotency keys.
    #[serde(default = "default_idempotency_ttl_secs")]
    pub idempotency_ttl_secs: u64,

    /// Maximum distinct idempotency keys retained in memory.
    #[serde(default = "default_gateway_idempotency_max_keys")]
    pub idempotency_max_keys: usize,

    /// Node-control protocol scaffold (`[gateway.node_control]`).
    #[serde(default)]
    pub node_control: NodeControlConfig,
}

/// Node-control scaffold settings under `[gateway.node_control]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct NodeControlConfig {
    /// Enable experimental node-control API endpoints.
    #[serde(default)]
    pub enabled: bool,

    /// Optional extra shared token for node-control API calls.
    /// When set, clients must send this value in `X-Node-Control-Token`.
    #[serde(default)]
    pub auth_token: Option<String>,

    /// Allowlist of remote node IDs for `node.describe`/`node.invoke`.
    /// Empty means "no explicit allowlist" (accept all IDs).
    #[serde(default)]
    pub allowed_node_ids: Vec<String>,
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
            rate_limit_max_keys: default_gateway_rate_limit_max_keys(),
            idempotency_ttl_secs: default_idempotency_ttl_secs(),
            idempotency_max_keys: default_gateway_idempotency_max_keys(),
            node_control: NodeControlConfig::default(),
        }
    }
}

// ── Composio (managed tool surface) ─────────────────────────────

/// Composio managed OAuth tools integration (`[composio]` section).
///
/// Provides access to 1000+ OAuth-connected tools via the Composio platform.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ComposioConfig {
    /// Enable Composio integration for 1000+ OAuth tools
    #[serde(default, alias = "enable")]
    pub enabled: bool,
    /// Composio API key (stored encrypted when secrets.encrypt = true)
    #[serde(default)]
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

// ── Secrets (encrypted credential store) ────────────────────────

/// Secrets encryption configuration (`[secrets]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserComputerUseConfig {
    /// Sidecar endpoint for computer-use actions (OS-level mouse/keyboard/screenshot)
    #[serde(default = "default_browser_computer_use_endpoint")]
    pub endpoint: String,
    /// Optional bearer token for computer-use sidecar
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserConfig {
    /// Enable `browser_open` tool (opens URLs in the system browser without scraping)
    #[serde(default)]
    pub enabled: bool,
    /// Allowed domains for `browser_open` (exact or subdomain match)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Browser for browser_open tool: "disable" | "brave" | "chrome" | "firefox" | "edge" | "msedge" | "default"
    #[serde(default = "default_browser_open")]
    pub browser_open: String,
    /// Browser session name (for agent-browser automation)
    #[serde(default)]
    pub session_name: Option<String>,
    /// Browser automation backend: "agent_browser" | "rust_native" | "computer_use" | "auto"
    #[serde(default = "default_browser_backend")]
    pub backend: String,
    /// Auto backend priority order (only used when backend = "auto")
    /// Supported values: "agent_browser", "rust_native", "computer_use"
    #[serde(default)]
    pub auto_backend_priority: Vec<String>,
    /// Agent-browser executable path/name
    #[serde(default = "default_agent_browser_command")]
    pub agent_browser_command: String,
    /// Additional arguments passed to agent-browser before each action command
    #[serde(default)]
    pub agent_browser_extra_args: Vec<String>,
    /// Timeout in milliseconds for each agent-browser command invocation
    #[serde(default = "default_agent_browser_timeout_ms")]
    pub agent_browser_timeout_ms: u64,
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
    pub computer_use: BrowserComputerUseConfig,
}

fn default_browser_backend() -> String {
    "agent_browser".into()
}

fn default_agent_browser_command() -> String {
    "agent-browser".into()
}

fn default_agent_browser_timeout_ms() -> u64 {
    30_000
}

fn default_browser_open() -> String {
    "default".into()
}

fn default_browser_webdriver_url() -> String {
    "http://127.0.0.1:9515".into()
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_domains: Vec::new(),
            browser_open: default_browser_open(),
            session_name: None,
            backend: default_browser_backend(),
            auto_backend_priority: Vec::new(),
            agent_browser_command: default_agent_browser_command(),
            agent_browser_extra_args: Vec::new(),
            agent_browser_timeout_ms: default_agent_browser_timeout_ms(),
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
/// Deny-by-default: if `allowed_domains` is empty, all HTTP requests are rejected.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HttpRequestCredentialProfile {
    /// Header name to inject (for example `Authorization` or `X-API-Key`)
    #[serde(default = "default_http_request_credential_header_name")]
    pub header_name: String,
    /// Environment variable containing the secret/token value
    #[serde(default)]
    pub env_var: String,
    /// Optional prefix prepended to the secret (for example `Bearer `)
    #[serde(default)]
    pub value_prefix: String,
}

impl Default for HttpRequestCredentialProfile {
    fn default() -> Self {
        Self {
            header_name: default_http_request_credential_header_name(),
            env_var: String::new(),
            value_prefix: default_http_request_credential_value_prefix(),
        }
    }
}

fn default_http_request_credential_header_name() -> String {
    "Authorization".into()
}

fn default_http_request_credential_value_prefix() -> String {
    "Bearer ".into()
}

/// HTTP request tool configuration (`[http_request]` section).
///
/// Deny-by-default: if `allowed_domains` is empty, all HTTP requests are rejected.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    /// User-Agent string sent with HTTP requests (env: ZEROCLAW_HTTP_REQUEST_USER_AGENT)
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// Optional named credential profiles for env-backed auth injection.
    ///
    /// Example:
    /// `[http_request.credential_profiles.github]`
    /// `env_var = "GITHUB_TOKEN"`
    /// `header_name = "Authorization"`
    /// `value_prefix = "Bearer "`
    #[serde(default)]
    pub credential_profiles: HashMap<String, HttpRequestCredentialProfile>,
}

impl Default for HttpRequestConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_domains: vec![],
            max_response_size: default_http_max_response_size(),
            timeout_secs: default_http_timeout_secs(),
            user_agent: default_user_agent(),
            credential_profiles: HashMap::new(),
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebFetchConfig {
    /// Enable `web_fetch` tool for fetching web page content
    #[serde(default)]
    pub enabled: bool,
    /// Provider: "fast_html2md", "nanohtml2text", "firecrawl", or "tavily"
    #[serde(default = "default_web_fetch_provider")]
    pub provider: String,
    /// Optional provider API key (required for provider = "firecrawl" or "tavily").
    /// Multiple keys can be comma-separated for round-robin load balancing.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional provider API URL override (for self-hosted providers)
    #[serde(default)]
    pub api_url: Option<String>,
    /// Allowed domains for web fetch (exact or subdomain match; `["*"]` = all public hosts)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Blocked domains (exact or subdomain match; always takes priority over allowed_domains)
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Maximum response size in bytes (default: 500KB, plain text is much smaller than raw HTML)
    #[serde(default = "default_web_fetch_max_response_size")]
    pub max_response_size: usize,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_web_fetch_timeout_secs")]
    pub timeout_secs: u64,
    /// User-Agent string sent with fetch requests (env: ZEROCLAW_WEB_FETCH_USER_AGENT)
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
}

fn default_web_fetch_max_response_size() -> usize {
    500_000 // 500KB
}

fn default_web_fetch_provider() -> String {
    "fast_html2md".into()
}

fn default_web_fetch_timeout_secs() -> u64 {
    30
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_web_fetch_provider(),
            api_key: None,
            api_url: None,
            allowed_domains: vec!["*".into()],
            blocked_domains: vec![],
            max_response_size: default_web_fetch_max_response_size(),
            timeout_secs: default_web_fetch_timeout_secs(),
            user_agent: default_user_agent(),
        }
    }
}

// ── Web search ───────────────────────────────────────────────────

/// Web search tool configuration (`[web_search]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebSearchConfig {
    /// Enable `web_search_tool` for web searches
    #[serde(default)]
    pub enabled: bool,
    /// Search provider: "duckduckgo"/"ddg" (free, no API key), "brave", "firecrawl",
    /// "tavily", "perplexity", "exa", or "jina"
    #[serde(default = "default_web_search_provider")]
    pub provider: String,
    /// Generic provider API key (used by firecrawl, tavily, and as fallback for brave).
    /// Multiple keys can be comma-separated for round-robin load balancing.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional provider API URL override (for self-hosted providers)
    #[serde(default)]
    pub api_url: Option<String>,
    /// Brave Search API key (required if provider is "brave")
    #[serde(default)]
    pub brave_api_key: Option<String>,
    /// Perplexity API key (used when provider is "perplexity")
    #[serde(default)]
    pub perplexity_api_key: Option<String>,
    /// Exa API key (used when provider is "exa")
    #[serde(default)]
    pub exa_api_key: Option<String>,
    /// Jina API key (optional; can raise limits for provider = "jina")
    #[serde(default)]
    pub jina_api_key: Option<String>,
    /// Fallback providers attempted after primary provider fails.
    /// Supported values: duckduckgo (or ddg), brave, firecrawl, tavily, perplexity, exa, jina
    #[serde(default)]
    pub fallback_providers: Vec<String>,
    /// Retry count per provider before falling back to next provider
    #[serde(default = "default_web_search_retries_per_provider")]
    pub retries_per_provider: u32,
    /// Retry backoff in milliseconds between provider retry attempts
    #[serde(default = "default_web_search_retry_backoff_ms")]
    pub retry_backoff_ms: u64,
    /// Optional domain filter forwarded to providers that support it
    #[serde(default)]
    pub domain_filter: Vec<String>,
    /// Optional language filter forwarded to providers that support it
    #[serde(default)]
    pub language_filter: Vec<String>,
    /// Optional country filter forwarded to providers that support it (e.g. "US")
    #[serde(default)]
    pub country: Option<String>,
    /// Optional recency filter forwarded to providers that support it
    #[serde(default)]
    pub recency_filter: Option<String>,
    /// Optional max tokens cap used by provider-specific APIs (for example Perplexity)
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Optional per-result token cap used by provider-specific APIs
    #[serde(default)]
    pub max_tokens_per_page: Option<u32>,
    /// Exa search type override: "auto" (default), "keyword", or "neural"
    #[serde(default = "default_web_search_exa_search_type")]
    pub exa_search_type: String,
    /// Include textual content payloads for Exa search responses
    #[serde(default)]
    pub exa_include_text: bool,
    /// Optional site filters for Jina search provider
    #[serde(default)]
    pub jina_site_filters: Vec<String>,
    /// Maximum results per search (1-10)
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    /// Request timeout in seconds
    #[serde(default = "default_web_search_timeout_secs")]
    pub timeout_secs: u64,
    /// User-Agent string sent with search requests (env: ZEROCLAW_WEB_SEARCH_USER_AGENT)
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
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

fn default_web_search_retries_per_provider() -> u32 {
    0
}

fn default_web_search_retry_backoff_ms() -> u64 {
    250
}

fn default_web_search_exa_search_type() -> String {
    "auto".into()
}

const BROWSER_OPEN_ALLOWED_VALUES: &[&str] = &[
    "disable", "brave", "chrome", "firefox", "edge", "msedge", "default",
];
const BROWSER_BACKEND_ALLOWED_VALUES: &[&str] =
    &["agent_browser", "rust_native", "computer_use", "auto"];
const BROWSER_AUTO_BACKEND_ALLOWED_VALUES: &[&str] =
    &["agent_browser", "rust_native", "computer_use"];
const WEB_SEARCH_PROVIDER_ALLOWED_VALUES: &[&str] = &[
    "duckduckgo",
    "ddg",
    "brave",
    "firecrawl",
    "tavily",
    "perplexity",
    "exa",
    "jina",
];
const WEB_SEARCH_EXA_SEARCH_TYPE_ALLOWED_VALUES: &[&str] = &["auto", "keyword", "neural"];

fn normalize_browser_open_choice(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "disable" => Some("disable"),
        "brave" => Some("brave"),
        "chrome" => Some("chrome"),
        "firefox" => Some("firefox"),
        "edge" | "msedge" => Some("edge"),
        "default" | "" => Some("default"),
        _ => None,
    }
}

fn normalize_browser_backend(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "agent_browser" | "agentbrowser" => Some("agent_browser"),
        "rust_native" | "native" => Some("rust_native"),
        "computer_use" | "computeruse" => Some("computer_use"),
        "auto" => Some("auto"),
        _ => None,
    }
}

fn normalize_browser_auto_backend(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "agent_browser" | "agentbrowser" => Some("agent_browser"),
        "rust_native" | "native" => Some("rust_native"),
        "computer_use" | "computeruse" => Some("computer_use"),
        _ => None,
    }
}

fn normalize_web_search_provider(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "duckduckgo" | "ddg" => Some("duckduckgo"),
        "brave" => Some("brave"),
        "firecrawl" => Some("firecrawl"),
        "tavily" => Some("tavily"),
        "perplexity" => Some("perplexity"),
        "exa" => Some("exa"),
        "jina" => Some("jina"),
        _ => None,
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_web_search_provider(),
            api_key: None,
            api_url: None,
            brave_api_key: None,
            perplexity_api_key: None,
            exa_api_key: None,
            jina_api_key: None,
            fallback_providers: Vec::new(),
            retries_per_provider: default_web_search_retries_per_provider(),
            retry_backoff_ms: default_web_search_retry_backoff_ms(),
            domain_filter: Vec::new(),
            language_filter: Vec::new(),
            country: None,
            recency_filter: None,
            max_tokens: None,
            max_tokens_per_page: None,
            exa_search_type: default_web_search_exa_search_type(),
            exa_include_text: false,
            jina_site_filters: Vec::new(),
            max_results: default_web_search_max_results(),
            timeout_secs: default_web_search_timeout_secs(),
            user_agent: default_user_agent(),
        }
    }
}

fn default_user_agent() -> String {
    "ZeroClaw/1.0".into()
}

// ── Proxy ───────────────────────────────────────────────────────

/// Proxy application scope — determines which outbound traffic uses the proxy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

fn validate_proxy_url(field: &str, url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .with_context(|| format!("Invalid {field} URL: '{url}' is not a valid URL"))?;

    match parsed.scheme() {
        "http" | "https" | "socks5" | "socks5h" => {}
        scheme => {
            anyhow::bail!(
                "Invalid {field} URL scheme '{scheme}'. Allowed: http, https, socks5, socks5h"
            );
        }
    }

    if parsed.host_str().is_none() {
        anyhow::bail!("Invalid {field} URL: host is required");
    }

    Ok(())
}

fn parse_cidr_notation(raw: &str) -> Result<(IpAddr, u8)> {
    let (ip_raw, prefix_raw) = raw
        .trim()
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("missing '/' separator"))?;
    let ip: IpAddr = ip_raw
        .trim()
        .parse()
        .with_context(|| format!("invalid IP address '{ip_raw}'"))?;
    let prefix: u8 = prefix_raw
        .trim()
        .parse()
        .with_context(|| format!("invalid prefix '{prefix_raw}'"))?;
    let max_prefix = match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    if prefix > max_prefix {
        anyhow::bail!("prefix {prefix} exceeds max {max_prefix} for {ip}");
    }
    Ok((ip, prefix))
}

fn set_proxy_env_pair(key: &str, value: Option<&str>) {
    let lowercase_key = key.to_ascii_lowercase();
    if let Some(value) = value.and_then(|candidate| normalize_proxy_url_option(Some(candidate))) {
        std::env::set_var(key, &value);
        std::env::set_var(lowercase_key, value);
    } else {
        std::env::remove_var(key);
        std::env::remove_var(lowercase_key);
    }
}

fn clear_proxy_env_pair(key: &str) {
    std::env::remove_var(key);
    std::env::remove_var(key.to_ascii_lowercase());
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct StorageConfig {
    /// Storage provider settings (e.g. sqlite, postgres).
    #[serde(default)]
    pub provider: StorageProviderSection,
}

/// Wrapper for the storage provider configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct StorageProviderSection {
    /// Storage provider backend settings.
    #[serde(default)]
    pub config: StorageProviderConfig,
}

/// Storage provider backend configuration (e.g. postgres connection details).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageProviderConfig {
    /// Storage engine key (e.g. "postgres", "sqlite").
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

    /// Enable TLS for the PostgreSQL connection.
    ///
    /// `true` — require TLS (skips certificate verification; suitable for
    /// self-signed certs and most managed databases).
    /// `false` (default) — plain TCP, backward-compatible.
    #[serde(default)]
    pub tls: bool,
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
            tls: false,
        }
    }
}

/// Memory backend configuration (`[memory]` section).
///
/// Controls conversation memory storage, embeddings, hybrid search, response caching,
/// and memory snapshot/hydration.
/// Configuration for Qdrant vector database backend (`[memory.qdrant]`).
/// Used when `[memory].backend = "qdrant"` or `"sqlite_qdrant_hybrid"`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[allow(clippy::struct_excessive_bools)]
pub struct MemoryConfig {
    /// "sqlite" | "sqlite_qdrant_hybrid" | "lucid" | "postgres" | "qdrant" | "markdown" | "none" (`none` = explicit no-op memory)
    ///
    /// `postgres` requires `[storage.provider.config]` with `db_url` (`dbURL` alias supported).
    /// `qdrant` and `sqlite_qdrant_hybrid` use `[memory.qdrant]` config or `QDRANT_URL` env var.
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

    // ── SQLite backend options ─────────────────────────────────
    /// For sqlite backend: max seconds to wait when opening the DB (e.g. file locked).
    /// None = wait indefinitely (default). Recommended max: 300.
    #[serde(default)]
    pub sqlite_open_timeout_secs: Option<u64>,

    /// SQLite journal mode: "wal" (default) or "delete".
    ///
    /// WAL (Write-Ahead Logging) provides better concurrency and is the
    /// recommended default. However, WAL requires shared-memory support
    /// (mmap/shm) which is **not available** on many network and virtual
    /// shared filesystems (NFS, SMB/CIFS, UTM/VirtioFS, VirtualBox shared
    /// folders, etc.), causing `xShmMap` I/O errors at startup.
    ///
    /// Set to `"delete"` when your workspace lives on such a filesystem.
    ///
    /// Example:
    /// ```toml
    /// [memory]
    /// sqlite_journal_mode = "delete"
    /// ```
    #[serde(default = "default_sqlite_journal_mode")]
    pub sqlite_journal_mode: String,

    // ── Qdrant backend options ─────────────────────────────────
    /// Configuration for Qdrant vector database backend.
    /// Used when `backend = "qdrant"` or `backend = "sqlite_qdrant_hybrid"`.
    #[serde(default)]
    pub qdrant: QdrantConfig,
}

fn default_sqlite_journal_mode() -> String {
    "wal".into()
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
            min_relevance_score: default_min_relevance_score(),
            embedding_cache_size: default_cache_size(),
            chunk_max_tokens: default_chunk_size(),
            response_cache_enabled: false,
            response_cache_ttl_minutes: default_response_cache_ttl(),
            response_cache_max_entries: default_response_cache_max(),
            snapshot_enabled: false,
            snapshot_on_hygiene: false,
            auto_hydrate: true,
            sqlite_open_timeout_secs: None,
            sqlite_journal_mode: default_sqlite_journal_mode(),
            qdrant: QdrantConfig::default(),
        }
    }
}

// ── Observability ─────────────────────────────────────────────────

/// Observability backend configuration (`[observability]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ObservabilityConfig {
    /// "none" | "log" | "prometheus" | "otel"
    pub backend: String,

    /// OTLP endpoint (e.g. "http://localhost:4318"). Only used when backend = "otel".
    #[serde(default)]
    pub otel_endpoint: Option<String>,

    /// Service name reported to the OTel collector. Defaults to "zeroclaw".
    #[serde(default)]
    pub otel_service_name: Option<String>,

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HooksConfig {
    /// Enable lifecycle hook execution.
    ///
    /// Hooks run in-process with the same privileges as the main runtime.
    /// Keep enabled hook handlers narrowly scoped and auditable.
    pub enabled: bool,
    #[serde(default)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BuiltinHooksConfig {
    /// Enable the boot-script hook (injects startup/runtime guidance).
    #[serde(default)]
    pub boot_script: bool,
    /// Enable the command-logger hook (logs tool calls for auditing).
    #[serde(default)]
    pub command_logger: bool,
    /// Enable the session-memory hook (persists session hints between turns).
    #[serde(default)]
    pub session_memory: bool,
}

// ── Plugin system ─────────────────────────────────────────────────────────────

/// Plugin system configuration (`[plugins]` section).
///
/// Controls plugin discovery, loading, and per-plugin settings.
/// Mirrors OpenClaw's `plugins` config block.
///
/// Example:
/// ```toml
/// [plugins]
/// enabled = true
/// allow = ["hello-world"]
///
/// [plugins.entries.hello-world]
/// enabled = true
///
/// [plugins.entries.hello-world.config]
/// greeting = "Howdy"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginsConfig {
    /// Master switch — set to `false` to disable all plugin loading. Default: `true`.
    #[serde(default = "default_plugins_enabled")]
    pub enabled: bool,

    /// Allowlist — if non-empty, only plugins with these IDs are loaded.
    /// An empty list means all discovered plugins are eligible.
    #[serde(default)]
    pub allow: Vec<String>,

    /// Denylist — plugins with these IDs are never loaded, even if in the allowlist.
    #[serde(default)]
    pub deny: Vec<String>,

    /// Extra directories to scan for plugins (in addition to the standard locations).
    /// Standard locations: `<binary_dir>/extensions/`, `~/.zeroclaw/extensions/`,
    /// `<workspace>/.zeroclaw/extensions/`.
    #[serde(default)]
    pub load_paths: Vec<String>,

    /// Per-plugin configuration entries.
    #[serde(default)]
    pub entries: std::collections::HashMap<String, PluginEntryConfig>,
}

fn default_plugins_enabled() -> bool {
    true
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow: Vec::new(),
            deny: Vec::new(),
            load_paths: Vec::new(),
            entries: std::collections::HashMap::new(),
        }
    }
}

/// Per-plugin configuration entry (`[plugins.entries.<id>]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PluginEntryConfig {
    /// Override the plugin's enabled state. If absent, the plugin is enabled
    /// unless it is bundled-and-disabled-by-default.
    pub enabled: Option<bool>,

    /// Plugin-specific configuration table, passed to `PluginApi::plugin_config()`.
    #[serde(default)]
    pub config: serde_json::Value,
}

impl Default for PluginEntryConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            config: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}
// ── Autonomy / Security ──────────────────────────────────────────

/// Natural-language behavior for non-CLI approval-management commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NonCliNaturalLanguageApprovalMode {
    /// Do not treat natural-language text as approval-management commands.
    /// Operators must use explicit slash commands.
    Disabled,
    /// Natural-language approval phrases create a pending request that must be
    /// confirmed with a request ID.
    RequestConfirm,
    /// Natural-language approval phrases directly approve the named tool.
    ///
    /// This keeps private-chat workflows simple while still requiring a human
    /// sender and passing the same approver allowlist checks as slash commands.
    #[default]
    Direct,
}

/// Action to apply when a command-context rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandContextRuleAction {
    /// Matching context is explicitly allowed.
    #[default]
    Allow,
    /// Matching context is explicitly denied.
    Deny,
}

/// Context-aware allow/deny rule for shell commands.
///
/// Rules are evaluated per command segment. Command matching accepts command
/// names (`curl`), explicit paths (`/usr/bin/curl`), and wildcard (`*`).
///
/// Matching semantics:
/// - `action = "deny"`: if all constraints match, the segment is rejected.
/// - `action = "allow"`: if at least one allow rule exists for a command,
///   segments must match at least one of those allow rules.
///
/// Constraints are optional:
/// - `allowed_domains`: require URL arguments to match these hosts/patterns.
/// - `allowed_path_prefixes`: require path-like arguments to stay under these prefixes.
/// - `denied_path_prefixes`: for deny rules, match when any path-like argument
///   is under these prefixes; for allow rules, require path arguments not to hit
///   these prefixes.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct CommandContextRuleConfig {
    /// Command name/path pattern (`git`, `/usr/bin/curl`, or `*`).
    pub command: String,

    /// Rule action (`allow` | `deny`). Defaults to `allow`.
    #[serde(default)]
    pub action: CommandContextRuleAction,

    /// Allowed host patterns for URL arguments.
    ///
    /// Supports exact hosts (`api.example.com`) and wildcard suffixes (`*.example.com`).
    #[serde(default)]
    pub allowed_domains: Vec<String>,

    /// Allowed path prefixes for path-like arguments.
    ///
    /// Prefixes may be absolute, `~/...`, or workspace-relative.
    #[serde(default)]
    pub allowed_path_prefixes: Vec<String>,

    /// Denied path prefixes for path-like arguments.
    ///
    /// Prefixes may be absolute, `~/...`, or workspace-relative.
    #[serde(default)]
    pub denied_path_prefixes: Vec<String>,

    /// Permit high-risk commands when this allow rule matches.
    ///
    /// The command still requires explicit `approved=true` in supervised mode.
    #[serde(default)]
    pub allow_high_risk: bool,
}

/// Autonomy and security policy configuration (`[autonomy]` section).
///
/// Controls what the agent is allowed to do: shell commands, filesystem access,
/// risk approval gates, and per-policy budgets.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AutonomyConfig {
    /// Autonomy level: `read_only`, `supervised` (default), or `full`.
    pub level: AutonomyLevel,
    /// Restrict absolute filesystem paths to workspace-relative references. Default: `true`.
    /// Resolved paths outside the workspace still require `allowed_roots`.
    pub workspace_only: bool,
    /// Allowlist of executable names permitted for shell execution.
    pub allowed_commands: Vec<String>,

    /// Context-aware shell command allow/deny rules.
    ///
    /// These rules are evaluated per command segment and can narrow or override
    /// global `allowed_commands` behavior for matching commands.
    #[serde(default)]
    pub command_context_rules: Vec<CommandContextRuleConfig>,
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

    /// Allow `file_read` to access sensitive workspace secrets such as `.env`,
    /// key material, and credential files.
    ///
    /// Default is `false` to reduce accidental secret exposure via tool output.
    #[serde(default)]
    pub allow_sensitive_file_reads: bool,

    /// Allow `file_write` / `file_edit` to modify sensitive workspace secrets
    /// such as `.env`, key material, and credential files.
    ///
    /// Default is `false` to reduce accidental secret corruption/exfiltration.
    #[serde(default)]
    pub allow_sensitive_file_writes: bool,

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
    #[serde(default = "default_non_cli_excluded_tools")]
    pub non_cli_excluded_tools: Vec<String>,

    /// Optional allowlist for who can manage non-CLI approval commands.
    ///
    /// When empty, any sender already admitted by the channel allowlist can
    /// use approval-management commands.
    ///
    /// Supported entry formats:
    /// - `"*"`: allow any sender on any channel
    /// - `"alice"`: allow sender `alice` on any channel
    /// - `"telegram:alice"`: allow sender `alice` only on `telegram`
    /// - `"telegram:*"`: allow any sender on `telegram`
    /// - `"*:alice"`: allow sender `alice` on any channel
    #[serde(default)]
    pub non_cli_approval_approvers: Vec<String>,

    /// Natural-language handling mode for non-CLI approval-management commands.
    ///
    /// Values:
    /// - `direct` (default): phrases like `授权工具 shell` immediately approve.
    /// - `request_confirm`: phrases create pending requests requiring confirm.
    /// - `disabled`: ignore natural-language approval commands (slash only).
    #[serde(default)]
    pub non_cli_natural_language_approval_mode: NonCliNaturalLanguageApprovalMode,

    /// Optional per-channel override for natural-language approval mode.
    ///
    /// Keys are channel names (for example: `telegram`, `discord`, `slack`).
    /// Values use the same enum as `non_cli_natural_language_approval_mode`.
    ///
    /// Example:
    /// - `telegram = "direct"` for private-chat ergonomics
    /// - `discord = "request_confirm"` for stricter team channels
    #[serde(default)]
    pub non_cli_natural_language_approval_mode_by_channel:
        HashMap<String, NonCliNaturalLanguageApprovalMode>,
}

fn default_auto_approve() -> Vec<String> {
    vec!["file_read".into(), "memory_recall".into()]
}

fn default_always_ask() -> Vec<String> {
    vec![]
}

fn default_non_cli_excluded_tools() -> Vec<String> {
    [
        "shell",
        "process",
        "file_write",
        "file_edit",
        "git_operations",
        "browser",
        "browser_open",
        "http_request",
        "schedule",
        "cron_add",
        "cron_remove",
        "cron_update",
        "cron_run",
        "memory_store",
        "memory_forget",
        "proxy_config",
        "web_search_config",
        "web_access_config",
        "model_routing_config",
        "channel_ack_config",
        "pushover",
        "composio",
        "delegate",
        "screenshot",
        "image_info",
    ]
    .into_iter()
    .map(std::string::ToString::to_string)
    .collect()
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
                "mkdir".into(),
                "touch".into(),
                "cp".into(),
                "mv".into(),
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
            ],
            command_context_rules: Vec::new(),
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
                "/mnt".into(),
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
                "~/.config".into(),
            ],
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 1000,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            shell_env_passthrough: vec![],
            allow_sensitive_file_reads: false,
            allow_sensitive_file_writes: false,
            auto_approve: default_auto_approve(),
            always_ask: default_always_ask(),
            allowed_roots: Vec::new(),
            non_cli_excluded_tools: default_non_cli_excluded_tools(),
            non_cli_approval_approvers: Vec::new(),
            non_cli_natural_language_approval_mode: NonCliNaturalLanguageApprovalMode::default(),
            non_cli_natural_language_approval_mode_by_channel: HashMap::new(),
        }
    }
}

// ── Runtime ──────────────────────────────────────────────────────

/// Runtime adapter configuration (`[runtime]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeConfig {
    /// Runtime kind (`native` | `docker` | `wasm`).
    #[serde(default = "default_runtime_kind")]
    pub kind: String,

    /// Docker runtime settings (used when `kind = "docker"`).
    #[serde(default)]
    pub docker: DockerRuntimeConfig,

    /// WASM runtime settings (used when `kind = "wasm"`).
    #[serde(default)]
    pub wasm: WasmRuntimeConfig,

    /// Global reasoning override for providers that expose explicit controls.
    /// - `None`: provider default behavior
    /// - `Some(true)`: request reasoning/thinking when supported
    /// - `Some(false)`: disable reasoning/thinking when supported
    #[serde(default)]
    pub reasoning_enabled: Option<bool>,

    /// Deprecated compatibility alias for `[provider].reasoning_level`.
    /// - Canonical key: `provider.reasoning_level`
    /// - Legacy key accepted for compatibility: `runtime.reasoning_level`
    /// - When both are set, provider-level value wins.
    #[serde(default)]
    pub reasoning_level: Option<String>,
}

/// Docker runtime configuration (`[runtime.docker]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// WASM runtime configuration (`[runtime.wasm]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WasmRuntimeConfig {
    /// Workspace-relative directory that stores `.wasm` modules.
    #[serde(default = "default_wasm_tools_dir")]
    pub tools_dir: String,

    /// Fuel limit per invocation (instruction budget).
    #[serde(default = "default_runtime_wasm_fuel_limit")]
    pub fuel_limit: u64,

    /// Memory limit per invocation in MB.
    #[serde(default = "default_runtime_wasm_memory_limit_mb")]
    pub memory_limit_mb: u64,

    /// Maximum `.wasm` module size in MB.
    #[serde(default = "default_wasm_max_module_size_mb")]
    pub max_module_size_mb: u64,

    /// Allow reading files from workspace inside WASM host calls (future-facing).
    #[serde(default)]
    pub allow_workspace_read: bool,

    /// Allow writing files to workspace inside WASM host calls (future-facing).
    #[serde(default)]
    pub allow_workspace_write: bool,

    /// Explicit host allowlist for outbound HTTP from WASM modules (future-facing).
    #[serde(default)]
    pub allowed_hosts: Vec<String>,

    /// WASM runtime security controls (`[runtime.wasm.security]` section).
    #[serde(default)]
    pub security: WasmSecurityConfig,
}

/// How to handle invocation capabilities that exceed baseline runtime policy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WasmCapabilityEscalationMode {
    /// Reject any invocation that asks for capabilities above runtime config.
    #[default]
    Deny,
    /// Automatically clamp invocation capabilities to runtime config ceilings.
    Clamp,
}

/// Integrity policy for WASM modules pinned by SHA-256 digest.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WasmModuleHashPolicy {
    /// Disable module hash validation.
    Disabled,
    /// Warn on missing or mismatched hashes, but allow execution.
    #[default]
    Warn,
    /// Require exact hash match before execution.
    Enforce,
}

/// Security policy controls for WASM runtime hardening.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WasmSecurityConfig {
    /// Require `runtime.wasm.tools_dir` to stay workspace-relative and traversal-free.
    #[serde(default = "default_true")]
    pub require_workspace_relative_tools_dir: bool,

    /// Reject module files that are symlinks before execution.
    #[serde(default = "default_true")]
    pub reject_symlink_modules: bool,

    /// Reject `runtime.wasm.tools_dir` when it is itself a symlink.
    #[serde(default = "default_true")]
    pub reject_symlink_tools_dir: bool,

    /// Strictly validate host allowlist entries (`host` or `host:port` only).
    #[serde(default = "default_true")]
    pub strict_host_validation: bool,

    /// Capability escalation handling policy.
    #[serde(default)]
    pub capability_escalation_mode: WasmCapabilityEscalationMode,

    /// Module digest verification policy.
    #[serde(default)]
    pub module_hash_policy: WasmModuleHashPolicy,

    /// Optional pinned SHA-256 digest map keyed by module name (without `.wasm`).
    #[serde(default)]
    pub module_sha256: BTreeMap<String, String>,
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

fn default_wasm_tools_dir() -> String {
    "tools/wasm".into()
}

fn default_runtime_wasm_fuel_limit() -> u64 {
    1_000_000
}

fn default_runtime_wasm_memory_limit_mb() -> u64 {
    64
}

fn default_wasm_max_module_size_mb() -> u64 {
    50
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

impl Default for WasmRuntimeConfig {
    fn default() -> Self {
        Self {
            tools_dir: default_wasm_tools_dir(),
            fuel_limit: default_runtime_wasm_fuel_limit(),
            memory_limit_mb: default_runtime_wasm_memory_limit_mb(),
            max_module_size_mb: default_wasm_max_module_size_mb(),
            allow_workspace_read: false,
            allow_workspace_write: false,
            allowed_hosts: Vec::new(),
            security: WasmSecurityConfig::default(),
        }
    }
}

impl Default for WasmSecurityConfig {
    fn default() -> Self {
        Self {
            require_workspace_relative_tools_dir: true,
            reject_symlink_modules: true,
            reject_symlink_tools_dir: true,
            strict_host_validation: true,
            capability_escalation_mode: WasmCapabilityEscalationMode::Deny,
            module_hash_policy: WasmModuleHashPolicy::Warn,
            module_sha256: BTreeMap::new(),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            kind: default_runtime_kind(),
            docker: DockerRuntimeConfig::default(),
            wasm: WasmRuntimeConfig::default(),
            reasoning_enabled: None,
            reasoning_level: None,
        }
    }
}

// ── Research Phase ───────────────────────────────────────────────

/// Research phase trigger mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum ResearchTrigger {
    /// Never trigger research phase.
    #[default]
    Never,
    /// Always trigger research phase before responding.
    Always,
    /// Trigger when message contains configured keywords.
    Keywords,
    /// Trigger when message exceeds minimum length.
    Length,
    /// Trigger when message contains a question mark.
    Question,
}

/// Research phase configuration (`[research]` section).
///
/// When enabled, the agent proactively gathers information using tools
/// before generating its main response. This creates a "thinking" phase
/// where the agent explores the codebase, searches memory, or fetches
/// external data to inform its answer.
///
/// ```toml
/// [research]
/// enabled = true
/// trigger = "keywords"
/// keywords = ["find", "search", "check", "investigate"]
/// max_iterations = 5
/// show_progress = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResearchPhaseConfig {
    /// Enable the research phase.
    #[serde(default)]
    pub enabled: bool,

    /// When to trigger research phase.
    #[serde(default)]
    pub trigger: ResearchTrigger,

    /// Keywords that trigger research phase (when `trigger = "keywords"`).
    #[serde(default = "default_research_keywords")]
    pub keywords: Vec<String>,

    /// Minimum message length to trigger research (when `trigger = "length"`).
    #[serde(default = "default_research_min_length")]
    pub min_message_length: usize,

    /// Maximum tool call iterations during research phase.
    #[serde(default = "default_research_max_iterations")]
    pub max_iterations: usize,

    /// Show detailed progress during research (tool calls, results).
    #[serde(default = "default_true")]
    pub show_progress: bool,

    /// Custom system prompt prefix for research phase.
    /// If empty, uses default research instructions.
    #[serde(default)]
    pub system_prompt_prefix: String,
}

fn default_research_keywords() -> Vec<String> {
    vec![
        "find".into(),
        "search".into(),
        "check".into(),
        "investigate".into(),
        "look".into(),
        "research".into(),
        "найди".into(),
        "проверь".into(),
        "исследуй".into(),
        "поищи".into(),
    ]
}

fn default_research_min_length() -> usize {
    50
}

fn default_research_max_iterations() -> usize {
    5
}

impl Default for ResearchPhaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trigger: ResearchTrigger::default(),
            keywords: default_research_keywords(),
            min_message_length: default_research_min_length(),
            max_iterations: default_research_max_iterations(),
            show_progress: true,
            system_prompt_prefix: String::new(),
        }
    }
}

// ── Reliability / supervision ────────────────────────────────────

/// Reliability and supervision configuration (`[reliability]` section).
///
/// Controls provider retries, fallback chains, API key rotation, and channel restart backoff.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    ///
    /// Compatibility behavior: keys matching configured provider names are treated
    /// as provider-scoped remap chains during provider fallback.
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModelRouteConfig {
    /// Task hint name (e.g. "reasoning", "fast", "code", "summarize")
    pub hint: String,
    /// Provider to route to (must match a known provider name)
    pub provider: String,
    /// Model to use with that provider
    pub model: String,
    /// Optional max_tokens override for this route.
    /// When set, provider requests cap output tokens to this value.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Optional API key override for this route's provider
    #[serde(default)]
    pub api_key: Option<String>,
    /// Optional route-specific transport override for this route.
    /// Supported values: "auto", "websocket", "sse".
    ///
    /// When `model_routes[].transport` is unset, the route inherits `provider.transport`.
    /// If both are unset, runtime defaults are used (`auto` for OpenAI Codex).
    /// Existing configs without this field remain valid.
    #[serde(default)]
    pub transport: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct QueryClassificationConfig {
    /// Enable automatic query classification. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Classification rules evaluated in priority order.
    #[serde(default)]
    pub rules: Vec<ClassificationRule>,
}

/// A single classification rule mapping message patterns to a model hint.
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HeartbeatConfig {
    /// Enable periodic heartbeat pings. Default: `false`.
    pub enabled: bool,
    /// Interval in minutes between heartbeat pings. Default: `30`.
    pub interval_minutes: u32,
    /// Optional fallback task text when `HEARTBEAT.md` has no task entries.
    #[serde(default)]
    pub message: Option<String>,
    /// Optional delivery channel for heartbeat output (for example: `telegram`).
    #[serde(default, alias = "channel")]
    pub target: Option<String>,
    /// Optional delivery recipient/chat identifier (required when `target` is set).
    #[serde(default, alias = "recipient")]
    pub to: Option<String>,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 30,
            message: None,
            target: None,
            to: None,
        }
    }
}

// ── Goal Loop Config ────────────────────────────────────────────

/// Configuration for the autonomous goal loop engine (`[goal_loop]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GoalLoopConfig {
    /// Enable autonomous goal execution. Default: `false`.
    pub enabled: bool,
    /// Interval in minutes between goal loop cycles. Default: `10`.
    pub interval_minutes: u32,
    /// Timeout in seconds for a single step execution. Default: `120`.
    pub step_timeout_secs: u64,
    /// Maximum steps to execute per cycle. Default: `3`.
    pub max_steps_per_cycle: u32,
    /// Optional channel to deliver goal events to (e.g. "lark", "telegram").
    #[serde(default)]
    pub channel: Option<String>,
    /// Optional recipient/chat_id for goal event delivery.
    #[serde(default)]
    pub target: Option<String>,
}

impl Default for GoalLoopConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 10,
            step_timeout_secs: 120,
            max_steps_per_cycle: 3,
            channel: None,
            target: None,
        }
    }
}

// ── Cron ────────────────────────────────────────────────────────

/// Cron job configuration (`[cron]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CronConfig {
    /// Enable the cron subsystem. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Maximum number of historical cron run records to retain. Default: `50`.
    #[serde(default = "default_max_run_history")]
    pub max_run_history: u32,
}

fn default_max_run_history() -> u32 {
    50
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_run_history: default_max_run_history(),
        }
    }
}

// ── Tunnel ──────────────────────────────────────────────────────

/// Tunnel configuration for exposing the gateway publicly (`[tunnel]` section).
///
/// Supported providers: `"none"` (default), `"cloudflare"`, `"tailscale"`, `"ngrok"`, `"custom"`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TunnelConfig {
    /// Tunnel provider: `"none"`, `"cloudflare"`, `"tailscale"`, `"ngrok"`, or `"custom"`. Default: `"none"`.
    pub provider: String,

    /// Cloudflare Tunnel configuration (used when `provider = "cloudflare"`).
    #[serde(default)]
    pub cloudflare: Option<CloudflareTunnelConfig>,

    /// Tailscale Funnel/Serve configuration (used when `provider = "tailscale"`).
    #[serde(default)]
    pub tailscale: Option<TailscaleTunnelConfig>,

    /// ngrok tunnel configuration (used when `provider = "ngrok"`).
    #[serde(default)]
    pub ngrok: Option<NgrokTunnelConfig>,

    /// Custom tunnel command configuration (used when `provider = "custom"`).
    #[serde(default)]
    pub custom: Option<CustomTunnelConfig>,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            cloudflare: None,
            tailscale: None,
            ngrok: None,
            custom: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloudflareTunnelConfig {
    /// Cloudflare Tunnel token (from Zero Trust dashboard)
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TailscaleTunnelConfig {
    /// Use Tailscale Funnel (public internet) vs Serve (tailnet only)
    #[serde(default)]
    pub funnel: bool,
    /// Optional hostname override
    pub hostname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NgrokTunnelConfig {
    /// ngrok auth token
    pub auth_token: String,
    /// Optional custom domain
    pub domain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CustomTunnelConfig {
    /// Command template to start the tunnel. Use {port} and {host} placeholders.
    /// Example: "bore local {port} --to bore.pub"
    pub start_command: String,
    /// Optional URL to check tunnel health
    pub health_url: Option<String>,
    /// Optional regex to extract public URL from command stdout
    pub url_pattern: Option<String>,
}

// ── Channels ─────────────────────────────────────────────────────

struct ConfigWrapper<T: ChannelConfig>(std::marker::PhantomData<T>);

impl<T: ChannelConfig> ConfigWrapper<T> {
    fn new(_: Option<&T>) -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: ChannelConfig> crate::config::traits::ConfigHandle for ConfigWrapper<T> {
    fn name(&self) -> &'static str {
        T::name()
    }
    fn desc(&self) -> &'static str {
        T::desc()
    }
}

/// Top-level channel configurations (`[channels_config]` section).
///
/// Each channel sub-section (e.g. `telegram`, `discord`) is optional;
/// setting it to `Some(...)` enables that channel.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelsConfig {
    /// Enable the CLI interactive channel. Default: `true`.
    pub cli: bool,
    /// ACP (Agent Client Protocol) channel configuration.
    pub acp: Option<AcpConfig>,
    /// Telegram bot channel configuration.
    pub telegram: Option<TelegramConfig>,
    /// Discord bot channel configuration.
    pub discord: Option<DiscordConfig>,
    /// Slack bot channel configuration.
    pub slack: Option<SlackConfig>,
    /// Mattermost bot channel configuration.
    pub mattermost: Option<MattermostConfig>,
    /// Webhook channel configuration.
    pub webhook: Option<WebhookConfig>,
    /// iMessage channel configuration (macOS only).
    pub imessage: Option<IMessageConfig>,
    /// Matrix channel configuration.
    pub matrix: Option<MatrixConfig>,
    /// Signal channel configuration.
    pub signal: Option<SignalConfig>,
    /// WhatsApp channel configuration (Cloud API or Web mode).
    pub whatsapp: Option<WhatsAppConfig>,
    /// Linq Partner API channel configuration.
    pub linq: Option<LinqConfig>,
    /// GitHub channel configuration.
    pub github: Option<GitHubConfig>,
    /// BlueBubbles iMessage bridge channel configuration.
    pub bluebubbles: Option<BlueBubblesConfig>,
    /// WATI WhatsApp Business API channel configuration.
    pub wati: Option<WatiConfig>,
    /// Nextcloud Talk bot channel configuration.
    pub nextcloud_talk: Option<NextcloudTalkConfig>,
    /// Email channel configuration.
    pub email: Option<crate::channels::email_channel::EmailConfig>,
    /// IRC channel configuration.
    pub irc: Option<IrcConfig>,
    /// Lark channel configuration.
    pub lark: Option<LarkConfig>,
    /// Feishu channel configuration.
    pub feishu: Option<FeishuConfig>,
    /// DingTalk channel configuration.
    pub dingtalk: Option<DingTalkConfig>,
    /// Napcat QQ protocol channel configuration.
    /// Also accepts legacy key `[channels_config.onebot]` for OneBot v11 compatibility.
    #[serde(alias = "onebot")]
    pub napcat: Option<NapcatConfig>,
    /// QQ Official Bot channel configuration.
    pub qq: Option<QQConfig>,
    pub nostr: Option<NostrConfig>,
    /// ClawdTalk voice channel configuration.
    pub clawdtalk: Option<crate::channels::clawdtalk::ClawdTalkConfig>,
    /// ACK emoji reaction policy overrides for channels that support message reactions.
    ///
    /// Use this table to control reaction enable/disable, emoji pools, and conditional rules
    /// without hardcoding behavior in channel implementations.
    #[serde(default)]
    pub ack_reaction: AckReactionChannelsConfig,
    /// Base timeout in seconds for processing a single channel message (LLM + tools).
    /// Runtime uses this as a per-turn budget that scales with tool-loop depth
    /// (up to 4x, capped) so one slow/retried model call does not consume the
    /// entire conversation budget.
    /// Default: 300s for on-device LLMs (Ollama) which are slower than cloud APIs.
    #[serde(default = "default_channel_message_timeout_secs")]
    pub message_timeout_secs: u64,
}

impl ChannelsConfig {
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
                Box::new(ConfigWrapper::new(self.github.as_ref())),
                self.github.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.bluebubbles.as_ref())),
                self.bluebubbles.is_some(),
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
                Box::new(ConfigWrapper::new(self.napcat.as_ref())),
                self.napcat.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.qq.as_ref())),
                self.qq
                    .as_ref()
                    .is_some_and(|qq| qq.receive_mode == QQReceiveMode::Websocket)
            ),
            (
                Box::new(ConfigWrapper::new(self.nostr.as_ref())),
                self.nostr.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.acp.as_ref())),
                self.acp.is_some(),
            ),
            (
                Box::new(ConfigWrapper::new(self.clawdtalk.as_ref())),
                self.clawdtalk.is_some(),
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

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            cli: true,
            acp: None,
            telegram: None,
            discord: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: None,
            matrix: None,
            signal: None,
            whatsapp: None,
            linq: None,
            github: None,
            bluebubbles: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            irc: None,
            lark: None,
            feishu: None,
            dingtalk: None,
            napcat: None,
            qq: None,
            nostr: None,
            clawdtalk: None,
            ack_reaction: AckReactionChannelsConfig::default(),
            message_timeout_secs: default_channel_message_timeout_secs(),
        }
    }
}

/// Streaming mode for channels that support progressive message updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StreamMode {
    /// No streaming -- send the complete response as a single message (default).
    #[default]
    Off,
    /// Update a draft message with every flush interval.
    Partial,
}

/// Progress verbosity for channels that support draft streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProgressMode {
    /// Show all progress lines (thinking rounds, tool-count lines, tool lifecycle).
    Verbose,
    /// Show only tool lifecycle lines (start + completion).
    #[default]
    Compact,
    /// Suppress progress lines and stream only final answer text.
    Off,
}

fn default_draft_update_interval_ms() -> u64 {
    1000
}

fn default_ack_enabled() -> bool {
    true
}

/// Group-chat reply trigger mode for channels that support mention gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupReplyMode {
    /// Reply only when the bot is explicitly @-mentioned in group chats.
    MentionOnly,
    /// Reply to every message in group chats.
    AllMessages,
}

impl GroupReplyMode {
    #[must_use]
    pub fn requires_mention(self) -> bool {
        matches!(self, Self::MentionOnly)
    }
}

/// Advanced group-chat trigger controls.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct GroupReplyConfig {
    /// Optional explicit trigger mode.
    ///
    /// If omitted, channel-specific legacy behavior is used for compatibility.
    #[serde(default)]
    pub mode: Option<GroupReplyMode>,
    /// Sender IDs that always trigger group replies.
    ///
    /// These IDs bypass mention gating in group chats, but do not bypass the
    /// channel-level inbound allowlist (`allowed_users` / equivalents).
    #[serde(default)]
    pub allowed_sender_ids: Vec<String>,
}

/// Reaction selection strategy for ACK emoji pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AckReactionStrategy {
    /// Select uniformly from the available emoji pool.
    #[default]
    Random,
    /// Always select the first emoji in the available pool.
    First,
}

/// Rule action for ACK reaction matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AckReactionRuleAction {
    /// React using the configured emoji pool.
    #[default]
    React,
    /// Suppress ACK reactions when this rule matches.
    Suppress,
}

/// Chat context selector for ACK emoji reaction rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AckReactionChatType {
    /// Direct/private chat context.
    Direct,
    /// Group/channel chat context.
    Group,
}

/// Conditional ACK emoji reaction rule.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AckReactionRuleConfig {
    /// Rule enable switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Match when message contains any keyword (case-insensitive).
    #[serde(default)]
    pub contains_any: Vec<String>,
    /// Match only when message contains all keywords (case-insensitive).
    #[serde(default)]
    pub contains_all: Vec<String>,
    /// Match only when message contains none of these keywords (case-insensitive).
    #[serde(default)]
    pub contains_none: Vec<String>,
    /// Match when any regex pattern matches message text.
    #[serde(default)]
    pub regex_any: Vec<String>,
    /// Match only when all regex patterns match message text.
    #[serde(default)]
    pub regex_all: Vec<String>,
    /// Match only when none of these regex patterns match message text.
    #[serde(default)]
    pub regex_none: Vec<String>,
    /// Match only for these sender IDs. `*` matches any sender.
    #[serde(default)]
    pub sender_ids: Vec<String>,
    /// Match only for these chat/channel IDs. `*` matches any chat.
    #[serde(default)]
    pub chat_ids: Vec<String>,
    /// Match only for selected chat types; empty means no chat-type constraint.
    #[serde(default)]
    pub chat_types: Vec<AckReactionChatType>,
    /// Match only for selected locale tags; supports prefix matching (`zh`, `zh_cn`).
    #[serde(default)]
    pub locale_any: Vec<String>,
    /// Rule action (`react` or `suppress`).
    #[serde(default)]
    pub action: AckReactionRuleAction,
    /// Optional probabilistic gate in `[0.0, 1.0]` for this rule.
    /// When omitted, falls back to channel-level `sample_rate`.
    #[serde(default)]
    pub sample_rate: Option<f64>,
    /// Per-rule strategy override (falls back to parent strategy when omitted).
    #[serde(default)]
    pub strategy: Option<AckReactionStrategy>,
    /// Emoji pool used when this rule matches.
    #[serde(default)]
    pub emojis: Vec<String>,
}

impl Default for AckReactionRuleConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            contains_any: Vec::new(),
            contains_all: Vec::new(),
            contains_none: Vec::new(),
            regex_any: Vec::new(),
            regex_all: Vec::new(),
            regex_none: Vec::new(),
            sender_ids: Vec::new(),
            chat_ids: Vec::new(),
            chat_types: Vec::new(),
            locale_any: Vec::new(),
            action: AckReactionRuleAction::React,
            sample_rate: None,
            strategy: None,
            emojis: Vec::new(),
        }
    }
}

/// Per-channel ACK emoji reaction policy.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AckReactionConfig {
    /// Global enable switch for ACK reactions on this channel.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Default emoji selection strategy.
    #[serde(default)]
    pub strategy: AckReactionStrategy,
    /// Probabilistic gate in `[0.0, 1.0]` applied to default fallback selection.
    /// Rule-level `sample_rate` overrides this for matched rules.
    #[serde(default = "default_ack_reaction_sample_rate")]
    pub sample_rate: f64,
    /// Default emoji pool. When empty, channel built-in defaults are used.
    #[serde(default)]
    pub emojis: Vec<String>,
    /// Conditional rules evaluated in order.
    #[serde(default)]
    pub rules: Vec<AckReactionRuleConfig>,
}

impl Default for AckReactionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: AckReactionStrategy::Random,
            sample_rate: default_ack_reaction_sample_rate(),
            emojis: Vec::new(),
            rules: Vec::new(),
        }
    }
}

fn default_ack_reaction_sample_rate() -> f64 {
    1.0
}

/// ACK reaction policy table under `[channels_config.ack_reaction]`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AckReactionChannelsConfig {
    /// Telegram ACK reaction policy.
    #[serde(default)]
    pub telegram: Option<AckReactionConfig>,
    /// Discord ACK reaction policy.
    #[serde(default)]
    pub discord: Option<AckReactionConfig>,
    /// Lark ACK reaction policy.
    #[serde(default)]
    pub lark: Option<AckReactionConfig>,
    /// Feishu ACK reaction policy.
    #[serde(default)]
    pub feishu: Option<AckReactionConfig>,
}

fn resolve_group_reply_mode(
    group_reply: Option<&GroupReplyConfig>,
    legacy_mention_only: Option<bool>,
    default_mode: GroupReplyMode,
) -> GroupReplyMode {
    if let Some(mode) = group_reply.and_then(|cfg| cfg.mode) {
        return mode;
    }
    if let Some(mention_only) = legacy_mention_only {
        return if mention_only {
            GroupReplyMode::MentionOnly
        } else {
            GroupReplyMode::AllMessages
        };
    }
    default_mode
}

fn clone_group_reply_allowed_sender_ids(group_reply: Option<&GroupReplyConfig>) -> Vec<String> {
    group_reply
        .map(|cfg| cfg.allowed_sender_ids.clone())
        .unwrap_or_default()
}

/// Telegram bot channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TelegramConfig {
    /// Telegram Bot API token (from @BotFather).
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
    /// Draft progress verbosity for streaming updates.
    #[serde(default)]
    pub progress_mode: ProgressMode,
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
    /// Optional custom base URL for Telegram-compatible APIs.
    /// Defaults to "https://api.telegram.org" when omitted.
    /// Example for Bale messenger: "https://tapi.bale.ai"
    #[serde(default)]
    pub base_url: Option<String>,
    /// When true, send emoji reaction acknowledgments (⚡️, 👌, 👀, 🔥, 👍) to incoming messages.
    /// When false, no reaction is sent. Default is true.
    #[serde(default = "default_ack_enabled")]
    pub ack_enabled: bool,
}

impl ChannelConfig for TelegramConfig {
    fn name() -> &'static str {
        "Telegram"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

impl TelegramConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(
            self.group_reply.as_ref(),
            Some(self.mention_only),
            GroupReplyMode::AllMessages,
        )
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

/// Discord bot channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiscordConfig {
    /// Discord bot token (from Discord Developer Portal).
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
    /// When true, only respond to messages that @-mention the bot.
    /// Other messages in the guild are silently ignored.
    #[serde(default)]
    pub mention_only: bool,
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
}

impl ChannelConfig for DiscordConfig {
    fn name() -> &'static str {
        "Discord"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

impl DiscordConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(
            self.group_reply.as_ref(),
            Some(self.mention_only),
            GroupReplyMode::AllMessages,
        )
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

/// Slack bot channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SlackConfig {
    /// Slack bot OAuth token (xoxb-...).
    pub bot_token: String,
    /// Slack app-level token for Socket Mode (xapp-...).
    pub app_token: Option<String>,
    /// Optional channel ID to restrict the bot to a single channel.
    /// Omit (or set `"*"`) to listen across all accessible channels.
    /// Ignored when `channel_ids` is non-empty.
    pub channel_id: Option<String>,
    /// Explicit list of channel/DM IDs to listen on simultaneously.
    /// Takes precedence over `channel_id`. Empty = fall back to `channel_id`.
    #[serde(default)]
    pub channel_ids: Vec<String>,
    /// Allowed Slack user IDs. Empty = deny all.
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
}

impl ChannelConfig for SlackConfig {
    fn name() -> &'static str {
        "Slack"
    }
    fn desc() -> &'static str {
        "connect your bot"
    }
}

impl SlackConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(self.group_reply.as_ref(), None, GroupReplyMode::AllMessages)
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

/// Mattermost bot channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MattermostConfig {
    /// Mattermost server URL (e.g. `"https://mattermost.example.com"`).
    pub url: String,
    /// Mattermost bot access token.
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
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
}

impl ChannelConfig for MattermostConfig {
    fn name() -> &'static str {
        "Mattermost"
    }
    fn desc() -> &'static str {
        "connect to your bot"
    }
}

impl MattermostConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(
            self.group_reply.as_ref(),
            Some(self.mention_only.unwrap_or(false)),
            GroupReplyMode::AllMessages,
        )
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

/// Webhook channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebhookConfig {
    /// Port to listen on for incoming webhooks.
    pub port: u16,
    /// Optional shared secret for webhook signature verification.
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IMessageConfig {
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MatrixConfig {
    /// Matrix homeserver URL (e.g. `"https://matrix.org"`).
    pub homeserver: String,
    /// Matrix access token for the bot account.
    pub access_token: String,
    /// Optional Matrix user ID (e.g. `"@bot:matrix.org"`).
    #[serde(default)]
    pub user_id: Option<String>,
    /// Optional Matrix device ID.
    #[serde(default)]
    pub device_id: Option<String>,
    /// Matrix room ID to listen in (e.g. `"!abc123:matrix.org"`).
    pub room_id: String,
    /// Allowed Matrix user IDs. Empty = deny all.
    pub allowed_users: Vec<String>,
    /// When true, only respond to direct rooms, explicit @-mentions, or replies to bot messages.
    #[serde(default)]
    pub mention_only: bool,
}

impl ChannelConfig for MatrixConfig {
    fn name() -> &'static str {
        "Matrix"
    }
    fn desc() -> &'static str {
        "self-hosted chat"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SignalConfig {
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
}

impl ChannelConfig for SignalConfig {
    fn name() -> &'static str {
        "Signal"
    }
    fn desc() -> &'static str {
        "An open-source, encrypted messaging service"
    }
}

/// WhatsApp channel configuration (Cloud API or Web mode).
///
/// Set `phone_number_id` for Cloud API mode, or `session_path` for Web mode.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WhatsAppConfig {
    /// Access token from Meta Business Suite (Cloud API mode)
    #[serde(default)]
    pub access_token: Option<String>,
    /// Phone number ID from Meta Business API (Cloud API mode)
    #[serde(default)]
    pub phone_number_id: Option<String>,
    /// Webhook verify token (you define this, Meta sends it back for verification)
    /// Only used in Cloud API mode
    #[serde(default)]
    pub verify_token: Option<String>,
    /// App secret from Meta Business Suite (for webhook signature verification)
    /// Can also be set via `ZEROCLAW_WHATSAPP_APP_SECRET` environment variable
    /// Only used in Cloud API mode
    #[serde(default)]
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
}

impl ChannelConfig for WhatsAppConfig {
    fn name() -> &'static str {
        "WhatsApp"
    }
    fn desc() -> &'static str {
        "Business Cloud API"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LinqConfig {
    /// Linq Partner API token (Bearer auth)
    pub api_token: String,
    /// Phone number to send from (E.164 format)
    pub from_phone: String,
    /// Webhook signing secret for signature verification
    #[serde(default)]
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

/// GitHub channel configuration (webhook receive + issue/PR comment send).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GitHubConfig {
    /// GitHub token used for outbound API calls.
    ///
    /// Supports fine-grained PAT or installation token with `issues:write` / `pull_requests:write`.
    pub access_token: String,
    /// Optional webhook secret to verify `X-Hub-Signature-256`.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// Optional GitHub API base URL (for GHES).
    /// Defaults to `https://api.github.com` when omitted.
    #[serde(default)]
    pub api_base_url: Option<String>,
    /// Allowed repositories (`owner/repo`), `owner/*`, or `*`.
    /// Empty list denies all repositories.
    #[serde(default)]
    pub allowed_repos: Vec<String>,
}

impl ChannelConfig for GitHubConfig {
    fn name() -> &'static str {
        "GitHub"
    }
    fn desc() -> &'static str {
        "issues/PR comments via webhook + REST API"
    }
}

/// BlueBubbles iMessage bridge channel configuration.
///
/// BlueBubbles is a self-hosted macOS server that exposes iMessage via a
/// REST API and webhook push notifications. See <https://bluebubbles.app>.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct BlueBubblesConfig {
    /// BlueBubbles server URL (e.g. `http://192.168.1.100:1234` or an ngrok URL).
    pub server_url: String,
    /// BlueBubbles server password.
    pub password: String,
    /// Allowed sender handles (phone numbers or Apple IDs). Use `["*"]` to allow all.
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Optional shared secret to authenticate inbound webhooks.
    /// If set, incoming requests must include `Authorization: Bearer <secret>`.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// Sender handles to silently ignore (e.g. suppress echoed outbound messages).
    #[serde(default)]
    pub ignore_senders: Vec<String>,
}

impl std::fmt::Debug for BlueBubblesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_server_url = redact_url_userinfo_for_debug(&self.server_url);
        f.debug_struct("BlueBubblesConfig")
            .field("server_url", &redacted_server_url)
            .field("password", &"[REDACTED]")
            .field("allowed_senders", &self.allowed_senders)
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

fn redact_url_userinfo_for_debug(raw: &str) -> String {
    let fallback = || {
        let Some(at) = raw.rfind('@') else {
            return raw.to_string();
        };
        let left = &raw[..at];
        if left.contains('/') || left.contains('?') || left.contains('#') {
            return raw.to_string();
        }
        format!("[REDACTED]@{}", &raw[at + 1..])
    };

    let Some(scheme_idx) = raw.find("://") else {
        return fallback();
    };

    let auth_start = scheme_idx + 3;
    let rest = &raw[auth_start..];
    let auth_end_rel = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..auth_end_rel];

    let Some(at) = authority.rfind('@') else {
        return raw.to_string();
    };

    let host = &authority[at + 1..];
    let mut sanitized = String::with_capacity(raw.len());
    sanitized.push_str(&raw[..auth_start]);
    sanitized.push_str("[REDACTED]@");
    sanitized.push_str(host);
    sanitized.push_str(&rest[auth_end_rel..]);
    sanitized
}

impl ChannelConfig for BlueBubblesConfig {
    fn name() -> &'static str {
        "BlueBubbles"
    }
    fn desc() -> &'static str {
        "iMessage via BlueBubbles self-hosted macOS server"
    }
}

/// WATI WhatsApp Business API channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WatiConfig {
    /// WATI API token (Bearer auth).
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NextcloudTalkConfig {
    /// Nextcloud base URL (e.g. "https://cloud.example.com").
    pub base_url: String,
    /// Bot app token used for OCS API bearer auth.
    pub app_token: String,
    /// Shared secret for webhook signature verification.
    ///
    /// Can also be set via `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET`.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// Allowed Nextcloud actor IDs (`[]` = deny all, `"*"` = allow all).
    #[serde(default)]
    pub allowed_users: Vec<String>,
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

/// IRC channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IrcConfig {
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
    pub server_password: Option<String>,
    /// NickServ IDENTIFY password
    pub nickserv_password: Option<String>,
    /// SASL PLAIN password (IRCv3)
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LarkReceiveMode {
    #[default]
    Websocket,
    Webhook,
}

pub fn default_lark_draft_update_interval_ms() -> u64 {
    3000
}

pub fn default_lark_max_draft_edits() -> u32 {
    20
}

/// Lark/Feishu configuration for messaging integration.
/// Lark is the international version; Feishu is the Chinese version.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LarkConfig {
    /// App ID from Lark/Feishu developer console
    pub app_id: String,
    /// App Secret from Lark/Feishu developer console
    pub app_secret: String,
    /// Encrypt key for webhook message decryption (optional)
    #[serde(default)]
    pub encrypt_key: Option<String>,
    /// Verification token for webhook validation (optional)
    #[serde(default)]
    pub verification_token: Option<String>,
    /// Allowed user IDs or union IDs (empty = deny all, "*" = allow all)
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// When true, only respond to messages that @-mention the bot in groups.
    /// Direct messages are always processed.
    #[serde(default)]
    pub mention_only: bool,
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
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
    /// Minimum interval (ms) between draft message edits. Default: 3000.
    #[serde(default = "default_lark_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// Maximum number of edits per draft message before stopping updates.
    #[serde(default = "default_lark_max_draft_edits")]
    pub max_draft_edits: u32,
}

impl ChannelConfig for LarkConfig {
    fn name() -> &'static str {
        "Lark"
    }
    fn desc() -> &'static str {
        "Lark Bot"
    }
}

impl LarkConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(
            self.group_reply.as_ref(),
            Some(self.mention_only),
            GroupReplyMode::AllMessages,
        )
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

/// Feishu configuration for messaging integration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FeishuConfig {
    /// App ID from Feishu developer console
    pub app_id: String,
    /// App Secret from Feishu developer console
    pub app_secret: String,
    /// Encrypt key for webhook message decryption (optional)
    #[serde(default)]
    pub encrypt_key: Option<String>,
    /// Verification token for webhook validation (optional)
    #[serde(default)]
    pub verification_token: Option<String>,
    /// Allowed user IDs or union IDs (empty = deny all, "*" = allow all)
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Group-chat trigger controls.
    #[serde(default)]
    pub group_reply: Option<GroupReplyConfig>,
    /// Event receive mode: "websocket" (default) or "webhook"
    #[serde(default)]
    pub receive_mode: LarkReceiveMode,
    /// HTTP port for webhook mode only. Must be set when receive_mode = "webhook".
    /// Not required (and ignored) for websocket mode.
    #[serde(default)]
    pub port: Option<u16>,
    /// Minimum interval between streaming draft edits (milliseconds).
    #[serde(default = "default_lark_draft_update_interval_ms")]
    pub draft_update_interval_ms: u64,
    /// Maximum number of draft edits per message before finalizing.
    #[serde(default = "default_lark_max_draft_edits")]
    pub max_draft_edits: u32,
}

impl ChannelConfig for FeishuConfig {
    fn name() -> &'static str {
        "Feishu"
    }
    fn desc() -> &'static str {
        "Feishu Bot"
    }
}

impl FeishuConfig {
    #[must_use]
    pub fn effective_group_reply_mode(&self) -> GroupReplyMode {
        resolve_group_reply_mode(self.group_reply.as_ref(), None, GroupReplyMode::AllMessages)
    }

    #[must_use]
    pub fn group_reply_allowed_sender_ids(&self) -> Vec<String> {
        clone_group_reply_allowed_sender_ids(self.group_reply.as_ref())
    }
}

// ── Security Config ─────────────────────────────────────────────────

/// Security configuration for sandboxing, resource limits, and audit logging
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct SecurityConfig {
    /// Sandbox configuration
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Resource limits
    #[serde(default)]
    pub resources: ResourceLimitsConfig,

    /// Audit logging configuration
    #[serde(default)]
    pub audit: AuditConfig,

    /// OTP gating configuration for sensitive actions/domains.
    #[serde(default)]
    pub otp: OtpConfig,

    /// Custom security role definitions used for user-level tool authorization.
    #[serde(default)]
    pub roles: Vec<SecurityRoleConfig>,

    /// Emergency-stop state machine configuration.
    #[serde(default)]
    pub estop: EstopConfig,

    /// Syscall anomaly detection profile for daemon shell/process execution.
    #[serde(default)]
    pub syscall_anomaly: SyscallAnomalyConfig,

    /// Lightweight statistical filter for adversarial suffixes (opt-in).
    #[serde(default)]
    pub perplexity_filter: PerplexityFilterConfig,

    /// Outbound credential leak guard for channel replies.
    #[serde(default)]
    pub outbound_leak_guard: OutboundLeakGuardConfig,

    /// Shared URL access policy for network-enabled tools.
    #[serde(default)]
    pub url_access: UrlAccessConfig,
}

/// Outbound leak handling mode for channel responses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OutboundLeakGuardAction {
    /// Redact suspicious credentials and continue delivery.
    #[default]
    Redact,
    /// Block delivery when suspicious credentials are detected.
    Block,
}

/// Outbound credential leak guard configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutboundLeakGuardConfig {
    /// Enable outbound credential leak scanning for channel responses.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Action to take when potential credentials are detected.
    #[serde(default)]
    pub action: OutboundLeakGuardAction,

    /// Detection sensitivity (0.0-1.0, higher = more aggressive).
    #[serde(default = "default_outbound_leak_guard_sensitivity")]
    pub sensitivity: f64,
}

fn default_outbound_leak_guard_sensitivity() -> f64 {
    0.7
}

impl Default for OutboundLeakGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            action: OutboundLeakGuardAction::Redact,
            sensitivity: default_outbound_leak_guard_sensitivity(),
        }
    }
}

/// Lightweight perplexity-style filter configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PerplexityFilterConfig {
    /// Enable probabilistic adversarial suffix filtering before provider calls.
    #[serde(default)]
    pub enable_perplexity_filter: bool,

    /// Character-class bigram perplexity threshold for anomaly blocking.
    #[serde(default = "default_perplexity_threshold")]
    pub perplexity_threshold: f64,

    /// Number of trailing characters sampled for suffix anomaly scoring.
    #[serde(default = "default_perplexity_suffix_window_chars")]
    pub suffix_window_chars: usize,

    /// Minimum input length before running the perplexity filter.
    #[serde(default = "default_perplexity_min_prompt_chars")]
    pub min_prompt_chars: usize,

    /// Minimum punctuation ratio in the sampled suffix required to block.
    #[serde(default = "default_perplexity_symbol_ratio_threshold")]
    pub symbol_ratio_threshold: f64,
}

fn default_perplexity_threshold() -> f64 {
    18.0
}

fn default_perplexity_suffix_window_chars() -> usize {
    64
}

fn default_perplexity_min_prompt_chars() -> usize {
    32
}

fn default_perplexity_symbol_ratio_threshold() -> f64 {
    0.20
}

impl Default for PerplexityFilterConfig {
    fn default() -> Self {
        Self {
            enable_perplexity_filter: false,
            perplexity_threshold: default_perplexity_threshold(),
            suffix_window_chars: default_perplexity_suffix_window_chars(),
            min_prompt_chars: default_perplexity_min_prompt_chars(),
            symbol_ratio_threshold: default_perplexity_symbol_ratio_threshold(),
        }
    }
}

/// Shared URL validation configuration used by network tools.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UrlAccessConfig {
    /// Block private/local IPs and hostnames by default.
    #[serde(default = "default_true")]
    pub block_private_ip: bool,

    /// Explicit CIDR ranges that bypass private/local-IP blocking.
    #[serde(default)]
    pub allow_cidrs: Vec<String>,

    /// Explicit domain patterns that bypass private/local-IP blocking.
    /// Supports exact, `*.example.com`, and `*`.
    #[serde(default)]
    pub allow_domains: Vec<String>,

    /// Allow loopback host/IP access (`localhost`, `127.0.0.1`, `::1`).
    #[serde(default)]
    pub allow_loopback: bool,

    /// Require explicit human confirmation before first-time access to an
    /// unseen domain. Confirmed domains are persisted in `approved_domains`.
    #[serde(default)]
    pub require_first_visit_approval: bool,

    /// Enforce a global domain allowlist in addition to per-tool allowlists.
    /// When enabled, hosts must match `domain_allowlist`.
    #[serde(default)]
    pub enforce_domain_allowlist: bool,

    /// Global trusted domain allowlist shared by all URL-based network tools.
    /// Supports exact, `*.example.com`, and `*`.
    #[serde(default)]
    pub domain_allowlist: Vec<String>,

    /// Global domain blocklist shared by all URL-based network tools.
    /// Supports exact, `*.example.com`, and `*`. Takes priority over allowlists.
    #[serde(default)]
    pub domain_blocklist: Vec<String>,

    /// Persisted first-visit approvals granted by a human operator.
    /// Supports exact, `*.example.com`, and `*`.
    #[serde(default)]
    pub approved_domains: Vec<String>,
}

impl Default for UrlAccessConfig {
    fn default() -> Self {
        Self {
            block_private_ip: true,
            allow_cidrs: Vec::new(),
            allow_domains: Vec::new(),
            allow_loopback: false,
            require_first_visit_approval: false,
            enforce_domain_allowlist: false,
            domain_allowlist: Vec::new(),
            domain_blocklist: Vec::new(),
            approved_domains: Vec::new(),
        }
    }
}

/// OTP validation strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, JsonSchema, PartialEq, Eq)]
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

/// Channel delivery mode for OTP challenges.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum OtpChallengeDelivery {
    /// Send OTP challenge in direct message/private channel.
    #[default]
    Dm,
    /// Send OTP challenge in thread where supported.
    Thread,
    /// Send OTP challenge as ephemeral message where supported.
    Ephemeral,
}

/// Security OTP configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OtpConfig {
    /// Enable OTP gating. Defaults to enabled.
    #[serde(default = "default_otp_enabled")]
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

    /// Delivery mode for OTP challenge prompts in chat channels.
    #[serde(default)]
    pub challenge_delivery: OtpChallengeDelivery,

    /// Maximum time a challenge remains valid, in seconds.
    #[serde(default = "default_otp_challenge_timeout_secs")]
    pub challenge_timeout_secs: u64,

    /// Maximum OTP attempts allowed per challenge.
    #[serde(default = "default_otp_challenge_max_attempts")]
    pub challenge_max_attempts: u8,
}

/// Custom role definition for user-level authorization.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SecurityRoleConfig {
    /// Stable role name used by user records.
    pub name: String,

    /// Optional human-readable description.
    #[serde(default)]
    pub description: String,

    /// Explicit allowlist of tools for this role.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Explicit denylist of tools for this role.
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Tool names requiring OTP for this role.
    #[serde(default)]
    pub totp_gated: Vec<String>,

    /// Optional parent role name used for inheritance.
    #[serde(default)]
    pub inherits: Option<String>,

    /// Role-scoped domain patterns requiring OTP.
    #[serde(default)]
    pub gated_domains: Vec<String>,

    /// Role-scoped domain categories requiring OTP.
    #[serde(default)]
    pub gated_domain_categories: Vec<String>,
}

fn default_otp_enabled() -> bool {
    true
}

fn default_otp_token_ttl_secs() -> u64 {
    30
}

fn default_otp_cache_valid_secs() -> u64 {
    300
}

fn default_otp_challenge_timeout_secs() -> u64 {
    120
}

fn default_otp_challenge_max_attempts() -> u8 {
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
            enabled: default_otp_enabled(),
            method: OtpMethod::Totp,
            token_ttl_secs: default_otp_token_ttl_secs(),
            cache_valid_secs: default_otp_cache_valid_secs(),
            gated_actions: default_otp_gated_actions(),
            gated_domains: Vec::new(),
            gated_domain_categories: Vec::new(),
            challenge_delivery: OtpChallengeDelivery::Dm,
            challenge_timeout_secs: default_otp_challenge_timeout_secs(),
            challenge_max_attempts: default_otp_challenge_max_attempts(),
        }
    }
}

/// Emergency stop configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

/// Syscall anomaly detection configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SyscallAnomalyConfig {
    /// Enable syscall anomaly detection.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Treat denied syscall lines as anomalies even when syscall is in baseline.
    #[serde(default)]
    pub strict_mode: bool,

    /// Emit anomaly alerts when a syscall appears outside the expected baseline.
    #[serde(default = "default_true")]
    pub alert_on_unknown_syscall: bool,

    /// Allowed denied-syscall events per rolling minute before triggering an alert.
    #[serde(default = "default_syscall_anomaly_max_denied_events_per_minute")]
    pub max_denied_events_per_minute: u32,

    /// Allowed total syscall telemetry events per rolling minute before triggering an alert.
    #[serde(default = "default_syscall_anomaly_max_total_events_per_minute")]
    pub max_total_events_per_minute: u32,

    /// Maximum anomaly alerts emitted per rolling minute (global guardrail).
    #[serde(default = "default_syscall_anomaly_max_alerts_per_minute")]
    pub max_alerts_per_minute: u32,

    /// Cooldown between identical anomaly alerts (seconds).
    #[serde(default = "default_syscall_anomaly_alert_cooldown_secs")]
    pub alert_cooldown_secs: u64,

    /// Path to syscall anomaly log file (relative to ~/.zeroclaw unless absolute).
    #[serde(default = "default_syscall_anomaly_log_path")]
    pub log_path: String,

    /// Expected syscall baseline. Unknown syscall names trigger anomaly when enabled.
    #[serde(default = "default_syscall_anomaly_baseline_syscalls")]
    pub baseline_syscalls: Vec<String>,
}

fn default_syscall_anomaly_max_denied_events_per_minute() -> u32 {
    5
}

fn default_syscall_anomaly_max_total_events_per_minute() -> u32 {
    120
}

fn default_syscall_anomaly_max_alerts_per_minute() -> u32 {
    30
}

fn default_syscall_anomaly_alert_cooldown_secs() -> u64 {
    20
}

fn default_syscall_anomaly_log_path() -> String {
    "syscall-anomalies.log".to_string()
}

fn default_syscall_anomaly_baseline_syscalls() -> Vec<String> {
    vec![
        "read".to_string(),
        "write".to_string(),
        "open".to_string(),
        "openat".to_string(),
        "close".to_string(),
        "stat".to_string(),
        "fstat".to_string(),
        "newfstatat".to_string(),
        "lseek".to_string(),
        "mmap".to_string(),
        "mprotect".to_string(),
        "munmap".to_string(),
        "brk".to_string(),
        "rt_sigaction".to_string(),
        "rt_sigprocmask".to_string(),
        "ioctl".to_string(),
        "fcntl".to_string(),
        "access".to_string(),
        "pipe2".to_string(),
        "dup".to_string(),
        "dup2".to_string(),
        "dup3".to_string(),
        "epoll_create1".to_string(),
        "epoll_ctl".to_string(),
        "epoll_wait".to_string(),
        "poll".to_string(),
        "ppoll".to_string(),
        "select".to_string(),
        "futex".to_string(),
        "clock_gettime".to_string(),
        "nanosleep".to_string(),
        "getpid".to_string(),
        "gettid".to_string(),
        "set_tid_address".to_string(),
        "set_robust_list".to_string(),
        "clone".to_string(),
        "clone3".to_string(),
        "fork".to_string(),
        "execve".to_string(),
        "wait4".to_string(),
        "exit".to_string(),
        "exit_group".to_string(),
        "socket".to_string(),
        "connect".to_string(),
        "accept".to_string(),
        "accept4".to_string(),
        "listen".to_string(),
        "sendto".to_string(),
        "recvfrom".to_string(),
        "sendmsg".to_string(),
        "recvmsg".to_string(),
        "getsockname".to_string(),
        "getpeername".to_string(),
        "setsockopt".to_string(),
        "getsockopt".to_string(),
        "getrandom".to_string(),
        "statx".to_string(),
    ]
}

impl Default for SyscallAnomalyConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            strict_mode: false,
            alert_on_unknown_syscall: default_true(),
            max_denied_events_per_minute: default_syscall_anomaly_max_denied_events_per_minute(),
            max_total_events_per_minute: default_syscall_anomaly_max_total_events_per_minute(),
            max_alerts_per_minute: default_syscall_anomaly_max_alerts_per_minute(),
            alert_cooldown_secs: default_syscall_anomaly_alert_cooldown_secs(),
            log_path: default_syscall_anomaly_log_path(),
            baseline_syscalls: default_syscall_anomaly_baseline_syscalls(),
        }
    }
}

/// Sandbox configuration for OS-level isolation
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
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
    /// No sandboxing (application-layer only)
    None,
}

/// Resource limits for command execution
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DingTalkConfig {
    /// Client ID (AppKey) from DingTalk developer console
    pub client_id: String,
    /// Client Secret (AppSecret) from DingTalk developer console
    pub client_secret: String,
    /// Allowed user IDs (staff IDs). Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl ChannelConfig for DingTalkConfig {
    fn name() -> &'static str {
        "DingTalk"
    }
    fn desc() -> &'static str {
        "DingTalk Stream Mode"
    }
}

/// Napcat channel configuration (QQ via OneBot-compatible API)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NapcatConfig {
    /// Napcat WebSocket endpoint (for example `ws://127.0.0.1:3001`)
    #[serde(alias = "ws_url")]
    pub websocket_url: String,
    /// Optional Napcat HTTP API base URL. If omitted, derived from websocket_url.
    #[serde(default)]
    pub api_base_url: String,
    /// Optional access token (Authorization Bearer token)
    pub access_token: Option<String>,
    /// Allowed user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

impl ChannelConfig for NapcatConfig {
    fn name() -> &'static str {
        "Napcat"
    }
    fn desc() -> &'static str {
        "QQ via Napcat (OneBot)"
    }
}

/// QQ Official Bot configuration (Tencent QQ Bot SDK)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QQReceiveMode {
    Websocket,
    #[default]
    Webhook,
}

/// QQ API environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QQEnvironment {
    #[default]
    Production,
    Sandbox,
}

/// QQ Official Bot configuration (Tencent QQ Bot SDK)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QQConfig {
    /// App ID from QQ Bot developer console
    pub app_id: String,
    /// App Secret from QQ Bot developer console
    pub app_secret: String,
    /// Allowed user IDs. Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_users: Vec<String>,
    /// Event receive mode: "webhook" (default) or "websocket".
    #[serde(default)]
    pub receive_mode: QQReceiveMode,
    /// API environment: "production" (default) or "sandbox".
    #[serde(default)]
    pub environment: QQEnvironment,
}

impl ChannelConfig for QQConfig {
    fn name() -> &'static str {
        "QQ Official"
    }
    fn desc() -> &'static str {
        "Tencent QQ Bot"
    }
}

/// Nostr channel configuration (NIP-04 + NIP-17 private messages)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NostrConfig {
    /// Private key in hex or nsec bech32 format
    pub private_key: String,
    /// Relay URLs (wss://). Defaults to popular public relays if omitted.
    #[serde(default = "default_nostr_relays")]
    pub relays: Vec<String>,
    /// Allowed sender public keys (hex or npub). Empty = deny all, "*" = allow all
    #[serde(default)]
    pub allowed_pubkeys: Vec<String>,
}

impl ChannelConfig for NostrConfig {
    fn name() -> &'static str {
        "Nostr"
    }
    fn desc() -> &'static str {
        "Nostr DMs"
    }
}

pub fn default_nostr_relays() -> Vec<String> {
    vec![
        "wss://relay.damus.io".to_string(),
        "wss://nos.lol".to_string(),
        "wss://relay.primal.net".to_string(),
        "wss://relay.snort.social".to_string(),
    ]
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
            api_key: None,
            api_url: None,
            default_provider: Some(DEFAULT_PROVIDER_NAME.to_string()),
            provider_api: None,
            default_model: Some(DEFAULT_MODEL_NAME.to_string()),
            model_providers: HashMap::new(),
            provider: ProviderConfig::default(),
            default_temperature: 0.7,
            observability: ObservabilityConfig::default(),
            autonomy: AutonomyConfig::default(),
            security: SecurityConfig::default(),
            runtime: RuntimeConfig::default(),
            research: ResearchPhaseConfig::default(),
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            agent: AgentConfig::default(),
            skills: SkillsConfig::default(),
            model_routes: Vec::new(),
            embedding_routes: Vec::new(),
            heartbeat: HeartbeatConfig::default(),
            cron: CronConfig::default(),
            goal_loop: GoalLoopConfig::default(),
            channels_config: ChannelsConfig::default(),
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            web_fetch: WebFetchConfig::default(),
            web_search: WebSearchConfig::default(),
            proxy: ProxyConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            economic: EconomicConfig::default(),
            peripherals: PeripheralsConfig::default(),
            agents: HashMap::new(),
            coordination: CoordinationConfig::default(),
            hooks: HooksConfig::default(),
            plugins: PluginsConfig::default(),
            hardware: HardwareConfig::default(),
            query_classification: QueryClassificationConfig::default(),
            transcription: TranscriptionConfig::default(),
            agents_ipc: AgentsIpcConfig::default(),
            mcp: McpConfig::default(),
            model_support_vision: None,
            wasm: WasmConfig::default(),
        }
    }
}

fn default_config_and_workspace_dirs() -> Result<(PathBuf, PathBuf)> {
    let config_dir = default_config_dir()?;
    Ok((config_dir.clone(), config_dir.join("workspace")))
}

const ACTIVE_WORKSPACE_STATE_FILE: &str = "active_workspace.toml";

#[derive(Debug, Serialize, Deserialize)]
struct ActiveWorkspaceState {
    config_dir: String,
}

fn default_config_dir() -> Result<PathBuf> {
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

    let parsed_dir = PathBuf::from(raw_config_dir);
    let config_dir = if parsed_dir.is_absolute() {
        parsed_dir
    } else {
        default_config_dir.join(parsed_dir)
    };

    // Guard: ignore stale marker paths that no longer exist.
    let config_meta = match fs::metadata(&config_dir).await {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(
                "Ignoring active workspace marker {} because config_dir {} is missing: {error}",
                state_path.display(),
                config_dir.display()
            );
            return Ok(None);
        }
    };
    if !config_meta.is_dir() {
        tracing::warn!(
            "Ignoring active workspace marker {} because config_dir {} is not a directory",
            state_path.display(),
            config_dir.display()
        );
        return Ok(None);
    }

    // Guard: marker must point to an initialized config profile.
    let config_toml_path = config_dir.join("config.toml");
    let config_toml_meta = match fs::metadata(&config_toml_path).await {
        Ok(meta) => meta,
        Err(error) => {
            tracing::warn!(
                "Ignoring active workspace marker {} because {} is missing: {error}",
                state_path.display(),
                config_toml_path.display()
            );
            return Ok(None);
        }
    };
    if !config_toml_meta.is_file() {
        tracing::warn!(
            "Ignoring active workspace marker {} because {} is not a file",
            state_path.display(),
            config_toml_path.display()
        );
        return Ok(None);
    }

    // Guard: if the default config location is not temporary, reject marker paths
    // that point into OS temp storage (typically stale ephemeral sessions).
    if !is_temp_directory(default_config_dir) && is_temp_directory(&config_dir) {
        tracing::warn!(
            "Ignoring active workspace marker {} because config_dir {} points to a temp directory",
            state_path.display(),
            config_dir.display()
        );
        return Ok(None);
    }

    Ok(Some((config_dir.clone(), config_dir.join("workspace"))))
}

pub(crate) async fn persist_active_workspace_config_dir(config_dir: &Path) -> Result<()> {
    let default_config_dir = default_config_dir()?;
    let state_path = active_workspace_state_path(&default_config_dir);

    // Guard: never persist a temp-directory path as the active workspace.
    // This prevents transient test runs or one-off invocations from hijacking
    // the daemon's config resolution.
    #[cfg(not(test))]
    if is_temp_directory(config_dir) {
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

    #[cfg(unix)]
    sync_directory(&default_config_dir).await?;
    #[cfg(not(unix))]
    sync_directory(&default_config_dir)?;
    Ok(())
}

pub(crate) fn resolve_config_dir_for_workspace(workspace_dir: &Path) -> (PathBuf, PathBuf) {
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
pub(crate) async fn resolve_runtime_dirs_for_onboarding() -> Result<(PathBuf, PathBuf)> {
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

async fn resolve_runtime_config_dirs(
    default_zeroclaw_dir: &Path,
    default_workspace_dir: &Path,
) -> Result<(PathBuf, PathBuf, ConfigResolutionSource)> {
    if let Ok(custom_config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let custom_config_dir = custom_config_dir.trim();
        if !custom_config_dir.is_empty() {
            let zeroclaw_dir = PathBuf::from(custom_config_dir);
            return Ok((
                zeroclaw_dir.clone(),
                zeroclaw_dir.join("workspace"),
                ConfigResolutionSource::EnvConfigDir,
            ));
        }
    }

    if let Ok(custom_workspace) = std::env::var("ZEROCLAW_WORKSPACE") {
        if !custom_workspace.is_empty() {
            let (zeroclaw_dir, workspace_dir) =
                resolve_config_dir_for_workspace(&PathBuf::from(custom_workspace));
            return Ok((
                zeroclaw_dir,
                workspace_dir,
                ConfigResolutionSource::EnvWorkspace,
            ));
        }
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

fn decrypt_optional_secret(
    store: &crate::security::SecretStore,
    value: &mut Option<String>,
    field_name: &str,
) -> Result<()> {
    if let Some(raw) = value.clone() {
        if crate::security::SecretStore::is_encrypted(&raw) {
            *value = Some(
                store
                    .decrypt(&raw)
                    .with_context(|| format!("Failed to decrypt {field_name}"))?,
            );
        }
    }
    Ok(())
}

fn decrypt_secret(
    store: &crate::security::SecretStore,
    value: &mut String,
    field_name: &str,
) -> Result<()> {
    if crate::security::SecretStore::is_encrypted(value) {
        *value = store
            .decrypt(value)
            .with_context(|| format!("Failed to decrypt {field_name}"))?;
    }
    Ok(())
}

fn decrypt_vec_secrets(
    store: &crate::security::SecretStore,
    values: &mut [String],
    field_name: &str,
) -> Result<()> {
    for (idx, value) in values.iter_mut().enumerate() {
        if crate::security::SecretStore::is_encrypted(value) {
            *value = store
                .decrypt(value)
                .with_context(|| format!("Failed to decrypt {field_name}[{idx}]"))?;
        }
    }
    Ok(())
}

fn encrypt_optional_secret(
    store: &crate::security::SecretStore,
    value: &mut Option<String>,
    field_name: &str,
) -> Result<()> {
    if let Some(raw) = value.clone() {
        if !crate::security::SecretStore::is_encrypted(&raw) {
            *value = Some(
                store
                    .encrypt(&raw)
                    .with_context(|| format!("Failed to encrypt {field_name}"))?,
            );
        }
    }
    Ok(())
}

fn encrypt_secret(
    store: &crate::security::SecretStore,
    value: &mut String,
    field_name: &str,
) -> Result<()> {
    if !crate::security::SecretStore::is_encrypted(value) {
        *value = store
            .encrypt(value)
            .with_context(|| format!("Failed to encrypt {field_name}"))?;
    }
    Ok(())
}

fn encrypt_vec_secrets(
    store: &crate::security::SecretStore,
    values: &mut [String],
    field_name: &str,
) -> Result<()> {
    for (idx, value) in values.iter_mut().enumerate() {
        if !crate::security::SecretStore::is_encrypted(value) {
            *value = store
                .encrypt(value)
                .with_context(|| format!("Failed to encrypt {field_name}[{idx}]"))?;
        }
    }
    Ok(())
}

fn decrypt_channel_secrets(
    store: &crate::security::SecretStore,
    channels: &mut ChannelsConfig,
) -> Result<()> {
    if let Some(ref mut telegram) = channels.telegram {
        decrypt_secret(
            store,
            &mut telegram.bot_token,
            "config.channels_config.telegram.bot_token",
        )?;
    }
    if let Some(ref mut discord) = channels.discord {
        decrypt_secret(
            store,
            &mut discord.bot_token,
            "config.channels_config.discord.bot_token",
        )?;
    }
    if let Some(ref mut slack) = channels.slack {
        decrypt_secret(
            store,
            &mut slack.bot_token,
            "config.channels_config.slack.bot_token",
        )?;
        decrypt_optional_secret(
            store,
            &mut slack.app_token,
            "config.channels_config.slack.app_token",
        )?;
    }
    if let Some(ref mut mattermost) = channels.mattermost {
        decrypt_secret(
            store,
            &mut mattermost.bot_token,
            "config.channels_config.mattermost.bot_token",
        )?;
    }
    if let Some(ref mut webhook) = channels.webhook {
        decrypt_optional_secret(
            store,
            &mut webhook.secret,
            "config.channels_config.webhook.secret",
        )?;
    }
    if let Some(ref mut matrix) = channels.matrix {
        decrypt_secret(
            store,
            &mut matrix.access_token,
            "config.channels_config.matrix.access_token",
        )?;
    }
    if let Some(ref mut whatsapp) = channels.whatsapp {
        decrypt_optional_secret(
            store,
            &mut whatsapp.access_token,
            "config.channels_config.whatsapp.access_token",
        )?;
        decrypt_optional_secret(
            store,
            &mut whatsapp.app_secret,
            "config.channels_config.whatsapp.app_secret",
        )?;
        decrypt_optional_secret(
            store,
            &mut whatsapp.verify_token,
            "config.channels_config.whatsapp.verify_token",
        )?;
    }
    if let Some(ref mut linq) = channels.linq {
        decrypt_secret(
            store,
            &mut linq.api_token,
            "config.channels_config.linq.api_token",
        )?;
        decrypt_optional_secret(
            store,
            &mut linq.signing_secret,
            "config.channels_config.linq.signing_secret",
        )?;
    }
    if let Some(ref mut github) = channels.github {
        decrypt_secret(
            store,
            &mut github.access_token,
            "config.channels_config.github.access_token",
        )?;
        decrypt_optional_secret(
            store,
            &mut github.webhook_secret,
            "config.channels_config.github.webhook_secret",
        )?;
    }
    if let Some(ref mut nextcloud) = channels.nextcloud_talk {
        decrypt_secret(
            store,
            &mut nextcloud.app_token,
            "config.channels_config.nextcloud_talk.app_token",
        )?;
        decrypt_optional_secret(
            store,
            &mut nextcloud.webhook_secret,
            "config.channels_config.nextcloud_talk.webhook_secret",
        )?;
    }
    if let Some(ref mut irc) = channels.irc {
        decrypt_optional_secret(
            store,
            &mut irc.server_password,
            "config.channels_config.irc.server_password",
        )?;
        decrypt_optional_secret(
            store,
            &mut irc.nickserv_password,
            "config.channels_config.irc.nickserv_password",
        )?;
        decrypt_optional_secret(
            store,
            &mut irc.sasl_password,
            "config.channels_config.irc.sasl_password",
        )?;
    }
    if let Some(ref mut lark) = channels.lark {
        decrypt_secret(
            store,
            &mut lark.app_secret,
            "config.channels_config.lark.app_secret",
        )?;
        decrypt_optional_secret(
            store,
            &mut lark.encrypt_key,
            "config.channels_config.lark.encrypt_key",
        )?;
        decrypt_optional_secret(
            store,
            &mut lark.verification_token,
            "config.channels_config.lark.verification_token",
        )?;
    }
    if let Some(ref mut dingtalk) = channels.dingtalk {
        decrypt_secret(
            store,
            &mut dingtalk.client_secret,
            "config.channels_config.dingtalk.client_secret",
        )?;
    }
    if let Some(ref mut napcat) = channels.napcat {
        decrypt_optional_secret(
            store,
            &mut napcat.access_token,
            "config.channels_config.napcat.access_token",
        )?;
    }
    if let Some(ref mut qq) = channels.qq {
        decrypt_secret(
            store,
            &mut qq.app_secret,
            "config.channels_config.qq.app_secret",
        )?;
    }
    if let Some(ref mut nostr) = channels.nostr {
        decrypt_secret(
            store,
            &mut nostr.private_key,
            "config.channels_config.nostr.private_key",
        )?;
    }
    if let Some(ref mut clawdtalk) = channels.clawdtalk {
        decrypt_secret(
            store,
            &mut clawdtalk.api_key,
            "config.channels_config.clawdtalk.api_key",
        )?;
        decrypt_optional_secret(
            store,
            &mut clawdtalk.webhook_secret,
            "config.channels_config.clawdtalk.webhook_secret",
        )?;
    }
    if let Some(ref mut bluebubbles) = channels.bluebubbles {
        decrypt_secret(
            store,
            &mut bluebubbles.password,
            "config.channels_config.bluebubbles.password",
        )?;
        decrypt_optional_secret(
            store,
            &mut bluebubbles.webhook_secret,
            "config.channels_config.bluebubbles.webhook_secret",
        )?;
    }
    Ok(())
}

fn encrypt_channel_secrets(
    store: &crate::security::SecretStore,
    channels: &mut ChannelsConfig,
) -> Result<()> {
    if let Some(ref mut telegram) = channels.telegram {
        encrypt_secret(
            store,
            &mut telegram.bot_token,
            "config.channels_config.telegram.bot_token",
        )?;
    }
    if let Some(ref mut discord) = channels.discord {
        encrypt_secret(
            store,
            &mut discord.bot_token,
            "config.channels_config.discord.bot_token",
        )?;
    }
    if let Some(ref mut slack) = channels.slack {
        encrypt_secret(
            store,
            &mut slack.bot_token,
            "config.channels_config.slack.bot_token",
        )?;
        encrypt_optional_secret(
            store,
            &mut slack.app_token,
            "config.channels_config.slack.app_token",
        )?;
    }
    if let Some(ref mut mattermost) = channels.mattermost {
        encrypt_secret(
            store,
            &mut mattermost.bot_token,
            "config.channels_config.mattermost.bot_token",
        )?;
    }
    if let Some(ref mut webhook) = channels.webhook {
        encrypt_optional_secret(
            store,
            &mut webhook.secret,
            "config.channels_config.webhook.secret",
        )?;
    }
    if let Some(ref mut matrix) = channels.matrix {
        encrypt_secret(
            store,
            &mut matrix.access_token,
            "config.channels_config.matrix.access_token",
        )?;
    }
    if let Some(ref mut whatsapp) = channels.whatsapp {
        encrypt_optional_secret(
            store,
            &mut whatsapp.access_token,
            "config.channels_config.whatsapp.access_token",
        )?;
        encrypt_optional_secret(
            store,
            &mut whatsapp.app_secret,
            "config.channels_config.whatsapp.app_secret",
        )?;
        encrypt_optional_secret(
            store,
            &mut whatsapp.verify_token,
            "config.channels_config.whatsapp.verify_token",
        )?;
    }
    if let Some(ref mut linq) = channels.linq {
        encrypt_secret(
            store,
            &mut linq.api_token,
            "config.channels_config.linq.api_token",
        )?;
        encrypt_optional_secret(
            store,
            &mut linq.signing_secret,
            "config.channels_config.linq.signing_secret",
        )?;
    }
    if let Some(ref mut github) = channels.github {
        encrypt_secret(
            store,
            &mut github.access_token,
            "config.channels_config.github.access_token",
        )?;
        encrypt_optional_secret(
            store,
            &mut github.webhook_secret,
            "config.channels_config.github.webhook_secret",
        )?;
    }
    if let Some(ref mut nextcloud) = channels.nextcloud_talk {
        encrypt_secret(
            store,
            &mut nextcloud.app_token,
            "config.channels_config.nextcloud_talk.app_token",
        )?;
        encrypt_optional_secret(
            store,
            &mut nextcloud.webhook_secret,
            "config.channels_config.nextcloud_talk.webhook_secret",
        )?;
    }
    if let Some(ref mut irc) = channels.irc {
        encrypt_optional_secret(
            store,
            &mut irc.server_password,
            "config.channels_config.irc.server_password",
        )?;
        encrypt_optional_secret(
            store,
            &mut irc.nickserv_password,
            "config.channels_config.irc.nickserv_password",
        )?;
        encrypt_optional_secret(
            store,
            &mut irc.sasl_password,
            "config.channels_config.irc.sasl_password",
        )?;
    }
    if let Some(ref mut lark) = channels.lark {
        encrypt_secret(
            store,
            &mut lark.app_secret,
            "config.channels_config.lark.app_secret",
        )?;
        encrypt_optional_secret(
            store,
            &mut lark.encrypt_key,
            "config.channels_config.lark.encrypt_key",
        )?;
        encrypt_optional_secret(
            store,
            &mut lark.verification_token,
            "config.channels_config.lark.verification_token",
        )?;
    }
    if let Some(ref mut dingtalk) = channels.dingtalk {
        encrypt_secret(
            store,
            &mut dingtalk.client_secret,
            "config.channels_config.dingtalk.client_secret",
        )?;
    }
    if let Some(ref mut napcat) = channels.napcat {
        encrypt_optional_secret(
            store,
            &mut napcat.access_token,
            "config.channels_config.napcat.access_token",
        )?;
    }
    if let Some(ref mut qq) = channels.qq {
        encrypt_secret(
            store,
            &mut qq.app_secret,
            "config.channels_config.qq.app_secret",
        )?;
    }
    if let Some(ref mut nostr) = channels.nostr {
        encrypt_secret(
            store,
            &mut nostr.private_key,
            "config.channels_config.nostr.private_key",
        )?;
    }
    if let Some(ref mut clawdtalk) = channels.clawdtalk {
        encrypt_secret(
            store,
            &mut clawdtalk.api_key,
            "config.channels_config.clawdtalk.api_key",
        )?;
        encrypt_optional_secret(
            store,
            &mut clawdtalk.webhook_secret,
            "config.channels_config.clawdtalk.webhook_secret",
        )?;
    }
    if let Some(ref mut bluebubbles) = channels.bluebubbles {
        encrypt_secret(
            store,
            &mut bluebubbles.password,
            "config.channels_config.bluebubbles.password",
        )?;
        encrypt_optional_secret(
            store,
            &mut bluebubbles.webhook_secret,
            "config.channels_config.bluebubbles.webhook_secret",
        )?;
    }
    Ok(())
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

fn normalize_wire_api(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "responses" => Some("responses"),
        "chat_completions" | "chat-completions" | "chat" | "chatcompletions" => {
            Some("chat_completions")
        }
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

fn legacy_feishu_table(raw_toml: &toml::Value) -> Option<&toml::map::Map<String, toml::Value>> {
    raw_toml
        .get("channels_config")?
        .as_table()?
        .get("feishu")?
        .as_table()
}

fn extract_legacy_feishu_mention_only(raw_toml: &toml::Value) -> Option<bool> {
    legacy_feishu_table(raw_toml)?
        .get("mention_only")
        .and_then(toml::Value::as_bool)
}

fn has_legacy_feishu_mention_only(raw_toml: &toml::Value) -> bool {
    legacy_feishu_table(raw_toml)
        .and_then(|table| table.get("mention_only"))
        .is_some()
}

fn has_legacy_feishu_use_feishu(raw_toml: &toml::Value) -> bool {
    legacy_feishu_table(raw_toml)
        .and_then(|table| table.get("use_feishu"))
        .is_some()
}

fn apply_feishu_legacy_compat(
    config: &mut Config,
    legacy_feishu_mention_only: Option<bool>,
    legacy_feishu_use_feishu_present: bool,
    saw_legacy_feishu_mention_only_path: bool,
    saw_legacy_feishu_use_feishu_path: bool,
) {
    // Backward compatibility: users sometimes migrate config snippets from
    // [channels_config.lark] to [channels_config.feishu] and keep old keys.
    if let Some(feishu_cfg) = config.channels_config.feishu.as_mut() {
        if let Some(legacy_mention_only) = legacy_feishu_mention_only {
            if feishu_cfg.group_reply.is_none() {
                let mapped_mode = if legacy_mention_only {
                    GroupReplyMode::MentionOnly
                } else {
                    GroupReplyMode::AllMessages
                };
                feishu_cfg.group_reply = Some(GroupReplyConfig {
                    mode: Some(mapped_mode),
                    allowed_sender_ids: Vec::new(),
                });
                tracing::warn!(
                    "Legacy key [channels_config.feishu].mention_only is deprecated; mapped to [channels_config.feishu.group_reply].mode."
                );
            } else if saw_legacy_feishu_mention_only_path {
                tracing::warn!(
                    "Legacy key [channels_config.feishu].mention_only is ignored because [channels_config.feishu.group_reply] is already set."
                );
            }
        } else if saw_legacy_feishu_mention_only_path {
            tracing::warn!(
                "Legacy key [channels_config.feishu].mention_only is invalid; expected boolean."
            );
        }

        if legacy_feishu_use_feishu_present || saw_legacy_feishu_use_feishu_path {
            tracing::warn!(
                "Legacy key [channels_config.feishu].use_feishu is redundant and ignored; [channels_config.feishu] always uses Feishu endpoints."
            );
        }
    }
}

impl Config {
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

        if config_path.exists() {
            // Warn if config file is world-readable (may contain API keys)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = fs::metadata(&config_path).await {
                    if meta.permissions().mode() & 0o004 != 0 {
                        tracing::warn!(
                            "Config file {:?} is world-readable (mode {:o}). \
                             Consider restricting with: chmod 600 {:?}",
                            config_path,
                            meta.permissions().mode() & 0o777,
                            config_path,
                        );
                    }
                }
            }

            let contents = fs::read_to_string(&config_path)
                .await
                .context("Failed to read config file")?;

            // Parse raw TOML first so legacy compatibility rewrites can be applied after
            // deserialization.
            let raw_toml: toml::Value =
                toml::from_str(&contents).context("Failed to parse config file")?;
            let legacy_feishu_mention_only = extract_legacy_feishu_mention_only(&raw_toml);
            let legacy_feishu_mention_only_present = has_legacy_feishu_mention_only(&raw_toml);
            let legacy_feishu_use_feishu_present = has_legacy_feishu_use_feishu(&raw_toml);
            let mut config: Config =
                toml::from_str(&contents).context("Failed to deserialize config file")?;

            apply_feishu_legacy_compat(
                &mut config,
                legacy_feishu_mention_only,
                legacy_feishu_use_feishu_present,
                legacy_feishu_mention_only_present,
                legacy_feishu_use_feishu_present,
            );
            // Set computed paths that are skipped during serialization
            config.config_path = config_path.clone();
            config.workspace_dir = workspace_dir;
            let store = crate::security::SecretStore::new(&zeroclaw_dir, config.secrets.encrypt);
            decrypt_optional_secret(&store, &mut config.api_key, "config.api_key")?;
            for (profile_name, profile) in config.model_providers.iter_mut() {
                let secret_path = format!("config.model_providers.{profile_name}.api_key");
                decrypt_optional_secret(&store, &mut profile.api_key, &secret_path)?;
            }
            decrypt_optional_secret(
                &store,
                &mut config.transcription.api_key,
                "config.transcription.api_key",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.composio.api_key,
                "config.composio.api_key",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.proxy.http_proxy,
                "config.proxy.http_proxy",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.proxy.https_proxy,
                "config.proxy.https_proxy",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.proxy.all_proxy,
                "config.proxy.all_proxy",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.browser.computer_use.api_key,
                "config.browser.computer_use.api_key",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.web_search.brave_api_key,
                "config.web_search.brave_api_key",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.web_search.perplexity_api_key,
                "config.web_search.perplexity_api_key",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.web_search.exa_api_key,
                "config.web_search.exa_api_key",
            )?;
            decrypt_optional_secret(
                &store,
                &mut config.web_search.jina_api_key,
                "config.web_search.jina_api_key",
            )?;

            decrypt_optional_secret(
                &store,
                &mut config.storage.provider.config.db_url,
                "config.storage.provider.config.db_url",
            )?;
            decrypt_vec_secrets(
                &store,
                &mut config.reliability.api_keys,
                "config.reliability.api_keys",
            )?;
            decrypt_vec_secrets(
                &store,
                &mut config.gateway.paired_tokens,
                "config.gateway.paired_tokens",
            )?;

            for agent in config.agents.values_mut() {
                decrypt_optional_secret(&store, &mut agent.api_key, "config.agents.*.api_key")?;
            }

            decrypt_channel_secrets(&store, &mut config.channels_config)?;

            config.apply_env_overrides();
            config.validate()?;
            tracing::info!(
                path = %config.config_path.display(),
                workspace = %config.workspace_dir.display(),
                source = resolution_source.as_str(),
                initialized = false,
                "Config loaded"
            );
            Ok(config)
        } else {
            let mut config = Config::default();
            config.config_path = config_path.clone();
            config.workspace_dir = workspace_dir;
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

    fn normalize_reasoning_level_override(raw: Option<&str>, source: &str) -> Option<String> {
        let value = raw?.trim();
        if value.is_empty() {
            return None;
        }
        let normalized = value.to_ascii_lowercase().replace(['-', '_'], "");
        match normalized.as_str() {
            "minimal" | "low" | "medium" | "high" | "xhigh" => Some(normalized),
            _ => {
                tracing::warn!(
                    reasoning_level = %value,
                    source,
                    "Ignoring invalid reasoning level override"
                );
                None
            }
        }
    }

    fn normalize_provider_transport(raw: Option<&str>, source: &str) -> Option<String> {
        let value = raw?.trim();
        if value.is_empty() {
            return None;
        }

        let normalized = value.to_ascii_lowercase().replace(['-', '_'], "");
        match normalized.as_str() {
            "auto" => Some("auto".to_string()),
            "websocket" | "ws" => Some("websocket".to_string()),
            "sse" | "http" => Some("sse".to_string()),
            _ => {
                tracing::warn!(
                    transport = %value,
                    source,
                    "Ignoring invalid provider transport override"
                );
                None
            }
        }
    }

    /// Resolve provider reasoning level with backward-compatible runtime alias.
    ///
    /// Priority:
    /// 1) `provider.reasoning_level` (canonical)
    /// 2) `runtime.reasoning_level` (deprecated compatibility alias)
    pub fn effective_provider_reasoning_level(&self) -> Option<String> {
        let provider_level = Self::normalize_reasoning_level_override(
            self.provider.reasoning_level.as_deref(),
            "provider.reasoning_level",
        );
        let runtime_level = Self::normalize_reasoning_level_override(
            self.runtime.reasoning_level.as_deref(),
            "runtime.reasoning_level",
        );

        match (provider_level, runtime_level) {
            (Some(provider_level), Some(runtime_level)) => {
                if provider_level == runtime_level {
                    tracing::warn!(
                        reasoning_level = %provider_level,
                        "`runtime.reasoning_level` is deprecated; keep only `provider.reasoning_level`"
                    );
                } else {
                    tracing::warn!(
                        provider_reasoning_level = %provider_level,
                        runtime_reasoning_level = %runtime_level,
                        "`runtime.reasoning_level` is deprecated and ignored when `provider.reasoning_level` is set"
                    );
                }
                Some(provider_level)
            }
            (Some(provider_level), None) => Some(provider_level),
            (None, Some(runtime_level)) => {
                tracing::warn!(
                    reasoning_level = %runtime_level,
                    "`runtime.reasoning_level` is deprecated; using it as compatibility fallback to `provider.reasoning_level`"
                );
                Some(runtime_level)
            }
            (None, None) => None,
        }
    }

    /// Resolve provider transport mode (`provider.transport`).
    ///
    /// Supported values:
    /// - `auto`
    /// - `websocket`
    /// - `sse`
    pub fn effective_provider_transport(&self) -> Option<String> {
        Self::normalize_provider_transport(self.provider.transport.as_deref(), "provider.transport")
    }

    fn lookup_model_provider_profile(
        &self,
        provider_name: &str,
    ) -> Option<(String, ModelProviderConfig)> {
        let needle = provider_name.trim();
        if needle.is_empty() {
            return None;
        }

        self.model_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(needle))
            .map(|(name, profile)| (name.clone(), profile.clone()))
    }

    fn apply_named_model_provider_profile(&mut self) {
        let Some(current_provider) = self.default_provider.clone() else {
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
        let profile_default_model = profile
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let profile_api_key = profile
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);

        if self
            .api_url
            .as_deref()
            .map(str::trim)
            .is_none_or(|value| value.is_empty())
        {
            if let Some(base_url) = base_url.as_ref() {
                self.api_url = Some(base_url.clone());
            }
        }

        if self
            .api_key
            .as_deref()
            .map(str::trim)
            .is_none_or(|value| value.is_empty())
        {
            if let Some(profile_api_key) = profile_api_key {
                self.api_key = Some(profile_api_key);
            }
        }

        if let Some(profile_default_model) = profile_default_model {
            let can_apply_profile_model =
                self.default_model
                    .as_deref()
                    .map(str::trim)
                    .is_none_or(|value| {
                        value.is_empty() || value.eq_ignore_ascii_case(DEFAULT_MODEL_NAME)
                    });
            if can_apply_profile_model {
                self.default_model = Some(profile_default_model);
            }
        }

        if profile.requires_openai_auth
            && self
                .api_key
                .as_deref()
                .map(str::trim)
                .is_none_or(|value| value.is_empty())
        {
            let codex_key = std::env::var("OPENAI_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .or_else(read_codex_openai_api_key);
            if let Some(codex_key) = codex_key {
                self.api_key = Some(codex_key);
            }
        }

        let normalized_wire_api = profile.wire_api.as_deref().and_then(normalize_wire_api);
        let profile_name = profile
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if normalized_wire_api == Some("responses") {
            self.default_provider = Some("openai-codex".to_string());
            return;
        }

        if let Some(profile_name) = profile_name {
            if !profile_name.eq_ignore_ascii_case(&profile_key) {
                self.default_provider = Some(profile_name.to_string());
                return;
            }
        }

        if let Some(base_url) = base_url {
            self.default_provider = Some(format!("custom:{base_url}"));
        }
    }

    /// Validate configuration values that would cause runtime failures.
    ///
    /// Called after TOML deserialization and env-override application to catch
    /// obviously invalid values early instead of failing at arbitrary runtime points.
    pub fn validate(&self) -> Result<()> {
        if let Some(acp) = &self.channels_config.acp {
            acp.validate()?;
        }

        // Gateway
        if self.gateway.host.trim().is_empty() {
            anyhow::bail!("gateway.host must not be empty");
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
        for (i, rule) in self.autonomy.command_context_rules.iter().enumerate() {
            let command = rule.command.trim();
            if command.is_empty() {
                anyhow::bail!("autonomy.command_context_rules[{i}].command must not be empty");
            }
            if !command
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '.' | '*'))
            {
                anyhow::bail!(
                    "autonomy.command_context_rules[{i}].command contains invalid characters: {command}"
                );
            }

            for (j, domain) in rule.allowed_domains.iter().enumerate() {
                let normalized = domain.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].allowed_domains[{j}] must not be empty"
                    );
                }
                if normalized.chars().any(char::is_whitespace) {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].allowed_domains[{j}] must not contain whitespace"
                    );
                }
            }

            for (j, prefix) in rule.allowed_path_prefixes.iter().enumerate() {
                let normalized = prefix.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].allowed_path_prefixes[{j}] must not be empty"
                    );
                }
                if normalized.contains('\0') {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].allowed_path_prefixes[{j}] must not contain null bytes"
                    );
                }
            }
            for (j, prefix) in rule.denied_path_prefixes.iter().enumerate() {
                let normalized = prefix.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].denied_path_prefixes[{j}] must not be empty"
                    );
                }
                if normalized.contains('\0') {
                    anyhow::bail!(
                        "autonomy.command_context_rules[{i}].denied_path_prefixes[{j}] must not contain null bytes"
                    );
                }
            }
        }
        let mut seen_non_cli_excluded = std::collections::HashSet::new();
        for (i, tool_name) in self.autonomy.non_cli_excluded_tools.iter().enumerate() {
            let normalized = tool_name.trim();
            if normalized.is_empty() {
                anyhow::bail!("autonomy.non_cli_excluded_tools[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "autonomy.non_cli_excluded_tools[{i}] contains invalid characters: {normalized}"
                );
            }
            if !seen_non_cli_excluded.insert(normalized.to_string()) {
                anyhow::bail!(
                    "autonomy.non_cli_excluded_tools contains duplicate entry: {normalized}"
                );
            }
        }

        // Security OTP / estop
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
        if self.security.otp.challenge_timeout_secs == 0 {
            anyhow::bail!("security.otp.challenge_timeout_secs must be greater than 0");
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
        .with_context(|| {
            "Invalid security.otp.gated_domains or security.otp.gated_domain_categories"
        })?;
        for (i, cidr) in self.security.url_access.allow_cidrs.iter().enumerate() {
            parse_cidr_notation(cidr).with_context(|| {
                format!("security.url_access.allow_cidrs[{i}] is invalid CIDR notation: {cidr}")
            })?;
        }
        for (i, domain) in self.security.url_access.allow_domains.iter().enumerate() {
            let normalized = domain.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.url_access.allow_domains[{i}] must not be empty");
            }
            if normalized.chars().any(char::is_whitespace) {
                anyhow::bail!("security.url_access.allow_domains[{i}] must not contain whitespace");
            }
        }
        for (i, domain) in self.security.url_access.domain_allowlist.iter().enumerate() {
            let normalized = domain.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.url_access.domain_allowlist[{i}] must not be empty");
            }
            if normalized.chars().any(char::is_whitespace) {
                anyhow::bail!(
                    "security.url_access.domain_allowlist[{i}] must not contain whitespace"
                );
            }
        }
        for (i, domain) in self.security.url_access.domain_blocklist.iter().enumerate() {
            let normalized = domain.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.url_access.domain_blocklist[{i}] must not be empty");
            }
            if normalized.chars().any(char::is_whitespace) {
                anyhow::bail!(
                    "security.url_access.domain_blocklist[{i}] must not contain whitespace"
                );
            }
        }
        for (i, domain) in self.security.url_access.approved_domains.iter().enumerate() {
            let normalized = domain.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.url_access.approved_domains[{i}] must not be empty");
            }
            if normalized.chars().any(char::is_whitespace) {
                anyhow::bail!(
                    "security.url_access.approved_domains[{i}] must not contain whitespace"
                );
            }
        }
        if self.security.url_access.enforce_domain_allowlist
            && self.security.url_access.domain_allowlist.is_empty()
        {
            anyhow::bail!(
                "security.url_access.enforce_domain_allowlist=true requires non-empty security.url_access.domain_allowlist"
            );
        }
        let mut seen_http_credential_profiles = std::collections::HashSet::new();
        for (profile_name, profile) in &self.http_request.credential_profiles {
            let normalized_name = profile_name.trim();
            if normalized_name.is_empty() {
                anyhow::bail!("http_request.credential_profiles keys must not be empty");
            }
            if !normalized_name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!(
                    "http_request.credential_profiles.{profile_name} contains invalid characters"
                );
            }
            let canonical_name = normalized_name.to_ascii_lowercase();
            if !seen_http_credential_profiles.insert(canonical_name) {
                anyhow::bail!(
                    "http_request.credential_profiles contains duplicate profile name: {normalized_name}"
                );
            }

            let header_name = profile.header_name.trim();
            if header_name.is_empty() {
                anyhow::bail!(
                    "http_request.credential_profiles.{profile_name}.header_name must not be empty"
                );
            }
            if let Err(e) = reqwest::header::HeaderName::from_bytes(header_name.as_bytes()) {
                anyhow::bail!(
                    "http_request.credential_profiles.{profile_name}.header_name is invalid: {e}"
                );
            }

            let env_var = profile.env_var.trim();
            if !is_valid_env_var_name(env_var) {
                anyhow::bail!(
                    "http_request.credential_profiles.{profile_name}.env_var is invalid ({env_var}); expected [A-Za-z_][A-Za-z0-9_]*"
                );
            }
        }
        let built_in_roles = ["owner", "admin", "operator", "viewer", "guest"];
        let mut custom_role_names = std::collections::HashSet::new();
        for (i, role) in self.security.roles.iter().enumerate() {
            let name = role.name.trim();
            if name.is_empty() {
                anyhow::bail!("security.roles[{i}].name must not be empty");
            }
            if !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                anyhow::bail!("security.roles[{i}].name contains invalid characters: {name}");
            }
            let normalized_name = name.to_ascii_lowercase();
            if built_in_roles
                .iter()
                .any(|built_in| built_in == &normalized_name.as_str())
            {
                anyhow::bail!(
                    "security.roles[{i}].name conflicts with built-in role: {normalized_name}"
                );
            }
            if !custom_role_names.insert(normalized_name.clone()) {
                anyhow::bail!("security.roles contains duplicate role: {normalized_name}");
            }

            for (tool_idx, tool_name) in role.allowed_tools.iter().enumerate() {
                let normalized = tool_name.trim();
                if normalized.is_empty() {
                    anyhow::bail!(
                        "security.roles[{i}].allowed_tools[{tool_idx}] must not be empty"
                    );
                }
                if !normalized
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '*')
                {
                    anyhow::bail!(
                        "security.roles[{i}].allowed_tools[{tool_idx}] contains invalid characters: {normalized}"
                    );
                }
            }
            for (tool_idx, tool_name) in role.denied_tools.iter().enumerate() {
                let normalized = tool_name.trim();
                if normalized.is_empty() {
                    anyhow::bail!("security.roles[{i}].denied_tools[{tool_idx}] must not be empty");
                }
                if !normalized
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '*')
                {
                    anyhow::bail!(
                        "security.roles[{i}].denied_tools[{tool_idx}] contains invalid characters: {normalized}"
                    );
                }
            }
            for (tool_idx, tool_name) in role.totp_gated.iter().enumerate() {
                let normalized = tool_name.trim();
                if normalized.is_empty() {
                    anyhow::bail!("security.roles[{i}].totp_gated[{tool_idx}] must not be empty");
                }
                if !normalized
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '*')
                {
                    anyhow::bail!(
                        "security.roles[{i}].totp_gated[{tool_idx}] contains invalid characters: {normalized}"
                    );
                }
            }
            DomainMatcher::new(&role.gated_domains, &role.gated_domain_categories)
                .with_context(|| format!("Invalid security.roles[{i}] domain settings"))?;
            if let Some(parent) = role.inherits.as_deref() {
                let normalized_parent = parent.trim().to_ascii_lowercase();
                if normalized_parent.is_empty() {
                    anyhow::bail!("security.roles[{i}].inherits must not be empty");
                }
                if normalized_parent == normalized_name {
                    anyhow::bail!("security.roles[{i}].inherits must not reference itself");
                }
            }
        }
        for (i, role) in self.security.roles.iter().enumerate() {
            if let Some(parent) = role.inherits.as_deref() {
                let normalized_parent = parent.trim().to_ascii_lowercase();
                let built_in_exists = built_in_roles
                    .iter()
                    .any(|built_in| built_in == &normalized_parent.as_str());
                let custom_exists = custom_role_names.contains(&normalized_parent);
                if !built_in_exists && !custom_exists {
                    anyhow::bail!(
                        "security.roles[{i}].inherits references unknown role: {normalized_parent}"
                    );
                }
            }
        }
        if self.security.estop.state_file.trim().is_empty() {
            anyhow::bail!("security.estop.state_file must not be empty");
        }
        if self.security.syscall_anomaly.max_denied_events_per_minute == 0 {
            anyhow::bail!(
                "security.syscall_anomaly.max_denied_events_per_minute must be greater than 0"
            );
        }
        if self.security.syscall_anomaly.max_total_events_per_minute == 0 {
            anyhow::bail!(
                "security.syscall_anomaly.max_total_events_per_minute must be greater than 0"
            );
        }
        if self.security.syscall_anomaly.max_denied_events_per_minute
            > self.security.syscall_anomaly.max_total_events_per_minute
        {
            anyhow::bail!(
                "security.syscall_anomaly.max_denied_events_per_minute must be less than or equal to security.syscall_anomaly.max_total_events_per_minute"
            );
        }
        if self.security.syscall_anomaly.max_alerts_per_minute == 0 {
            anyhow::bail!("security.syscall_anomaly.max_alerts_per_minute must be greater than 0");
        }
        if self.security.syscall_anomaly.alert_cooldown_secs == 0 {
            anyhow::bail!("security.syscall_anomaly.alert_cooldown_secs must be greater than 0");
        }
        if self.security.syscall_anomaly.log_path.trim().is_empty() {
            anyhow::bail!("security.syscall_anomaly.log_path must not be empty");
        }
        for (i, syscall_name) in self
            .security
            .syscall_anomaly
            .baseline_syscalls
            .iter()
            .enumerate()
        {
            let normalized = syscall_name.trim();
            if normalized.is_empty() {
                anyhow::bail!("security.syscall_anomaly.baseline_syscalls[{i}] must not be empty");
            }
            if !normalized
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '#')
            {
                anyhow::bail!(
                    "security.syscall_anomaly.baseline_syscalls[{i}] contains invalid characters: {normalized}"
                );
            }
        }
        if self.security.perplexity_filter.perplexity_threshold <= 1.0 {
            anyhow::bail!(
                "security.perplexity_filter.perplexity_threshold must be greater than 1.0"
            );
        }
        if self.security.perplexity_filter.suffix_window_chars < 8 {
            anyhow::bail!("security.perplexity_filter.suffix_window_chars must be at least 8");
        }
        if self.security.perplexity_filter.min_prompt_chars < 8 {
            anyhow::bail!("security.perplexity_filter.min_prompt_chars must be at least 8");
        }
        if !(0.0..=1.0).contains(&self.security.perplexity_filter.symbol_ratio_threshold) {
            anyhow::bail!(
                "security.perplexity_filter.symbol_ratio_threshold must be between 0.0 and 1.0"
            );
        }
        if !(0.0..=1.0).contains(&self.security.outbound_leak_guard.sensitivity) {
            anyhow::bail!("security.outbound_leak_guard.sensitivity must be between 0.0 and 1.0");
        }

        // Browser
        if normalize_browser_open_choice(&self.browser.browser_open).is_none() {
            anyhow::bail!(
                "browser.browser_open must be one of: {}",
                BROWSER_OPEN_ALLOWED_VALUES.join(", ")
            );
        }
        if normalize_browser_backend(&self.browser.backend).is_none() {
            anyhow::bail!(
                "browser.backend must be one of: {}",
                BROWSER_BACKEND_ALLOWED_VALUES.join(", ")
            );
        }
        for (i, backend) in self.browser.auto_backend_priority.iter().enumerate() {
            if normalize_browser_auto_backend(backend).is_none() {
                anyhow::bail!(
                    "browser.auto_backend_priority[{i}] must be one of: {}",
                    BROWSER_AUTO_BACKEND_ALLOWED_VALUES.join(", ")
                );
            }
        }

        // Web search
        if normalize_web_search_provider(&self.web_search.provider).is_none() {
            anyhow::bail!(
                "web_search.provider must be one of: {}",
                WEB_SEARCH_PROVIDER_ALLOWED_VALUES.join(", ")
            );
        }
        for (i, provider) in self.web_search.fallback_providers.iter().enumerate() {
            if normalize_web_search_provider(provider).is_none() {
                anyhow::bail!(
                    "web_search.fallback_providers[{i}] must be one of: {}",
                    WEB_SEARCH_PROVIDER_ALLOWED_VALUES.join(", ")
                );
            }
        }
        let exa_search_type = self.web_search.exa_search_type.trim().to_ascii_lowercase();
        if !WEB_SEARCH_EXA_SEARCH_TYPE_ALLOWED_VALUES.contains(&exa_search_type.as_str()) {
            anyhow::bail!(
                "web_search.exa_search_type must be one of: {}",
                WEB_SEARCH_EXA_SEARCH_TYPE_ALLOWED_VALUES.join(", ")
            );
        }
        if self.web_search.retries_per_provider > 5 {
            anyhow::bail!("web_search.retries_per_provider must be between 0 and 5");
        }
        if self.web_search.retry_backoff_ms == 0 {
            anyhow::bail!("web_search.retry_backoff_ms must be greater than 0");
        }
        if !(1..=10).contains(&self.web_search.max_results) {
            anyhow::bail!("web_search.max_results must be between 1 and 10");
        }
        if self.web_search.timeout_secs == 0 {
            anyhow::bail!("web_search.timeout_secs must be greater than 0");
        }

        // Cost
        if self.cost.warn_at_percent > 100 {
            anyhow::bail!("cost.warn_at_percent must be between 0 and 100");
        }
        if self.cost.enforcement.reserve_percent > 100 {
            anyhow::bail!("cost.enforcement.reserve_percent must be between 0 and 100");
        }
        if matches!(self.cost.enforcement.mode, CostEnforcementMode::RouteDown) {
            let route_down_model = self
                .cost
                .enforcement
                .route_down_model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cost.enforcement.route_down_model must be set when mode is route_down"
                    )
                })?;

            if let Some(route_hint) = route_down_model
                .strip_prefix("hint:")
                .map(str::trim)
                .filter(|hint| !hint.is_empty())
            {
                if !self
                    .model_routes
                    .iter()
                    .any(|route| route.hint.trim() == route_hint)
                {
                    anyhow::bail!(
                        "cost.enforcement.route_down_model uses hint '{route_hint}', but no matching [[model_routes]] entry exists"
                    );
                }
            }
        }

        // Scheduler
        if self.scheduler.max_concurrent == 0 {
            anyhow::bail!("scheduler.max_concurrent must be greater than 0");
        }
        if self.scheduler.max_tasks == 0 {
            anyhow::bail!("scheduler.max_tasks must be greater than 0");
        }

        // Model routes
        for (i, route) in self.model_routes.iter().enumerate() {
            if route.hint.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].hint must not be empty");
            }
            if route.provider.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].provider must not be empty");
            }
            if route.model.trim().is_empty() {
                anyhow::bail!("model_routes[{i}].model must not be empty");
            }
            if route.max_tokens == Some(0) {
                anyhow::bail!("model_routes[{i}].max_tokens must be greater than 0");
            }
            if route
                .transport
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
                && Self::normalize_provider_transport(
                    route.transport.as_deref(),
                    "model_routes[].transport",
                )
                .is_none()
            {
                anyhow::bail!("model_routes[{i}].transport must be one of: auto, websocket, sse");
            }
        }

        if let Some(default_hint) = self
            .default_model
            .as_deref()
            .and_then(|model| model.strip_prefix("hint:"))
            .map(str::trim)
            .filter(|hint| !hint.is_empty())
        {
            if !self
                .model_routes
                .iter()
                .any(|route| route.hint.trim() == default_hint)
            {
                anyhow::bail!(
                    "default_model uses hint '{default_hint}', but no matching [[model_routes]] entry exists"
                );
            }
        }

        if self
            .provider
            .transport
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && Self::normalize_provider_transport(
                self.provider.transport.as_deref(),
                "provider.transport",
            )
            .is_none()
        {
            anyhow::bail!("provider.transport must be one of: auto, websocket, sse");
        }

        if self.provider_api.is_some()
            && !self
                .default_provider
                .as_deref()
                .is_some_and(|provider| provider.starts_with("custom:"))
        {
            anyhow::bail!(
                "provider_api is only valid when default_provider uses the custom:<url> format"
            );
        }

        // Embedding routes
        for (i, route) in self.embedding_routes.iter().enumerate() {
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

        for (profile_key, profile) in &self.model_providers {
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

            if !has_name && !has_base_url {
                anyhow::bail!(
                    "model_providers.{profile_name} must define at least one of `name` or `base_url`"
                );
            }

            if let Some(base_url) = profile.base_url.as_deref().map(str::trim) {
                if !base_url.is_empty() {
                    let parsed = reqwest::Url::parse(base_url).with_context(|| {
                        format!("model_providers.{profile_name}.base_url is not a valid URL")
                    })?;
                    if !matches!(parsed.scheme(), "http" | "https") {
                        anyhow::bail!(
                            "model_providers.{profile_name}.base_url must use http/https"
                        );
                    }
                }
            }

            if let Some(wire_api) = profile.wire_api.as_deref().map(str::trim) {
                if !wire_api.is_empty() && normalize_wire_api(wire_api).is_none() {
                    anyhow::bail!(
                        "model_providers.{profile_name}.wire_api must be one of: responses, chat_completions"
                    );
                }
            }
        }

        // Ollama cloud-routing safety checks
        if self
            .default_provider
            .as_deref()
            .is_some_and(|provider| provider.trim().eq_ignore_ascii_case("ollama"))
            && self
                .default_model
                .as_deref()
                .is_some_and(|model| model.trim().ends_with(":cloud"))
        {
            if is_local_ollama_endpoint(self.api_url.as_deref()) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but api_url is local or unset. Set api_url to a remote Ollama endpoint (for example https://ollama.com)."
                );
            }

            if !has_ollama_cloud_credential(self.api_key.as_deref()) {
                anyhow::bail!(
                    "default_model uses ':cloud' with provider 'ollama', but no API key is configured. Set api_key or OLLAMA_API_KEY."
                );
            }
        }

        // MCP
        if self.mcp.enabled {
            validate_mcp_config(&self.mcp)?;
        }

        // Proxy (delegate to existing validation)
        self.proxy.validate()?;

        // Delegate coordination runtime safety bounds.
        if self.coordination.enabled && self.coordination.lead_agent.trim().is_empty() {
            anyhow::bail!("coordination.lead_agent must not be empty when coordination is enabled");
        }
        if self.coordination.max_inbox_messages_per_agent == 0 {
            anyhow::bail!("coordination.max_inbox_messages_per_agent must be greater than 0");
        }
        if self.coordination.max_dead_letters == 0 {
            anyhow::bail!("coordination.max_dead_letters must be greater than 0");
        }
        if self.coordination.max_context_entries == 0 {
            anyhow::bail!("coordination.max_context_entries must be greater than 0");
        }
        if self.coordination.max_seen_message_ids == 0 {
            anyhow::bail!("coordination.max_seen_message_ids must be greater than 0");
        }

        // WASM config
        if self.wasm.memory_limit_mb == 0 || self.wasm.memory_limit_mb > 256 {
            anyhow::bail!(
                "wasm.memory_limit_mb must be between 1 and 256, got {}",
                self.wasm.memory_limit_mb
            );
        }
        if self.wasm.fuel_limit == 0 {
            anyhow::bail!("wasm.fuel_limit must be greater than 0");
        }
        {
            let url = &self.wasm.registry_url;
            // Extract what comes after "https://" and check that the host part
            // (up to the first '/', '?', '#', or ':') is non-empty.
            let has_valid_host = url
                .strip_prefix("https://")
                .map(|rest| {
                    let host = rest.split(&['/', '?', '#', ':'][..]).next().unwrap_or("");
                    !host.is_empty()
                })
                .unwrap_or(false);
            if !has_valid_host {
                anyhow::bail!(
                    "wasm.registry_url must be a valid HTTPS URL with a non-empty host, got '{url}'"
                );
            }
        }

        Ok(())
    }

    /// Apply environment variable overrides to config
    pub fn apply_env_overrides(&mut self) {
        let mut has_explicit_zeroclaw_api_key = false;

        // API Key: ZEROCLAW_API_KEY always wins (explicit intent).
        // API_KEY (generic) is only used as a fallback when config has no api_key,
        // because API_KEY is a very common env var name that may be set by unrelated
        // tools and should not silently override an already-configured key.
        if let Ok(key) = std::env::var("ZEROCLAW_API_KEY") {
            if !key.is_empty() {
                self.api_key = Some(key);
                has_explicit_zeroclaw_api_key = true;
            }
        } else if self.api_key.as_ref().map_or(true, |k| k.is_empty()) {
            if let Ok(key) = std::env::var("API_KEY") {
                if !key.is_empty() {
                    self.api_key = Some(key);
                }
            }
        }
        // API Key: GLM_API_KEY overrides when provider is a GLM/Zhipu variant.
        if !has_explicit_zeroclaw_api_key
            && self.default_provider.as_deref().is_some_and(is_glm_alias)
        {
            if let Ok(key) = std::env::var("GLM_API_KEY") {
                if !key.is_empty() {
                    self.api_key = Some(key);
                }
            }
        }

        // API Key: ZAI_API_KEY overrides when provider is a Z.AI variant.
        if !has_explicit_zeroclaw_api_key
            && self.default_provider.as_deref().is_some_and(is_zai_alias)
        {
            if let Ok(key) = std::env::var("ZAI_API_KEY") {
                if !key.is_empty() {
                    self.api_key = Some(key);
                }
            }
        }

        // Provider override precedence:
        // 1) ZEROCLAW_PROVIDER always wins when set.
        // 2) ZEROCLAW_MODEL_PROVIDER/MODEL_PROVIDER (Codex app-server style).
        // 3) Legacy PROVIDER is honored only when config still uses default provider.
        if let Ok(provider) = std::env::var("ZEROCLAW_PROVIDER") {
            if !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        } else if let Ok(provider) =
            std::env::var("ZEROCLAW_MODEL_PROVIDER").or_else(|_| std::env::var("MODEL_PROVIDER"))
        {
            if !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        } else if let Ok(provider) = std::env::var("PROVIDER") {
            let should_apply_legacy_provider =
                self.default_provider.as_deref().map_or(true, |configured| {
                    configured
                        .trim()
                        .eq_ignore_ascii_case(DEFAULT_PROVIDER_NAME)
                });
            if should_apply_legacy_provider && !provider.is_empty() {
                self.default_provider = Some(provider);
            }
        }

        // Model: ZEROCLAW_MODEL or MODEL
        if let Ok(model) = std::env::var("ZEROCLAW_MODEL").or_else(|_| std::env::var("MODEL")) {
            if !model.is_empty() {
                self.default_model = Some(model);
            }
        }

        // Apply named provider profile remapping (Codex app-server compatibility).
        self.apply_named_model_provider_profile();

        // Workspace directory: ZEROCLAW_WORKSPACE
        if let Ok(workspace) = std::env::var("ZEROCLAW_WORKSPACE") {
            if !workspace.is_empty() {
                let (_, workspace_dir) =
                    resolve_config_dir_for_workspace(&PathBuf::from(workspace));
                self.workspace_dir = workspace_dir;
            }
        }

        // Open-skills opt-in flag: ZEROCLAW_OPEN_SKILLS_ENABLED
        if let Ok(flag) = std::env::var("ZEROCLAW_OPEN_SKILLS_ENABLED") {
            if !flag.trim().is_empty() {
                match flag.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => self.skills.open_skills_enabled = true,
                    "0" | "false" | "no" | "off" => self.skills.open_skills_enabled = false,
                    _ => tracing::warn!(
                        "Ignoring invalid ZEROCLAW_OPEN_SKILLS_ENABLED (valid: 1|0|true|false|yes|no|on|off)"
                    ),
                }
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
        if let Ok(flag) = std::env::var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS") {
            if !flag.trim().is_empty() {
                match flag.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => self.skills.allow_scripts = true,
                    "0" | "false" | "no" | "off" => self.skills.allow_scripts = false,
                    _ => tracing::warn!(
                        "Ignoring invalid ZEROCLAW_SKILLS_ALLOW_SCRIPTS (valid: 1|0|true|false|yes|no|on|off)"
                    ),
                }
            }
        }

        // Skills prompt mode override: ZEROCLAW_SKILLS_PROMPT_MODE
        if let Ok(mode) = std::env::var("ZEROCLAW_SKILLS_PROMPT_MODE") {
            if !mode.trim().is_empty() {
                if let Some(parsed) = parse_skills_prompt_injection_mode(&mode) {
                    self.skills.prompt_injection_mode = parsed;
                } else {
                    tracing::warn!(
                        "Ignoring invalid ZEROCLAW_SKILLS_PROMPT_MODE (valid: full|compact)"
                    );
                }
            }
        }

        // Gateway port: ZEROCLAW_GATEWAY_PORT or PORT
        if let Ok(port_str) =
            std::env::var("ZEROCLAW_GATEWAY_PORT").or_else(|_| std::env::var("PORT"))
        {
            if let Ok(port) = port_str.parse::<u16>() {
                self.gateway.port = port;
            }
        }

        // Gateway host: ZEROCLAW_GATEWAY_HOST or HOST
        if let Ok(host) = std::env::var("ZEROCLAW_GATEWAY_HOST").or_else(|_| std::env::var("HOST"))
        {
            if !host.is_empty() {
                self.gateway.host = host;
            }
        }

        // Allow public bind: ZEROCLAW_ALLOW_PUBLIC_BIND
        if let Ok(val) = std::env::var("ZEROCLAW_ALLOW_PUBLIC_BIND") {
            self.gateway.allow_public_bind = val == "1" || val.eq_ignore_ascii_case("true");
        }

        // Temperature: ZEROCLAW_TEMPERATURE
        if let Ok(temp_str) = std::env::var("ZEROCLAW_TEMPERATURE") {
            if let Ok(temp) = temp_str.parse::<f64>() {
                if (0.0..=2.0).contains(&temp) {
                    self.default_temperature = temp;
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

        // Deprecated reasoning level alias: ZEROCLAW_REASONING_LEVEL or REASONING_LEVEL
        let alias_level = std::env::var("ZEROCLAW_REASONING_LEVEL")
            .ok()
            .map(|value| ("ZEROCLAW_REASONING_LEVEL", value))
            .or_else(|| {
                std::env::var("REASONING_LEVEL")
                    .ok()
                    .map(|value| ("REASONING_LEVEL", value))
            });
        if let Some((env_name, level)) = alias_level {
            if let Some(normalized) =
                Self::normalize_reasoning_level_override(Some(&level), env_name)
            {
                tracing::warn!(
                    env_name,
                    reasoning_level = %normalized,
                    "{env_name} is deprecated; prefer provider.reasoning_level in config"
                );
                self.runtime.reasoning_level = Some(normalized);
            }
        }

        // Provider transport override: ZEROCLAW_PROVIDER_TRANSPORT or PROVIDER_TRANSPORT
        if let Ok(transport) = std::env::var("ZEROCLAW_PROVIDER_TRANSPORT")
            .or_else(|_| std::env::var("PROVIDER_TRANSPORT"))
        {
            if let Some(normalized) =
                Self::normalize_provider_transport(Some(&transport), "env:provider_transport")
            {
                self.provider.transport = Some(normalized);
            }
        }

        // Vision support override: ZEROCLAW_MODEL_SUPPORT_VISION or MODEL_SUPPORT_VISION
        if let Ok(flag) = std::env::var("ZEROCLAW_MODEL_SUPPORT_VISION")
            .or_else(|_| std::env::var("MODEL_SUPPORT_VISION"))
        {
            let normalized = flag.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => self.model_support_vision = Some(true),
                "0" | "false" | "no" | "off" => self.model_support_vision = Some(false),
                _ => {}
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

        // Perplexity API key: ZEROCLAW_PERPLEXITY_API_KEY or PERPLEXITY_API_KEY
        if let Ok(api_key) = std::env::var("ZEROCLAW_PERPLEXITY_API_KEY")
            .or_else(|_| std::env::var("PERPLEXITY_API_KEY"))
        {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                self.web_search.perplexity_api_key = Some(api_key.to_string());
            }
        }

        // Exa API key: ZEROCLAW_EXA_API_KEY or EXA_API_KEY
        if let Ok(api_key) =
            std::env::var("ZEROCLAW_EXA_API_KEY").or_else(|_| std::env::var("EXA_API_KEY"))
        {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                self.web_search.exa_api_key = Some(api_key.to_string());
            }
        }

        // Jina API key: ZEROCLAW_JINA_API_KEY or JINA_API_KEY
        if let Ok(api_key) =
            std::env::var("ZEROCLAW_JINA_API_KEY").or_else(|_| std::env::var("JINA_API_KEY"))
        {
            let api_key = api_key.trim();
            if !api_key.is_empty() {
                self.web_search.jina_api_key = Some(api_key.to_string());
            }
        }

        // Web search max results: ZEROCLAW_WEB_SEARCH_MAX_RESULTS or WEB_SEARCH_MAX_RESULTS
        if let Ok(max_results) = std::env::var("ZEROCLAW_WEB_SEARCH_MAX_RESULTS")
            .or_else(|_| std::env::var("WEB_SEARCH_MAX_RESULTS"))
        {
            if let Ok(max_results) = max_results.parse::<usize>() {
                if (1..=10).contains(&max_results) {
                    self.web_search.max_results = max_results;
                }
            }
        }

        // Web search fallback providers (comma-separated)
        if let Ok(fallbacks) = std::env::var("ZEROCLAW_WEB_SEARCH_FALLBACK_PROVIDERS")
            .or_else(|_| std::env::var("WEB_SEARCH_FALLBACK_PROVIDERS"))
        {
            self.web_search.fallback_providers = fallbacks
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        // Web search retries per provider
        if let Ok(retries) = std::env::var("ZEROCLAW_WEB_SEARCH_RETRIES_PER_PROVIDER")
            .or_else(|_| std::env::var("WEB_SEARCH_RETRIES_PER_PROVIDER"))
        {
            if let Ok(retries) = retries.parse::<u32>() {
                self.web_search.retries_per_provider = retries;
            }
        }

        // Web search retry backoff (ms)
        if let Ok(backoff_ms) = std::env::var("ZEROCLAW_WEB_SEARCH_RETRY_BACKOFF_MS")
            .or_else(|_| std::env::var("WEB_SEARCH_RETRY_BACKOFF_MS"))
        {
            if let Ok(backoff_ms) = backoff_ms.parse::<u64>() {
                if backoff_ms > 0 {
                    self.web_search.retry_backoff_ms = backoff_ms;
                }
            }
        }

        // Web search domain filter
        if let Ok(filters) = std::env::var("ZEROCLAW_WEB_SEARCH_DOMAIN_FILTER")
            .or_else(|_| std::env::var("WEB_SEARCH_DOMAIN_FILTER"))
        {
            self.web_search.domain_filter = filters
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        // Web search language filter
        if let Ok(filters) = std::env::var("ZEROCLAW_WEB_SEARCH_LANGUAGE_FILTER")
            .or_else(|_| std::env::var("WEB_SEARCH_LANGUAGE_FILTER"))
        {
            self.web_search.language_filter = filters
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        // Web search country
        if let Ok(country) = std::env::var("ZEROCLAW_WEB_SEARCH_COUNTRY")
            .or_else(|_| std::env::var("WEB_SEARCH_COUNTRY"))
        {
            let country = country.trim();
            self.web_search.country = if country.is_empty() {
                None
            } else {
                Some(country.to_string())
            };
        }

        // Web search recency filter
        if let Ok(recency_filter) = std::env::var("ZEROCLAW_WEB_SEARCH_RECENCY_FILTER")
            .or_else(|_| std::env::var("WEB_SEARCH_RECENCY_FILTER"))
        {
            let recency_filter = recency_filter.trim();
            self.web_search.recency_filter = if recency_filter.is_empty() {
                None
            } else {
                Some(recency_filter.to_string())
            };
        }

        // Web search max tokens
        if let Ok(max_tokens) = std::env::var("ZEROCLAW_WEB_SEARCH_MAX_TOKENS")
            .or_else(|_| std::env::var("WEB_SEARCH_MAX_TOKENS"))
        {
            if let Ok(max_tokens) = max_tokens.parse::<u32>() {
                if max_tokens > 0 {
                    self.web_search.max_tokens = Some(max_tokens);
                }
            }
        }

        // Web search max tokens per page
        if let Ok(max_tokens_per_page) = std::env::var("ZEROCLAW_WEB_SEARCH_MAX_TOKENS_PER_PAGE")
            .or_else(|_| std::env::var("WEB_SEARCH_MAX_TOKENS_PER_PAGE"))
        {
            if let Ok(max_tokens_per_page) = max_tokens_per_page.parse::<u32>() {
                if max_tokens_per_page > 0 {
                    self.web_search.max_tokens_per_page = Some(max_tokens_per_page);
                }
            }
        }

        // Exa search type
        if let Ok(search_type) = std::env::var("ZEROCLAW_WEB_SEARCH_EXA_SEARCH_TYPE")
            .or_else(|_| std::env::var("WEB_SEARCH_EXA_SEARCH_TYPE"))
        {
            let search_type = search_type.trim();
            if !search_type.is_empty() {
                self.web_search.exa_search_type = search_type.to_string();
            }
        }

        // Exa include text
        if let Ok(include_text) = std::env::var("ZEROCLAW_WEB_SEARCH_EXA_INCLUDE_TEXT")
            .or_else(|_| std::env::var("WEB_SEARCH_EXA_INCLUDE_TEXT"))
        {
            self.web_search.exa_include_text =
                include_text == "1" || include_text.eq_ignore_ascii_case("true");
        }

        // Jina site filters
        if let Ok(filters) = std::env::var("ZEROCLAW_WEB_SEARCH_JINA_SITE_FILTERS")
            .or_else(|_| std::env::var("WEB_SEARCH_JINA_SITE_FILTERS"))
        {
            self.web_search.jina_site_filters = filters
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        // Web search timeout: ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS or WEB_SEARCH_TIMEOUT_SECS
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS")
            .or_else(|_| std::env::var("WEB_SEARCH_TIMEOUT_SECS"))
        {
            if let Ok(timeout_secs) = timeout_secs.parse::<u64>() {
                if timeout_secs > 0 {
                    self.web_search.timeout_secs = timeout_secs;
                }
            }
        }

        // Shared URL-access policy toggles and lists
        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_BLOCK_PRIVATE_IP")
            .or_else(|_| std::env::var("URL_ACCESS_BLOCK_PRIVATE_IP"))
        {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => self.security.url_access.block_private_ip = true,
                "0" | "false" | "no" | "off" => self.security.url_access.block_private_ip = false,
                _ => {}
            }
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_ALLOW_LOOPBACK")
            .or_else(|_| std::env::var("URL_ACCESS_ALLOW_LOOPBACK"))
        {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => self.security.url_access.allow_loopback = true,
                "0" | "false" | "no" | "off" => self.security.url_access.allow_loopback = false,
                _ => {}
            }
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL")
            .or_else(|_| std::env::var("URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL"))
        {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => {
                    self.security.url_access.require_first_visit_approval = true
                }
                "0" | "false" | "no" | "off" => {
                    self.security.url_access.require_first_visit_approval = false
                }
                _ => {}
            }
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST")
            .or_else(|_| std::env::var("URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST"))
        {
            let normalized = value.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "1" | "true" | "yes" | "on" => {
                    self.security.url_access.enforce_domain_allowlist = true
                }
                "0" | "false" | "no" | "off" => {
                    self.security.url_access.enforce_domain_allowlist = false
                }
                _ => {}
            }
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_ALLOW_CIDRS")
            .or_else(|_| std::env::var("URL_ACCESS_ALLOW_CIDRS"))
        {
            self.security.url_access.allow_cidrs = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_ALLOW_DOMAINS")
            .or_else(|_| std::env::var("URL_ACCESS_ALLOW_DOMAINS"))
        {
            self.security.url_access.allow_domains = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_DOMAIN_ALLOWLIST")
            .or_else(|_| std::env::var("URL_ACCESS_DOMAIN_ALLOWLIST"))
        {
            self.security.url_access.domain_allowlist = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_DOMAIN_BLOCKLIST")
            .or_else(|_| std::env::var("URL_ACCESS_DOMAIN_BLOCKLIST"))
        {
            self.security.url_access.domain_blocklist = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
        }

        if let Ok(value) = std::env::var("ZEROCLAW_URL_ACCESS_APPROVED_DOMAINS")
            .or_else(|_| std::env::var("URL_ACCESS_APPROVED_DOMAINS"))
        {
            self.security.url_access.approved_domains = value
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect();
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
        if let Ok(timeout_secs) = std::env::var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS") {
            if let Ok(timeout_secs) = timeout_secs.parse::<u64>() {
                if timeout_secs > 0 {
                    self.storage.provider.config.connect_timeout_secs = Some(timeout_secs);
                }
            }
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
    }

    pub async fn save(&self) -> Result<()> {
        // Encrypt secrets before serialization
        let mut config_to_save = self.clone();
        let zeroclaw_dir = self
            .config_path
            .parent()
            .context("Config path must have a parent directory")?;
        let store = crate::security::SecretStore::new(zeroclaw_dir, self.secrets.encrypt);

        encrypt_optional_secret(&store, &mut config_to_save.api_key, "config.api_key")?;
        for (profile_name, profile) in config_to_save.model_providers.iter_mut() {
            let secret_path = format!("config.model_providers.{profile_name}.api_key");
            encrypt_optional_secret(&store, &mut profile.api_key, &secret_path)?;
        }
        encrypt_optional_secret(
            &store,
            &mut config_to_save.transcription.api_key,
            "config.transcription.api_key",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.composio.api_key,
            "config.composio.api_key",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.proxy.http_proxy,
            "config.proxy.http_proxy",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.proxy.https_proxy,
            "config.proxy.https_proxy",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.proxy.all_proxy,
            "config.proxy.all_proxy",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.browser.computer_use.api_key,
            "config.browser.computer_use.api_key",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.web_search.brave_api_key,
            "config.web_search.brave_api_key",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.web_search.perplexity_api_key,
            "config.web_search.perplexity_api_key",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.web_search.exa_api_key,
            "config.web_search.exa_api_key",
        )?;
        encrypt_optional_secret(
            &store,
            &mut config_to_save.web_search.jina_api_key,
            "config.web_search.jina_api_key",
        )?;

        encrypt_optional_secret(
            &store,
            &mut config_to_save.storage.provider.config.db_url,
            "config.storage.provider.config.db_url",
        )?;
        encrypt_vec_secrets(
            &store,
            &mut config_to_save.reliability.api_keys,
            "config.reliability.api_keys",
        )?;
        encrypt_vec_secrets(
            &store,
            &mut config_to_save.gateway.paired_tokens,
            "config.gateway.paired_tokens",
        )?;

        for agent in config_to_save.agents.values_mut() {
            encrypt_optional_secret(&store, &mut agent.api_key, "config.agents.*.api_key")?;
        }

        encrypt_channel_secrets(&store, &mut config_to_save.channels_config)?;

        let toml_str =
            toml::to_string_pretty(&config_to_save).context("Failed to serialize config")?;

        let parent_dir = self
            .config_path
            .parent()
            .context("Config path must have a parent directory")?;

        fs::create_dir_all(parent_dir).await.with_context(|| {
            format!(
                "Failed to create config directory: {}",
                parent_dir.display()
            )
        })?;

        let file_name = self
            .config_path
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
        #[cfg(unix)]
        {
            use std::{fs::Permissions, os::unix::fs::PermissionsExt};
            fs::set_permissions(&temp_path, Permissions::from_mode(0o600))
                .await
                .with_context(|| {
                    format!(
                        "Failed to set secure permissions on temporary config file: {}",
                        temp_path.display()
                    )
                })?;
        }
        temp_file
            .write_all(toml_str.as_bytes())
            .await
            .context("Failed to write temporary config contents")?;
        temp_file
            .sync_all()
            .await
            .context("Failed to fsync temporary config file")?;
        drop(temp_file);

        let had_existing_config = self.config_path.exists();
        if had_existing_config {
            fs::copy(&self.config_path, &backup_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to create config backup before atomic replace: {}",
                        backup_path.display()
                    )
                })?;
        }

        if let Err(e) = fs::rename(&temp_path, &self.config_path).await {
            let _ = fs::remove_file(&temp_path).await;
            if had_existing_config && backup_path.exists() {
                fs::copy(&backup_path, &self.config_path)
                    .await
                    .context("Failed to restore config backup")?;
            }
            anyhow::bail!("Failed to atomically replace config file: {e}");
        }

        #[cfg(unix)]
        {
            use std::{fs::Permissions, os::unix::fs::PermissionsExt};
            fs::set_permissions(&self.config_path, Permissions::from_mode(0o600))
                .await
                .with_context(|| {
                    format!(
                        "Failed to enforce secure permissions on config file: {}",
                        self.config_path.display()
                    )
                })?;
        }

        #[cfg(unix)]
        sync_directory(parent_dir).await?;
        #[cfg(not(unix))]
        sync_directory(parent_dir)?;

        if had_existing_config {
            let _ = fs::remove_file(&backup_path).await;
        }

        Ok(())
    }
}

#[cfg(unix)]
async fn sync_directory(path: &Path) -> Result<()> {
    let dir = File::open(path)
        .await
        .with_context(|| format!("Failed to open directory for fsync: {}", path.display()))?;
    dir.sync_all()
        .await
        .with_context(|| format!("Failed to fsync directory metadata: {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(path: &Path) -> Result<()> {
    let _ = path;
    Ok(())
}

/// ACP (Agent Client Protocol) channel configuration.
///
/// Enables ZeroClaw to act as an ACP client, connecting to an OpenCode ACP server
/// via `opencode acp` command for JSON-RPC 2.0 communication over stdio.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcpConfig {
    /// OpenCode binary path (default: "opencode").
    #[serde(default = "default_acp_opencode_path")]
    pub opencode_path: Option<String>,
    /// Working directory for OpenCode process.
    pub workdir: Option<String>,
    /// Additional arguments to pass to `opencode acp`.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Allowed user identifiers (empty = deny all, "*" = allow all).
    #[serde(default)]
    pub allowed_users: Vec<String>,
}

fn default_acp_opencode_path() -> Option<String> {
    Some("opencode".to_string())
}

impl AcpConfig {
    fn validate(&self) -> Result<()> {
        if self
            .opencode_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            anyhow::bail!("channels_config.acp.opencode_path must not be empty when set");
        }

        if self
            .workdir
            .as_deref()
            .is_some_and(|dir| dir.trim().is_empty())
        {
            anyhow::bail!("channels_config.acp.workdir must not be empty when set");
        }

        Ok(())
    }
}

impl ChannelConfig for AcpConfig {
    fn name() -> &'static str {
        "ACP"
    }

    fn desc() -> &'static str {
        "Agent Client Protocol channel for OpenCode integration"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tokio::sync::{Mutex, MutexGuard};
    use tokio::test;
    use tokio_stream::wrappers::ReadDirStream;
    use tokio_stream::StreamExt;

    // ── Defaults ─────────────────────────────────────────────

    #[test]
    async fn http_request_config_default_has_correct_values() {
        let cfg = HttpRequestConfig::default();
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_response_size, 1_000_000);
        assert!(!cfg.enabled);
        assert!(cfg.allowed_domains.is_empty());
        assert!(cfg.credential_profiles.is_empty());
    }

    #[test]
    async fn config_default_has_sane_values() {
        let c = Config::default();
        assert_eq!(c.default_provider.as_deref(), Some("openrouter"));
        assert!(c.default_model.as_deref().unwrap().contains("claude"));
        assert!((c.default_temperature - 0.7).abs() < f64::EPSILON);
        assert!(c.api_key.is_none());
        assert!(!c.skills.open_skills_enabled);
        assert!(!c.skills.allow_scripts);
        assert_eq!(
            c.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Full
        );
        assert!(c.workspace_dir.to_string_lossy().contains("workspace"));
        assert!(c.config_path.to_string_lossy().contains("config.toml"));
    }

    #[test]
    async fn wasm_config_default_has_correct_values() {
        let cfg = WasmConfig::default();
        assert!(cfg.enabled, "WASM tools should be enabled by default");
        assert_eq!(cfg.memory_limit_mb, 64);
        assert_eq!(cfg.fuel_limit, 1_000_000_000);
        assert_eq!(cfg.registry_url, "https://zeromarket.vercel.app/api");
    }

    #[test]
    async fn wasm_config_invalid_values_rejected() {
        let mut c = Config::default();

        // memory_limit_mb = 0
        c.wasm.memory_limit_mb = 0;
        assert!(c.validate().is_err(), "memory_limit_mb=0 should fail");

        // memory_limit_mb = 257
        c.wasm = WasmConfig::default();
        c.wasm.memory_limit_mb = 257;
        assert!(c.validate().is_err(), "memory_limit_mb=257 should fail");

        // fuel_limit = 0
        c.wasm = WasmConfig::default();
        c.wasm.fuel_limit = 0;
        assert!(c.validate().is_err(), "fuel_limit=0 should fail");

        // empty registry_url
        c.wasm = WasmConfig::default();
        c.wasm.registry_url = String::new();
        assert!(c.validate().is_err(), "empty registry_url should fail");

        // http:// instead of https://
        c.wasm = WasmConfig::default();
        c.wasm.registry_url = "http://example.com".to_string();
        assert!(c.validate().is_err(), "http registry_url should fail");

        // bare "https://"
        c.wasm = WasmConfig::default();
        c.wasm.registry_url = "https://".to_string();
        assert!(c.validate().is_err(), "https:// without host should fail");

        // port-only, no hostname
        c.wasm = WasmConfig::default();
        c.wasm.registry_url = "https://:443".to_string();
        assert!(c.validate().is_err(), "https://:443 should fail");

        // query-only, no hostname
        c.wasm = WasmConfig::default();
        c.wasm.registry_url = "https://?q=1".to_string();
        assert!(c.validate().is_err(), "https://?q=1 should fail");
    }

    #[test]
    async fn config_debug_redacts_sensitive_values() {
        let mut config = Config::default();
        config.workspace_dir = PathBuf::from("/tmp/workspace");
        config.config_path = PathBuf::from("/tmp/config.toml");
        config.api_key = Some("root-credential".into());
        config.storage.provider.config.db_url = Some("postgres://user:pw@host/db".into());
        config.browser.computer_use.api_key = Some("browser-credential".into());
        config.gateway.paired_tokens = vec!["zc_0123456789abcdef".into()];
        config.channels_config.telegram = Some(TelegramConfig {
            bot_token: "telegram-credential".into(),
            allowed_users: Vec::new(),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
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
            },
        );

        let debug_output = format!("{config:?}");
        assert!(debug_output.contains("***REDACTED***"));

        for (idx, secret) in [
            "root-credential",
            "postgres://user:pw@host/db",
            "browser-credential",
            "zc_0123456789abcdef",
            "telegram-credential",
            "agent-credential",
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                !debug_output.contains(secret),
                "debug output leaked secret value at index {idx}"
            );
        }

        assert!(!debug_output.contains("paired_tokens"));
        assert!(!debug_output.contains("bot_token"));
        assert!(!debug_output.contains("db_url"));
    }

    #[test]
    async fn bluebubbles_debug_redacts_server_url_userinfo() {
        let cfg = BlueBubblesConfig {
            server_url: "https://alice:super-secret@example.com:1234/api/v1".to_string(),
            password: "channel-password".to_string(),
            allowed_senders: vec!["*".to_string()],
            webhook_secret: Some("hook-secret".to_string()),
            ignore_senders: vec![],
        };

        let debug_output = format!("{cfg:?}");
        assert!(debug_output.contains("https://[REDACTED]@example.com:1234/api/v1"));
        assert!(!debug_output.contains("alice:super-secret"));
        assert!(!debug_output.contains("channel-password"));
        assert!(!debug_output.contains("hook-secret"));
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

        assert!(properties.contains_key("default_provider"));
        assert!(properties.contains_key("skills"));
        assert!(properties.contains_key("gateway"));
        assert!(properties.contains_key("channels_config"));
        assert!(!properties.contains_key("workspace_dir"));
        assert!(!properties.contains_key("config_path"));

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
        let temp = tempfile::TempDir::new().expect("temp dir");
        let config_path = temp.path().join("config.toml");
        let workspace_dir = temp.path().join("workspace");

        let mut config = Config::default();
        config.config_path = config_path.clone();
        config.workspace_dir = workspace_dir;

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
        assert!(a.allowed_commands.contains(&"mkdir".to_string()));
        assert!(a.allowed_commands.contains(&"touch".to_string()));
        assert!(a.allowed_commands.contains(&"cargo".to_string()));
        assert!(a.forbidden_paths.contains(&"/etc".to_string()));
        assert_eq!(a.max_actions_per_hour, 100);
        assert_eq!(a.max_cost_per_day_cents, 1000);
        assert!(a.require_approval_for_medium_risk);
        assert!(a.block_high_risk_commands);
        assert!(a.shell_env_passthrough.is_empty());
        assert!(a.command_context_rules.is_empty());
        assert!(!a.allow_sensitive_file_reads);
        assert!(!a.allow_sensitive_file_writes);
        assert!(a.non_cli_excluded_tools.contains(&"shell".to_string()));
        assert!(a.non_cli_excluded_tools.contains(&"process".to_string()));
        assert!(a.non_cli_excluded_tools.contains(&"delegate".to_string()));
    }

    #[test]
    async fn autonomy_config_serde_defaults_non_cli_excluded_tools() {
        let raw = r#"
level = "supervised"
workspace_only = true
allowed_commands = ["git"]
forbidden_paths = ["/etc"]
max_actions_per_hour = 20
max_cost_per_day_cents = 500
require_approval_for_medium_risk = true
block_high_risk_commands = true
shell_env_passthrough = []
auto_approve = ["file_read"]
always_ask = []
allowed_roots = []
"#;
        let parsed: AutonomyConfig = toml::from_str(raw).unwrap();
        assert!(
            !parsed.allow_sensitive_file_reads,
            "Missing allow_sensitive_file_reads must default to false"
        );
        assert!(
            !parsed.allow_sensitive_file_writes,
            "Missing allow_sensitive_file_writes must default to false"
        );
        assert!(
            parsed.command_context_rules.is_empty(),
            "Missing command_context_rules must default to empty"
        );
        assert!(parsed.non_cli_excluded_tools.contains(&"shell".to_string()));
        assert!(parsed
            .non_cli_excluded_tools
            .contains(&"process".to_string()));
        assert!(parsed
            .non_cli_excluded_tools
            .contains(&"browser".to_string()));
    }

    #[test]
    async fn config_validate_rejects_invalid_command_context_rule_command() {
        let mut cfg = Config::default();
        cfg.autonomy.command_context_rules = vec![CommandContextRuleConfig {
            command: "curl;rm".into(),
            action: CommandContextRuleAction::Allow,
            allowed_domains: vec![],
            allowed_path_prefixes: vec![],
            denied_path_prefixes: vec![],
            allow_high_risk: false,
        }];
        let err = cfg.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("autonomy.command_context_rules[0].command"));
    }

    #[test]
    async fn config_validate_rejects_empty_command_context_rule_domain() {
        let mut cfg = Config::default();
        cfg.autonomy.command_context_rules = vec![CommandContextRuleConfig {
            command: "curl".into(),
            action: CommandContextRuleAction::Allow,
            allowed_domains: vec!["   ".into()],
            allowed_path_prefixes: vec![],
            denied_path_prefixes: vec![],
            allow_high_risk: true,
        }];
        let err = cfg.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("autonomy.command_context_rules[0].allowed_domains[0]"));
    }

    #[test]
    async fn config_validate_rejects_duplicate_non_cli_excluded_tools() {
        let mut cfg = Config::default();
        cfg.autonomy.non_cli_excluded_tools = vec!["shell".into(), "shell".into()];
        let err = cfg.validate().unwrap_err();
        assert!(err
            .to_string()
            .contains("autonomy.non_cli_excluded_tools contains duplicate entry"));
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
        assert_eq!(r.wasm.tools_dir, "tools/wasm");
        assert_eq!(r.wasm.fuel_limit, 1_000_000);
        assert_eq!(r.wasm.memory_limit_mb, 64);
        assert_eq!(r.wasm.max_module_size_mb, 50);
        assert!(!r.wasm.allow_workspace_read);
        assert!(!r.wasm.allow_workspace_write);
        assert!(r.wasm.allowed_hosts.is_empty());
        assert!(r.wasm.security.require_workspace_relative_tools_dir);
        assert!(r.wasm.security.reject_symlink_modules);
        assert!(r.wasm.security.reject_symlink_tools_dir);
        assert!(r.wasm.security.strict_host_validation);
        assert_eq!(
            r.wasm.security.capability_escalation_mode,
            WasmCapabilityEscalationMode::Deny
        );
        assert_eq!(
            r.wasm.security.module_hash_policy,
            WasmModuleHashPolicy::Warn
        );
        assert!(r.wasm.security.module_sha256.is_empty());
    }

    #[test]
    async fn heartbeat_config_default() {
        let h = HeartbeatConfig::default();
        assert!(!h.enabled);
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
            max_run_history: 100,
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: CronConfig = serde_json::from_str(&json).unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.max_run_history, 100);
    }

    #[test]
    async fn config_defaults_cron_when_section_missing() {
        let toml_str = r#"
workspace_dir = "/tmp/workspace"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;

        let parsed: Config = toml::from_str(toml_str).unwrap();
        assert!(parsed.cron.enabled);
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
    async fn channels_config_default() {
        let c = ChannelsConfig::default();
        assert!(c.cli);
        assert!(c.telegram.is_none());
        assert!(c.discord.is_none());
    }

    #[test]
    async fn channels_config_accepts_onebot_alias_with_ws_url() {
        let toml = r#"
cli = true

[onebot]
ws_url = "ws://127.0.0.1:3001"
access_token = "onebot-token"
allowed_users = ["10001"]
"#;

        let parsed: ChannelsConfig =
            toml::from_str(toml).expect("config should accept onebot alias for napcat");
        let napcat = parsed
            .napcat
            .expect("channels_config.onebot should map to napcat config");

        assert_eq!(napcat.websocket_url, "ws://127.0.0.1:3001");
        assert_eq!(napcat.access_token.as_deref(), Some("onebot-token"));
        assert_eq!(napcat.allowed_users, vec!["10001"]);
    }

    #[test]
    async fn channels_config_napcat_still_accepts_ws_url_alias() {
        let toml = r#"
cli = true

[napcat]
ws_url = "ws://127.0.0.1:3002"
"#;

        let parsed: ChannelsConfig =
            toml::from_str(toml).expect("napcat config should accept ws_url as websocket alias");
        let napcat = parsed
            .napcat
            .expect("channels_config.napcat should be present");

        assert_eq!(napcat.websocket_url, "ws://127.0.0.1:3002");
        assert!(napcat.access_token.is_none());
    }

    // ── Serde round-trip ─────────────────────────────────────

    #[test]
    async fn config_toml_roundtrip() {
        let config = Config {
            workspace_dir: PathBuf::from("/tmp/test/workspace"),
            config_path: PathBuf::from("/tmp/test/config.toml"),
            api_key: Some("sk-test-key".into()),
            api_url: None,
            default_provider: Some("openrouter".into()),
            provider_api: None,
            default_model: Some("gpt-4o".into()),
            model_providers: HashMap::new(),
            provider: ProviderConfig::default(),
            default_temperature: 0.5,
            observability: ObservabilityConfig {
                backend: "log".into(),
                ..ObservabilityConfig::default()
            },
            autonomy: AutonomyConfig {
                level: AutonomyLevel::Full,
                workspace_only: false,
                allowed_commands: vec!["docker".into()],
                command_context_rules: vec![],
                forbidden_paths: vec!["/secret".into()],
                max_actions_per_hour: 50,
                max_cost_per_day_cents: 1000,
                require_approval_for_medium_risk: false,
                block_high_risk_commands: true,
                shell_env_passthrough: vec!["DATABASE_URL".into()],
                allow_sensitive_file_reads: false,
                allow_sensitive_file_writes: false,
                auto_approve: vec!["file_read".into()],
                always_ask: vec![],
                allowed_roots: vec![],
                non_cli_excluded_tools: vec![],
                non_cli_approval_approvers: vec![],
                non_cli_natural_language_approval_mode:
                    NonCliNaturalLanguageApprovalMode::RequestConfirm,
                non_cli_natural_language_approval_mode_by_channel: HashMap::new(),
            },
            security: SecurityConfig::default(),
            runtime: RuntimeConfig {
                kind: "docker".into(),
                ..RuntimeConfig::default()
            },
            research: ResearchPhaseConfig::default(),
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            coordination: CoordinationConfig::default(),
            skills: SkillsConfig::default(),
            plugins: PluginsConfig::default(),
            model_routes: Vec::new(),
            embedding_routes: Vec::new(),
            query_classification: QueryClassificationConfig::default(),
            heartbeat: HeartbeatConfig {
                enabled: true,
                interval_minutes: 15,
                message: Some("Check London time".into()),
                target: Some("telegram".into()),
                to: Some("123456".into()),
            },
            cron: CronConfig::default(),
            goal_loop: GoalLoopConfig::default(),
            channels_config: ChannelsConfig {
                cli: true,
                acp: None,
                telegram: Some(TelegramConfig {
                    bot_token: "123:ABC".into(),
                    allowed_users: vec!["user1".into()],
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: default_draft_update_interval_ms(),
                    interrupt_on_new_message: false,
                    mention_only: false,
                    progress_mode: ProgressMode::default(),
                    ack_enabled: true,
                    group_reply: None,
                    base_url: None,
                }),
                discord: None,
                slack: None,
                mattermost: None,
                webhook: None,
                imessage: None,
                matrix: None,
                signal: None,
                whatsapp: None,
                linq: None,
                github: None,
                bluebubbles: None,
                wati: None,
                nextcloud_talk: None,
                email: None,
                irc: None,
                lark: None,
                feishu: None,
                dingtalk: None,
                napcat: None,
                qq: None,
                nostr: None,
                clawdtalk: None,
                ack_reaction: AckReactionChannelsConfig::default(),
                message_timeout_secs: 300,
            },
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            web_fetch: WebFetchConfig::default(),
            web_search: WebSearchConfig::default(),
            proxy: ProxyConfig::default(),
            agent: AgentConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            economic: EconomicConfig::default(),
            peripherals: PeripheralsConfig::default(),
            agents: HashMap::new(),
            hooks: HooksConfig::default(),
            hardware: HardwareConfig::default(),
            transcription: TranscriptionConfig::default(),
            agents_ipc: AgentsIpcConfig::default(),
            mcp: McpConfig::default(),
            model_support_vision: None,
            wasm: WasmConfig::default(),
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.api_key, config.api_key);
        assert_eq!(parsed.default_provider, config.default_provider);
        assert_eq!(parsed.default_model, config.default_model);
        assert!((parsed.default_temperature - config.default_temperature).abs() < f64::EPSILON);
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
        assert!(parsed.channels_config.telegram.is_some());
        assert_eq!(
            parsed.channels_config.telegram.unwrap().bot_token,
            "123:ABC"
        );
    }

    #[test]
    async fn config_minimal_toml_uses_defaults() {
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed: Config = toml::from_str(minimal).unwrap();
        assert!(parsed.api_key.is_none());
        assert!(parsed.default_provider.is_none());
        assert_eq!(parsed.observability.backend, "none");
        assert_eq!(parsed.observability.runtime_trace_mode, "none");
        assert_eq!(parsed.autonomy.level, AutonomyLevel::Supervised);
        assert_eq!(parsed.runtime.kind, "native");
        assert!(!parsed.heartbeat.enabled);
        assert!(parsed.channels_config.cli);
        assert!(parsed.memory.hygiene_enabled);
        assert_eq!(parsed.memory.archive_after_days, 7);
        assert_eq!(parsed.memory.purge_after_days, 30);
        assert_eq!(parsed.memory.conversation_retention_days, 30);
    }

    #[test]
    async fn storage_provider_dburl_alias_deserializes() {
        let raw = r#"
default_temperature = 0.7

[storage.provider.config]
provider = "postgres"
dbURL = "postgres://postgres:postgres@localhost:5432/zeroclaw"
schema = "public"
table = "memories"
connect_timeout_secs = 12
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.storage.provider.config.provider, "postgres");
        assert_eq!(
            parsed.storage.provider.config.db_url.as_deref(),
            Some("postgres://postgres:postgres@localhost:5432/zeroclaw")
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

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.runtime.reasoning_enabled, Some(false));
    }

    #[test]
    async fn runtime_wasm_deserializes() {
        let raw = r#"
default_temperature = 0.7

[runtime]
kind = "wasm"

[runtime.wasm]
tools_dir = "skills/wasm"
fuel_limit = 500000
memory_limit_mb = 32
max_module_size_mb = 8
allow_workspace_read = true
allow_workspace_write = false
allowed_hosts = ["api.example.com", "cdn.example.com:443"]

[runtime.wasm.security]
require_workspace_relative_tools_dir = false
reject_symlink_modules = false
reject_symlink_tools_dir = false
strict_host_validation = false
capability_escalation_mode = "clamp"
module_hash_policy = "enforce"

[runtime.wasm.security.module_sha256]
calc = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.runtime.kind, "wasm");
        assert_eq!(parsed.runtime.wasm.tools_dir, "skills/wasm");
        assert_eq!(parsed.runtime.wasm.fuel_limit, 500_000);
        assert_eq!(parsed.runtime.wasm.memory_limit_mb, 32);
        assert_eq!(parsed.runtime.wasm.max_module_size_mb, 8);
        assert!(parsed.runtime.wasm.allow_workspace_read);
        assert!(!parsed.runtime.wasm.allow_workspace_write);
        assert_eq!(
            parsed.runtime.wasm.allowed_hosts,
            vec!["api.example.com", "cdn.example.com:443"]
        );
        assert!(
            !parsed
                .runtime
                .wasm
                .security
                .require_workspace_relative_tools_dir
        );
        assert!(!parsed.runtime.wasm.security.reject_symlink_modules);
        assert!(!parsed.runtime.wasm.security.reject_symlink_tools_dir);
        assert!(!parsed.runtime.wasm.security.strict_host_validation);
        assert_eq!(
            parsed.runtime.wasm.security.capability_escalation_mode,
            WasmCapabilityEscalationMode::Clamp
        );
        assert_eq!(
            parsed.runtime.wasm.security.module_hash_policy,
            WasmModuleHashPolicy::Enforce
        );
        assert_eq!(
            parsed.runtime.wasm.security.module_sha256.get("calc"),
            Some(&"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())
        );
    }

    #[test]
    async fn runtime_wasm_dev_template_deserializes() {
        let raw = include_str!("../../dev/config.wasm.dev.toml");
        let parsed: Config = toml::from_str(raw).expect("dev wasm template should parse");

        assert_eq!(parsed.runtime.kind, "wasm");
        assert!(parsed.runtime.wasm.allow_workspace_read);
        assert!(parsed.runtime.wasm.allow_workspace_write);
        assert_eq!(
            parsed.runtime.wasm.security.capability_escalation_mode,
            WasmCapabilityEscalationMode::Clamp
        );
    }

    #[test]
    async fn runtime_wasm_staging_template_deserializes() {
        let raw = include_str!("../../dev/config.wasm.staging.toml");
        let parsed: Config = toml::from_str(raw).expect("staging wasm template should parse");

        assert_eq!(parsed.runtime.kind, "wasm");
        assert!(parsed.runtime.wasm.allow_workspace_read);
        assert!(!parsed.runtime.wasm.allow_workspace_write);
        assert_eq!(
            parsed.runtime.wasm.security.capability_escalation_mode,
            WasmCapabilityEscalationMode::Deny
        );
    }

    #[test]
    async fn runtime_wasm_prod_template_deserializes() {
        let raw = include_str!("../../dev/config.wasm.prod.toml");
        let parsed: Config = toml::from_str(raw).expect("prod wasm template should parse");

        assert_eq!(parsed.runtime.kind, "wasm");
        assert!(!parsed.runtime.wasm.allow_workspace_read);
        assert!(!parsed.runtime.wasm.allow_workspace_write);
        assert!(parsed.runtime.wasm.allowed_hosts.is_empty());
        assert_eq!(
            parsed.runtime.wasm.security.capability_escalation_mode,
            WasmCapabilityEscalationMode::Deny
        );
    }

    #[test]
    async fn model_support_vision_deserializes() {
        let raw = r#"
default_temperature = 0.7
model_support_vision = true
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.model_support_vision, Some(true));

        // Default (omitted) should be None
        let raw_no_vision = r#"
default_temperature = 0.7
"#;
        let parsed2: Config = toml::from_str(raw_no_vision).unwrap();
        assert_eq!(parsed2.model_support_vision, None);
    }

    #[test]
    async fn provider_reasoning_level_deserializes() {
        let raw = r#"
default_temperature = 0.7

[provider]
reasoning_level = "high"
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.provider.reasoning_level.as_deref(), Some("high"));
        assert_eq!(
            parsed.effective_provider_reasoning_level().as_deref(),
            Some("high")
        );
    }

    #[test]
    async fn runtime_reasoning_level_alias_deserializes() {
        let raw = r#"
default_temperature = 0.7

[runtime]
reasoning_level = "xhigh"
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(parsed.runtime.reasoning_level.as_deref(), Some("xhigh"));
        assert_eq!(
            parsed.effective_provider_reasoning_level().as_deref(),
            Some("xhigh")
        );
    }

    #[test]
    async fn provider_reasoning_level_wins_over_runtime_alias() {
        let raw = r#"
default_temperature = 0.7

[provider]
reasoning_level = "medium"

[runtime]
reasoning_level = "high"
"#;

        let parsed: Config = toml::from_str(raw).unwrap();
        assert_eq!(
            parsed.effective_provider_reasoning_level().as_deref(),
            Some("medium")
        );
    }

    #[test]
    async fn agent_config_defaults() {
        let cfg = AgentConfig::default();
        assert!(cfg.compact_context);
        assert_eq!(cfg.max_tool_iterations, 20);
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
        let parsed: Config = toml::from_str(raw).unwrap();
        assert!(parsed.agent.compact_context);
        assert_eq!(parsed.agent.max_tool_iterations, 20);
        assert_eq!(parsed.agent.max_history_messages, 80);
        assert!(parsed.agent.parallel_tools);
        assert_eq!(parsed.agent.tool_dispatcher, "xml");
    }

    #[tokio::test]
    async fn sync_directory_handles_existing_directory() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_sync_directory_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        #[cfg(unix)]
        sync_directory(&dir).await.unwrap();
        #[cfg(not(unix))]
        sync_directory(&dir).unwrap();

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_and_load_tmpdir() {
        let dir = std::env::temp_dir().join("zeroclaw_test_config");
        let _ = fs::remove_dir_all(&dir).await;
        fs::create_dir_all(&dir).await.unwrap();

        let config_path = dir.join("config.toml");
        let config = Config {
            workspace_dir: dir.join("workspace"),
            config_path: config_path.clone(),
            api_key: Some("sk-roundtrip".into()),
            api_url: None,
            default_provider: Some("openrouter".into()),
            provider_api: None,
            default_model: Some("test-model".into()),
            model_providers: HashMap::new(),
            provider: ProviderConfig::default(),
            default_temperature: 0.9,
            observability: ObservabilityConfig::default(),
            autonomy: AutonomyConfig::default(),
            security: SecurityConfig::default(),
            runtime: RuntimeConfig::default(),
            research: ResearchPhaseConfig::default(),
            reliability: ReliabilityConfig::default(),
            scheduler: SchedulerConfig::default(),
            coordination: CoordinationConfig::default(),
            skills: SkillsConfig::default(),
            plugins: PluginsConfig::default(),
            model_routes: Vec::new(),
            embedding_routes: Vec::new(),
            query_classification: QueryClassificationConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            cron: CronConfig::default(),
            goal_loop: GoalLoopConfig::default(),
            channels_config: ChannelsConfig::default(),
            memory: MemoryConfig::default(),
            storage: StorageConfig::default(),
            tunnel: TunnelConfig::default(),
            gateway: GatewayConfig::default(),
            composio: ComposioConfig::default(),
            secrets: SecretsConfig::default(),
            browser: BrowserConfig::default(),
            http_request: HttpRequestConfig::default(),
            multimodal: MultimodalConfig::default(),
            web_fetch: WebFetchConfig::default(),
            web_search: WebSearchConfig::default(),
            proxy: ProxyConfig::default(),
            agent: AgentConfig::default(),
            identity: IdentityConfig::default(),
            cost: CostConfig::default(),
            economic: EconomicConfig::default(),
            peripherals: PeripheralsConfig::default(),
            agents: HashMap::new(),
            hooks: HooksConfig::default(),
            hardware: HardwareConfig::default(),
            transcription: TranscriptionConfig::default(),
            agents_ipc: AgentsIpcConfig::default(),
            mcp: McpConfig::default(),
            model_support_vision: None,
            wasm: WasmConfig::default(),
        };

        config.save().await.unwrap();
        assert!(config_path.exists());

        let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
        let loaded: Config = toml::from_str(&contents).unwrap();
        assert!(loaded
            .api_key
            .as_deref()
            .is_some_and(crate::security::SecretStore::is_encrypted));
        let store = crate::security::SecretStore::new(&dir, true);
        let decrypted = store.decrypt(loaded.api_key.as_deref().unwrap()).unwrap();
        assert_eq!(decrypted, "sk-roundtrip");
        assert_eq!(loaded.default_model.as_deref(), Some("test-model"));
        assert!((loaded.default_temperature - 0.9).abs() < f64::EPSILON);

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_encrypts_nested_credentials() {
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw_test_nested_credentials_{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).await.unwrap();

        let mut config = Config::default();
        config.workspace_dir = dir.join("workspace");
        config.config_path = dir.join("config.toml");
        config.api_key = Some("root-credential".into());
        config.transcription.api_key = Some("transcription-credential".into());
        config.composio.api_key = Some("composio-credential".into());
        config.proxy.http_proxy = Some("http://user:pass@proxy.internal:8080".into());
        config.proxy.https_proxy = Some("https://user:pass@proxy.internal:8443".into());
        config.proxy.all_proxy = Some("socks5://user:pass@proxy.internal:1080".into());
        config.browser.computer_use.api_key = Some("browser-credential".into());
        config.web_search.brave_api_key = Some("brave-credential".into());
        config.web_search.perplexity_api_key = Some("perplexity-credential".into());
        config.web_search.exa_api_key = Some("exa-credential".into());
        config.web_search.jina_api_key = Some("jina-credential".into());
        config.storage.provider.config.db_url = Some("postgres://user:pw@host/db".into());
        config.reliability.api_keys = vec!["backup-credential".into()];
        config.gateway.paired_tokens = vec!["zc_0123456789abcdef".into()];
        config.channels_config.telegram = Some(TelegramConfig {
            bot_token: "telegram-credential".into(),
            allowed_users: Vec::new(),
            stream_mode: StreamMode::Off,
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            progress_mode: ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
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
            },
        );

        config.save().await.unwrap();

        let contents = tokio::fs::read_to_string(config.config_path.clone())
            .await
            .unwrap();
        let stored: Config = toml::from_str(&contents).unwrap();
        let store = crate::security::SecretStore::new(&dir, true);

        let root_encrypted = stored.api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(root_encrypted));
        assert_eq!(store.decrypt(root_encrypted).unwrap(), "root-credential");

        let transcription_encrypted = stored.transcription.api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            transcription_encrypted
        ));
        assert_eq!(
            store.decrypt(transcription_encrypted).unwrap(),
            "transcription-credential"
        );

        let composio_encrypted = stored.composio.api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            composio_encrypted
        ));
        assert_eq!(
            store.decrypt(composio_encrypted).unwrap(),
            "composio-credential"
        );

        let proxy_http_encrypted = stored.proxy.http_proxy.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            proxy_http_encrypted
        ));
        assert_eq!(
            store.decrypt(proxy_http_encrypted).unwrap(),
            "http://user:pass@proxy.internal:8080"
        );
        let proxy_https_encrypted = stored.proxy.https_proxy.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            proxy_https_encrypted
        ));
        assert_eq!(
            store.decrypt(proxy_https_encrypted).unwrap(),
            "https://user:pass@proxy.internal:8443"
        );
        let proxy_all_encrypted = stored.proxy.all_proxy.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            proxy_all_encrypted
        ));
        assert_eq!(
            store.decrypt(proxy_all_encrypted).unwrap(),
            "socks5://user:pass@proxy.internal:1080"
        );

        let browser_encrypted = stored.browser.computer_use.api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            browser_encrypted
        ));
        assert_eq!(
            store.decrypt(browser_encrypted).unwrap(),
            "browser-credential"
        );

        let web_search_encrypted = stored.web_search.brave_api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            web_search_encrypted
        ));
        assert_eq!(
            store.decrypt(web_search_encrypted).unwrap(),
            "brave-credential"
        );
        let perplexity_encrypted = stored.web_search.perplexity_api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(
            perplexity_encrypted
        ));
        assert_eq!(
            store.decrypt(perplexity_encrypted).unwrap(),
            "perplexity-credential"
        );
        let exa_encrypted = stored.web_search.exa_api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(exa_encrypted));
        assert_eq!(store.decrypt(exa_encrypted).unwrap(), "exa-credential");
        let jina_encrypted = stored.web_search.jina_api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(jina_encrypted));
        assert_eq!(store.decrypt(jina_encrypted).unwrap(), "jina-credential");

        let worker = stored.agents.get("worker").unwrap();
        let worker_encrypted = worker.api_key.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(worker_encrypted));
        assert_eq!(store.decrypt(worker_encrypted).unwrap(), "agent-credential");

        let storage_db_url = stored.storage.provider.config.db_url.as_deref().unwrap();
        assert!(crate::security::SecretStore::is_encrypted(storage_db_url));
        assert_eq!(
            store.decrypt(storage_db_url).unwrap(),
            "postgres://user:pw@host/db"
        );

        let reliability_key = &stored.reliability.api_keys[0];
        assert!(crate::security::SecretStore::is_encrypted(reliability_key));
        assert_eq!(store.decrypt(reliability_key).unwrap(), "backup-credential");

        let paired_token = &stored.gateway.paired_tokens[0];
        assert!(crate::security::SecretStore::is_encrypted(paired_token));
        assert_eq!(store.decrypt(paired_token).unwrap(), "zc_0123456789abcdef");

        let telegram_token = stored
            .channels_config
            .telegram
            .as_ref()
            .unwrap()
            .bot_token
            .clone();
        assert!(crate::security::SecretStore::is_encrypted(&telegram_token));
        assert_eq!(
            store.decrypt(&telegram_token).unwrap(),
            "telegram-credential"
        );

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn config_save_atomic_cleanup() {
        let dir =
            std::env::temp_dir().join(format!("zeroclaw_test_config_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).await.unwrap();

        let config_path = dir.join("config.toml");
        let mut config = Config::default();
        config.workspace_dir = dir.join("workspace");
        config.config_path = config_path.clone();
        config.default_model = Some("model-a".into());
        config.save().await.unwrap();
        assert!(config_path.exists());

        config.default_model = Some("model-b".into());
        config.save().await.unwrap();

        let contents = tokio::fs::read_to_string(&config_path).await.unwrap();
        assert!(contents.contains("model-b"));

        let names: Vec<String> = ReadDirStream::new(fs::read_dir(&dir).await.unwrap())
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect()
            .await;
        assert!(!names.iter().any(|name| name.contains(".tmp-")));
        assert!(!names.iter().any(|name| name.ends_with(".bak")));

        let _ = fs::remove_dir_all(&dir).await;
    }

    // ── Telegram / Discord config ────────────────────────────

    #[test]
    async fn telegram_config_serde() {
        let tc = TelegramConfig {
            bot_token: "123:XYZ".into(),
            allowed_users: vec!["alice".into(), "bob".into()],
            stream_mode: StreamMode::Partial,
            draft_update_interval_ms: 500,
            interrupt_on_new_message: true,
            mention_only: false,
            progress_mode: ProgressMode::default(),
            ack_enabled: true,
            group_reply: None,
            base_url: None,
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
        assert_eq!(parsed.progress_mode, ProgressMode::Compact);
        assert_eq!(parsed.draft_update_interval_ms, 1000);
        assert!(!parsed.interrupt_on_new_message);
        assert!(parsed.base_url.is_none());
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
        assert!(parsed.group_reply_allowed_sender_ids().is_empty());
    }

    #[test]
    async fn telegram_config_custom_base_url() {
        let json = r#"{"bot_token":"tok","allowed_users":[],"base_url":"https://tapi.bale.ai"}"#;
        let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.base_url, Some("https://tapi.bale.ai".to_string()));
    }

    #[test]
    async fn progress_mode_deserializes_variants() {
        let verbose: ProgressMode = serde_json::from_str(r#""verbose""#).unwrap();
        let compact: ProgressMode = serde_json::from_str(r#""compact""#).unwrap();
        let off: ProgressMode = serde_json::from_str(r#""off""#).unwrap();

        assert_eq!(verbose, ProgressMode::Verbose);
        assert_eq!(compact, ProgressMode::Compact);
        assert_eq!(off, ProgressMode::Off);
    }

    #[test]
    async fn telegram_config_deserializes_progress_mode_verbose() {
        let json = r#"{"bot_token":"tok","allowed_users":[],"progress_mode":"verbose"}"#;
        let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.progress_mode, ProgressMode::Verbose);
    }

    #[test]
    async fn telegram_config_deserializes_progress_mode_off() {
        let json = r#"{"bot_token":"tok","allowed_users":[],"progress_mode":"off"}"#;
        let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.progress_mode, ProgressMode::Off);
    }

    #[test]
    async fn telegram_group_reply_config_overrides_legacy_mention_only() {
        let json = r#"{
            "bot_token":"tok",
            "allowed_users":["*"],
            "mention_only":false,
            "group_reply":{
                "mode":"mention_only",
                "allowed_sender_ids":["1001","1002"]
            }
        }"#;

        let parsed: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["1001".to_string(), "1002".to_string()]
        );
    }

    #[test]
    async fn discord_config_serde() {
        let dc = DiscordConfig {
            bot_token: "discord-token".into(),
            guild_id: Some("12345".into()),
            allowed_users: vec![],
            listen_to_bots: false,
            mention_only: false,
            group_reply: None,
        };
        let json = serde_json::to_string(&dc).unwrap();
        let parsed: DiscordConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bot_token, "discord-token");
        assert_eq!(parsed.guild_id.as_deref(), Some("12345"));
    }

    #[test]
    async fn discord_config_optional_guild() {
        let dc = DiscordConfig {
            bot_token: "tok".into(),
            guild_id: None,
            allowed_users: vec![],
            listen_to_bots: false,
            mention_only: false,
            group_reply: None,
        };
        let json = serde_json::to_string(&dc).unwrap();
        let parsed: DiscordConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.guild_id.is_none());
    }

    #[test]
    async fn discord_group_reply_mode_falls_back_to_legacy_mention_only() {
        let json = r#"{
            "bot_token":"tok",
            "mention_only":true
        }"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
        assert!(parsed.group_reply_allowed_sender_ids().is_empty());
    }

    #[test]
    async fn discord_group_reply_mode_overrides_legacy_mention_only() {
        let json = r#"{
            "bot_token":"tok",
            "mention_only":true,
            "group_reply":{
                "mode":"all_messages",
                "allowed_sender_ids":["111"]
            }
        }"#;
        let parsed: DiscordConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["111".to_string()]
        );
    }

    // ── iMessage / Matrix config ────────────────────────────

    #[test]
    async fn imessage_config_serde() {
        let ic = IMessageConfig {
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
            allowed_contacts: vec![],
        };
        let json = serde_json::to_string(&ic).unwrap();
        let parsed: IMessageConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.allowed_contacts.is_empty());
    }

    #[test]
    async fn imessage_config_wildcard() {
        let ic = IMessageConfig {
            allowed_contacts: vec!["*".into()],
        };
        let toml_str = toml::to_string(&ic).unwrap();
        let parsed: IMessageConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.allowed_contacts, vec!["*"]);
    }

    #[test]
    async fn matrix_config_serde() {
        let mc = MatrixConfig {
            homeserver: "https://matrix.org".into(),
            access_token: "syt_token_abc".into(),
            user_id: Some("@bot:matrix.org".into()),
            device_id: Some("DEVICE123".into()),
            room_id: "!room123:matrix.org".into(),
            allowed_users: vec!["@user:matrix.org".into()],
            mention_only: false,
        };
        let json = serde_json::to_string(&mc).unwrap();
        let parsed: MatrixConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.homeserver, "https://matrix.org");
        assert_eq!(parsed.access_token, "syt_token_abc");
        assert_eq!(parsed.user_id.as_deref(), Some("@bot:matrix.org"));
        assert_eq!(parsed.device_id.as_deref(), Some("DEVICE123"));
        assert_eq!(parsed.room_id, "!room123:matrix.org");
        assert_eq!(parsed.allowed_users.len(), 1);
    }

    #[test]
    async fn matrix_config_toml_roundtrip() {
        let mc = MatrixConfig {
            homeserver: "https://synapse.local:8448".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            room_id: "!abc:synapse.local".into(),
            allowed_users: vec!["@admin:synapse.local".into(), "*".into()],
            mention_only: true,
        };
        let toml_str = toml::to_string(&mc).unwrap();
        let parsed: MatrixConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.homeserver, "https://synapse.local:8448");
        assert_eq!(parsed.allowed_users.len(), 2);
    }

    #[test]
    async fn matrix_config_backward_compatible_without_session_hints() {
        let toml = r#"
homeserver = "https://matrix.org"
access_token = "tok"
room_id = "!ops:matrix.org"
allowed_users = ["@ops:matrix.org"]
"#;

        let parsed: MatrixConfig = toml::from_str(toml).unwrap();
        assert_eq!(parsed.homeserver, "https://matrix.org");
        assert!(parsed.user_id.is_none());
        assert!(parsed.device_id.is_none());
        assert!(!parsed.mention_only);
    }

    #[test]
    async fn signal_config_serde() {
        let sc = SignalConfig {
            http_url: "http://127.0.0.1:8686".into(),
            account: "+1234567890".into(),
            group_id: Some("group123".into()),
            allowed_from: vec!["+1111111111".into()],
            ignore_attachments: true,
            ignore_stories: false,
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
            http_url: "http://localhost:8080".into(),
            account: "+9876543210".into(),
            group_id: None,
            allowed_from: vec!["*".into()],
            ignore_attachments: false,
            ignore_stories: true,
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
    async fn channels_config_with_imessage_and_matrix() {
        let c = ChannelsConfig {
            cli: true,
            acp: None,
            telegram: None,
            discord: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: Some(IMessageConfig {
                allowed_contacts: vec!["+1".into()],
            }),
            matrix: Some(MatrixConfig {
                homeserver: "https://m.org".into(),
                access_token: "tok".into(),
                user_id: None,
                device_id: None,
                room_id: "!r:m".into(),
                allowed_users: vec!["@u:m".into()],
                mention_only: false,
            }),
            signal: None,
            whatsapp: None,
            linq: None,
            github: None,
            bluebubbles: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            irc: None,
            lark: None,
            feishu: None,
            dingtalk: None,
            napcat: None,
            qq: None,
            nostr: None,
            clawdtalk: None,
            ack_reaction: AckReactionChannelsConfig::default(),
            message_timeout_secs: 300,
        };
        let toml_str = toml::to_string_pretty(&c).unwrap();
        let parsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.imessage.is_some());
        assert!(parsed.matrix.is_some());
        assert_eq!(parsed.imessage.unwrap().allowed_contacts, vec!["+1"]);
        assert_eq!(parsed.matrix.unwrap().homeserver, "https://m.org");
    }

    #[test]
    async fn channels_config_default_has_no_imessage_matrix() {
        let c = ChannelsConfig::default();
        assert!(c.imessage.is_none());
        assert!(c.matrix.is_none());
    }

    #[test]
    async fn channels_ack_reaction_config_roundtrip() {
        let c = ChannelsConfig {
            ack_reaction: AckReactionChannelsConfig {
                telegram: Some(AckReactionConfig {
                    enabled: true,
                    strategy: AckReactionStrategy::First,
                    sample_rate: 0.8,
                    emojis: vec!["✅".into(), "👍".into()],
                    rules: vec![AckReactionRuleConfig {
                        enabled: true,
                        contains_any: vec!["deploy".into()],
                        contains_all: vec!["ok".into()],
                        contains_none: vec!["dry-run".into()],
                        regex_any: vec![r"deploy\s+ok".into()],
                        regex_all: Vec::new(),
                        regex_none: vec![r"panic|fatal".into()],
                        sender_ids: vec!["u123".into()],
                        chat_ids: vec!["-100200300".into()],
                        chat_types: vec![AckReactionChatType::Group],
                        locale_any: vec!["en".into()],
                        action: AckReactionRuleAction::React,
                        sample_rate: Some(0.5),
                        strategy: Some(AckReactionStrategy::Random),
                        emojis: vec!["🚀".into()],
                    }],
                }),
                discord: None,
                lark: None,
                feishu: None,
            },
            ..ChannelsConfig::default()
        };

        let toml_str = toml::to_string_pretty(&c).unwrap();
        let parsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
        let telegram = parsed.ack_reaction.telegram.expect("telegram ack config");
        assert!(telegram.enabled);
        assert_eq!(telegram.strategy, AckReactionStrategy::First);
        assert_eq!(telegram.sample_rate, 0.8);
        assert_eq!(telegram.emojis, vec!["✅", "👍"]);
        assert_eq!(telegram.rules.len(), 1);
        let first_rule = &telegram.rules[0];
        assert_eq!(first_rule.contains_any, vec!["deploy"]);
        assert_eq!(first_rule.contains_none, vec!["dry-run"]);
        assert_eq!(first_rule.regex_any, vec![r"deploy\s+ok"]);
        assert_eq!(first_rule.chat_ids, vec!["-100200300"]);
        assert_eq!(first_rule.action, AckReactionRuleAction::React);
        assert_eq!(first_rule.sample_rate, Some(0.5));
        assert_eq!(first_rule.chat_types, vec![AckReactionChatType::Group]);
    }

    #[test]
    async fn channels_ack_reaction_defaults_empty() {
        let parsed: ChannelsConfig = toml::from_str("cli = true").unwrap();
        assert!(parsed.ack_reaction.telegram.is_none());
        assert!(parsed.ack_reaction.discord.is_none());
        assert!(parsed.ack_reaction.lark.is_none());
        assert!(parsed.ack_reaction.feishu.is_none());
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
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
    }

    #[test]
    async fn slack_config_deserializes_with_allowed_users() {
        let json = r#"{"bot_token":"xoxb-tok","allowed_users":["U111"]}"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.channel_ids.is_empty());
        assert_eq!(parsed.allowed_users, vec!["U111"]);
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
    async fn slack_config_toml_backward_compat() {
        let toml_str = r#"
bot_token = "xoxb-tok"
channel_id = "C123"
"#;
        let parsed: SlackConfig = toml::from_str(toml_str).unwrap();
        assert!(parsed.channel_ids.is_empty());
        assert!(parsed.allowed_users.is_empty());
        assert_eq!(parsed.channel_id.as_deref(), Some("C123"));
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
    }

    #[test]
    async fn slack_group_reply_config_supports_sender_overrides() {
        let json = r#"{
            "bot_token":"xoxb-tok",
            "group_reply":{
                "mode":"mention_only",
                "allowed_sender_ids":["U111"]
            }
        }"#;
        let parsed: SlackConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["U111".to_string()]
        );
    }

    #[test]
    async fn mattermost_group_reply_mode_falls_back_to_legacy_mention_only() {
        let json = r#"{
            "url":"https://mm.example.com",
            "bot_token":"token",
            "mention_only":true
        }"#;
        let parsed: MattermostConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
    }

    #[test]
    async fn mattermost_group_reply_mode_overrides_legacy_mention_only() {
        let json = r#"{
            "url":"https://mm.example.com",
            "bot_token":"token",
            "mention_only":true,
            "group_reply":{
                "mode":"all_messages",
                "allowed_sender_ids":["u1","u2"]
            }
        }"#;
        let parsed: MattermostConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["u1".to_string(), "u2".to_string()]
        );
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
            access_token: Some("EAABx...".into()),
            phone_number_id: Some("123456789".into()),
            verify_token: Some("my-verify-token".into()),
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1234567890".into(), "+9876543210".into()],
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
            access_token: Some("tok".into()),
            phone_number_id: Some("12345".into()),
            verify_token: Some("verify".into()),
            app_secret: Some("secret123".into()),
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1".into()],
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
            access_token: Some("tok".into()),
            phone_number_id: Some("123".into()),
            verify_token: Some("ver".into()),
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["*".into()],
        };
        let toml_str = toml::to_string(&wc).unwrap();
        let parsed: WhatsAppConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.allowed_numbers, vec!["*"]);
    }

    #[test]
    async fn whatsapp_config_backend_type_cloud_precedence_when_ambiguous() {
        let wc = WhatsAppConfig {
            access_token: Some("tok".into()),
            phone_number_id: Some("123".into()),
            verify_token: Some("ver".into()),
            app_secret: None,
            session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec!["+1".into()],
        };
        assert!(wc.is_ambiguous_config());
        assert_eq!(wc.backend_type(), "cloud");
    }

    #[test]
    async fn whatsapp_config_backend_type_web() {
        let wc = WhatsAppConfig {
            access_token: None,
            phone_number_id: None,
            verify_token: None,
            app_secret: None,
            session_path: Some("~/.zeroclaw/state/whatsapp-web/session.db".into()),
            pair_phone: None,
            pair_code: None,
            allowed_numbers: vec![],
        };
        assert!(!wc.is_ambiguous_config());
        assert_eq!(wc.backend_type(), "web");
    }

    #[test]
    async fn channels_config_with_whatsapp() {
        let c = ChannelsConfig {
            cli: true,
            acp: None,
            telegram: None,
            discord: None,
            slack: None,
            mattermost: None,
            webhook: None,
            imessage: None,
            matrix: None,
            signal: None,
            whatsapp: Some(WhatsAppConfig {
                access_token: Some("tok".into()),
                phone_number_id: Some("123".into()),
                verify_token: Some("ver".into()),
                app_secret: None,
                session_path: None,
                pair_phone: None,
                pair_code: None,
                allowed_numbers: vec!["+1".into()],
            }),
            linq: None,
            github: None,
            bluebubbles: None,
            wati: None,
            nextcloud_talk: None,
            email: None,
            irc: None,
            lark: None,
            feishu: None,
            dingtalk: None,
            napcat: None,
            qq: None,
            nostr: None,
            clawdtalk: None,
            ack_reaction: AckReactionChannelsConfig::default(),
            message_timeout_secs: 300,
        };
        let toml_str = toml::to_string_pretty(&c).unwrap();
        let parsed: ChannelsConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.whatsapp.is_some());
        let wa = parsed.whatsapp.unwrap();
        assert_eq!(wa.phone_number_id, Some("123".into()));
        assert_eq!(wa.allowed_numbers, vec!["+1"]);
    }

    #[test]
    async fn channels_config_default_has_no_whatsapp() {
        let c = ChannelsConfig::default();
        assert!(c.whatsapp.is_none());
    }

    #[test]
    async fn channels_config_default_has_no_nextcloud_talk() {
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
        assert!(!g.node_control.enabled);
        assert!(g.node_control.auth_token.is_none());
        assert!(g.node_control.allowed_node_ids.is_empty());
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
            rate_limit_max_keys: 2048,
            idempotency_ttl_secs: 600,
            idempotency_max_keys: 4096,
            node_control: NodeControlConfig {
                enabled: true,
                auth_token: Some("node-token".into()),
                allowed_node_ids: vec!["node-1".into(), "node-2".into()],
            },
        };
        let toml_str = toml::to_string(&g).unwrap();
        let parsed: GatewayConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.require_pairing);
        assert!(!parsed.allow_public_bind);
        assert_eq!(parsed.paired_tokens, vec!["zc_test_token"]);
        assert_eq!(parsed.pair_rate_limit_per_minute, 12);
        assert_eq!(parsed.webhook_rate_limit_per_minute, 80);
        assert!(parsed.trust_forwarded_headers);
        assert_eq!(parsed.rate_limit_max_keys, 2048);
        assert_eq!(parsed.idempotency_ttl_secs, 600);
        assert_eq!(parsed.idempotency_max_keys, 4096);
        assert!(parsed.node_control.enabled);
        assert_eq!(
            parsed.node_control.auth_token.as_deref(),
            Some("node-token")
        );
        assert_eq!(
            parsed.node_control.allowed_node_ids,
            vec!["node-1", "node-2"]
        );
    }

    #[test]
    async fn checklist_gateway_backward_compat_no_gateway_section() {
        // Old configs without [gateway] should get secure defaults
        let minimal = r#"
workspace_dir = "/tmp/ws"
config_path = "/tmp/config.toml"
default_temperature = 0.7
"#;
        let parsed: Config = toml::from_str(minimal).unwrap();
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
        // Public contract: `/mnt` is blocked by default for safer host isolation.
        // Rollback path remains explicit user override via `autonomy.forbidden_paths`.
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
            a.forbidden_paths.contains(&"/mnt".to_string()),
            "Must block /mnt"
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
        let parsed: Config = toml::from_str(minimal).unwrap();
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
        let parsed: Config = toml::from_str(minimal).unwrap();
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
        assert!(!c.browser.enabled);
        assert!(c.browser.allowed_domains.is_empty());
    }

    #[test]
    async fn browser_config_default_disabled() {
        let b = BrowserConfig::default();
        assert!(!b.enabled);
        assert!(b.allowed_domains.is_empty());
        assert_eq!(b.backend, "agent_browser");
        assert!(b.auto_backend_priority.is_empty());
        assert_eq!(b.agent_browser_command, "agent-browser");
        assert!(b.agent_browser_extra_args.is_empty());
        assert_eq!(b.agent_browser_timeout_ms, 30_000);
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
            browser_open: "chrome".into(),
            session_name: None,
            backend: "auto".into(),
            auto_backend_priority: vec!["rust_native".into(), "agent_browser".into()],
            agent_browser_command: "/usr/local/bin/agent-browser".into(),
            agent_browser_extra_args: vec!["--sandbox".into(), "--trace".into()],
            agent_browser_timeout_ms: 45_000,
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
        assert_eq!(
            parsed.auto_backend_priority,
            vec!["rust_native".to_string(), "agent_browser".to_string()]
        );
        assert_eq!(parsed.agent_browser_command, "/usr/local/bin/agent-browser");
        assert_eq!(
            parsed.agent_browser_extra_args,
            vec!["--sandbox".to_string(), "--trace".to_string()]
        );
        assert_eq!(parsed.agent_browser_timeout_ms, 45_000);
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
        let parsed: Config = toml::from_str(minimal).unwrap();
        assert!(!parsed.browser.enabled);
        assert!(parsed.browser.allowed_domains.is_empty());
    }

    #[test]
    async fn web_search_config_default_extended_fields() {
        let ws = WebSearchConfig::default();
        assert_eq!(ws.provider, "duckduckgo");
        assert!(ws.fallback_providers.is_empty());
        assert_eq!(ws.retries_per_provider, 0);
        assert_eq!(ws.retry_backoff_ms, 250);
        assert!(ws.domain_filter.is_empty());
        assert!(ws.language_filter.is_empty());
        assert!(ws.country.is_none());
        assert!(ws.recency_filter.is_none());
        assert!(ws.max_tokens.is_none());
        assert!(ws.max_tokens_per_page.is_none());
        assert_eq!(ws.exa_search_type, "auto");
        assert!(!ws.exa_include_text);
        assert!(ws.jina_site_filters.is_empty());
    }

    #[test]
    async fn config_validate_rejects_unknown_browser_open_value() {
        let mut config = Config::default();
        config.browser.browser_open = "safari".into();

        let error = config
            .validate()
            .expect_err("expected browser.browser_open validation failure");
        assert!(error.to_string().contains("browser.browser_open"));
    }

    #[test]
    async fn config_validate_rejects_unknown_browser_backend_value() {
        let mut config = Config::default();
        config.browser.backend = "playwright".into();

        let error = config
            .validate()
            .expect_err("expected browser.backend validation failure");
        assert!(error.to_string().contains("browser.backend"));
    }

    #[test]
    async fn config_validate_rejects_invalid_auto_backend_priority_value() {
        let mut config = Config::default();
        config.browser.backend = "auto".into();
        config.browser.auto_backend_priority = vec!["auto".into()];

        let error = config
            .validate()
            .expect_err("expected browser.auto_backend_priority validation failure");
        assert!(error
            .to_string()
            .contains("browser.auto_backend_priority[0]"));
    }

    #[test]
    async fn config_validate_accepts_web_search_ddg_alias() {
        let mut config = Config::default();
        config.web_search.provider = "ddg".into();
        config.web_search.fallback_providers = vec!["jina".into()];

        config
            .validate()
            .expect("ddg alias should be accepted for web_search.provider");
    }

    #[test]
    async fn config_validate_rejects_unknown_web_search_provider() {
        let mut config = Config::default();
        config.web_search.provider = "serpapi".into();

        let error = config
            .validate()
            .expect_err("expected web_search.provider validation failure");
        assert!(error.to_string().contains("web_search.provider"));
    }

    #[test]
    async fn config_validate_rejects_unknown_web_search_fallback_provider() {
        let mut config = Config::default();
        config.web_search.fallback_providers = vec!["serpapi".into()];

        let error = config
            .validate()
            .expect_err("expected web_search.fallback_providers validation failure");
        assert!(error
            .to_string()
            .contains("web_search.fallback_providers[0]"));
    }

    #[test]
    async fn config_validate_rejects_invalid_web_search_exa_search_type() {
        let mut config = Config::default();
        config.web_search.exa_search_type = "semantic".into();

        let error = config
            .validate()
            .expect_err("expected web_search.exa_search_type validation failure");
        assert!(error.to_string().contains("web_search.exa_search_type"));
    }

    #[test]
    async fn config_validate_rejects_web_search_out_of_range_values() {
        let mut config = Config::default();
        config.web_search.max_results = 11;

        let error = config
            .validate()
            .expect_err("expected web_search.max_results validation failure");
        assert!(error.to_string().contains("web_search.max_results"));
    }

    #[test]
    async fn config_validate_rejects_web_search_excessive_retries() {
        let mut config = Config::default();
        config.web_search.retries_per_provider = 6;

        let error = config
            .validate()
            .expect_err("expected web_search.retries_per_provider validation failure");
        assert!(error
            .to_string()
            .contains("web_search.retries_per_provider"));
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
            std::env::remove_var(key);
        }
    }

    #[test]
    async fn env_override_api_key() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert!(config.api_key.is_none());

        std::env::set_var("ZEROCLAW_API_KEY", "sk-test-env-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("sk-test-env-key"));

        std::env::remove_var("ZEROCLAW_API_KEY");
    }

    #[test]
    async fn env_override_api_key_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_API_KEY");
        std::env::set_var("API_KEY", "sk-fallback-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("sk-fallback-key"));

        std::env::remove_var("API_KEY");
    }

    #[test]
    async fn env_override_api_key_generic_does_not_override_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.api_key = Some("sk-config-key".to_string());

        std::env::remove_var("ZEROCLAW_API_KEY");
        std::env::set_var("API_KEY", "sk-generic-env-key");
        config.apply_env_overrides();
        // Generic API_KEY must NOT override an existing config key
        assert_eq!(config.api_key.as_deref(), Some("sk-config-key"));

        std::env::remove_var("API_KEY");
    }

    #[test]
    async fn env_override_zeroclaw_api_key_overrides_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.api_key = Some("sk-config-key".to_string());

        std::env::set_var("ZEROCLAW_API_KEY", "sk-explicit-env-key");
        config.apply_env_overrides();
        // ZEROCLAW_API_KEY should always win, even over config
        assert_eq!(config.api_key.as_deref(), Some("sk-explicit-env-key"));

        std::env::remove_var("ZEROCLAW_API_KEY");
    }

    #[test]
    async fn env_override_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("ZEROCLAW_PROVIDER", "anthropic");
        config.apply_env_overrides();
        assert_eq!(config.default_provider.as_deref(), Some("anthropic"));

        std::env::remove_var("ZEROCLAW_PROVIDER");
    }

    #[test]
    async fn env_override_model_provider_alias() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_PROVIDER");
        std::env::set_var("ZEROCLAW_MODEL_PROVIDER", "openai-codex");
        config.apply_env_overrides();
        assert_eq!(config.default_provider.as_deref(), Some("openai-codex"));

        std::env::remove_var("ZEROCLAW_MODEL_PROVIDER");
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
model = "gpt-5.3-codex"
api_key = "profile-key"
requires_openai_auth = true
"#;

        let parsed: Config = toml::from_str(raw).expect("config should parse");
        assert_eq!(parsed.default_provider.as_deref(), Some("sub2api"));
        assert_eq!(parsed.default_model.as_deref(), Some("gpt-5.3-codex"));
        let profile = parsed
            .model_providers
            .get("sub2api")
            .expect("profile should exist");
        assert_eq!(profile.wire_api.as_deref(), Some("responses"));
        assert_eq!(profile.default_model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(profile.api_key.as_deref(), Some("profile-key"));
        assert!(profile.requires_openai_auth);
    }

    #[test]
    async fn env_override_open_skills_enabled_and_dir() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert!(!config.skills.open_skills_enabled);
        assert!(!config.skills.allow_scripts);
        assert!(config.skills.open_skills_dir.is_none());
        assert_eq!(
            config.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Full
        );

        std::env::set_var("ZEROCLAW_OPEN_SKILLS_ENABLED", "true");
        std::env::set_var("ZEROCLAW_OPEN_SKILLS_DIR", "/tmp/open-skills");
        std::env::set_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS", "yes");
        std::env::set_var("ZEROCLAW_SKILLS_PROMPT_MODE", "compact");
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

        std::env::remove_var("ZEROCLAW_OPEN_SKILLS_ENABLED");
        std::env::remove_var("ZEROCLAW_OPEN_SKILLS_DIR");
        std::env::remove_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS");
        std::env::remove_var("ZEROCLAW_SKILLS_PROMPT_MODE");
    }

    #[test]
    async fn env_override_open_skills_enabled_invalid_value_keeps_existing_value() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.skills.open_skills_enabled = true;
        config.skills.allow_scripts = true;
        config.skills.prompt_injection_mode = SkillsPromptInjectionMode::Compact;

        std::env::set_var("ZEROCLAW_OPEN_SKILLS_ENABLED", "maybe");
        std::env::set_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS", "maybe");
        std::env::set_var("ZEROCLAW_SKILLS_PROMPT_MODE", "invalid");
        config.apply_env_overrides();

        assert!(config.skills.open_skills_enabled);
        assert!(config.skills.allow_scripts);
        assert_eq!(
            config.skills.prompt_injection_mode,
            SkillsPromptInjectionMode::Compact
        );
        std::env::remove_var("ZEROCLAW_OPEN_SKILLS_ENABLED");
        std::env::remove_var("ZEROCLAW_SKILLS_ALLOW_SCRIPTS");
        std::env::remove_var("ZEROCLAW_SKILLS_PROMPT_MODE");
    }

    #[test]
    async fn env_override_provider_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_PROVIDER");
        std::env::set_var("PROVIDER", "openai");
        config.apply_env_overrides();
        assert_eq!(config.default_provider.as_deref(), Some("openai"));

        std::env::remove_var("PROVIDER");
    }

    #[test]
    async fn env_override_provider_fallback_does_not_replace_non_default_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("custom:https://proxy.example.com/v1".to_string()),
            ..Config::default()
        };

        std::env::remove_var("ZEROCLAW_PROVIDER");
        std::env::set_var("PROVIDER", "openrouter");
        config.apply_env_overrides();
        assert_eq!(
            config.default_provider.as_deref(),
            Some("custom:https://proxy.example.com/v1")
        );

        std::env::remove_var("PROVIDER");
    }

    #[test]
    async fn env_override_zero_claw_provider_overrides_non_default_provider() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("custom:https://proxy.example.com/v1".to_string()),
            ..Config::default()
        };

        std::env::set_var("ZEROCLAW_PROVIDER", "openrouter");
        std::env::set_var("PROVIDER", "anthropic");
        config.apply_env_overrides();
        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));

        std::env::remove_var("ZEROCLAW_PROVIDER");
        std::env::remove_var("PROVIDER");
    }

    #[test]
    async fn provider_api_requires_custom_default_provider() {
        let mut config = Config::default();
        config.default_provider = Some("openai".to_string());
        config.provider_api = Some(ProviderApiMode::OpenAiResponses);

        let err = config
            .validate()
            .expect_err("provider_api should be rejected for non-custom provider");
        assert!(err.to_string().contains(
            "provider_api is only valid when default_provider uses the custom:<url> format"
        ));
    }

    #[test]
    async fn provider_api_invalid_value_is_rejected() {
        let toml = r#"
default_provider = "custom:https://example.com/v1"
default_model = "gpt-4o"
default_temperature = 0.7
provider_api = "not-a-real-mode"
"#;
        let parsed = toml::from_str::<Config>(toml);
        assert!(
            parsed.is_err(),
            "invalid provider_api should fail to deserialize"
        );
    }

    #[test]
    async fn model_route_max_tokens_must_be_positive_when_set() {
        let mut config = Config::default();
        config.model_routes = vec![ModelRouteConfig {
            hint: "reasoning".to_string(),
            provider: "openrouter".to_string(),
            model: "anthropic/claude-sonnet-4.6".to_string(),
            max_tokens: Some(0),
            api_key: None,
            transport: None,
        }];

        let err = config
            .validate()
            .expect_err("model route max_tokens=0 should be rejected");
        assert!(err
            .to_string()
            .contains("model_routes[0].max_tokens must be greater than 0"));
    }

    #[test]
    async fn default_model_hint_requires_matching_model_route() {
        let mut config = Config::default();
        config.default_model = Some("hint:reasoning".to_string());
        config.model_routes = vec![ModelRouteConfig {
            hint: "fast".to_string(),
            provider: "openrouter".to_string(),
            model: "openai/gpt-5.2".to_string(),
            max_tokens: None,
            api_key: None,
            transport: None,
        }];

        let err = config
            .validate()
            .expect_err("default_model hint without matching route should fail");
        assert!(err
            .to_string()
            .contains("default_model uses hint 'reasoning'"));
    }

    #[test]
    async fn default_model_hint_accepts_matching_model_route() {
        let mut config = Config::default();
        config.default_model = Some("hint:reasoning".to_string());
        config.model_routes = vec![ModelRouteConfig {
            hint: "reasoning".to_string(),
            provider: "openrouter".to_string(),
            model: "openai/gpt-5.2".to_string(),
            max_tokens: None,
            api_key: None,
            transport: None,
        }];

        let result = config.validate();
        assert!(
            result.is_ok(),
            "matching default hint route should validate"
        );
    }

    #[test]
    async fn default_model_hint_accepts_matching_model_route_with_whitespace() {
        let mut config = Config::default();
        config.default_model = Some("hint: reasoning ".to_string());
        config.model_routes = vec![ModelRouteConfig {
            hint: " reasoning ".to_string(),
            provider: "openrouter".to_string(),
            model: "openai/gpt-5.2".to_string(),
            max_tokens: None,
            api_key: None,
            transport: None,
        }];

        let result = config.validate();
        assert!(
            result.is_ok(),
            "trimmed default hint should match trimmed route hint"
        );
    }

    #[test]
    async fn provider_transport_normalizes_aliases() {
        let mut config = Config::default();
        config.provider.transport = Some("WS".to_string());
        assert_eq!(
            config.effective_provider_transport().as_deref(),
            Some("websocket")
        );
    }

    #[test]
    async fn provider_transport_invalid_is_rejected() {
        let mut config = Config::default();
        config.provider.transport = Some("udp".to_string());
        let err = config
            .validate()
            .expect_err("provider.transport should reject invalid values");
        assert!(err
            .to_string()
            .contains("provider.transport must be one of: auto, websocket, sse"));
    }

    #[test]
    async fn model_route_transport_invalid_is_rejected() {
        let mut config = Config::default();
        config.model_routes = vec![ModelRouteConfig {
            hint: "reasoning".to_string(),
            provider: "openrouter".to_string(),
            model: "anthropic/claude-sonnet-4.6".to_string(),
            max_tokens: None,
            api_key: None,
            transport: Some("udp".to_string()),
        }];

        let err = config
            .validate()
            .expect_err("model_routes[].transport should reject invalid values");
        assert!(err
            .to_string()
            .contains("model_routes[0].transport must be one of: auto, websocket, sse"));
    }

    #[test]
    async fn env_override_glm_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("glm-cn".to_string()),
            ..Config::default()
        };

        std::env::set_var("GLM_API_KEY", "glm-regional-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("glm-regional-key"));

        std::env::remove_var("GLM_API_KEY");
    }

    #[test]
    async fn env_override_zeroclaw_api_key_beats_glm_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("glm-cn".to_string()),
            ..Config::default()
        };

        std::env::set_var("ZEROCLAW_API_KEY", "sk-explicit-env-key");
        std::env::set_var("GLM_API_KEY", "glm-regional-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("sk-explicit-env-key"));

        std::env::remove_var("ZEROCLAW_API_KEY");
        std::env::remove_var("GLM_API_KEY");
    }

    #[test]
    async fn env_override_zai_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("zai-cn".to_string()),
            ..Config::default()
        };

        std::env::set_var("ZAI_API_KEY", "zai-regional-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("zai-regional-key"));

        std::env::remove_var("ZAI_API_KEY");
    }

    #[test]
    async fn env_override_zeroclaw_api_key_beats_zai_api_key_for_regional_aliases() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("zai-cn".to_string()),
            ..Config::default()
        };

        std::env::set_var("ZEROCLAW_API_KEY", "sk-explicit-env-key");
        std::env::set_var("ZAI_API_KEY", "zai-regional-key");
        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("sk-explicit-env-key"));

        std::env::remove_var("ZEROCLAW_API_KEY");
        std::env::remove_var("ZAI_API_KEY");
    }

    #[test]
    async fn env_override_model() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("ZEROCLAW_MODEL", "gpt-4o");
        config.apply_env_overrides();
        assert_eq!(config.default_model.as_deref(), Some("gpt-4o"));

        std::env::remove_var("ZEROCLAW_MODEL");
    }

    #[test]
    async fn resolve_default_model_id_prefers_configured_model() {
        let resolved =
            resolve_default_model_id(Some("  anthropic/claude-opus-4.6  "), Some("openrouter"));
        assert_eq!(resolved, "anthropic/claude-opus-4.6");
    }

    #[test]
    async fn resolve_default_model_id_uses_provider_specific_fallback() {
        let openai = resolve_default_model_id(None, Some("openai"));
        assert_eq!(openai, "gpt-5.2");

        let stepfun = resolve_default_model_id(None, Some("stepfun"));
        assert_eq!(stepfun, "step-3.5-flash");

        let bedrock = resolve_default_model_id(None, Some("aws-bedrock"));
        assert_eq!(bedrock, "anthropic.claude-sonnet-4-5-20250929-v1:0");
    }

    #[test]
    async fn resolve_default_model_id_handles_special_provider_aliases() {
        let qwen_coding_plan = resolve_default_model_id(None, Some("qwen-coding-plan"));
        assert_eq!(qwen_coding_plan, "qwen3-coder-plus");

        let google_alias = resolve_default_model_id(None, Some("google-gemini"));
        assert_eq!(google_alias, "gemini-2.5-pro");

        let step_alias = resolve_default_model_id(None, Some("step"));
        assert_eq!(step_alias, "step-3.5-flash");

        let step_ai_alias = resolve_default_model_id(None, Some("step-ai"));
        assert_eq!(step_ai_alias, "step-3.5-flash");
    }

    #[test]
    async fn model_provider_profile_maps_to_custom_endpoint() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("sub2api".to_string()),
            model_providers: HashMap::from([(
                "sub2api".to_string(),
                ModelProviderConfig {
                    name: Some("sub2api".to_string()),
                    base_url: Some("https://api.tonsof.blue/v1".to_string()),
                    wire_api: None,
                    default_model: None,
                    api_key: None,
                    requires_openai_auth: false,
                },
            )]),
            ..Config::default()
        };

        config.apply_env_overrides();
        assert_eq!(
            config.default_provider.as_deref(),
            Some("custom:https://api.tonsof.blue/v1")
        );
        assert_eq!(
            config.api_url.as_deref(),
            Some("https://api.tonsof.blue/v1")
        );
    }

    #[test]
    async fn model_provider_profile_responses_uses_openai_codex_and_openai_key() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("sub2api".to_string()),
            model_providers: HashMap::from([(
                "sub2api".to_string(),
                ModelProviderConfig {
                    name: Some("sub2api".to_string()),
                    base_url: Some("https://api.tonsof.blue".to_string()),
                    wire_api: Some("responses".to_string()),
                    default_model: None,
                    api_key: None,
                    requires_openai_auth: true,
                },
            )]),
            api_key: None,
            ..Config::default()
        };

        std::env::set_var("OPENAI_API_KEY", "sk-test-codex-key");
        config.apply_env_overrides();
        std::env::remove_var("OPENAI_API_KEY");

        assert_eq!(config.default_provider.as_deref(), Some("openai-codex"));
        assert_eq!(config.api_url.as_deref(), Some("https://api.tonsof.blue"));
        assert_eq!(config.api_key.as_deref(), Some("sk-test-codex-key"));
    }

    #[test]
    async fn validate_ollama_cloud_model_requires_remote_api_url() {
        let _env_guard = env_override_lock().await;
        let config = Config {
            default_provider: Some("ollama".to_string()),
            default_model: Some("glm-5:cloud".to_string()),
            api_url: None,
            api_key: Some("ollama-key".to_string()),
            ..Config::default()
        };

        let error = config.validate().expect_err("expected validation to fail");
        assert!(error.to_string().contains(
            "default_model uses ':cloud' with provider 'ollama', but api_url is local or unset"
        ));
    }

    #[test]
    async fn validate_ollama_cloud_model_accepts_remote_endpoint_and_env_key() {
        let _env_guard = env_override_lock().await;
        let config = Config {
            default_provider: Some("ollama".to_string()),
            default_model: Some("glm-5:cloud".to_string()),
            api_url: Some("https://ollama.com/api".to_string()),
            api_key: None,
            ..Config::default()
        };

        std::env::set_var("OLLAMA_API_KEY", "ollama-env-key");
        let result = config.validate();
        std::env::remove_var("OLLAMA_API_KEY");

        assert!(result.is_ok(), "expected validation to pass: {result:?}");
    }

    #[test]
    async fn validate_rejects_unknown_model_provider_wire_api() {
        let _env_guard = env_override_lock().await;
        let config = Config {
            default_provider: Some("sub2api".to_string()),
            model_providers: HashMap::from([(
                "sub2api".to_string(),
                ModelProviderConfig {
                    name: Some("sub2api".to_string()),
                    base_url: Some("https://api.tonsof.blue/v1".to_string()),
                    wire_api: Some("ws".to_string()),
                    default_model: None,
                    api_key: None,
                    requires_openai_auth: false,
                },
            )]),
            ..Config::default()
        };

        let error = config.validate().expect_err("expected validation failure");
        assert!(error
            .to_string()
            .contains("wire_api must be one of: responses, chat_completions"));
    }

    #[test]
    async fn model_provider_profile_uses_profile_api_key_when_global_is_missing() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("sub2api".to_string()),
            api_key: None,
            model_providers: HashMap::from([(
                "sub2api".to_string(),
                ModelProviderConfig {
                    name: Some("sub2api".to_string()),
                    base_url: Some("https://api.tonsof.blue/v1".to_string()),
                    wire_api: None,
                    default_model: None,
                    api_key: Some("profile-api-key".to_string()),
                    requires_openai_auth: false,
                },
            )]),
            ..Config::default()
        };

        config.apply_env_overrides();
        assert_eq!(config.api_key.as_deref(), Some("profile-api-key"));
    }

    #[test]
    async fn model_provider_profile_can_override_default_model_when_openrouter_default_is_set() {
        let _env_guard = env_override_lock().await;
        let mut config = Config {
            default_provider: Some("sub2api".to_string()),
            default_model: Some(DEFAULT_MODEL_NAME.to_string()),
            model_providers: HashMap::from([(
                "sub2api".to_string(),
                ModelProviderConfig {
                    name: Some("sub2api".to_string()),
                    base_url: Some("https://api.tonsof.blue/v1".to_string()),
                    wire_api: None,
                    default_model: Some("qwen-max".to_string()),
                    api_key: None,
                    requires_openai_auth: false,
                },
            )]),
            ..Config::default()
        };

        config.apply_env_overrides();
        assert_eq!(config.default_model.as_deref(), Some("qwen-max"));
    }

    #[test]
    async fn env_override_model_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_MODEL");
        std::env::set_var("MODEL", "anthropic/claude-3.5-sonnet");
        config.apply_env_overrides();
        assert_eq!(
            config.default_model.as_deref(),
            Some("anthropic/claude-3.5-sonnet")
        );

        std::env::remove_var("MODEL");
    }

    #[test]
    async fn env_override_workspace() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("ZEROCLAW_WORKSPACE", "/custom/workspace");
        config.apply_env_overrides();
        assert_eq!(config.workspace_dir, PathBuf::from("/custom/workspace"));

        std::env::remove_var("ZEROCLAW_WORKSPACE");
    }

    #[test]
    async fn resolve_runtime_config_dirs_uses_env_workspace_first() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let workspace_dir = default_config_dir.join("profile-a");

        std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir);
        let (config_dir, resolved_workspace_dir, source) =
            resolve_runtime_config_dirs(&default_config_dir, &default_workspace_dir)
                .await
                .unwrap();

        assert_eq!(source, ConfigResolutionSource::EnvWorkspace);
        assert_eq!(config_dir, workspace_dir);
        assert_eq!(resolved_workspace_dir, workspace_dir.join("workspace"));

        std::env::remove_var("ZEROCLAW_WORKSPACE");
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

        std::env::set_var("ZEROCLAW_CONFIG_DIR", &explicit_config_dir);
        std::env::remove_var("ZEROCLAW_WORKSPACE");

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

        std::env::remove_var("ZEROCLAW_CONFIG_DIR");
        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_uses_active_workspace_marker() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let marker_config_dir = default_config_dir.join("profiles").join("alpha");
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        fs::create_dir_all(&default_config_dir).await.unwrap();
        fs::create_dir_all(&marker_config_dir).await.unwrap();
        fs::write(
            marker_config_dir.join("config.toml"),
            "default_model = \"marker-profile\"\n",
        )
        .await
        .unwrap();
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
    async fn resolve_runtime_config_dirs_ignores_marker_when_config_dir_missing() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let marker_config_dir = default_config_dir.join("profiles").join("missing-alpha");
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        std::env::remove_var("ZEROCLAW_WORKSPACE");
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

        assert_eq!(source, ConfigResolutionSource::DefaultConfigDir);
        assert_eq!(config_dir, default_config_dir);
        assert_eq!(resolved_workspace_dir, default_workspace_dir);

        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_ignores_marker_when_config_toml_missing() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");
        let marker_config_dir = default_config_dir.join("profiles").join("alpha-no-config");
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        fs::create_dir_all(&default_config_dir).await.unwrap();
        fs::create_dir_all(&marker_config_dir).await.unwrap();
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

        assert_eq!(source, ConfigResolutionSource::DefaultConfigDir);
        assert_eq!(config_dir, default_config_dir);
        assert_eq!(resolved_workspace_dir, default_workspace_dir);

        let _ = fs::remove_dir_all(default_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_ignores_temp_marker_outside_temp_default_root() {
        let _env_guard = env_override_lock().await;
        let base = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap());
        let non_temp_root = base.join(format!("zeroclaw_marker_guard_{}", uuid::Uuid::new_v4()));
        let default_config_dir = non_temp_root.join(".zeroclaw");
        let default_workspace_dir = default_config_dir.join("workspace");
        let marker_config_dir = std::env::temp_dir().join(format!(
            "zeroclaw_temp_marker_profile_{}",
            uuid::Uuid::new_v4()
        ));
        let state_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        if is_temp_directory(&default_config_dir) {
            // Extremely uncommon environment; skip this guard-specific test.
            return;
        }

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        fs::create_dir_all(&default_config_dir).await.unwrap();
        fs::create_dir_all(&marker_config_dir).await.unwrap();
        fs::write(
            marker_config_dir.join("config.toml"),
            "default_model = \"temp-marker\"\n",
        )
        .await
        .unwrap();
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

        assert_eq!(source, ConfigResolutionSource::DefaultConfigDir);
        assert_eq!(config_dir, default_config_dir);
        assert_eq!(resolved_workspace_dir, default_workspace_dir);

        let _ = fs::remove_dir_all(non_temp_root).await;
        let _ = fs::remove_dir_all(marker_config_dir).await;
    }

    #[test]
    async fn resolve_runtime_config_dirs_falls_back_to_default_layout() {
        let _env_guard = env_override_lock().await;
        let default_config_dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let default_workspace_dir = default_config_dir.join("workspace");

        std::env::remove_var("ZEROCLAW_WORKSPACE");
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
        std::env::set_var("HOME", &temp_home);
        std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir);

        let config = Config::load_or_init().await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir.join("workspace"));
        assert_eq!(config.config_path, workspace_dir.join("config.toml"));
        assert!(workspace_dir.join("config.toml").exists());

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
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
        std::env::set_var("HOME", &temp_home);
        std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir);

        let config = Config::load_or_init().await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir);
        assert_eq!(config.config_path, legacy_config_path);
        assert!(config.config_path.exists());

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
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
        std::env::set_var("HOME", &temp_home);
        std::env::set_var("ZEROCLAW_WORKSPACE", &workspace_dir);

        let config = Config::load_or_init().await.unwrap();

        assert_eq!(config.workspace_dir, workspace_dir);
        assert_eq!(config.config_path, legacy_config_path);
        assert_eq!(config.default_model.as_deref(), Some("legacy-model"));

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_uses_persisted_active_workspace_marker() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let custom_config_dir = temp_home.join("profiles").join("agent-alpha");

        fs::create_dir_all(&custom_config_dir).await.unwrap();
        fs::write(
            custom_config_dir.join("config.toml"),
            "default_temperature = 0.7\ndefault_model = \"persisted-profile\"\n",
        )
        .await
        .unwrap();

        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &temp_home);
        std::env::remove_var("ZEROCLAW_WORKSPACE");

        persist_active_workspace_config_dir(&custom_config_dir)
            .await
            .unwrap();

        let config = Config::load_or_init().await.unwrap();

        assert_eq!(config.config_path, custom_config_dir.join("config.toml"));
        assert_eq!(config.workspace_dir, custom_config_dir.join("workspace"));
        assert_eq!(config.default_model.as_deref(), Some("persisted-profile"));

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn load_or_init_env_workspace_override_takes_priority_over_marker() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let marker_config_dir = temp_home.join("profiles").join("persisted-profile");
        let env_workspace_dir = temp_home.join("env-workspace");

        fs::create_dir_all(&marker_config_dir).await.unwrap();
        fs::write(
            marker_config_dir.join("config.toml"),
            "default_temperature = 0.7\ndefault_model = \"marker-model\"\n",
        )
        .await
        .unwrap();

        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &temp_home);
        persist_active_workspace_config_dir(&marker_config_dir)
            .await
            .unwrap();
        std::env::set_var("ZEROCLAW_WORKSPACE", &env_workspace_dir);

        let config = Config::load_or_init().await.unwrap();

        assert_eq!(config.workspace_dir, env_workspace_dir.join("workspace"));
        assert_eq!(config.config_path, env_workspace_dir.join("config.toml"));

        std::env::remove_var("ZEROCLAW_WORKSPACE");
        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn persist_active_workspace_marker_is_cleared_for_default_config_dir() {
        let _env_guard = env_override_lock().await;
        let temp_home =
            std::env::temp_dir().join(format!("zeroclaw_test_home_{}", uuid::Uuid::new_v4()));
        let default_config_dir = temp_home.join(".zeroclaw");
        let custom_config_dir = temp_home.join("profiles").join("custom-profile");
        let marker_path = default_config_dir.join(ACTIVE_WORKSPACE_STATE_FILE);

        let original_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", &temp_home);

        persist_active_workspace_config_dir(&custom_config_dir)
            .await
            .unwrap();
        assert!(marker_path.exists());

        persist_active_workspace_config_dir(&default_config_dir)
            .await
            .unwrap();
        assert!(!marker_path.exists());

        if let Some(home) = original_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = fs::remove_dir_all(temp_home).await;
    }

    #[test]
    async fn env_override_empty_values_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_provider = config.default_provider.clone();

        std::env::set_var("ZEROCLAW_PROVIDER", "");
        config.apply_env_overrides();
        assert_eq!(config.default_provider, original_provider);

        std::env::remove_var("ZEROCLAW_PROVIDER");
    }

    #[test]
    async fn env_override_gateway_port() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.gateway.port, 42617);

        std::env::set_var("ZEROCLAW_GATEWAY_PORT", "8080");
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, 8080);

        std::env::remove_var("ZEROCLAW_GATEWAY_PORT");
    }

    #[test]
    async fn env_override_port_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_GATEWAY_PORT");
        std::env::set_var("PORT", "9000");
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, 9000);

        std::env::remove_var("PORT");
    }

    #[test]
    async fn env_override_gateway_host() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.gateway.host, "127.0.0.1");

        std::env::set_var("ZEROCLAW_GATEWAY_HOST", "0.0.0.0");
        config.apply_env_overrides();
        assert_eq!(config.gateway.host, "0.0.0.0");

        std::env::remove_var("ZEROCLAW_GATEWAY_HOST");
    }

    #[test]
    async fn env_override_host_fallback() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_GATEWAY_HOST");
        std::env::set_var("HOST", "0.0.0.0");
        config.apply_env_overrides();
        assert_eq!(config.gateway.host, "0.0.0.0");

        std::env::remove_var("HOST");
    }

    #[test]
    async fn env_override_temperature() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("ZEROCLAW_TEMPERATURE", "0.5");
        config.apply_env_overrides();
        assert!((config.default_temperature - 0.5).abs() < f64::EPSILON);

        std::env::remove_var("ZEROCLAW_TEMPERATURE");
    }

    #[test]
    async fn env_override_temperature_out_of_range_ignored() {
        let _env_guard = env_override_lock().await;
        // Clean up any leftover env vars from other tests
        std::env::remove_var("ZEROCLAW_TEMPERATURE");

        let mut config = Config::default();
        let original_temp = config.default_temperature;

        // Temperature > 2.0 should be ignored
        std::env::set_var("ZEROCLAW_TEMPERATURE", "3.0");
        config.apply_env_overrides();
        assert!(
            (config.default_temperature - original_temp).abs() < f64::EPSILON,
            "Temperature 3.0 should be ignored (out of range)"
        );

        std::env::remove_var("ZEROCLAW_TEMPERATURE");
    }

    #[test]
    async fn env_override_reasoning_enabled() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.runtime.reasoning_enabled, None);

        std::env::set_var("ZEROCLAW_REASONING_ENABLED", "false");
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(false));

        std::env::set_var("ZEROCLAW_REASONING_ENABLED", "true");
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(true));

        std::env::remove_var("ZEROCLAW_REASONING_ENABLED");
    }

    #[test]
    async fn env_override_reasoning_invalid_value_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.runtime.reasoning_enabled = Some(false);

        std::env::set_var("ZEROCLAW_REASONING_ENABLED", "maybe");
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_enabled, Some(false));

        std::env::remove_var("ZEROCLAW_REASONING_ENABLED");
    }

    #[test]
    async fn env_override_reasoning_level_alias() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.runtime.reasoning_level, None);

        std::env::set_var("ZEROCLAW_REASONING_LEVEL", "xhigh");
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_level.as_deref(), Some("xhigh"));
        assert_eq!(
            config.effective_provider_reasoning_level().as_deref(),
            Some("xhigh")
        );

        std::env::remove_var("ZEROCLAW_REASONING_LEVEL");
    }

    #[test]
    async fn env_override_reasoning_level_alias_invalid_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.runtime.reasoning_level = Some("medium".to_string());

        std::env::set_var("ZEROCLAW_REASONING_LEVEL", "invalid");
        config.apply_env_overrides();
        assert_eq!(config.runtime.reasoning_level.as_deref(), Some("medium"));

        std::env::remove_var("ZEROCLAW_REASONING_LEVEL");
    }

    #[test]
    async fn env_override_provider_transport_normalizes_zeroclaw_alias() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("PROVIDER_TRANSPORT");
        std::env::set_var("ZEROCLAW_PROVIDER_TRANSPORT", "WS");
        config.apply_env_overrides();
        assert_eq!(config.provider.transport.as_deref(), Some("websocket"));

        std::env::remove_var("ZEROCLAW_PROVIDER_TRANSPORT");
    }

    #[test]
    async fn env_override_provider_transport_normalizes_legacy_alias() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::remove_var("ZEROCLAW_PROVIDER_TRANSPORT");
        std::env::set_var("PROVIDER_TRANSPORT", "HTTP");
        config.apply_env_overrides();
        assert_eq!(config.provider.transport.as_deref(), Some("sse"));

        std::env::remove_var("PROVIDER_TRANSPORT");
    }

    #[test]
    async fn env_override_provider_transport_invalid_zeroclaw_does_not_override_existing() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.provider.transport = Some("sse".to_string());

        std::env::remove_var("PROVIDER_TRANSPORT");
        std::env::set_var("ZEROCLAW_PROVIDER_TRANSPORT", "udp");
        config.apply_env_overrides();
        assert_eq!(config.provider.transport.as_deref(), Some("sse"));

        std::env::remove_var("ZEROCLAW_PROVIDER_TRANSPORT");
    }

    #[test]
    async fn env_override_provider_transport_invalid_legacy_does_not_override_existing() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        config.provider.transport = Some("auto".to_string());

        std::env::remove_var("ZEROCLAW_PROVIDER_TRANSPORT");
        std::env::set_var("PROVIDER_TRANSPORT", "udp");
        config.apply_env_overrides();
        assert_eq!(config.provider.transport.as_deref(), Some("auto"));

        std::env::remove_var("PROVIDER_TRANSPORT");
    }

    #[test]
    async fn env_override_model_support_vision() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        assert_eq!(config.model_support_vision, None);

        std::env::set_var("ZEROCLAW_MODEL_SUPPORT_VISION", "true");
        config.apply_env_overrides();
        assert_eq!(config.model_support_vision, Some(true));

        std::env::set_var("ZEROCLAW_MODEL_SUPPORT_VISION", "false");
        config.apply_env_overrides();
        assert_eq!(config.model_support_vision, Some(false));

        std::env::set_var("ZEROCLAW_MODEL_SUPPORT_VISION", "maybe");
        config.model_support_vision = Some(true);
        config.apply_env_overrides();
        assert_eq!(config.model_support_vision, Some(true));

        std::env::remove_var("ZEROCLAW_MODEL_SUPPORT_VISION");
    }

    #[test]
    async fn env_override_invalid_port_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_port = config.gateway.port;

        std::env::set_var("PORT", "not_a_number");
        config.apply_env_overrides();
        assert_eq!(config.gateway.port, original_port);

        std::env::remove_var("PORT");
    }

    #[test]
    async fn env_override_web_search_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("WEB_SEARCH_ENABLED", "false");
        std::env::set_var("WEB_SEARCH_PROVIDER", "brave");
        std::env::set_var("WEB_SEARCH_MAX_RESULTS", "7");
        std::env::set_var("WEB_SEARCH_TIMEOUT_SECS", "20");
        std::env::set_var("WEB_SEARCH_FALLBACK_PROVIDERS", "tavily,firecrawl");
        std::env::set_var("WEB_SEARCH_RETRIES_PER_PROVIDER", "2");
        std::env::set_var("WEB_SEARCH_RETRY_BACKOFF_MS", "400");
        std::env::set_var("WEB_SEARCH_DOMAIN_FILTER", "docs.rs,github.com");
        std::env::set_var("WEB_SEARCH_LANGUAGE_FILTER", "en,zh");
        std::env::set_var("WEB_SEARCH_COUNTRY", "US");
        std::env::set_var("WEB_SEARCH_RECENCY_FILTER", "day");
        std::env::set_var("WEB_SEARCH_MAX_TOKENS", "4096");
        std::env::set_var("WEB_SEARCH_MAX_TOKENS_PER_PAGE", "1024");
        std::env::set_var("WEB_SEARCH_EXA_SEARCH_TYPE", "neural");
        std::env::set_var("WEB_SEARCH_EXA_INCLUDE_TEXT", "true");
        std::env::set_var("WEB_SEARCH_JINA_SITE_FILTERS", "arxiv.org,openai.com");
        std::env::set_var("BRAVE_API_KEY", "brave-test-key");
        std::env::set_var("PERPLEXITY_API_KEY", "perplexity-test-key");
        std::env::set_var("EXA_API_KEY", "exa-test-key");
        std::env::set_var("JINA_API_KEY", "jina-test-key");

        config.apply_env_overrides();

        assert!(!config.web_search.enabled);
        assert_eq!(config.web_search.provider, "brave");
        assert_eq!(config.web_search.max_results, 7);
        assert_eq!(config.web_search.timeout_secs, 20);
        assert_eq!(
            config.web_search.fallback_providers,
            vec!["tavily".to_string(), "firecrawl".to_string()]
        );
        assert_eq!(config.web_search.retries_per_provider, 2);
        assert_eq!(config.web_search.retry_backoff_ms, 400);
        assert_eq!(
            config.web_search.domain_filter,
            vec!["docs.rs".to_string(), "github.com".to_string()]
        );
        assert_eq!(
            config.web_search.language_filter,
            vec!["en".to_string(), "zh".to_string()]
        );
        assert_eq!(config.web_search.country.as_deref(), Some("US"));
        assert_eq!(config.web_search.recency_filter.as_deref(), Some("day"));
        assert_eq!(config.web_search.max_tokens, Some(4096));
        assert_eq!(config.web_search.max_tokens_per_page, Some(1024));
        assert_eq!(config.web_search.exa_search_type, "neural");
        assert!(config.web_search.exa_include_text);
        assert_eq!(
            config.web_search.jina_site_filters,
            vec!["arxiv.org".to_string(), "openai.com".to_string()]
        );
        assert_eq!(
            config.web_search.brave_api_key.as_deref(),
            Some("brave-test-key")
        );
        assert_eq!(
            config.web_search.perplexity_api_key.as_deref(),
            Some("perplexity-test-key")
        );
        assert_eq!(
            config.web_search.exa_api_key.as_deref(),
            Some("exa-test-key")
        );
        assert_eq!(
            config.web_search.jina_api_key.as_deref(),
            Some("jina-test-key")
        );

        std::env::remove_var("WEB_SEARCH_ENABLED");
        std::env::remove_var("WEB_SEARCH_PROVIDER");
        std::env::remove_var("WEB_SEARCH_MAX_RESULTS");
        std::env::remove_var("WEB_SEARCH_TIMEOUT_SECS");
        std::env::remove_var("WEB_SEARCH_FALLBACK_PROVIDERS");
        std::env::remove_var("WEB_SEARCH_RETRIES_PER_PROVIDER");
        std::env::remove_var("WEB_SEARCH_RETRY_BACKOFF_MS");
        std::env::remove_var("WEB_SEARCH_DOMAIN_FILTER");
        std::env::remove_var("WEB_SEARCH_LANGUAGE_FILTER");
        std::env::remove_var("WEB_SEARCH_COUNTRY");
        std::env::remove_var("WEB_SEARCH_RECENCY_FILTER");
        std::env::remove_var("WEB_SEARCH_MAX_TOKENS");
        std::env::remove_var("WEB_SEARCH_MAX_TOKENS_PER_PAGE");
        std::env::remove_var("WEB_SEARCH_EXA_SEARCH_TYPE");
        std::env::remove_var("WEB_SEARCH_EXA_INCLUDE_TEXT");
        std::env::remove_var("WEB_SEARCH_JINA_SITE_FILTERS");
        std::env::remove_var("BRAVE_API_KEY");
        std::env::remove_var("PERPLEXITY_API_KEY");
        std::env::remove_var("EXA_API_KEY");
        std::env::remove_var("JINA_API_KEY");
    }

    #[test]
    async fn env_override_web_search_invalid_values_ignored() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();
        let original_max_results = config.web_search.max_results;
        let original_timeout = config.web_search.timeout_secs;

        std::env::set_var("WEB_SEARCH_MAX_RESULTS", "99");
        std::env::set_var("WEB_SEARCH_TIMEOUT_SECS", "0");

        config.apply_env_overrides();

        assert_eq!(config.web_search.max_results, original_max_results);
        assert_eq!(config.web_search.timeout_secs, original_timeout);

        std::env::remove_var("WEB_SEARCH_MAX_RESULTS");
        std::env::remove_var("WEB_SEARCH_TIMEOUT_SECS");
    }

    #[test]
    async fn env_override_url_access_policy() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL", "true");
        std::env::set_var("URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST", "1");
        std::env::set_var("URL_ACCESS_DOMAIN_ALLOWLIST", "docs.rs,github.com");
        std::env::set_var(
            "URL_ACCESS_DOMAIN_BLOCKLIST",
            "evil.example,*.tracking.local",
        );
        std::env::set_var("URL_ACCESS_APPROVED_DOMAINS", "rust-lang.org");

        config.apply_env_overrides();

        assert!(config.security.url_access.require_first_visit_approval);
        assert!(config.security.url_access.enforce_domain_allowlist);
        assert_eq!(
            config.security.url_access.domain_allowlist,
            vec!["docs.rs".to_string(), "github.com".to_string()]
        );
        assert_eq!(
            config.security.url_access.domain_blocklist,
            vec!["evil.example".to_string(), "*.tracking.local".to_string()]
        );
        assert_eq!(
            config.security.url_access.approved_domains,
            vec!["rust-lang.org".to_string()]
        );

        std::env::remove_var("URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL");
        std::env::remove_var("URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST");
        std::env::remove_var("URL_ACCESS_DOMAIN_ALLOWLIST");
        std::env::remove_var("URL_ACCESS_DOMAIN_BLOCKLIST");
        std::env::remove_var("URL_ACCESS_APPROVED_DOMAINS");
    }

    #[test]
    async fn env_override_storage_provider_config() {
        let _env_guard = env_override_lock().await;
        let mut config = Config::default();

        std::env::set_var("ZEROCLAW_STORAGE_PROVIDER", "postgres");
        std::env::set_var("ZEROCLAW_STORAGE_DB_URL", "postgres://example/db");
        std::env::set_var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS", "15");

        config.apply_env_overrides();

        assert_eq!(config.storage.provider.config.provider, "postgres");
        assert_eq!(
            config.storage.provider.config.db_url.as_deref(),
            Some("postgres://example/db")
        );
        assert_eq!(
            config.storage.provider.config.connect_timeout_secs,
            Some(15)
        );

        std::env::remove_var("ZEROCLAW_STORAGE_PROVIDER");
        std::env::remove_var("ZEROCLAW_STORAGE_DB_URL");
        std::env::remove_var("ZEROCLAW_STORAGE_CONNECT_TIMEOUT_SECS");
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
        std::env::set_var("ZEROCLAW_PROXY_ENABLED", "true");
        std::env::set_var("ZEROCLAW_HTTP_PROXY", "http://127.0.0.1:7890");
        std::env::set_var(
            "ZEROCLAW_PROXY_SERVICES",
            "provider.openai, tool.http_request",
        );
        std::env::set_var("ZEROCLAW_PROXY_SCOPE", "services");

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
        std::env::set_var("ZEROCLAW_PROXY_ENABLED", "true");
        std::env::set_var("ZEROCLAW_PROXY_SCOPE", "environment");
        std::env::set_var("ZEROCLAW_HTTP_PROXY", "http://127.0.0.1:7890");
        std::env::set_var("ZEROCLAW_HTTPS_PROXY", "http://127.0.0.1:7891");
        std::env::set_var("ZEROCLAW_NO_PROXY", "localhost,127.0.0.1");

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
        assert!(std::env::var("NO_PROXY")
            .ok()
            .is_some_and(|value| value.contains("localhost")));

        clear_proxy_env_test_vars();
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
        assert!(!g.node_control.enabled);
        assert!(g.node_control.auth_token.is_none());
        assert!(g.node_control.allowed_node_ids.is_empty());
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
            app_id: "cli_123456".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["user_123".into(), "user_456".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: true,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
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
            app_id: "cli_123456".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["*".into()],
            mention_only: false,
            group_reply: None,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
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
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
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
    async fn lark_group_reply_mode_overrides_legacy_mention_only() {
        let json = r#"{
            "app_id":"cli_123",
            "app_secret":"secret",
            "mention_only":true,
            "group_reply":{
                "mode":"all_messages",
                "allowed_sender_ids":["ou_1"]
            }
        }"#;
        let parsed: LarkConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["ou_1".to_string()]
        );
    }

    #[test]
    async fn feishu_config_serde() {
        let fc = FeishuConfig {
            app_id: "cli_feishu_123".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["user_123".into(), "user_456".into()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
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
            app_id: "cli_feishu_123".into(),
            app_secret: "secret_abc".into(),
            encrypt_key: Some("encrypt_key".into()),
            verification_token: Some("verify_token".into()),
            allowed_users: vec!["*".into()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(9898),
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
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
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
    }

    #[test]
    async fn feishu_group_reply_mode_supports_mention_only() {
        let json = r#"{
            "app_id":"cli_123",
            "app_secret":"secret",
            "group_reply":{
                "mode":"mention_only",
                "allowed_sender_ids":["ou_9"]
            }
        }"#;
        let parsed: FeishuConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
        assert_eq!(
            parsed.group_reply_allowed_sender_ids(),
            vec!["ou_9".to_string()]
        );
    }

    #[test]
    async fn feishu_legacy_key_extractors_detect_compat_fields() {
        let raw: toml::Value = toml::from_str(
            r#"
[channels_config.feishu]
app_id = "cli_123"
app_secret = "secret"
mention_only = true
use_feishu = true
"#,
        )
        .unwrap();

        assert_eq!(extract_legacy_feishu_mention_only(&raw), Some(true));
        assert!(has_legacy_feishu_mention_only(&raw));
        assert!(has_legacy_feishu_use_feishu(&raw));
    }

    #[test]
    async fn feishu_legacy_mention_only_maps_to_group_reply_mode() {
        let mut parsed = Config::default();
        parsed.channels_config.feishu = Some(FeishuConfig {
            app_id: "cli_123".into(),
            app_secret: "secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            group_reply: None,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
        });

        apply_feishu_legacy_compat(&mut parsed, Some(true), true, true, true);

        let feishu = parsed
            .channels_config
            .feishu
            .expect("feishu config should exist");
        assert_eq!(
            feishu.effective_group_reply_mode(),
            GroupReplyMode::MentionOnly
        );
    }

    #[test]
    async fn feishu_legacy_mention_only_does_not_override_group_reply() {
        let mut parsed = Config::default();
        parsed.channels_config.feishu = Some(FeishuConfig {
            app_id: "cli_123".into(),
            app_secret: "secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            group_reply: Some(GroupReplyConfig {
                mode: Some(GroupReplyMode::AllMessages),
                allowed_sender_ids: vec![],
            }),
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: default_lark_draft_update_interval_ms(),
            max_draft_edits: default_lark_max_draft_edits(),
        });

        apply_feishu_legacy_compat(&mut parsed, Some(true), false, true, false);

        let feishu = parsed
            .channels_config
            .feishu
            .expect("feishu config should exist");
        assert_eq!(
            feishu.effective_group_reply_mode(),
            GroupReplyMode::AllMessages
        );
    }

    #[test]
    async fn qq_config_defaults_to_webhook_receive_mode() {
        let json = r#"{"app_id":"123","app_secret":"secret"}"#;
        let parsed: QQConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.receive_mode, QQReceiveMode::Webhook);
        assert_eq!(parsed.environment, QQEnvironment::Production);
        assert!(parsed.allowed_users.is_empty());
    }

    #[test]
    async fn qq_config_toml_roundtrip_receive_mode() {
        let qc = QQConfig {
            app_id: "123".into(),
            app_secret: "secret".into(),
            allowed_users: vec!["*".into()],
            receive_mode: QQReceiveMode::Websocket,
            environment: QQEnvironment::Sandbox,
        };
        let toml_str = toml::to_string(&qc).unwrap();
        let parsed: QQConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.receive_mode, QQReceiveMode::Websocket);
        assert_eq!(parsed.environment, QQEnvironment::Sandbox);
        assert_eq!(parsed.allowed_users, vec!["*"]);
    }

    #[test]
    async fn dingtalk_config_defaults_allowed_users_to_empty() {
        let json = r#"{"client_id":"ding-app-key","client_secret":"ding-app-secret"}"#;
        let parsed: DingTalkConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.client_id, "ding-app-key");
        assert_eq!(parsed.client_secret, "ding-app-secret");
        assert!(parsed.allowed_users.is_empty());
    }

    #[test]
    async fn dingtalk_config_toml_roundtrip() {
        let dc = DingTalkConfig {
            client_id: "ding-app-key".into(),
            client_secret: "ding-app-secret".into(),
            allowed_users: vec!["*".into(), "staff123".into()],
        };
        let toml_str = toml::to_string(&dc).unwrap();
        let parsed: DingTalkConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.client_id, "ding-app-key");
        assert_eq!(parsed.client_secret, "ding-app-secret");
        assert_eq!(parsed.allowed_users, vec!["*", "staff123"]);
    }

    #[test]
    async fn channels_except_webhook_reports_dingtalk_as_enabled() {
        let mut channels = ChannelsConfig::default();
        channels.dingtalk = Some(DingTalkConfig {
            client_id: "ding-app-key".into(),
            client_secret: "ding-app-secret".into(),
            allowed_users: vec!["*".into()],
        });

        let dingtalk_state = channels
            .channels_except_webhook()
            .iter()
            .find_map(|(handle, enabled)| (handle.name() == "DingTalk").then_some(*enabled));

        assert_eq!(dingtalk_state, Some(true));
    }

    #[test]
    async fn nextcloud_talk_config_serde() {
        let nc = NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: Some("webhook-secret".into()),
            allowed_users: vec!["user_a".into(), "*".into()],
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
        let mut config = Config::default();
        config.config_path = config_path.clone();
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

        let mut config = Config::default();
        config.config_path = config_path.clone();
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

        config.default_temperature = 0.6;
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
        assert!(tc.api_key.is_none());
        assert!(tc.api_url.contains("groq.com"));
        assert_eq!(tc.model, "whisper-large-v3-turbo");
        assert!(tc.language.is_none());
        assert_eq!(tc.max_duration_secs, 120);
    }

    #[test]
    async fn config_roundtrip_with_transcription() {
        let mut config = Config::default();
        config.transcription.enabled = true;
        config.transcription.api_key = Some("transcription-key".into());
        config.transcription.language = Some("en".into());

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();

        assert!(parsed.transcription.enabled);
        assert_eq!(
            parsed.transcription.api_key.as_deref(),
            Some("transcription-key")
        );
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
        let parsed: Config = toml::from_str(toml_str).unwrap();
        assert!(!parsed.transcription.enabled);
        assert_eq!(parsed.transcription.max_duration_secs, 120);
    }

    #[test]
    async fn security_defaults_are_backward_compatible() {
        let parsed: Config = toml::from_str(
            r#"
default_provider = "openrouter"
default_model = "anthropic/claude-sonnet-4.6"
default_temperature = 0.7
"#,
        )
        .unwrap();

        assert!(parsed.security.otp.enabled);
        assert_eq!(parsed.security.otp.method, OtpMethod::Totp);
        assert_eq!(
            parsed.security.otp.challenge_delivery,
            OtpChallengeDelivery::Dm
        );
        assert_eq!(parsed.security.otp.challenge_timeout_secs, 120);
        assert_eq!(parsed.security.otp.challenge_max_attempts, 3);
        assert!(parsed.security.roles.is_empty());
        assert!(!parsed.security.estop.enabled);
        assert!(parsed.security.estop.require_otp_to_resume);
        assert!(parsed.security.syscall_anomaly.enabled);
        assert!(parsed.security.syscall_anomaly.alert_on_unknown_syscall);
        assert!(!parsed.security.syscall_anomaly.baseline_syscalls.is_empty());
        assert!(parsed.security.url_access.block_private_ip);
        assert!(parsed.security.url_access.allow_cidrs.is_empty());
        assert!(parsed.security.url_access.allow_domains.is_empty());
        assert!(!parsed.security.url_access.allow_loopback);
        assert!(!parsed.security.url_access.require_first_visit_approval);
        assert!(!parsed.security.url_access.enforce_domain_allowlist);
        assert!(parsed.security.url_access.domain_allowlist.is_empty());
        assert!(parsed.security.url_access.domain_blocklist.is_empty());
        assert!(parsed.security.url_access.approved_domains.is_empty());
        assert!(!parsed.security.perplexity_filter.enable_perplexity_filter);
        assert!(parsed.security.outbound_leak_guard.enabled);
        assert_eq!(
            parsed.security.outbound_leak_guard.action,
            OutboundLeakGuardAction::Redact
        );
        assert_eq!(parsed.security.outbound_leak_guard.sensitivity, 0.7);
    }

    #[test]
    async fn security_toml_parses_otp_and_estop_sections() {
        let parsed: Config = toml::from_str(
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
challenge_delivery = "thread"
challenge_timeout_secs = 180
challenge_max_attempts = 4

[[security.roles]]
name = "developer"
description = "Developer role"
allowed_tools = ["shell", "file_read", "file_write"]
denied_tools = ["memory_forget"]
totp_gated = ["shell", "file_write"]
inherits = "operator"
gated_domains = ["*.chase.com"]
gated_domain_categories = ["banking"]

[security.estop]
enabled = true
state_file = "~/.zeroclaw/estop-state.json"
require_otp_to_resume = true

[security.syscall_anomaly]
enabled = true
strict_mode = true
alert_on_unknown_syscall = true
max_denied_events_per_minute = 3
max_total_events_per_minute = 60
max_alerts_per_minute = 10
alert_cooldown_secs = 15
log_path = "syscall-anomalies.log"
baseline_syscalls = ["read", "write", "openat", "close"]

[security.perplexity_filter]
enable_perplexity_filter = true
perplexity_threshold = 16.5
suffix_window_chars = 72
min_prompt_chars = 40
symbol_ratio_threshold = 0.25

[security.outbound_leak_guard]
enabled = true
action = "block"
sensitivity = 0.9
"#,
        )
        .unwrap();

        assert!(parsed.security.otp.enabled);
        assert!(parsed.security.estop.enabled);
        assert!(parsed.security.syscall_anomaly.strict_mode);
        assert_eq!(
            parsed.security.syscall_anomaly.max_denied_events_per_minute,
            3
        );
        assert_eq!(
            parsed.security.syscall_anomaly.max_total_events_per_minute,
            60
        );
        assert_eq!(parsed.security.syscall_anomaly.max_alerts_per_minute, 10);
        assert_eq!(parsed.security.syscall_anomaly.alert_cooldown_secs, 15);
        assert_eq!(parsed.security.syscall_anomaly.baseline_syscalls.len(), 4);
        assert!(parsed.security.perplexity_filter.enable_perplexity_filter);
        assert_eq!(parsed.security.perplexity_filter.perplexity_threshold, 16.5);
        assert_eq!(parsed.security.perplexity_filter.suffix_window_chars, 72);
        assert_eq!(parsed.security.perplexity_filter.min_prompt_chars, 40);
        assert_eq!(
            parsed.security.perplexity_filter.symbol_ratio_threshold,
            0.25
        );
        assert!(parsed.security.outbound_leak_guard.enabled);
        assert_eq!(
            parsed.security.outbound_leak_guard.action,
            OutboundLeakGuardAction::Block
        );
        assert_eq!(parsed.security.outbound_leak_guard.sensitivity, 0.9);
        assert_eq!(parsed.security.otp.gated_actions.len(), 2);
        assert_eq!(parsed.security.otp.gated_domains.len(), 2);
        assert_eq!(
            parsed.security.otp.challenge_delivery,
            OtpChallengeDelivery::Thread
        );
        assert_eq!(parsed.security.otp.challenge_timeout_secs, 180);
        assert_eq!(parsed.security.otp.challenge_max_attempts, 4);
        assert_eq!(parsed.security.roles.len(), 1);
        assert_eq!(parsed.security.roles[0].name, "developer");
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
    async fn security_validation_rejects_invalid_url_access_cidr() {
        let mut config = Config::default();
        config.security.url_access.allow_cidrs = vec!["10.0.0.0".into()];
        let err = config.validate().expect_err("expected invalid CIDR");
        assert!(err.to_string().contains("security.url_access.allow_cidrs"));
    }

    #[test]
    async fn security_validation_rejects_blank_url_access_domain() {
        let mut config = Config::default();
        config.security.url_access.allow_domains = vec!["   ".into()];
        let err = config
            .validate()
            .expect_err("expected invalid URL allow domain");
        assert!(err
            .to_string()
            .contains("security.url_access.allow_domains"));
    }

    #[test]
    async fn security_validation_rejects_blank_url_access_domain_allowlist_entry() {
        let mut config = Config::default();
        config.security.url_access.domain_allowlist = vec!["  ".into()];
        let err = config
            .validate()
            .expect_err("expected invalid URL domain_allowlist entry");
        assert!(err
            .to_string()
            .contains("security.url_access.domain_allowlist"));
    }

    #[test]
    async fn security_validation_rejects_blank_url_access_domain_blocklist_entry() {
        let mut config = Config::default();
        config.security.url_access.domain_blocklist = vec!["  ".into()];
        let err = config
            .validate()
            .expect_err("expected invalid URL domain_blocklist entry");
        assert!(err
            .to_string()
            .contains("security.url_access.domain_blocklist"));
    }

    #[test]
    async fn security_validation_rejects_blank_url_access_approved_domain_entry() {
        let mut config = Config::default();
        config.security.url_access.approved_domains = vec!["  ".into()];
        let err = config
            .validate()
            .expect_err("expected invalid URL approved_domains entry");
        assert!(err
            .to_string()
            .contains("security.url_access.approved_domains"));
    }

    #[test]
    async fn security_validation_requires_allowlist_when_enforcement_enabled() {
        let mut config = Config::default();
        config.security.url_access.enforce_domain_allowlist = true;
        let err = config
            .validate()
            .expect_err("expected allowlist enforcement validation failure");
        assert!(err
            .to_string()
            .contains("security.url_access.enforce_domain_allowlist"));
    }

    #[test]
    async fn security_validation_rejects_invalid_http_credential_profile_env_var() {
        let mut config = Config::default();
        config.http_request.credential_profiles.insert(
            "github".to_string(),
            HttpRequestCredentialProfile {
                env_var: "NOT VALID".to_string(),
                ..HttpRequestCredentialProfile::default()
            },
        );

        let err = config
            .validate()
            .expect_err("expected invalid http credential env var");
        assert!(err
            .to_string()
            .contains("http_request.credential_profiles.github.env_var"));
    }

    #[test]
    async fn security_validation_rejects_empty_http_credential_profile_header_name() {
        let mut config = Config::default();
        config.http_request.credential_profiles.insert(
            "linear".to_string(),
            HttpRequestCredentialProfile {
                header_name: "   ".to_string(),
                env_var: "LINEAR_API_KEY".to_string(),
                ..HttpRequestCredentialProfile::default()
            },
        );

        let err = config
            .validate()
            .expect_err("expected empty header_name validation failure");
        assert!(err
            .to_string()
            .contains("http_request.credential_profiles.linear.header_name"));
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

    #[test]
    async fn security_validation_rejects_zero_challenge_timeout() {
        let mut config = Config::default();
        config.security.otp.challenge_timeout_secs = 0;

        let err = config
            .validate()
            .expect_err("expected challenge timeout validation failure");
        assert!(err.to_string().contains("challenge_timeout_secs"));
    }

    #[test]
    async fn security_validation_rejects_zero_challenge_attempts() {
        let mut config = Config::default();
        config.security.otp.challenge_max_attempts = 0;

        let err = config
            .validate()
            .expect_err("expected challenge attempts validation failure");
        assert!(err.to_string().contains("challenge_max_attempts"));
    }

    #[test]
    async fn security_validation_rejects_unknown_role_parent() {
        let mut config = Config::default();
        config.security.roles = vec![SecurityRoleConfig {
            name: "developer".to_string(),
            inherits: Some("missing-parent".to_string()),
            ..SecurityRoleConfig::default()
        }];

        let err = config
            .validate()
            .expect_err("expected unknown role parent validation failure");
        assert!(err.to_string().contains("inherits references unknown role"));
    }

    #[test]
    async fn security_validation_rejects_duplicate_role_name() {
        let mut config = Config::default();
        config.security.roles = vec![
            SecurityRoleConfig {
                name: "developer".to_string(),
                ..SecurityRoleConfig::default()
            },
            SecurityRoleConfig {
                name: "Developer".to_string(),
                ..SecurityRoleConfig::default()
            },
        ];

        let err = config
            .validate()
            .expect_err("expected duplicate role validation failure");
        assert!(err.to_string().contains("duplicate role"));
    }

    #[test]
    async fn security_validation_rejects_zero_syscall_threshold() {
        let mut config = Config::default();
        config.security.syscall_anomaly.max_denied_events_per_minute = 0;

        let err = config
            .validate()
            .expect_err("expected syscall threshold validation failure");
        assert!(err.to_string().contains("max_denied_events_per_minute"));
    }

    #[test]
    async fn security_validation_rejects_invalid_syscall_baseline_name() {
        let mut config = Config::default();
        config.security.syscall_anomaly.baseline_syscalls =
            vec!["openat".into(), "bad name".into()];

        let err = config
            .validate()
            .expect_err("expected syscall baseline name validation failure");
        assert!(err.to_string().contains("baseline_syscalls"));
    }

    #[test]
    async fn security_validation_rejects_zero_syscall_alert_budget() {
        let mut config = Config::default();
        config.security.syscall_anomaly.max_alerts_per_minute = 0;

        let err = config
            .validate()
            .expect_err("expected syscall alert budget validation failure");
        assert!(err.to_string().contains("max_alerts_per_minute"));
    }

    #[test]
    async fn security_validation_rejects_zero_syscall_cooldown() {
        let mut config = Config::default();
        config.security.syscall_anomaly.alert_cooldown_secs = 0;

        let err = config
            .validate()
            .expect_err("expected syscall cooldown validation failure");
        assert!(err.to_string().contains("alert_cooldown_secs"));
    }

    #[test]
    async fn security_validation_rejects_denied_threshold_above_total_threshold() {
        let mut config = Config::default();
        config.security.syscall_anomaly.max_denied_events_per_minute = 10;
        config.security.syscall_anomaly.max_total_events_per_minute = 5;

        let err = config
            .validate()
            .expect_err("expected syscall threshold ordering validation failure");
        assert!(err
            .to_string()
            .contains("max_denied_events_per_minute must be less than or equal"));
    }

    #[test]
    async fn security_validation_rejects_invalid_perplexity_threshold() {
        let mut config = Config::default();
        config.security.perplexity_filter.perplexity_threshold = 1.0;

        let err = config
            .validate()
            .expect_err("expected perplexity threshold validation failure");
        assert!(err.to_string().contains("perplexity_threshold"));
    }

    #[test]
    async fn security_validation_rejects_invalid_perplexity_symbol_ratio_threshold() {
        let mut config = Config::default();
        config.security.perplexity_filter.symbol_ratio_threshold = 1.5;

        let err = config
            .validate()
            .expect_err("expected perplexity symbol ratio validation failure");
        assert!(err.to_string().contains("symbol_ratio_threshold"));
    }

    #[test]
    async fn security_validation_rejects_invalid_outbound_leak_guard_sensitivity() {
        let mut config = Config::default();
        config.security.outbound_leak_guard.sensitivity = 1.2;

        let err = config
            .validate()
            .expect_err("expected outbound leak guard sensitivity validation failure");
        assert!(err
            .to_string()
            .contains("security.outbound_leak_guard.sensitivity"));
    }

    #[test]
    async fn coordination_config_defaults() {
        let config = Config::default();
        assert!(config.coordination.enabled);
        assert_eq!(config.coordination.lead_agent, "delegate-lead");
        assert_eq!(config.coordination.max_inbox_messages_per_agent, 256);
        assert_eq!(config.coordination.max_dead_letters, 256);
        assert_eq!(config.coordination.max_context_entries, 512);
        assert_eq!(config.coordination.max_seen_message_ids, 4096);
    }

    #[test]
    async fn config_roundtrip_with_coordination_section() {
        let mut config = Config::default();
        config.coordination.enabled = true;
        config.coordination.lead_agent = "runtime-lead".into();
        config.coordination.max_inbox_messages_per_agent = 128;
        config.coordination.max_dead_letters = 64;
        config.coordination.max_context_entries = 32;
        config.coordination.max_seen_message_ids = 1024;

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert!(parsed.coordination.enabled);
        assert_eq!(parsed.coordination.lead_agent, "runtime-lead");
        assert_eq!(parsed.coordination.max_inbox_messages_per_agent, 128);
        assert_eq!(parsed.coordination.max_dead_letters, 64);
        assert_eq!(parsed.coordination.max_context_entries, 32);
        assert_eq!(parsed.coordination.max_seen_message_ids, 1024);
    }

    #[test]
    async fn coordination_validation_rejects_invalid_limits_and_lead_agent() {
        let mut config = Config::default();
        config.coordination.max_inbox_messages_per_agent = 0;
        let err = config
            .validate()
            .expect_err("expected coordination inbox limit validation failure");
        assert!(err
            .to_string()
            .contains("coordination.max_inbox_messages_per_agent"));

        let mut config = Config::default();
        config.coordination.max_dead_letters = 0;
        let err = config
            .validate()
            .expect_err("expected coordination dead-letter limit validation failure");
        assert!(err.to_string().contains("coordination.max_dead_letters"));

        let mut config = Config::default();
        config.coordination.max_context_entries = 0;
        let err = config
            .validate()
            .expect_err("expected coordination context limit validation failure");
        assert!(err.to_string().contains("coordination.max_context_entries"));

        let mut config = Config::default();
        config.coordination.max_seen_message_ids = 0;
        let err = config
            .validate()
            .expect_err("expected coordination dedupe-window validation failure");
        assert!(err
            .to_string()
            .contains("coordination.max_seen_message_ids"));

        let mut config = Config::default();
        config.coordination.lead_agent = "   ".into();
        let err = config
            .validate()
            .expect_err("expected coordination lead-agent validation failure");
        assert!(err.to_string().contains("coordination.lead_agent"));
    }

    #[test]
    async fn coordination_validation_allows_empty_lead_agent_when_disabled() {
        let mut config = Config::default();
        config.coordination.enabled = false;
        config.coordination.lead_agent = String::new();
        config
            .validate()
            .expect("disabled coordination should allow empty lead agent");
    }

    #[test]
    async fn cost_enforcement_defaults_are_stable() {
        let cost = CostConfig::default();
        assert_eq!(cost.enforcement.mode, CostEnforcementMode::Warn);
        assert_eq!(
            cost.enforcement.route_down_model.as_deref(),
            Some("hint:fast")
        );
        assert_eq!(cost.enforcement.reserve_percent, 10);
    }

    #[test]
    async fn cost_enforcement_config_parses_route_down_mode() {
        let parsed: CostConfig = toml::from_str(
            r#"
enabled = true

[enforcement]
mode = "route_down"
route_down_model = "hint:fast"
reserve_percent = 15
"#,
        )
        .expect("cost enforcement should parse");

        assert!(parsed.enabled);
        assert_eq!(parsed.enforcement.mode, CostEnforcementMode::RouteDown);
        assert_eq!(
            parsed.enforcement.route_down_model.as_deref(),
            Some("hint:fast")
        );
        assert_eq!(parsed.enforcement.reserve_percent, 15);
    }

    #[test]
    async fn validation_rejects_cost_enforcement_reserve_over_100() {
        let mut config = Config::default();
        config.cost.enforcement.reserve_percent = 150;
        let err = config
            .validate()
            .expect_err("expected cost.enforcement.reserve_percent validation failure");
        assert!(err.to_string().contains("cost.enforcement.reserve_percent"));
    }

    #[test]
    async fn validation_rejects_route_down_hint_without_matching_route() {
        let mut config = Config::default();
        config.cost.enforcement.mode = CostEnforcementMode::RouteDown;
        config.cost.enforcement.route_down_model = Some("hint:fast".to_string());
        let err = config
            .validate()
            .expect_err("route_down hint should require a matching model route");
        assert!(err
            .to_string()
            .contains("cost.enforcement.route_down_model uses hint 'fast'"));
    }

    #[test]
    async fn validation_accepts_route_down_hint_with_matching_route() {
        let mut config = Config::default();
        config.cost.enforcement.mode = CostEnforcementMode::RouteDown;
        config.cost.enforcement.route_down_model = Some("hint:fast".to_string());
        config.model_routes = vec![ModelRouteConfig {
            hint: "fast".to_string(),
            provider: "openrouter".to_string(),
            model: "openai/gpt-4.1-mini".to_string(),
            api_key: None,
            max_tokens: None,
            transport: None,
        }];

        config
            .validate()
            .expect("matching route_down hint route should validate");
    }
}
