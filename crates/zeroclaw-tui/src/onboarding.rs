use anyhow::{Context, Result};
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use std::io::{self, IsTerminal};

use zeroclaw_config::schema::Config;
use zeroclaw_config::schema::{
    DiscordConfig, FeishuConfig, IMessageConfig, IrcConfig, LarkConfig, LarkReceiveMode,
    MatrixConfig, MattermostConfig, NextcloudTalkConfig, SignalConfig, SlackConfig, StreamMode,
    TelegramConfig, WhatsAppChatPolicy, WhatsAppConfig, WhatsAppWebMode,
};

use super::theme;
use super::widgets::{
    Banner, ConfirmedLine, InfoPanel, InputPrompt, SelectableItem, SelectableList, StepIndicator,
    StepStatus,
};

// ── Version info ────────────────────────────────────────────────────

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Docs base URL ───────────────────────────────────────────────────

const DOCS_BASE: &str = "https://www.zeroclawlabs.ai/docs";

// ── Screens ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Welcome,
    SecurityWarning,
    SetupMode,
    ExistingConfig,
    ConfigHandling,
    QuickStartSummary,
    ProviderTier,
    ProviderSelect,
    ApiKeyInput,
    ProviderNotes,
    ModelConfigured,
    ModelSelect,
    ChannelStatus,
    HowChannelsWork,
    ChannelSelect,
    WebSearchInfo,
    WebSearchProvider,
    WebSearchApiKey,
    SkillsStatus,
    SkillsInstall,
    HooksInfo,
    HooksEnable,
    GatewayService,
    HealthCheck,
    OptionalApps,
    ControlUI,
    WorkspaceBackup,
    FinalSecurity,
    WebSearchConfirm,
    WhatNow,
    Complete,
}

// ── Provider/Channel/Search data ────────────────────────────────────

const PROVIDER_TIERS: &[(&str, &str)] = &[
    (
        "\u{2b50} Recommended",
        "OpenRouter, Venice, Anthropic, OpenAI, Gemini",
    ),
    (
        "\u{26a1} Fast inference",
        "Groq, Fireworks, Together AI, NVIDIA NIM",
    ),
    (
        "\u{1f310} Gateway / proxy",
        "Vercel AI, Cloudflare AI, Amazon Bedrock",
    ),
    (
        "\u{1f52c} Specialized",
        "Moonshot/Kimi, GLM/Zhipu, MiniMax, Qwen, Z.AI",
    ),
    (
        "\u{1f3e0} Local / private",
        "Ollama, llama.cpp, vLLM — no API key",
    ),
    ("\u{1f527} Custom", "Bring your own OpenAI-compatible API"),
];

/// (display_name, description, config_id)
const TIER_PROVIDERS: &[&[(&str, &str, &str)]] = &[
    // Tier 0: Recommended
    &[
        (
            "OpenRouter",
            "200+ models, 1 API key (recommended)",
            "openrouter",
        ),
        ("Venice AI", "Privacy-first (Llama, Opus)", "venice"),
        ("Anthropic", "Claude Sonnet & Opus (direct)", "anthropic"),
        ("OpenAI", "GPT-4o, o1, GPT-5 (direct)", "openai"),
        (
            "OpenAI Codex",
            "ChatGPT subscription OAuth, no API key",
            "openai-codex",
        ),
        ("DeepSeek", "V3 & R1 (affordable)", "deepseek"),
        ("Mistral", "Large & Codestral", "mistral"),
        ("xAI", "Grok 3 & 4", "xai"),
        ("Perplexity", "Search-augmented AI", "perplexity"),
        (
            "Google Gemini",
            "Gemini 2.0 Flash & Pro (supports CLI auth)",
            "gemini",
        ),
    ],
    // Tier 1: Fast inference
    &[
        ("Groq", "Ultra-fast LPU inference", "groq"),
        ("Fireworks AI", "Fast open-source inference", "fireworks"),
        ("Novita AI", "Affordable open-source inference", "novita"),
        ("Together AI", "Open-source model hosting", "together-ai"),
        ("NVIDIA NIM", "DeepSeek, Llama, & more", "nvidia"),
    ],
    // Tier 2: Gateway / proxy
    &[
        ("Vercel AI Gateway", "", "vercel"),
        ("Cloudflare AI Gateway", "", "cloudflare"),
        ("Astrai", "Compliant AI routing, PII stripping", "astrai"),
        (
            "Avian",
            "OpenAI-compatible (DeepSeek, Kimi, GLM, MiniMax)",
            "avian",
        ),
        ("Amazon Bedrock", "AWS managed models", "bedrock"),
    ],
    // Tier 3: Specialized
    &[
        ("Kimi Code", "Coding-optimized Kimi API", "kimi-code"),
        (
            "Qwen Code",
            "OAuth tokens from ~/.qwen/oauth_creds.json",
            "qwen-code",
        ),
        ("Moonshot", "Kimi API (China endpoint)", "moonshot"),
        (
            "Moonshot Intl",
            "Kimi API (international endpoint)",
            "moonshot-intl",
        ),
        ("GLM", "ChatGLM / Zhipu (international)", "glm"),
        ("GLM CN", "ChatGLM / Zhipu (China)", "glm-cn"),
        ("MiniMax", "International endpoint", "minimax"),
        ("MiniMax CN", "China endpoint", "minimax-cn"),
        ("Qwen", "DashScope China endpoint", "qwen"),
        ("Qwen Intl", "DashScope international endpoint", "qwen-intl"),
        ("Qwen US", "DashScope US endpoint", "qwen-us"),
        ("Qianfan", "Baidu AI models (China)", "qianfan"),
        ("Z.AI", "Global coding endpoint", "zai"),
        ("Z.AI CN", "China coding endpoint", "zai-cn"),
        ("Synthetic", "Synthetic AI models", "synthetic"),
        ("OpenCode Zen", "Code-focused AI", "opencode"),
        ("OpenCode Go", "Subsidized code-focused AI", "opencode-go"),
        ("Cohere", "Command R+ & embeddings", "cohere"),
    ],
    // Tier 4: Local / private
    &[
        ("Ollama", "Local models (Llama, Mistral, Phi)", "ollama"),
        ("llama.cpp", "Local OpenAI-compatible endpoint", "llamacpp"),
        ("SGLang", "High-performance local serving", "sglang"),
        ("vLLM", "High-performance local inference", "vllm"),
        (
            "Osaurus",
            "Unified AI edge runtime (MLX + cloud + MCP)",
            "osaurus",
        ),
    ],
    // Tier 5: Custom
    &[(
        "Custom OpenAI-compatible",
        "Any OpenAI-compatible endpoint",
        "custom",
    )],
];

const CHANNELS: &[(&str, &str, bool)] = &[
    ("Telegram", "Bot API", false),
    ("WhatsApp", "QR link", true),
    ("Discord", "Bot API", false),
    ("IRC", "Server + Nick", false),
    ("Google Chat", "Chat API", true),
    ("Slack", "Socket Mode", false),
    ("Signal", "signal-cli", false),
    ("iMessage", "imsg", false),
    ("LINE", "Messaging API", false),
    ("Mattermost", "plugin", false),
    ("Nextcloud Talk", "self-hosted", false),
    ("Feishu/Lark", "\u{98de}\u{4e66}", false),
    ("BlueBubbles", "macOS app", false),
    ("Zalo", "Bot API", false),
    ("Synology Chat", "Webhook", false),
    ("Nostr", "NIP-04 DMs", true),
    ("Microsoft Teams", "Teams SDK", true),
    ("Matrix", "plugin", true),
    ("Zalo Personal", "Personal Account", true),
    ("Tlon", "Urbit", true),
    ("Twitch", "Chat", true),
    ("Skip for now", "configure later", false),
];

const SETUP_MODES: &[&str] = &["QuickStart", "Full Setup (9 steps)", "Skip for now"];

const MODELS: &[&str] = &[
    "Auto (recommended)",
    "claude-sonnet-4-20250514",
    "claude-opus-4-20250514",
    "gpt-4o",
    "gemini-2.0-flash",
    "glm-5",
    "Custom model ID...",
];

const SEARCH_PROVIDERS: &[(&str, &str)] = &[
    ("Brave Search", "API key required"),
    ("SearxNG", "Self-hosted, key-free"),
    ("Tavily", "API key required"),
    ("Google Custom Search", "API key required"),
    ("DuckDuckGo", "Key-free (limited)"),
    ("Skip for now", "configure later"),
];

const SKILLS: &[(&str, &str)] = &[
    ("Skip for now", ""),
    ("\u{1f510} 1password", "Password manager"),
    ("\u{1f43b} bear-notes", "Note taking"),
    ("\u{1f4f0} blogwatcher", "RSS feeds"),
    ("\u{1fab0} blucli", "Bluetooth CLI"),
    ("\u{1f4f8} camsnap", "Camera capture"),
    ("\u{1f9e9} clawhub", "Plugin registry"),
    ("\u{1f6cc} eightctl", "Sleep tracking"),
    ("\u{1f9f2} gifgrep", "GIF search"),
    ("\u{1f3ae} gog", "Game library"),
    ("\u{1f4cd} goplaces", "Google Places"),
    ("\u{1f4e7} himalaya", "Email CLI"),
    ("\u{1f4e6} mcporter", "MCP tools"),
    ("\u{1f4ca} model-usage", "LLM usage stats"),
    ("\u{1f4c4} nano-pdf", "PDF tools"),
    ("\u{1f48e} obsidian", "Knowledge base"),
    ("\u{1f3a4} openai-whisper", "Speech-to-text"),
    ("\u{1f4a1} openhue", "Smart lights"),
    ("\u{1f9ff} oracle", "Divination"),
    ("\u{1f6f5} ordercli", "Order tracking"),
    ("\u{1f440} peekaboo", "Screen peek"),
    ("\u{1f50a} sag", "Audio gen"),
    ("\u{1f30a} songsee", "Music ID"),
    ("\u{1f50a} sonoscli", "Sonos control"),
    ("\u{1f9fe} summarize", "Text summary"),
    ("\u{2705} things-mac", "Task manager"),
    ("\u{1f4f1} wacli", "WhatsApp CLI"),
    ("\u{1f426} xurl", "URL tools"),
];

// ── App state ───────────────────────────────────────────────────────

struct App {
    screen: Screen,
    should_quit: bool,

    // Security
    security_accepted: bool,

    // Setup mode
    setup_mode_idx: usize,

    // Config handling
    config_handling_idx: usize,

    // Provider
    provider_tier_idx: usize,
    provider_idx: usize,
    provider_scroll: usize,

    // API key
    api_key_input: String,

    // Model
    model_idx: usize,

    // Channel
    channel_idx: usize,
    channel_scroll: usize,

    // Web search
    search_provider_idx: usize,
    search_api_key_input: String,

    // Skills
    skills_idx: usize,
    skills_scroll: usize,

    // Hooks
    hooks_idx: usize,

    // Gateway
    gateway_port: u16,
    gateway_host: String,
    pairing_code: String,
    pairing_required: bool,
}

