use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::default_true;

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

// ── Microsoft 365 (Graph API integration) ───────────────────────

/// Microsoft 365 integration via Microsoft Graph API (`[microsoft365]` section).
///
/// Provides access to Outlook mail, Teams messages, Calendar events,
/// OneDrive files, and SharePoint search.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
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
    pub computer_use: BrowserComputerUseConfig,
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
    pub firecrawl: FirecrawlConfig,
}

/// Firecrawl fallback mode: scrape a single page or crawl linked pages.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebSearchConfig {
    /// Enable `web_search_tool` for web searches
    #[serde(default)]
    pub enabled: bool,
    /// Search provider: "duckduckgo" (free), "brave" (requires API key), or "searxng" (self-hosted)
    #[serde(default = "default_web_search_provider")]
    pub provider: String,
    /// Brave Search API key (required if provider is "brave")
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

// ── Standalone Image Generation ─────────────────────────────────

/// Standalone image generation tool configuration (`[image_gen]`).
///
/// When enabled, registers an `image_gen` tool that generates images via
/// fal.ai's synchronous API (Flux / Nano Banana models) and saves them
/// to the workspace `images/` directory.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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
