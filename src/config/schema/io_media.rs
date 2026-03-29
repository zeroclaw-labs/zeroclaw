use anyhow::{Context, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    pub openai: Option<OpenAiSttConfig>,
    /// Deepgram STT provider configuration.
    #[serde(default)]
    pub deepgram: Option<DeepgramSttConfig>,
    /// AssemblyAI STT provider configuration.
    #[serde(default)]
    pub assemblyai: Option<AssemblyAiSttConfig>,
    /// Google Cloud Speech-to-Text provider configuration.
    #[serde(default)]
    pub google: Option<GoogleSttConfig>,
    /// Local/self-hosted Whisper-compatible STT provider.
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

pub(super) const MCP_MAX_TOOL_TIMEOUT_SECS: u64 = 600;

pub(super) fn validate_mcp_config(config: &McpConfig) -> Result<()> {
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

/// Verifiable Intent (VI) credential verification and issuance (`[verifiable_intent]` section).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    pub openai: Option<OpenAiTtsConfig>,
    /// ElevenLabs TTS provider configuration (`[tts.elevenlabs]`).
    #[serde(default)]
    pub elevenlabs: Option<ElevenLabsTtsConfig>,
    /// Google Cloud TTS provider configuration (`[tts.google]`).
    #[serde(default)]
    pub google: Option<GoogleTtsConfig>,
    /// Edge TTS provider configuration (`[tts.edge]`).
    #[serde(default)]
    pub edge: Option<EdgeTtsConfig>,
    /// Piper TTS provider configuration (`[tts.piper]`).
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenAiTtsConfig {
    /// API key for OpenAI TTS.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Model name (default `"tts-1"`).
    #[serde(default = "default_openai_tts_model")]
    pub model: String,
    /// Playback speed multiplier (default `1.0`).
    #[serde(default = "default_openai_tts_speed")]
    pub speed: f64,
}

/// ElevenLabs TTS provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ElevenLabsTtsConfig {
    /// API key for ElevenLabs.
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GoogleTtsConfig {
    /// API key for Google Cloud TTS.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Language code (default `"en-US"`).
    #[serde(default = "default_google_tts_language_code")]
    pub language_code: String,
}

/// Edge TTS provider configuration (free, subprocess-based).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EdgeTtsConfig {
    /// Path to the `edge-tts` binary (default `"edge-tts"`).
    #[serde(default = "default_edge_tts_binary_path")]
    pub binary_path: String,
}

/// Piper TTS provider configuration (local GPU-accelerated, OpenAI-compatible endpoint).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PiperTtsConfig {
    /// Base URL for the Piper TTS HTTP server (e.g. `"http://127.0.0.1:5000/v1/audio/speech"`).
    #[serde(default = "default_piper_tts_api_url")]
    pub api_url: String,
}

/// Determines when a `ToolFilterGroup` is active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OpenAiSttConfig {
    /// OpenAI API key for Whisper transcription.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whisper model name (default: "whisper-1").
    #[serde(default = "default_openai_stt_model")]
    pub model: String,
}

/// Deepgram STT provider configuration (`[transcription.deepgram]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeepgramSttConfig {
    /// Deepgram API key.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Deepgram model name (default: "nova-2").
    #[serde(default = "default_deepgram_stt_model")]
    pub model: String,
}

/// AssemblyAI STT provider configuration (`[transcription.assemblyai]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssemblyAiSttConfig {
    /// AssemblyAI API key.
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Google Cloud Speech-to-Text provider configuration (`[transcription.google]`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GoogleSttConfig {
    /// Google Cloud API key.
    #[serde(default)]
    pub api_key: Option<String>,
    /// BCP-47 language code (default: "en-US").
    #[serde(default = "default_google_stt_language_code")]
    pub language_code: String,
}

/// Local/self-hosted Whisper-compatible STT endpoint (`[transcription.local_whisper]`).
///
/// Configures a self-hosted STT endpoint. Can be on localhost, a private network host, or any reachable URL.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LocalWhisperConfig {
    /// HTTP or HTTPS endpoint URL, e.g. `"http://10.10.0.1:8001/v1/transcribe"`.
    pub url: String,
    /// Bearer token for endpoint authentication.
    /// Omit for unauthenticated local endpoints.
    #[serde(default)]
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