impl App {
    fn new() -> Self {
        // Resolve gateway port: env vars → default
        let port = std::env::var("ZEROCLAW_GATEWAY_PORT")
            .or_else(|_| std::env::var("PORT"))
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(42617);

        // Resolve gateway host: env var → default
        let host =
            std::env::var("ZEROCLAW_GATEWAY_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

        Self {
            screen: Screen::Welcome,
            should_quit: false,
            security_accepted: false,
            setup_mode_idx: 0,
            config_handling_idx: 0,
            provider_tier_idx: 0,
            provider_idx: 0,
            provider_scroll: 0,
            api_key_input: String::new(),
            model_idx: 0,
            channel_idx: 0,
            channel_scroll: 0,
            search_provider_idx: 0,
            search_api_key_input: String::new(),
            skills_idx: 0,
            skills_scroll: 0,
            hooks_idx: 0,
            gateway_port: port,
            gateway_host: host,
            pairing_code: String::from("......"),
            pairing_required: true,
        }
    }

    fn gateway_base_url(&self) -> String {
        format!("http://{}:{}", self.gateway_host, self.gateway_port)
    }

    /// Fetch or generate a real pairing code from the running gateway.
    /// Works across all deployment methods: cargo, brew, docker, macOS app.
    async fn fetch_pairing_code(&mut self) {
        let client = reqwest::Client::new();
        let timeout = std::time::Duration::from_secs(3);

        // 1. Try localhost admin endpoint (works for cargo/brew/local installs)
        let admin_url = format!("http://127.0.0.1:{}/admin/paircode", self.gateway_port);
        if let Some((code, required)) = Self::try_fetch_code(&client, &admin_url, timeout).await {
            self.pairing_code = code;
            self.pairing_required = required;
            return;
        }

        // 2. Try public endpoint (works during initial setup before first pair)
        let public_url = format!("http://127.0.0.1:{}/pair/code", self.gateway_port);
        if let Some((code, required)) = Self::try_fetch_code(&client, &public_url, timeout).await {
            self.pairing_code = code;
            self.pairing_required = required;
            return;
        }

        // 3. Try configured host (docker/remote where host != 127.0.0.1)
        if self.gateway_host != "127.0.0.1" {
            let remote_url = format!(
                "http://{}:{}/pair/code",
                self.gateway_host, self.gateway_port
            );
            if let Some((code, required)) =
                Self::try_fetch_code(&client, &remote_url, timeout).await
            {
                self.pairing_code = code;
                self.pairing_required = required;
                return;
            }
        }

        // 4. Try generating a new code via CLI subprocess.
        //    This works for Docker (`docker exec`), local installs, brew, etc.
        //    The CLI command talks to the gateway internally and bypasses the
        //    localhost restriction that blocks HTTP admin endpoints via port-forward.
        if let Some(code) = Self::generate_code_via_cli().await {
            self.pairing_code = code;
            self.pairing_required = true;
            return;
        }

        // 5. Try generating via docker exec if gateway runs in a container
        if let Some(code) = Self::generate_code_via_docker().await {
            self.pairing_code = code;
            self.pairing_required = true;
            return;
        }

        // 6. Try admin POST endpoint (works for truly local gateways)
        let new_url = format!("http://127.0.0.1:{}/admin/paircode/new", self.gateway_port);
        if let Ok(resp) = client.post(&new_url).timeout(timeout).send().await
            && let Ok(json) = resp.json::<serde_json::Value>().await
            && let Some(code) = json.get("pairing_code").and_then(|v| v.as_str())
        {
            self.pairing_code = code.to_string();
            return;
        }

        // 7. Gateway not reachable — show instructions instead of a fake code
        self.pairing_code = String::from("------");
        self.pairing_required = true;
    }

    /// Run `zeroclaw gateway get-paircode --new` locally to generate a code.
    async fn generate_code_via_cli() -> Option<String> {
        let output = tokio::process::Command::new("zeroclaw")
            .args(["gateway", "get-paircode", "--new"])
            .output()
            .await
            .ok()?;
        Self::extract_code_from_output(&output.stdout)
    }

    /// Run `docker exec <container> zeroclaw gateway get-paircode --new`.
    async fn generate_code_via_docker() -> Option<String> {
        // Find zeroclaw container
        let ps = tokio::process::Command::new("docker")
            .args([
                "ps",
                "--filter",
                "ancestor=ghcr.io/zeroclaw-labs/zeroclaw",
                "--format",
                "{{.Names}}",
            ])
            .output()
            .await
            .ok()?;
        let container = String::from_utf8_lossy(&ps.stdout)
            .lines()
            .next()?
            .trim()
            .to_string();
        if container.is_empty() {
            // Also try by container name
            let ps2 = tokio::process::Command::new("docker")
                .args(["ps", "--filter", "name=zeroclaw", "--format", "{{.Names}}"])
                .output()
                .await
                .ok()?;
            let container = String::from_utf8_lossy(&ps2.stdout)
                .lines()
                .next()?
                .trim()
                .to_string();
            if container.is_empty() {
                return None;
            }
            let output = tokio::process::Command::new("docker")
                .args([
                    "exec",
                    &container,
                    "zeroclaw",
                    "gateway",
                    "get-paircode",
                    "--new",
                ])
                .output()
                .await
                .ok()?;
            return Self::extract_code_from_output(&output.stdout);
        }
        let output = tokio::process::Command::new("docker")
            .args([
                "exec",
                &container,
                "zeroclaw",
                "gateway",
                "get-paircode",
                "--new",
            ])
            .output()
            .await
            .ok()?;
        Self::extract_code_from_output(&output.stdout)
    }

    /// Parse a 6-digit pairing code from CLI output.
    fn extract_code_from_output(stdout: &[u8]) -> Option<String> {
        let text = String::from_utf8_lossy(stdout);
        // Look for the code in the box: │  294382  │
        for line in text.lines() {
            let trimmed = line.trim().trim_matches('│').trim();
            if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
                return Some(trimmed.to_string());
            }
        }
        None
    }

    async fn try_fetch_code(
        client: &reqwest::Client,
        url: &str,
        timeout: std::time::Duration,
    ) -> Option<(String, bool)> {
        let resp = client.get(url).timeout(timeout).send().await.ok()?;
        let json: serde_json::Value = resp.json().await.ok()?;
        let required = json
            .get("pairing_required")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let code = json.get("pairing_code").and_then(|v| v.as_str())?;
        Some((code.to_string(), required))
    }

    fn selected_provider(&self) -> &str {
        TIER_PROVIDERS
            .get(self.provider_tier_idx)
            .and_then(|tier| tier.get(self.provider_idx))
            .map_or("Unknown", |p| p.0)
    }

    fn selected_provider_id(&self) -> &str {
        TIER_PROVIDERS
            .get(self.provider_tier_idx)
            .and_then(|tier| tier.get(self.provider_idx))
            .map_or("openrouter", |p| p.2)
    }

    fn current_tier_providers(&self) -> &[(&str, &str, &str)] {
        TIER_PROVIDERS
            .get(self.provider_tier_idx)
            .map_or(&[], |t| *t)
    }

    fn selected_model(&self) -> &str {
        MODELS.get(self.model_idx).map_or("auto", |m| m)
    }

    fn selected_channel(&self) -> &str {
        CHANNELS.get(self.channel_idx).map_or("Skip", |c| c.0)
    }

    fn selected_search_provider(&self) -> &str {
        SEARCH_PROVIDERS
            .get(self.search_provider_idx)
            .map_or("None", |p| p.0)
    }
}

fn provider_supports_keyless_local_usage(provider_id: &str) -> bool {
    matches!(
        provider_id,
        "ollama" | "llamacpp" | "sglang" | "vllm" | "osaurus"
    )
}

fn provider_uses_oauth_without_api_key(provider_id: &str) -> bool {
    matches!(provider_id, "openai-codex")
}

fn provider_skips_api_key_input(provider_id: &str) -> bool {
    provider_supports_keyless_local_usage(provider_id)
        || provider_uses_oauth_without_api_key(provider_id)
}

// ── Public entry point ──────────────────────────────────────────────

pub async fn run_tui_onboarding() -> Result<()> {
    // When launched via `curl | bash`, stdin is a pipe, not a TTY.
    // Crossterm reads terminal events from stdin, so we must reopen
    // stdin from /dev/tty before entering raw mode.
    #[cfg(unix)]
    if !io::stdin().is_terminal() {
        use std::fs::File;
        let tty = File::open("/dev/tty").context("Failed to open /dev/tty for TUI input")?;
        let fd = std::os::unix::io::IntoRawFd::into_raw_fd(tty);
        // Safety: we just opened this fd and are replacing stdin (fd 0) with it.
        unsafe {
            if libc::dup2(fd, 0) == -1 {
                libc::close(fd);
                anyhow::bail!("Failed to redirect stdin from /dev/tty");
            }
            libc::close(fd);
        }
    }

    enable_raw_mode().context("Failed to enable raw mode")?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("Failed to enter alternate screen")?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    let mut app = App::new();
    app.fetch_pairing_code().await;
    let result = run_app(&mut terminal, &mut app);

    disable_raw_mode().context("Failed to disable raw mode")?;
    io::stdout()
        .execute(LeaveAlternateScreen)
        .context("Failed to leave alternate screen")?;

    result?;

    if app.screen == Screen::Complete {
        // ── Persist configuration ──
        #[allow(clippy::large_futures)]
        match save_tui_config(&app).await {
            Ok(()) => {
                let skill = SKILLS
                    .get(app.skills_idx)
                    .map(|(name, _)| *name)
                    .unwrap_or("Skip for now");
                let hooks_label = if app.hooks_idx == 0 {
                    "enabled"
                } else {
                    "disabled"
                };

                println!();
                println!("  \u{1f980} ZeroClaw {VERSION} configured successfully!");
                println!(
                    "     Provider:   {} ({})",
                    app.selected_provider(),
                    app.selected_provider_id()
                );
                println!("     Model:      {}", app.selected_model());
                println!("     Channel:    {}", app.selected_channel());
                println!("     Web search: {}", app.selected_search_provider());
                println!("     Skills:     {skill}");
                println!("     Hooks:      {hooks_label}");
                println!("     Gateway:    {}:{}", app.gateway_host, app.gateway_port);
                println!(
                    "     Pairing:    {}",
                    if app.pairing_required {
                        "required"
                    } else {
                        "disabled"
                    }
                );
                println!("     Dashboard:  {}", app.gateway_base_url());
                if app.pairing_required && app.pairing_code != "------" {
                    println!("     Pair code:  {}", app.pairing_code);
                }
                println!();
                let channel = app.selected_channel();
                if channel != "Skip for now" {
                    println!("  Next: edit config.toml to add your {channel} credentials.");
                    println!("        zeroclaw config edit");
                    println!();
                }
                println!("  Run `zeroclaw daemon` to start your agent.");
                println!();
            }
            Err(e) => {
                eprintln!();
                eprintln!("  \u{2717} Failed to save configuration: {e}");
                eprintln!("  You can re-run: zeroclaw onboard --tui");
                eprintln!();
            }
        }
    }

    Ok(())
}

// ── Config persistence ──────────────────────────────────────────────

/// Save the TUI selections to the real config.toml.
///
/// This persists every field the wizard collects so the config is complete
/// across CLI, dashboard, macOS app, and Docker deployments.
#[allow(clippy::large_futures)]
async fn save_tui_config(app: &App) -> Result<()> {
    let mut config = Config::load_or_init().await?;
    apply_tui_selections_to_config(app, &mut config);
    config.save().await?;

    // Also push config to Docker container if running
    push_config_to_docker(app).await;

    Ok(())
}

/// Apply all TUI wizard selections to a Config struct (pure logic, no I/O).
///
/// Separated from `save_tui_config` so it can be tested without touching
/// the filesystem or network.
fn apply_tui_selections_to_config(app: &App, config: &mut Config) {
    // ── Provider ────────────────────────────────────────────────────
    let provider_id = app.selected_provider_id();
    config.providers.fallback = Some(provider_id.to_string());

    let entry = config
        .providers
        .models
        .entry(provider_id.to_string())
        .or_default();

    // Clear stale custom provider URL if switching away from custom
    if !provider_id.starts_with("custom") {
        entry.base_url = None;
    }

    // API key (if entered)
    if !app.api_key_input.is_empty() {
        entry.api_key = Some(app.api_key_input.clone());
    }

    // ── Model ───────────────────────────────────────────────────────
    let model = app.selected_model();
    if model == "Auto (recommended)" {
        entry.model = None; // Let provider pick default
    } else {
        entry.model = Some(model.to_string());
    }

    // Provider fields are now resolved directly from providers — no cache needed.

    // ── Channel ─────────────────────────────────────────────────────
    // Create a stub config for the selected channel with placeholder
    // values so the section appears in config.toml. The user fills in
    // real tokens via `zeroclaw config edit` or the dashboard.
    let channel = app.selected_channel();
    match channel {
        "Telegram" => {
            if config.channels.telegram.is_none() {
                config.channels.telegram = Some(TelegramConfig {
                    enabled: true,
                    bot_token: String::from("YOUR_TELEGRAM_BOT_TOKEN"),
                    allowed_users: vec![],
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: 1000,
                    interrupt_on_new_message: false,
                    mention_only: false,
                    ack_reactions: None,
                    proxy_url: None,
                    approval_timeout_secs: 120,
                });
            }
        }
        "Discord" => {
            if config.channels.discord.is_none() {
                config.channels.discord = Some(DiscordConfig {
                    enabled: true,
                    bot_token: String::from("YOUR_DISCORD_BOT_TOKEN"),
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
                });
            }
        }
        "Slack" => {
            if config.channels.slack.is_none() {
                config.channels.slack = Some(SlackConfig {
                    enabled: true,
                    bot_token: String::from("xoxb-YOUR_SLACK_BOT_TOKEN"),
                    app_token: Some(String::from("xapp-YOUR_SLACK_APP_TOKEN")),
                    channel_ids: vec![],
                    allowed_users: vec![],
                    interrupt_on_new_message: false,
                    thread_replies: None,
                    mention_only: false,
                    use_markdown_blocks: false,
                    proxy_url: None,
                    stream_drafts: false,
                    draft_update_interval_ms: 1200,
                    cancel_reaction: None,
                });
            }
        }
        "WhatsApp" => {
            if config.channels.whatsapp.is_none() {
                config.channels.whatsapp = Some(WhatsAppConfig {
                    enabled: true,
                    access_token: Some(String::from("YOUR_WHATSAPP_ACCESS_TOKEN")),
                    phone_number_id: Some(String::from("YOUR_PHONE_NUMBER_ID")),
                    verify_token: Some(String::from("YOUR_VERIFY_TOKEN")),
                    app_secret: None,
                    session_path: None,
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
                });
            }
        }
        "Signal" => {
            if config.channels.signal.is_none() {
                config.channels.signal = Some(SignalConfig {
                    enabled: true,
                    http_url: String::from("http://127.0.0.1:8080"),
                    account: String::from("YOUR_SIGNAL_PHONE_NUMBER"),
                    group_id: None,
                    allowed_from: vec![],
                    ignore_attachments: false,
                    ignore_stories: true,
                    proxy_url: None,
                });
            }
        }
        "IRC" => {
            if config.channels.irc.is_none() {
                config.channels.irc = Some(IrcConfig {
                    enabled: true,
                    server: String::from("irc.libera.chat"),
                    port: 6697,
                    nickname: String::from("zeroclaw-bot"),
                    username: None,
                    channels: vec![String::from("#your-channel")],
                    allowed_users: vec![],
                    server_password: None,
                    nickserv_password: None,
                    sasl_password: None,
                    verify_tls: None,
                });
            }
        }
        "iMessage" => {
            if config.channels.imessage.is_none() {
                config.channels.imessage = Some(IMessageConfig {
                    enabled: true,
                    allowed_contacts: vec![],
                });
            }
        }
        "Matrix" => {
            let existing_mx = config.channels.matrix.as_ref();
            if existing_mx.is_none() {
                config.channels.matrix = Some(MatrixConfig {
                    enabled: true,
                    homeserver: String::from("https://matrix.org"),
                    access_token: String::from("YOUR_MATRIX_ACCESS_TOKEN"),
                    user_id: None,
                    device_id: None,
                    allowed_users: vec![],
                    allowed_rooms: vec![String::from("!YOUR_ROOM_ID:matrix.org")],
                    interrupt_on_new_message: false,
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: 500,
                    multi_message_delay_ms: 800,
                    mention_only: existing_mx.map(|m| m.mention_only).unwrap_or(false),
                    recovery_key: existing_mx.and_then(|m| m.recovery_key.clone()),
                    password: existing_mx.and_then(|m| m.password.clone()),
                });
            }
        }
        "Mattermost" => {
            if config.channels.mattermost.is_none() {
                config.channels.mattermost = Some(MattermostConfig {
                    enabled: true,
                    url: String::from("https://mattermost.example.com"),
                    bot_token: String::from("YOUR_MATTERMOST_BOT_TOKEN"),
                    channel_id: None,
                    allowed_users: vec![],
                    thread_replies: None,
                    mention_only: None,
                    interrupt_on_new_message: false,
                    proxy_url: None,
                });
            }
        }
        "Nextcloud Talk" => {
            if config.channels.nextcloud_talk.is_none() {
                config.channels.nextcloud_talk = Some(NextcloudTalkConfig {
                    enabled: true,
                    base_url: String::from("https://cloud.example.com"),
                    app_token: String::from("YOUR_NEXTCLOUD_APP_TOKEN"),
                    webhook_secret: None,
                    allowed_users: vec![],
                    proxy_url: None,
                    bot_name: None,
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: 1000,
                });
            }
        }
        "Feishu/Lark" => {
            if config.channels.feishu.is_none() {
                config.channels.feishu = Some(FeishuConfig {
                    enabled: true,
                    app_id: String::from("YOUR_FEISHU_APP_ID"),
                    app_secret: String::from("YOUR_FEISHU_APP_SECRET"),
                    encrypt_key: None,
                    verification_token: None,
                    allowed_users: vec![],
                    mention_only: false,
                    receive_mode: LarkReceiveMode::default(),
                    port: None,
                    proxy_url: None,
                });
            }
            if config.channels.lark.is_none() {
                config.channels.lark = Some(LarkConfig {
                    enabled: true,
                    app_id: String::from("YOUR_LARK_APP_ID"),
                    app_secret: String::from("YOUR_LARK_APP_SECRET"),
                    encrypt_key: None,
                    verification_token: None,
                    allowed_users: vec![],
                    mention_only: false,
                    use_feishu: false,
                    receive_mode: LarkReceiveMode::default(),
                    port: None,
                    proxy_url: None,
                });
            }
        }
        // Channels without config structs yet — skip silently
        _ => {}
    }

    // ── Web search ──────────────────────────────────────────────────
    let search = app.selected_search_provider();
    if search != "Skip for now" && search != "None" {
        let search_id = match search {
            "Brave Search" => "brave",
            "SearxNG" => "searxng",
            "Tavily" => "tavily",
            "Google Custom Search" => "google",
            _ => "duckduckgo",
        };
        config.web_search.enabled = true;
        config.web_search.provider = search_id.to_string();

        if !app.search_api_key_input.is_empty() {
            match search_id {
                "brave" => {
                    config.web_search.brave_api_key = Some(app.search_api_key_input.clone());
                }
                "searxng" => {
                    // For SearXNG the "API key" input is actually the instance URL
                    config.web_search.searxng_instance_url = Some(app.search_api_key_input.clone());
                }
                _ => {}
            }
        }
    }

    // ── Skills ──────────────────────────────────────────────────────
    let skill = SKILLS
        .get(app.skills_idx)
        .map(|(name, _)| *name)
        .unwrap_or("Skip for now");
    if skill != "Skip for now" {
        config.skills.open_skills_enabled = true;
    }

    // ── Hooks ───────────────────────────────────────────────────────
    // hooks_idx: 0 = "Enable hooks", 1 = "Skip for now"
    config.hooks.enabled = app.hooks_idx == 0;
    if app.hooks_idx == 0 {
        config.hooks.builtin.command_logger = true;
    }

    // ── Gateway ─────────────────────────────────────────────────────
    config.gateway.port = app.gateway_port;
    config.gateway.host = app.gateway_host.clone();

    // ── Pairing / security ──────────────────────────────────────────
    config.gateway.require_pairing = app.pairing_required;
}

/// If a ZeroClaw Docker container is running, reconfigure it via `docker exec`.
async fn push_config_to_docker(app: &App) {
    // Find zeroclaw container
    let container = find_docker_container().await;
    let container = match container {
        Some(c) => c,
        None => return,
    };

    let provider_id = app.selected_provider_id();

    // Use `zeroclaw onboard --quick` inside the container to reconfigure
    let mut args = vec![
        "exec".to_string(),
        container,
        "zeroclaw".to_string(),
        "onboard".to_string(),
        "--quick".to_string(),
        "--provider".to_string(),
        provider_id.to_string(),
    ];

    if !app.api_key_input.is_empty() {
        args.push("--api-key".to_string());
        args.push(app.api_key_input.clone());
    }

    let model = app.selected_model();
    if model != "Auto (recommended)" {
        args.push("--model".to_string());
        args.push(model.to_string());
    }

    let _ = tokio::process::Command::new("docker")
        .args(&args)
        .output()
        .await;
}

async fn find_docker_container() -> Option<String> {
    // Try by image name
    let ps = tokio::process::Command::new("docker")
        .args([
            "ps",
            "--filter",
            "ancestor=ghcr.io/zeroclaw-labs/zeroclaw",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .await
        .ok()?;
    let name = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if !name.is_empty() {
        return Some(name);
    }
    // Try by container name
    let ps2 = tokio::process::Command::new("docker")
        .args(["ps", "--filter", "name=zeroclaw", "--format", "{{.Names}}"])
        .output()
        .await
        .ok()?;
    let name = String::from_utf8_lossy(&ps2.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty() { None } else { Some(name) }
}

// ── Main loop ───────────────────────────────────────────────────────

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if app.should_quit {
            break;
        }

        if let Event::Key(key) = event::read().context("Failed to read event")? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                app.should_quit = true;
                continue;
            }

            handle_input(app, key.code);
        }
    }
    Ok(())
}

// ── Generic list navigation helper ──────────────────────────────────

fn nav_up(idx: &mut usize) {
    if *idx > 0 {
        *idx -= 1;
    }
}

fn nav_down(idx: &mut usize, max: usize) {
    if *idx < max {
        *idx += 1;
    }
}

fn scroll_into_view(scroll: &mut usize, idx: usize, visible: usize) {
    if idx < *scroll {
        *scroll = idx;
    } else if idx >= *scroll + visible {
        *scroll = idx.saturating_sub(visible - 1);
    }
}

// ── Input handling ──────────────────────────────────────────────────

fn handle_input(app: &mut App, key: KeyCode) {
    match app.screen {
        Screen::Welcome => match key {
            KeyCode::Enter => app.screen = Screen::SecurityWarning,
            KeyCode::Char('q') => app.should_quit = true,
            _ => {}
        },

        Screen::SecurityWarning => match key {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                app.security_accepted = true;
                app.screen = Screen::SetupMode;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                app.should_quit = true;
            }
            _ => {}
        },

        Screen::SetupMode => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.setup_mode_idx),
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.setup_mode_idx, SETUP_MODES.len() - 1);
            }
            KeyCode::Enter => app.screen = Screen::ExistingConfig,
            KeyCode::Esc => app.screen = Screen::SecurityWarning,
            _ => {}
        },

        Screen::ExistingConfig => match key {
            KeyCode::Enter => app.screen = Screen::ConfigHandling,
            KeyCode::Esc => app.screen = Screen::SetupMode,
            _ => {}
        },

        Screen::ConfigHandling => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.config_handling_idx),
            KeyCode::Down | KeyCode::Char('j') => nav_down(&mut app.config_handling_idx, 1),
            KeyCode::Enter => app.screen = Screen::QuickStartSummary,
            KeyCode::Esc => app.screen = Screen::ExistingConfig,
            _ => {}
        },

        Screen::QuickStartSummary => match key {
            KeyCode::Enter => app.screen = Screen::ProviderTier,
            KeyCode::Esc => app.screen = Screen::ConfigHandling,
            _ => {}
        },

        Screen::ProviderTier => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.provider_tier_idx),
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.provider_tier_idx, PROVIDER_TIERS.len() - 1);
            }
            KeyCode::Enter => {
                app.provider_idx = 0;
                app.provider_scroll = 0;
                app.screen = Screen::ProviderSelect;
            }
            KeyCode::Esc => app.screen = Screen::QuickStartSummary,
            _ => {}
        },

        Screen::ProviderSelect => match key {
            KeyCode::Up | KeyCode::Char('k') => {
                nav_up(&mut app.provider_idx);
                scroll_into_view(&mut app.provider_scroll, app.provider_idx, 16);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = app.current_tier_providers().len().saturating_sub(1);
                nav_down(&mut app.provider_idx, max);
                scroll_into_view(&mut app.provider_scroll, app.provider_idx, 16);
            }
            KeyCode::Enter => {
                if provider_skips_api_key_input(app.selected_provider_id()) {
                    app.api_key_input.clear();
                    app.screen = Screen::ProviderNotes;
                } else {
                    app.screen = Screen::ApiKeyInput;
                }
            }
            KeyCode::Esc => app.screen = Screen::ProviderTier,
            _ => {}
        },

        Screen::ApiKeyInput => match key {
            KeyCode::Char(c) => app.api_key_input.push(c),
            KeyCode::Backspace => {
                app.api_key_input.pop();
            }
            KeyCode::Enter => {
                app.screen = Screen::ProviderNotes;
            }
            KeyCode::Esc => {
                app.api_key_input.clear();
                app.screen = Screen::ProviderSelect;
            }
            _ => {}
        },

        Screen::ProviderNotes => match key {
            KeyCode::Enter => app.screen = Screen::ModelConfigured,
            KeyCode::Esc => app.screen = Screen::ApiKeyInput,
            _ => {}
        },

        Screen::ModelConfigured => match key {
            KeyCode::Enter => app.screen = Screen::ModelSelect,
            KeyCode::Esc => app.screen = Screen::ProviderNotes,
            _ => {}
        },

        Screen::ModelSelect => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.model_idx),
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.model_idx, MODELS.len() - 1);
            }
            KeyCode::Enter => app.screen = Screen::ChannelStatus,
            KeyCode::Esc => app.screen = Screen::ModelConfigured,
            _ => {}
        },

        Screen::ChannelStatus => match key {
            KeyCode::Enter => app.screen = Screen::HowChannelsWork,
            KeyCode::Esc => app.screen = Screen::ModelSelect,
            _ => {}
        },

        Screen::HowChannelsWork => match key {
            KeyCode::Enter => app.screen = Screen::ChannelSelect,
            KeyCode::Esc => app.screen = Screen::ChannelStatus,
            _ => {}
        },

        Screen::ChannelSelect => match key {
            KeyCode::Up | KeyCode::Char('k') => {
                nav_up(&mut app.channel_idx);
                if app.channel_idx < app.channel_scroll {
                    app.channel_scroll = app.channel_idx;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.channel_idx, CHANNELS.len() - 1);
                // Scroll down: handled in render via auto-scroll
            }
            KeyCode::Enter => app.screen = Screen::WebSearchInfo,
            KeyCode::Esc => app.screen = Screen::HowChannelsWork,
            _ => {}
        },

        Screen::WebSearchInfo => match key {
            KeyCode::Enter => app.screen = Screen::WebSearchProvider,
            KeyCode::Esc => app.screen = Screen::ChannelSelect,
            _ => {}
        },

        Screen::WebSearchProvider => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.search_provider_idx),
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.search_provider_idx, SEARCH_PROVIDERS.len() - 1);
            }
            KeyCode::Enter => {
                // Skip API key for key-free providers and "Skip for now"
                let needs_key = matches!(app.search_provider_idx, 0 | 2 | 3);
                app.screen = if needs_key {
                    Screen::WebSearchApiKey
                } else {
                    Screen::SkillsStatus
                };
            }
            KeyCode::Esc => app.screen = Screen::WebSearchInfo,
            _ => {}
        },

        Screen::WebSearchApiKey => match key {
            KeyCode::Char(c) => app.search_api_key_input.push(c),
            KeyCode::Backspace => {
                app.search_api_key_input.pop();
            }
            KeyCode::Enter if !app.search_api_key_input.is_empty() => {
                app.screen = Screen::SkillsStatus;
            }
            KeyCode::Esc => {
                app.search_api_key_input.clear();
                app.screen = Screen::WebSearchProvider;
            }
            _ => {}
        },

        Screen::SkillsStatus => match key {
            KeyCode::Enter => app.screen = Screen::SkillsInstall,
            KeyCode::Esc => app.screen = Screen::WebSearchProvider,
            _ => {}
        },

        Screen::SkillsInstall => match key {
            KeyCode::Up | KeyCode::Char('k') => {
                nav_up(&mut app.skills_idx);
                if app.skills_idx < app.skills_scroll {
                    app.skills_scroll = app.skills_idx;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                nav_down(&mut app.skills_idx, SKILLS.len() - 1);
                // Scroll down: handled in render via auto-scroll
            }
            KeyCode::Enter => app.screen = Screen::HooksInfo,
            KeyCode::Esc => app.screen = Screen::SkillsStatus,
            _ => {}
        },

        Screen::HooksInfo => match key {
            KeyCode::Enter => app.screen = Screen::HooksEnable,
            KeyCode::Esc => app.screen = Screen::SkillsInstall,
            _ => {}
        },

        Screen::HooksEnable => match key {
            KeyCode::Up | KeyCode::Char('k') => nav_up(&mut app.hooks_idx),
            KeyCode::Down | KeyCode::Char('j') => nav_down(&mut app.hooks_idx, 1),
            KeyCode::Enter => app.screen = Screen::GatewayService,
            KeyCode::Esc => app.screen = Screen::HooksInfo,
            _ => {}
        },

        Screen::GatewayService => match key {
            KeyCode::Enter => app.screen = Screen::HealthCheck,
            KeyCode::Esc => app.screen = Screen::HooksEnable,
            _ => {}
        },

        Screen::HealthCheck => match key {
            KeyCode::Enter => app.screen = Screen::OptionalApps,
            KeyCode::Esc => app.screen = Screen::GatewayService,
            _ => {}
        },

        Screen::OptionalApps => match key {
            KeyCode::Enter => app.screen = Screen::ControlUI,
            KeyCode::Esc => app.screen = Screen::HealthCheck,
            _ => {}
        },

        Screen::ControlUI => match key {
            KeyCode::Enter => app.screen = Screen::WorkspaceBackup,
            KeyCode::Esc => app.screen = Screen::OptionalApps,
            _ => {}
        },

        Screen::WorkspaceBackup => match key {
            KeyCode::Enter => app.screen = Screen::FinalSecurity,
            KeyCode::Esc => app.screen = Screen::ControlUI,
            _ => {}
        },

        Screen::FinalSecurity => match key {
            KeyCode::Enter => app.screen = Screen::WebSearchConfirm,
            KeyCode::Esc => app.screen = Screen::WorkspaceBackup,
            _ => {}
        },

        Screen::WebSearchConfirm => match key {
            KeyCode::Enter => app.screen = Screen::WhatNow,
            KeyCode::Esc => app.screen = Screen::FinalSecurity,
            _ => {}
        },

        Screen::WhatNow => match key {
            KeyCode::Enter => app.screen = Screen::Complete,
            KeyCode::Esc => app.screen = Screen::WebSearchConfirm,
            _ => {}
        },

        Screen::Complete => match key {
            KeyCode::Enter | KeyCode::Char('q') | KeyCode::Esc => {
                app.should_quit = true;
            }
            _ => {}
        },
    }
}

// ── Rendering ───────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Dark background
    let bg_block = Block::default().style(ratatui::style::Style::default().bg(theme::FROST_BG));
    frame.render_widget(bg_block, area);

    // Layout: banner + version + content + footer
    let outer = Layout::vertical([
        Constraint::Length(10),
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .split(area);

    // Banner
    frame.render_widget(Banner, outer[0]);

    // Version line
    let version_line = Line::from(vec![
        Span::styled("\u{1f980} ", theme::accent_style()),
        Span::styled(format!("ZeroClaw {VERSION}"), theme::heading_style()),
        Span::styled(
            "  \u{2502}  Zero overhead. Zero compromise.",
            theme::dim_style(),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(version_line).alignment(Alignment::Center),
        outer[1],
    );

    // Footer (context-sensitive)
    let footer = match app.screen {
        Screen::ApiKeyInput | Screen::WebSearchApiKey => Line::from(vec![
            Span::styled(" Enter", theme::heading_style()),
            Span::styled(" confirm  ", theme::dim_style()),
            Span::styled("Esc", theme::heading_style()),
            Span::styled(" back  ", theme::dim_style()),
            Span::styled("Ctrl+C", theme::heading_style()),
            Span::styled(" quit", theme::dim_style()),
        ]),
        Screen::Complete => Line::from(vec![
            Span::styled(" Enter/q", theme::heading_style()),
            Span::styled(" exit", theme::dim_style()),
        ]),
        Screen::ExistingConfig
        | Screen::QuickStartSummary
        | Screen::ProviderNotes
        | Screen::ModelConfigured
        | Screen::ChannelStatus
        | Screen::HowChannelsWork
        | Screen::WebSearchInfo
        | Screen::SkillsStatus
        | Screen::HooksInfo
        | Screen::GatewayService
        | Screen::HealthCheck
        | Screen::OptionalApps
        | Screen::ControlUI
        | Screen::WorkspaceBackup
        | Screen::FinalSecurity
        | Screen::WebSearchConfirm
        | Screen::WhatNow => Line::from(vec![
            Span::styled(" Enter", theme::heading_style()),
            Span::styled(" continue  ", theme::dim_style()),
            Span::styled("Ctrl+C", theme::heading_style()),
            Span::styled(" quit", theme::dim_style()),
        ]),
        _ => Line::from(vec![
            Span::styled(" \u{2191}\u{2193}", theme::heading_style()),
            Span::styled(" navigate  ", theme::dim_style()),
            Span::styled("Enter", theme::heading_style()),
            Span::styled(" select  ", theme::dim_style()),
            Span::styled("Esc", theme::heading_style()),
            Span::styled(" back  ", theme::dim_style()),
            Span::styled("Ctrl+C", theme::heading_style()),
            Span::styled(" quit", theme::dim_style()),
        ]),
    };
    frame.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        outer[3],
    );

    // Main content with horizontal padding
    let padded = Layout::horizontal([
        Constraint::Length(2),
        Constraint::Min(40),
        Constraint::Length(2),
    ])
    .split(outer[2]);
    let content = padded[1];

    match app.screen {
        Screen::Welcome => render_welcome(frame, content),
        Screen::SecurityWarning => render_security(frame, content),
        Screen::SetupMode => render_setup_mode(frame, content, app),
        Screen::ExistingConfig => render_existing_config(frame, content),
        Screen::ConfigHandling => render_config_handling(frame, content, app),
        Screen::QuickStartSummary => render_quickstart_summary(frame, content, app),
        Screen::ProviderTier => render_provider_tier(frame, content, app),
        Screen::ProviderSelect => render_provider_select(frame, content, app),
        Screen::ApiKeyInput => render_api_key(frame, content, app),
        Screen::ProviderNotes => render_provider_notes(frame, content, app),
        Screen::ModelConfigured => render_model_configured(frame, content, app),
        Screen::ModelSelect => render_model_select(frame, content, app),
        Screen::ChannelStatus => render_channel_status(frame, content),
        Screen::HowChannelsWork => render_how_channels_work(frame, content),
        Screen::ChannelSelect => render_channel_select(frame, content, app),
        Screen::WebSearchInfo => render_web_search_info(frame, content),
        Screen::WebSearchProvider => render_web_search_provider(frame, content, app),
        Screen::WebSearchApiKey => render_web_search_api_key(frame, content, app),
        Screen::SkillsStatus => render_skills_status(frame, content),
        Screen::SkillsInstall => render_skills_install(frame, content, app),
        Screen::HooksInfo => render_hooks_info(frame, content),
        Screen::HooksEnable => render_hooks_enable(frame, content, app),
        Screen::GatewayService => render_gateway_service(frame, content, app),
        Screen::HealthCheck => render_health_check(frame, content, app),
        Screen::OptionalApps => render_optional_apps(frame, content),
        Screen::ControlUI => render_control_ui(frame, content, app),
        Screen::WorkspaceBackup => render_workspace_backup(frame, content),
        Screen::FinalSecurity => render_final_security(frame, content),
        Screen::WebSearchConfirm => render_web_search_confirm(frame, content, app),
        Screen::WhatNow => render_what_now(frame, content),
        Screen::Complete => render_complete(frame, content, app),
    }
}

// ── Helper: setup title line ────────────────────────────────────────

fn setup_title() -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled("\u{250c}  ", theme::border_style()),
        Span::styled("ZeroClaw setup", theme::heading_style()),
    ]))
}

fn continue_hint() -> Paragraph<'static> {
    Paragraph::new(Line::from(Span::styled(
        "Press Enter to continue...",
        theme::dim_style(),
    )))
}

// ── Screen: Welcome ─────────────────────────────────────────────────

fn render_welcome(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "\u{250c}  ZeroClaw setup",
            theme::heading_style(),
        )),
        Line::from(Span::styled("\u{2502}", theme::border_style())),
        Line::from(vec![
            Span::styled("\u{2502}  ", theme::border_style()),
            Span::styled(
                "Welcome to ZeroClaw \u{2014} the fastest, smallest AI assistant.",
                theme::body_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled("\u{2502}  ", theme::border_style()),
            Span::styled(
                "This wizard will configure your agent in under 60 seconds.",
                theme::dim_style(),
            ),
        ]),
        Line::from(Span::styled("\u{2502}", theme::border_style())),
        Line::from(vec![
            Span::styled("\u{2514}  ", theme::border_style()),
            Span::styled(
                "Press Enter to begin...",
                theme::heading_style().add_modifier(Modifier::SLOW_BLINK),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

// ── Screen: Security ────────────────────────────────────────────────

fn render_security(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(3),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    let lines = vec![
        Line::from(Span::styled(
            "Security warning \u{2014} please read.",
            theme::warn_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "ZeroClaw is optimized for single-operator deployments.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "By default, ZeroClaw is a personal agent: one trusted operator",
            theme::body_style(),
        )),
        Line::from(Span::styled("boundary.", theme::body_style())),
        Line::from(Span::styled(
            "This bot can read files and run actions if tools are enabled.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "A bad prompt can trick it into doing unsafe things.",
            theme::body_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "ZeroClaw is not a hostile multi-tenant boundary by default.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "If multiple users can message one tool-enabled agent, they share",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "that delegated tool authority.",
            theme::body_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "If you're not comfortable with security hardening and access",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "control, don't run ZeroClaw.",
            theme::body_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Recommended baseline:",
            theme::heading_style(),
        )),
        Line::from(Span::styled(
            "  - Pairing/allowlists + mention gating.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  - Multi-user/shared inbox: split trust boundaries (separate",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "    gateway/credentials, ideally separate OS users/hosts).",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  - Sandbox + least-privilege tools.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  - Shared inboxes: isolate DM sessions (`session.dmScope:",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "    per-channel-peer`) and keep tool access minimal.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  - Keep secrets out of the agent's reachable filesystem.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  - Use the strongest available model for any bot with tools or",
            theme::body_style(),
        )),
        Line::from(Span::styled("    untrusted inboxes.", theme::body_style())),
        Line::from(""),
        Line::from(Span::styled("Run regularly:", theme::heading_style())),
        Line::from(Span::styled(
            "  zeroclaw security audit --deep",
            theme::dim_style(),
        )),
        Line::from(Span::styled(
            "  zeroclaw security audit --fix",
            theme::dim_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Must read: {DOCS_BASE}/gateway/security"),
            theme::dim_style(),
        )),
    ];

    frame.render_widget(
        InfoPanel {
            title: "Security",
            lines,
        },
        layout[1],
    );

    let prompt = Line::from(vec![
        Span::styled("\u{25c6}  ", theme::accent_style()),
        Span::styled(
            "I understand this is personal-by-default and shared/multi-user use ",
            theme::heading_style(),
        ),
    ]);
    let prompt2 = Line::from(vec![
        Span::raw("   "),
        Span::styled("requires lock-down. Continue? ", theme::heading_style()),
        Span::styled("[y/N]", theme::dim_style()),
    ]);
    frame.render_widget(Paragraph::new(vec![prompt, prompt2]), layout[2]);
}

// ── Screen: Setup mode ──────────────────────────────────────────────

fn render_setup_mode(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Security accepted",
            value: "Yes",
        },
        layout[1],
    );

    let items: Vec<SelectableItem> = SETUP_MODES
        .iter()
        .enumerate()
        .map(|(i, mode)| SelectableItem {
            label: mode.to_string(),
            hint: match i {
                0 => "recommended".to_string(),
                1 => "advanced".to_string(),
                _ => "skip".to_string(),
            },
            is_active: i == app.setup_mode_idx,
            installed: false,
        })
        .collect();

    frame.render_widget(
        SelectableList {
            title: "Setup mode",
            items: &items,
            selected: app.setup_mode_idx,
            scroll_offset: 0,
        },
        layout[2],
    );
}

// ── Screen: Existing config ─────────────────────────────────────────

fn render_existing_config(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Setup mode",
            value: "QuickStart",
        },
        layout[1],
    );

    frame.render_widget(
        InfoPanel {
            title: "Existing config detected",
            lines: vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  gateway.bind: ", theme::dim_style()),
                    Span::styled("lan", theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  gateway.port: ", theme::dim_style()),
                    Span::styled("42617", theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  gateway.auth: ", theme::dim_style()),
                    Span::styled("Token (default)", theme::heading_style()),
                ]),
                Line::from(""),
            ],
        },
        layout[2],
    );

    frame.render_widget(continue_hint(), layout[3]);
}

// ── Screen: Config handling ─────────────────────────────────────────

fn render_config_handling(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Setup mode",
            value: "QuickStart",
        },
        layout[1],
    );

    let items = vec![
        SelectableItem {
            label: "Use existing values".to_string(),
            hint: "keep current config".to_string(),
            is_active: app.config_handling_idx == 0,
            installed: false,
        },
        SelectableItem {
            label: "Overwrite".to_string(),
            hint: "start fresh".to_string(),
            is_active: app.config_handling_idx == 1,
            installed: false,
        },
    ];

    frame.render_widget(
        SelectableList {
            title: "Config handling",
            items: &items,
            selected: app.config_handling_idx,
            scroll_offset: 0,
        },
        layout[2],
    );
}

// ── Screen: QuickStart summary ──────────────────────────────────────

fn render_quickstart_summary(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(12),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Setup mode",
            value: "QuickStart",
        },
        layout[1],
    );
    frame.render_widget(
        ConfirmedLine {
            label: "Config handling",
            value: if app.config_handling_idx == 0 {
                "Use existing values"
            } else {
                "Overwrite"
            },
        },
        layout[2],
    );

    frame.render_widget(
        InfoPanel {
            title: "QuickStart",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Keeping your current gateway settings:",
                    theme::body_style(),
                )),
                Line::from(vec![
                    Span::styled("  Gateway port: ", theme::dim_style()),
                    Span::styled(format!("{}", app.gateway_port), theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Gateway bind: ", theme::dim_style()),
                    Span::styled("LAN", theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Gateway auth: ", theme::dim_style()),
                    Span::styled("Token (default)", theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Tailscale exposure: ", theme::dim_style()),
                    Span::styled("Off", theme::heading_style()),
                ]),
                Line::from(Span::styled(
                    "  Direct to chat channels.",
                    theme::body_style(),
                )),
                Line::from(""),
            ],
        },
        layout[3],
    );

    frame.render_widget(continue_hint(), layout[4]);
}

// ── Screen: Provider tier ───────────────────────────────────────────

fn render_provider_tier(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Setup mode",
            value: SETUP_MODES[app.setup_mode_idx],
        },
        layout[1],
    );

    let items: Vec<SelectableItem> = PROVIDER_TIERS
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| SelectableItem {
            label: name.to_string(),
            hint: desc.to_string(),
            is_active: i == app.provider_tier_idx,
            installed: false,
        })
        .collect();

    frame.render_widget(
        SelectableList {
            title: "Select provider category",
            items: &items,
            selected: app.provider_tier_idx,
            scroll_offset: 0,
        },
        layout[2],
    );
}

// ── Screen: Provider select ─────────────────────────────────────────

fn render_provider_select(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Setup mode",
            value: SETUP_MODES[app.setup_mode_idx],
        },
        layout[1],
    );
    frame.render_widget(
        ConfirmedLine {
            label: "Category",
            value: PROVIDER_TIERS[app.provider_tier_idx].0,
        },
        layout[2],
    );

    let providers = app.current_tier_providers();
    let items: Vec<SelectableItem> = providers
        .iter()
        .enumerate()
        .map(|(i, (name, desc, _id))| SelectableItem {
            label: name.to_string(),
            hint: desc.to_string(),
            is_active: i == app.provider_idx,
            installed: false,
        })
        .collect();

    frame.render_widget(
        SelectableList {
            title: "Select your AI provider",
            items: &items,
            selected: app.provider_idx,
            scroll_offset: app.provider_scroll,
        },
        layout[3],
    );
}

// ── Screen: API key input ───────────────────────────────────────────

fn render_api_key(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Provider",
            value: app.selected_provider(),
        },
        layout[1],
    );
    let provider_id = app.selected_provider_id();
    let prompt = if provider_uses_oauth_without_api_key(provider_id) {
        format!(
            "{} uses OAuth (no API key). Press Enter to continue.",
            app.selected_provider()
        )
    } else if provider_supports_keyless_local_usage(provider_id) {
        format!(
            "{} is local-first (no API key required). Press Enter to continue.",
            app.selected_provider()
        )
    } else if provider_id == "bedrock" {
        "Bedrock uses AWS credentials (AK/SK), not a single API key. Press Enter to continue."
            .to_string()
    } else {
        format!(
            "Enter {} API key (or press Enter to skip)",
            app.selected_provider()
        )
    };

    frame.render_widget(
        InputPrompt {
            label: &prompt,
            input: &app.api_key_input,
            masked: true,
        },
        layout[2],
    );
}

// ── Screen: Provider notes ──────────────────────────────────────────

fn render_provider_notes(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Provider",
            value: app.selected_provider(),
        },
        layout[1],
    );
    let provider_id = app.selected_provider_id();
    let api_key_status = if !app.api_key_input.is_empty() {
        "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022} (set)".to_string()
    } else if provider_uses_oauth_without_api_key(provider_id) {
        "OAuth login required (no API key)".to_string()
    } else if provider_supports_keyless_local_usage(provider_id) {
        "not required (local provider)".to_string()
    } else if provider_id == "bedrock" {
        "use AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY".to_string()
    } else {
        "not set (optional for now)".to_string()
    };

    frame.render_widget(
        ConfirmedLine {
            label: "API key",
            value: &api_key_status,
        },
        layout[2],
    );

    frame.render_widget(
        InfoPanel {
            title: "Provider notes",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!(
                        "  Verified {} on default endpoint.",
                        app.selected_provider()
                    ),
                    theme::success_style(),
                )),
                Line::from(""),
            ],
        },
        layout[3],
    );

    frame.render_widget(continue_hint(), layout[4]);
}

// ── Screen: Model configured ────────────────────────────────────────

fn render_model_configured(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Provider",
            value: app.selected_provider(),
        },
        layout[1],
    );

    let model_name = match app.selected_provider() {
        "Z.AI" => "zai/glm-5",
        "Anthropic" => "anthropic/claude-sonnet-4",
        "OpenAI" => "openai/gpt-4o",
        "Google" => "google/gemini-2.0-flash",
        "Groq" => "groq/llama-3.3-70b",
        "Ollama" => "ollama/llama3",
        _ => "auto",
    };

    frame.render_widget(
        InfoPanel {
            title: "Model configured",
            lines: vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Default model set to ", theme::body_style()),
                    Span::styled(model_name, theme::heading_style()),
                ]),
                Line::from(""),
            ],
        },
        layout[2],
    );

    frame.render_widget(continue_hint(), layout[3]);
}

// ── Screen: Model select ────────────────────────────────────────────

fn render_model_select(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Provider",
            value: app.selected_provider(),
        },
        layout[1],
    );

    let items: Vec<SelectableItem> = MODELS
        .iter()
        .enumerate()
        .map(|(i, model)| SelectableItem {
            label: model.to_string(),
            hint: if i == 0 {
                "default".to_string()
            } else {
                String::new()
            },
            is_active: i == app.model_idx,
            installed: false,
        })
        .collect();

    frame.render_widget(
        SelectableList {
            title: "Default model",
            items: &items,
            selected: app.model_idx,
            scroll_offset: 0,
        },
        layout[2],
    );
}

// ── Screen: Channel status ──────────────────────────────────────────

fn render_channel_status(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    let status_lines: Vec<Line> = vec![
        ("Telegram", "needs token", false),
        ("Discord", "needs token", false),
        ("IRC", "needs host + nick", false),
        ("Slack", "needs tokens", false),
        ("Signal", "needs setup", false),
        ("signal-cli", "missing (signal-cli)", false),
        ("iMessage", "needs setup", false),
        ("imsg", "found (imsg)", true),
        ("LINE", "needs token + secret", false),
        ("Mattermost", "needs token + url", false),
        ("Nextcloud Talk", "needs setup", false),
        ("Feishu", "needs app credentials", false),
        ("BlueBubbles", "needs setup", false),
        ("Zalo", "needs token", false),
        ("Synology Chat", "needs token + incoming webhook", false),
        ("WhatsApp", "not configured", false),
        ("Google Chat", "installed", true),
        ("Nostr", "installed", true),
        ("Microsoft Teams", "installed", true),
        ("Matrix", "installed", true),
        ("Zalo Personal", "installed", true),
        ("Tlon", "installed", true),
        ("Twitch", "installed", true),
        ("WhatsApp", "installed", true),
    ]
    .into_iter()
    .map(|(name, status, ok)| {
        Line::from(vec![
            Span::styled(format!("  {name}: "), theme::body_style()),
            Span::styled(
                status,
                if ok {
                    theme::success_style()
                } else {
                    theme::warn_style()
                },
            ),
        ])
    })
    .collect();

    frame.render_widget(
        InfoPanel {
            title: "Channel status",
            lines: status_lines,
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: How channels work ───────────────────────────────────────

fn render_how_channels_work(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    let lines = vec![
        Line::from(Span::styled(
            "  DM security: default is pairing; unknown DMs get a pairing code.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Approve with: zeroclaw pairing approve <channel> <code>",
            theme::dim_style(),
        )),
        Line::from(Span::styled(
            "  Public DMs require dmPolicy=\"open\" + allowFrom=[\"*\"].",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Multi-user DMs: run: zeroclaw config set session.dmScope",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  \"per-channel-peer\" to isolate sessions.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            format!("  Docs: {DOCS_BASE}/channels/pairing"),
            theme::dim_style(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Telegram: simplest way to get started \u{2014} register a bot with",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  @BotFather and get going.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  WhatsApp: works with your own number; recommend a separate phone",
            theme::body_style(),
        )),
        Line::from(Span::styled("  + eSIM.", theme::body_style())),
        Line::from(Span::styled(
            "  Discord: very well supported right now.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  IRC: classic IRC networks with DM/channel routing and pairing",
            theme::body_style(),
        )),
        Line::from(Span::styled("  controls.", theme::body_style())),
        Line::from(Span::styled(
            "  Slack: supported (Socket Mode).",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Signal: signal-cli linked device; more setup.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  iMessage: this is still a work in progress.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Matrix: open protocol; install the plugin to enable.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Nostr: Decentralized protocol; encrypted DMs via NIP-04.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Twitch: Twitch chat integration",
            theme::body_style(),
        )),
    ];

    frame.render_widget(
        InfoPanel {
            title: "How channels work",
            lines,
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Channel select ──────────────────────────────────────────

fn render_channel_select(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);

    frame.render_widget(setup_title(), layout[0]);

    let items: Vec<SelectableItem> = CHANNELS
        .iter()
        .enumerate()
        .map(|(i, (name, hint, installed))| SelectableItem {
            label: name.to_string(),
            hint: if *installed {
                format!("{hint} \u{2713} installed")
            } else {
                hint.to_string()
            },
            is_active: i == app.channel_idx,
            installed: *installed,
        })
        .collect();

    let visible = (layout[1].height.saturating_sub(2)) as usize;
    let scroll = if app.channel_idx >= app.channel_scroll + visible {
        app.channel_idx.saturating_sub(visible - 1)
    } else {
        app.channel_scroll
    };

    frame.render_widget(
        SelectableList {
            title: "Select channel (QuickStart)",
            items: &items,
            selected: app.channel_idx,
            scroll_offset: scroll,
        },
        layout[1],
    );
}

// ── Screen: Web search info ─────────────────────────────────────────

fn render_web_search_info(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(10),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Web search",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Web search lets your agent look things up online.",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  Choose a provider. Some providers need an API key, and some work",
                    theme::body_style(),
                )),
                Line::from(Span::styled("  key-free.", theme::body_style())),
                Line::from(Span::styled(
                    format!("  Docs: {DOCS_BASE}/tools/web"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Web search provider ─────────────────────────────────────

fn render_web_search_provider(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);

    frame.render_widget(setup_title(), layout[0]);

    let items: Vec<SelectableItem> = SEARCH_PROVIDERS
        .iter()
        .enumerate()
        .map(|(i, (name, hint))| SelectableItem {
            label: name.to_string(),
            hint: hint.to_string(),
            is_active: i == app.search_provider_idx,
            installed: false,
        })
        .collect();

    frame.render_widget(
        SelectableList {
            title: "Search provider",
            items: &items,
            selected: app.search_provider_idx,
            scroll_offset: 0,
        },
        layout[2 - 1], // layout[1]
    );
}

// ── Screen: Web search API key ──────────────────────────────────────

fn render_web_search_api_key(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Min(1),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        ConfirmedLine {
            label: "Search provider",
            value: app.selected_search_provider(),
        },
        layout[1],
    );
    frame.render_widget(
        InputPrompt {
            label: &format!("{} API key", app.selected_search_provider()),
            input: &app.search_api_key_input,
            masked: false,
        },
        layout[2],
    );
}

// ── Screen: Skills status ───────────────────────────────────────────

fn render_skills_status(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(10),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    let skill_count = SKILLS.len() - 1; // exclude "Skip"
    frame.render_widget(
        InfoPanel {
            title: "Skills status",
            lines: vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Eligible: ", theme::dim_style()),
                    Span::styled(format!("{skill_count}"), theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Missing requirements: ", theme::dim_style()),
                    Span::styled(format!("{skill_count}"), theme::warn_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Unsupported on this OS: ", theme::dim_style()),
                    Span::styled("0", theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  Blocked by allowlist: ", theme::dim_style()),
                    Span::styled("0", theme::heading_style()),
                ]),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Skills install ──────────────────────────────────────────

fn render_skills_install(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);

    frame.render_widget(setup_title(), layout[0]);

    let items: Vec<SelectableItem> = SKILLS
        .iter()
        .enumerate()
        .map(|(i, (name, desc))| SelectableItem {
            label: name.to_string(),
            hint: desc.to_string(),
            is_active: i == app.skills_idx,
            installed: false,
        })
        .collect();

    let visible = (layout[1].height.saturating_sub(2)) as usize;
    let scroll = if app.skills_idx >= app.skills_scroll + visible {
        app.skills_idx.saturating_sub(visible - 1)
    } else {
        app.skills_scroll
    };

    frame.render_widget(
        SelectableList {
            title: "Install missing skill dependencies",
            items: &items,
            selected: app.skills_idx,
            scroll_offset: scroll,
        },
        layout[1],
    );
}

// ── Screen: Hooks info ──────────────────────────────────────────────

fn render_hooks_info(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(10),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Hooks",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Hooks let you automate actions when agent commands are issued.",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  Example: Save session context to memory when you issue /new or",
                    theme::body_style(),
                )),
                Line::from(Span::styled("  /reset.", theme::body_style())),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Learn more: {DOCS_BASE}/automation/hooks"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Hooks enable ────────────────────────────────────────────

fn render_hooks_enable(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(6)]).split(area);

    frame.render_widget(setup_title(), layout[0]);

    let items = vec![
        SelectableItem {
            label: "Enable hooks".to_string(),
            hint: "recommended".to_string(),
            is_active: app.hooks_idx == 0,
            installed: false,
        },
        SelectableItem {
            label: "Skip for now".to_string(),
            hint: String::new(),
            is_active: app.hooks_idx == 1,
            installed: false,
        },
    ];

    frame.render_widget(
        SelectableList {
            title: "Enable hooks?",
            items: &items,
            selected: app.hooks_idx,
            scroll_offset: 0,
        },
        layout[1],
    );
}

// ── Screen: Gateway service ─────────────────────────────────────────

fn render_gateway_service(frame: &mut Frame, area: Rect, _app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Length(4),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Gateway service runtime",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  QuickStart uses the native Rust gateway service",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  (stable + optimized for minimal overhead).",
                    theme::body_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    // Simulated install
    frame.render_widget(
        StepIndicator {
            current: 1,
            total: 1,
            label: "Gateway service installed.",
            status: StepStatus::Complete,
        },
        layout[2],
    );

    frame.render_widget(continue_hint(), layout[3]);
}

// ── Screen: Health check ────────────────────────────────────────────

fn render_health_check(frame: &mut Frame, area: Rect, _app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        StepIndicator {
            current: 1,
            total: 1,
            label: "Health check passed",
            status: StepStatus::Complete,
        },
        layout[1],
    );

    frame.render_widget(
        InfoPanel {
            title: "Health check help",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled("  Docs:", theme::dim_style())),
                Line::from(Span::styled(
                    format!("  {DOCS_BASE}/gateway/health"),
                    theme::dim_style(),
                )),
                Line::from(Span::styled(
                    format!("  {DOCS_BASE}/gateway/troubleshooting"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[2],
    );

    frame.render_widget(continue_hint(), layout[3]);
}

// ── Screen: Optional apps ───────────────────────────────────────────

fn render_optional_apps(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(10),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Optional apps",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Add nodes for extra features:",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  - macOS app (system + notifications)",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  - iOS app (camera/canvas)",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  - Android app (camera/canvas)",
                    theme::body_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Control UI ──────────────────────────────────────────────

fn render_control_ui(frame: &mut Frame, area: Rect, app: &App) {
    let base = app.gateway_base_url();
    let ws = format!("ws://{}:{}", app.gateway_host, app.gateway_port);

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Web UI: ", theme::dim_style()),
            Span::styled(format!("{base}/"), theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Gateway WS: ", theme::dim_style()),
            Span::styled(&ws, theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Gateway: ", theme::dim_style()),
            Span::styled("detected", theme::success_style()),
        ]),
    ];

    if app.pairing_required {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  \u{1f510} PAIRING CODE \u{2014} enter this in the web dashboard to connect:",
            theme::warn_style(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "     \u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}",
                theme::accent_style(),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("     \u{2502}  ", theme::accent_style()),
            Span::styled(
                &app.pairing_code,
                theme::title_style().add_modifier(Modifier::BOLD),
            ),
            Span::styled("  \u{2502}", theme::accent_style()),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "     \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}",
                theme::accent_style(),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Also works with: Docker, macOS app, iOS/Android",
            theme::dim_style(),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("  Pairing: ", theme::dim_style()),
            Span::styled("disabled (open access)", theme::warn_style()),
        ]));
        lines.push(Line::from(Span::styled(
            "  Enable with: require_pairing = true in config.toml",
            theme::dim_style(),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  Docs: {DOCS_BASE}/web/control-ui"),
        theme::dim_style(),
    )));
    lines.push(Line::from(""));

    let panel_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2); // +2 for border
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(panel_height),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);
    frame.render_widget(
        InfoPanel {
            title: "Control UI",
            lines,
        },
        layout[1],
    );
    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Workspace backup ────────────────────────────────────────

fn render_workspace_backup(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Workspace backup",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Back up your agent workspace.",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    format!("  Docs: {DOCS_BASE}/concepts/agent-workspace"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Final security ──────────────────────────────────────────

fn render_final_security(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "Security",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Running agents on your computer is risky \u{2014} harden your setup:",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    format!("  {DOCS_BASE}/security"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Web search confirm ──────────────────────────────────────

fn render_web_search_confirm(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(12),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    let provider = app.selected_search_provider();
    let has_key = !app.search_api_key_input.is_empty();

    frame.render_widget(
        InfoPanel {
            title: "Web search",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Web search is enabled, so your agent can look things up online",
                    theme::body_style(),
                )),
                Line::from(Span::styled("  when needed.", theme::body_style())),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Provider: ", theme::dim_style()),
                    Span::styled(provider, theme::heading_style()),
                ]),
                Line::from(vec![
                    Span::styled("  API key: ", theme::dim_style()),
                    Span::styled(
                        if has_key {
                            "stored in config."
                        } else {
                            "not required."
                        },
                        theme::heading_style(),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("  Docs: {DOCS_BASE}/tools/web"),
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: What now ────────────────────────────────────────────────

fn render_what_now(frame: &mut Frame, area: Rect) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(8),
        Constraint::Min(2),
    ])
    .split(area);

    frame.render_widget(setup_title(), layout[0]);

    frame.render_widget(
        InfoPanel {
            title: "What now",
            lines: vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  What now: https://zeroclawlabs.ai/showcase",
                    theme::body_style(),
                )),
                Line::from(Span::styled(
                    "  (\"What People Are Building\")",
                    theme::dim_style(),
                )),
                Line::from(""),
            ],
        },
        layout[1],
    );

    frame.render_widget(continue_hint(), layout[2]);
}

// ── Screen: Complete ────────────────────────────────────────────────

fn render_complete(frame: &mut Frame, area: Rect, app: &App) {
    let layout = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(20),
        Constraint::Min(2),
    ])
    .split(area);

    let title = Line::from(vec![
        Span::styled("\u{2514}  ", theme::border_style()),
        Span::styled(
            "Onboarding complete. Use the dashboard link above to control ZeroClaw.",
            theme::heading_style(),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), layout[0]);

    let url = app.gateway_base_url();

    let mut summary_lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  \u{1f980} ZeroClaw configured successfully!",
            theme::success_style().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Provider:      ", theme::dim_style()),
            Span::styled(app.selected_provider(), theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Model:         ", theme::dim_style()),
            Span::styled(app.selected_model(), theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Channel:       ", theme::dim_style()),
            Span::styled(app.selected_channel(), theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Web search:    ", theme::dim_style()),
            Span::styled(app.selected_search_provider(), theme::heading_style()),
        ]),
        Line::from(vec![
            Span::styled("  Dashboard:     ", theme::dim_style()),
            Span::styled(&url, theme::heading_style()),
        ]),
    ];

    if app.pairing_required {
        summary_lines.push(Line::from(vec![
            Span::styled("  Pairing code:  ", theme::dim_style()),
            Span::styled(
                &app.pairing_code,
                theme::title_style().add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        summary_lines.push(Line::from(vec![
            Span::styled("  Pairing:       ", theme::dim_style()),
            Span::styled("disabled (open access)", theme::warn_style()),
        ]));
    }

    summary_lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "  Run `zeroclaw daemon` to start your agent.",
            theme::body_style(),
        )),
        Line::from(Span::styled(
            "  Run `zeroclaw doctor` to validate your setup.",
            theme::body_style(),
        )),
        Line::from(""),
    ]);

    frame.render_widget(
        InfoPanel {
            title: "Setup complete",
            lines: summary_lines,
        },
        layout[1],
    );

    let cont = Line::from(Span::styled(
        "Press Enter or q to exit.",
        theme::dim_style(),
    ));
    frame.render_widget(Paragraph::new(cont), layout[2]);
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an App with sensible defaults for testing.
    fn test_app() -> App {
        App {
            screen: Screen::Complete,
            should_quit: false,
            security_accepted: true,
            setup_mode_idx: 0,
            config_handling_idx: 0,
            provider_tier_idx: 0,
            provider_idx: 0,
            provider_scroll: 0,
            api_key_input: String::new(),
            model_idx: 0,
            channel_idx: 0,
            channel_scroll: 0,
            search_provider_idx: 0,
            search_api_key_input: String::new(),
            skills_idx: 0,
            skills_scroll: 0,
            hooks_idx: 0,
            gateway_port: 42617,
            gateway_host: "127.0.0.1".to_string(),
            pairing_code: "123456".to_string(),
            pairing_required: true,
        }
    }

    // ── Provider persistence ────────────────────────────────────────

    #[test]
    fn save_provider_openrouter() {
        let app = test_app(); // tier 0, provider 0 = OpenRouter
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.providers.fallback.as_deref(), Some("openrouter"));
    }

    #[test]
    fn save_provider_anthropic() {
        let mut app = test_app();
        app.provider_tier_idx = 0;
        app.provider_idx = 2; // Anthropic
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.providers.fallback.as_deref(), Some("anthropic"));
    }

    #[test]
    fn save_provider_ollama_local() {
        let mut app = test_app();
        app.provider_tier_idx = 4; // Local / private
        app.provider_idx = 0; // Ollama
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.providers.fallback.as_deref(), Some("ollama"));
    }

    #[test]
    fn save_provider_custom_clears_api_url() {
        let mut app = test_app();
        app.provider_tier_idx = 0;
        app.provider_idx = 0; // OpenRouter (non-custom)
        let mut config = Config::default();
        config.ensure_fallback_provider().base_url = Some("http://old-custom-url.com".to_string());
        apply_tui_selections_to_config(&app, &mut config);
        assert!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.base_url.as_deref())
                .is_none(),
            "api_url should be cleared for non-custom providers"
        );
    }

    // ── API key persistence ─────────────────────────────────────────

    #[test]
    fn save_api_key_when_provided() {
        let mut app = test_app();
        app.api_key_input = "sk-test-key-12345".to_string();
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("sk-test-key-12345")
        );
    }

    #[test]
    fn save_no_api_key_when_empty() {
        let app = test_app(); // api_key_input is empty
        let mut config = Config::default();
        config.providers.fallback = Some("openrouter".into());
        config.providers.models.insert(
            "openrouter".into(),
            zeroclaw_config::schema::ModelProviderConfig {
                api_key: Some("existing-key".to_string()),
                ..Default::default()
            },
        );
        apply_tui_selections_to_config(&app, &mut config);
        // Should preserve existing key, not overwrite with empty
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("existing-key")
        );
    }

    // ── Model persistence ───────────────────────────────────────────

    #[test]
    fn save_model_auto_clears_default() {
        let app = test_app(); // model_idx 0 = "Auto (recommended)"
        let mut config = Config::default();
        config.ensure_fallback_provider().model = Some("old-model".to_string());
        apply_tui_selections_to_config(&app, &mut config);
        assert!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref())
                .is_none(),
            "Auto should clear default_model"
        );
    }

    #[test]
    fn save_model_specific() {
        let mut app = test_app();
        app.model_idx = 1; // "claude-sonnet-4-20250514"
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn save_model_gpt4o() {
        let mut app = test_app();
        app.model_idx = 3; // "gpt-4o"
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-4o")
        );
    }

    // ── Channel persistence ─────────────────────────────────────────

    #[test]
    fn save_channel_telegram() {
        let mut app = test_app();
        app.channel_idx = 0; // Telegram
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let tg = config
            .channels
            .telegram
            .as_ref()
            .expect("telegram should be Some");
        assert_eq!(tg.bot_token, "YOUR_TELEGRAM_BOT_TOKEN");
    }

    #[test]
    fn save_channel_discord() {
        let mut app = test_app();
        app.channel_idx = 2; // Discord
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let dc = config
            .channels
            .discord
            .as_ref()
            .expect("discord should be Some");
        assert_eq!(dc.bot_token, "YOUR_DISCORD_BOT_TOKEN");
        assert!(dc.guild_id.is_none());
    }

    #[test]
    fn save_channel_slack() {
        let mut app = test_app();
        app.channel_idx = 5; // Slack
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let sl = config
            .channels
            .slack
            .as_ref()
            .expect("slack should be Some");
        assert!(sl.bot_token.starts_with("xoxb-"));
        assert!(sl.app_token.as_ref().unwrap().starts_with("xapp-"));
    }

    #[test]
    fn save_channel_whatsapp() {
        let mut app = test_app();
        app.channel_idx = 1; // WhatsApp
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let wa = config
            .channels
            .whatsapp
            .as_ref()
            .expect("whatsapp should be Some");
        assert!(wa.access_token.is_some());
        assert!(wa.phone_number_id.is_some());
        assert!(wa.verify_token.is_some());
    }

    #[test]
    fn save_channel_signal() {
        let mut app = test_app();
        app.channel_idx = 6; // Signal
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let sig = config
            .channels
            .signal
            .as_ref()
            .expect("signal should be Some");
        assert_eq!(sig.http_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn save_channel_irc() {
        let mut app = test_app();
        app.channel_idx = 3; // IRC
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let irc = config.channels.irc.as_ref().expect("irc should be Some");
        assert_eq!(irc.server, "irc.libera.chat");
        assert_eq!(irc.port, 6697);
        assert_eq!(irc.nickname, "zeroclaw-bot");
    }

    #[test]
    fn save_channel_imessage() {
        let mut app = test_app();
        app.channel_idx = 7; // iMessage
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.channels.imessage.is_some());
    }

    #[test]
    fn save_channel_matrix() {
        let mut app = test_app();
        // Find Matrix index in CHANNELS
        let matrix_idx = CHANNELS.iter().position(|c| c.0 == "Matrix").unwrap();
        app.channel_idx = matrix_idx;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let mx = config
            .channels
            .matrix
            .as_ref()
            .expect("matrix should be Some");
        assert_eq!(mx.homeserver, "https://matrix.org");
    }

    #[test]
    fn save_channel_mattermost() {
        let mut app = test_app();
        app.channel_idx = 9; // Mattermost
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let mm = config
            .channels
            .mattermost
            .as_ref()
            .expect("mattermost should be Some");
        assert_eq!(mm.url, "https://mattermost.example.com");
    }

    #[test]
    fn save_channel_nextcloud_talk() {
        let mut app = test_app();
        let idx = CHANNELS
            .iter()
            .position(|c| c.0 == "Nextcloud Talk")
            .unwrap();
        app.channel_idx = idx;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        let nc = config
            .channels
            .nextcloud_talk
            .as_ref()
            .expect("nextcloud should be Some");
        assert_eq!(nc.base_url, "https://cloud.example.com");
    }

    #[test]
    fn save_channel_feishu_lark() {
        let mut app = test_app();
        let idx = CHANNELS.iter().position(|c| c.0 == "Feishu/Lark").unwrap();
        app.channel_idx = idx;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.channels.feishu.is_some(), "feishu should be set");
        assert!(config.channels.lark.is_some(), "lark should be set");
    }

    #[test]
    fn save_channel_skip_does_not_create_stubs() {
        let mut app = test_app();
        let idx = CHANNELS.iter().position(|c| c.0 == "Skip for now").unwrap();
        app.channel_idx = idx;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.channels.telegram.is_none());
        assert!(config.channels.discord.is_none());
        assert!(config.channels.slack.is_none());
    }

    #[test]
    fn save_channel_does_not_overwrite_existing() {
        let mut app = test_app();
        app.channel_idx = 0; // Telegram
        let mut config = Config::default();
        // Pre-set a Telegram config with a real token
        config.channels.telegram = Some(TelegramConfig {
            enabled: true,
            bot_token: "REAL_TOKEN_123".to_string(),
            allowed_users: vec!["alice".to_string()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });
        apply_tui_selections_to_config(&app, &mut config);
        let tg = config.channels.telegram.as_ref().unwrap();
        assert_eq!(
            tg.bot_token, "REAL_TOKEN_123",
            "should NOT overwrite existing config"
        );
        assert_eq!(tg.allowed_users, vec!["alice"]);
    }

    // ── Web search persistence ──────────────────────────────────────

    #[test]
    fn save_web_search_brave() {
        let mut app = test_app();
        app.search_provider_idx = 0; // Brave Search
        app.search_api_key_input = "brv-key-abc".to_string();
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.provider, "brave");
        assert_eq!(
            config.web_search.brave_api_key.as_deref(),
            Some("brv-key-abc")
        );
    }

    #[test]
    fn save_web_search_searxng() {
        let mut app = test_app();
        app.search_provider_idx = 1; // SearxNG
        app.search_api_key_input = "https://searx.example.com".to_string();
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.provider, "searxng");
        assert_eq!(
            config.web_search.searxng_instance_url.as_deref(),
            Some("https://searx.example.com")
        );
    }

    #[test]
    fn save_web_search_duckduckgo() {
        let mut app = test_app();
        app.search_provider_idx = 4; // DuckDuckGo
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.provider, "duckduckgo");
    }

    #[test]
    fn save_web_search_tavily_maps_to_tavily() {
        let mut app = test_app();
        app.search_provider_idx = 2; // Tavily
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.web_search.provider, "tavily");
    }

    #[test]
    fn save_web_search_skip() {
        let mut app = test_app();
        app.search_provider_idx = 5; // Skip for now
        let mut config = Config::default();
        let old_enabled = config.web_search.enabled;
        apply_tui_selections_to_config(&app, &mut config);
        // Should not change web_search settings
        assert_eq!(config.web_search.enabled, old_enabled);
    }

    // ── Skills persistence ──────────────────────────────────────────

    #[test]
    fn save_skills_enabled() {
        let mut app = test_app();
        app.skills_idx = 1; // First real skill (1password)
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.skills.open_skills_enabled);
    }

    #[test]
    fn save_skills_skip() {
        let app = test_app(); // skills_idx 0 = "Skip for now"
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(!config.skills.open_skills_enabled);
    }

    // ── Hooks persistence ───────────────────────────────────────────

    #[test]
    fn save_hooks_enabled() {
        let mut app = test_app();
        app.hooks_idx = 0; // Enable hooks
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.hooks.enabled);
        assert!(config.hooks.builtin.command_logger);
    }

    #[test]
    fn save_hooks_disabled() {
        let mut app = test_app();
        app.hooks_idx = 1; // Skip for now
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(!config.hooks.enabled);
    }

    // ── Gateway persistence ─────────────────────────────────────────

    #[test]
    fn save_gateway_port_and_host() {
        let mut app = test_app();
        app.gateway_port = 9999;
        app.gateway_host = "0.0.0.0".to_string();
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.gateway.port, 9999);
        assert_eq!(config.gateway.host, "0.0.0.0");
    }

    #[test]
    fn save_gateway_default_values() {
        let app = test_app();
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert_eq!(config.gateway.port, 42617);
        assert_eq!(config.gateway.host, "127.0.0.1");
    }

    // ── Pairing persistence ─────────────────────────────────────────

    #[test]
    fn save_pairing_required() {
        let mut app = test_app();
        app.pairing_required = true;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(config.gateway.require_pairing);
    }

    #[test]
    fn save_pairing_not_required() {
        let mut app = test_app();
        app.pairing_required = false;
        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);
        assert!(!config.gateway.require_pairing);
    }

    // ── End-to-end: full wizard flow ────────────────────────────────

    #[test]
    fn e2e_full_setup_anthropic_telegram_brave() {
        let mut app = test_app();
        // Provider: Anthropic (tier 0, idx 2)
        app.provider_tier_idx = 0;
        app.provider_idx = 2;
        app.api_key_input = "sk-ant-api-key".to_string();
        // Model: Claude Opus
        app.model_idx = 2; // claude-opus-4-20250514
        // Channel: Telegram
        app.channel_idx = 0;
        // Web search: Brave
        app.search_provider_idx = 0;
        app.search_api_key_input = "brave-key-123".to_string();
        // Skills: obsidian (idx 12)
        app.skills_idx = 12;
        // Hooks: enabled
        app.hooks_idx = 0;
        // Gateway
        app.gateway_port = 8080;
        app.gateway_host = "192.168.1.100".to_string();
        app.pairing_required = true;

        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);

        // Verify everything was persisted
        assert_eq!(config.providers.fallback.as_deref(), Some("anthropic"));
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref()),
            Some("sk-ant-api-key")
        );
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("claude-opus-4-20250514")
        );
        assert!(config.channels.telegram.is_some());
        assert!(config.web_search.enabled);
        assert_eq!(config.web_search.provider, "brave");
        assert_eq!(
            config.web_search.brave_api_key.as_deref(),
            Some("brave-key-123")
        );
        assert!(config.skills.open_skills_enabled);
        assert!(config.hooks.enabled);
        assert!(config.hooks.builtin.command_logger);
        assert_eq!(config.gateway.port, 8080);
        assert_eq!(config.gateway.host, "192.168.1.100");
        assert!(config.gateway.require_pairing);
    }

    #[test]
    fn e2e_minimal_setup_ollama_skip_everything() {
        let mut app = test_app();
        // Provider: Ollama (tier 4, idx 0)
        app.provider_tier_idx = 4;
        app.provider_idx = 0;
        // No API key needed for local
        app.api_key_input = String::new();
        // Model: Auto
        app.model_idx = 0;
        // Channel: Skip
        let skip_idx = CHANNELS.iter().position(|c| c.0 == "Skip for now").unwrap();
        app.channel_idx = skip_idx;
        // Web search: Skip
        app.search_provider_idx = 5;
        // Skills: Skip
        app.skills_idx = 0;
        // Hooks: Skip
        app.hooks_idx = 1;
        // Pairing: not required
        app.pairing_required = false;

        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);

        assert_eq!(config.providers.fallback.as_deref(), Some("ollama"));
        assert!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.api_key.as_deref())
                .is_none()
        );
        assert!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref())
                .is_none()
        );
        assert!(config.channels.telegram.is_none());
        assert!(config.channels.discord.is_none());
        assert!(!config.skills.open_skills_enabled);
        assert!(!config.hooks.enabled);
        assert!(!config.gateway.require_pairing);
    }

    #[test]
    fn e2e_discord_searxng_with_hooks() {
        let mut app = test_app();
        // Provider: OpenAI (tier 0, idx 3)
        app.provider_tier_idx = 0;
        app.provider_idx = 3;
        app.api_key_input = "sk-openai-key".to_string();
        // Model: gpt-4o
        app.model_idx = 3;
        // Channel: Discord (idx 2)
        app.channel_idx = 2;
        // Web search: SearxNG (idx 1) with instance URL
        app.search_provider_idx = 1;
        app.search_api_key_input = "https://search.local".to_string();
        // Skills: Skip
        app.skills_idx = 0;
        // Hooks: enabled
        app.hooks_idx = 0;
        app.gateway_host = "0.0.0.0".to_string();

        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);

        assert_eq!(config.providers.fallback.as_deref(), Some("openai"));
        assert_eq!(
            config
                .providers
                .fallback_provider()
                .and_then(|e| e.model.as_deref()),
            Some("gpt-4o")
        );
        let dc = config.channels.discord.as_ref().unwrap();
        assert_eq!(dc.bot_token, "YOUR_DISCORD_BOT_TOKEN");
        assert_eq!(config.web_search.provider, "searxng");
        assert_eq!(
            config.web_search.searxng_instance_url.as_deref(),
            Some("https://search.local")
        );
        assert!(config.hooks.enabled);
        assert_eq!(config.gateway.host, "0.0.0.0");
    }

    #[test]
    fn provider_select_skips_api_key_for_openai_codex() {
        let mut app = test_app();
        app.screen = Screen::ProviderSelect;
        app.provider_tier_idx = 0;
        app.provider_idx = 4; // OpenAI Codex

        handle_input(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::ProviderNotes);
    }

    #[test]
    fn provider_select_skips_api_key_for_ollama_local() {
        let mut app = test_app();
        app.screen = Screen::ProviderSelect;
        app.provider_tier_idx = 4;
        app.provider_idx = 0; // Ollama

        handle_input(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::ProviderNotes);
    }

    #[test]
    fn api_key_screen_allows_empty_enter_to_continue() {
        let mut app = test_app();
        app.screen = Screen::ApiKeyInput;
        app.api_key_input.clear();

        handle_input(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::ProviderNotes);
    }

    // ── TOML round-trip: verify serialization ───────────────────────

    #[test]
    fn config_serializes_to_valid_toml() {
        let mut app = test_app();
        app.provider_tier_idx = 0;
        app.provider_idx = 0;
        app.channel_idx = 0; // Telegram
        app.hooks_idx = 0;
        app.search_provider_idx = 0;
        app.search_api_key_input = "brave-key".to_string();

        let mut config = Config::default();
        apply_tui_selections_to_config(&app, &mut config);

        // Serialize to TOML and parse back
        let toml_str = toml::to_string(&config).expect("config should serialize to TOML");
        assert!(toml_str.contains("YOUR_TELEGRAM_BOT_TOKEN"));
        assert!(toml_str.contains("openrouter"));

        // Verify it parses back
        let _: Config = toml::from_str::<zeroclaw_config::migration::V1Compat>(&toml_str)
            .expect("serialized TOML should parse back")
            .into_config();
    }

    #[test]
    fn config_with_all_channels_serializes() {
        // Test that every channel stub serializes cleanly
        let channels_to_test = [
            "Telegram",
            "WhatsApp",
            "Discord",
            "IRC",
            "Slack",
            "Signal",
            "iMessage",
            "Mattermost",
            "Nextcloud Talk",
            "Feishu/Lark",
        ];
        for channel_name in &channels_to_test {
            let mut app = test_app();
            let idx = CHANNELS
                .iter()
                .position(|c| c.0 == *channel_name)
                .unwrap_or_else(|| panic!("channel {channel_name} not found in CHANNELS"));
            app.channel_idx = idx;

            let mut config = Config::default();
            apply_tui_selections_to_config(&app, &mut config);

            let toml_str = toml::to_string(&config)
                .unwrap_or_else(|e| panic!("failed to serialize config for {channel_name}: {e}"));
            let _: Config = toml::from_str::<zeroclaw_config::migration::V1Compat>(&toml_str)
                .unwrap_or_else(|e| panic!("failed to deserialize config for {channel_name}: {e}"))
                .into_config();
        }
    }
}
