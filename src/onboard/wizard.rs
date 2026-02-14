use crate::config::{
    AutonomyConfig, BrowserConfig, ChannelsConfig, ComposioConfig, Config, DiscordConfig,
    HeartbeatConfig, IMessageConfig, MatrixConfig, MemoryConfig, ObservabilityConfig,
    RuntimeConfig, SecretsConfig, SlackConfig, TelegramConfig, WebhookConfig,
};
use anyhow::{Context, Result};
use console::style;
use dialoguer::{Confirm, Input, Select};
use std::fs;
use std::path::{Path, PathBuf};

// ── Project context collected during wizard ──────────────────────

/// User-provided personalization baked into workspace MD files.
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    pub user_name: String,
    pub timezone: String,
    pub agent_name: String,
    pub communication_style: String,
}

// ── Banner ───────────────────────────────────────────────────────

const BANNER: &str = r"
    ⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡

    ███████╗███████╗██████╗  ██████╗  ██████╗██╗      █████╗ ██╗    ██╗
    ╚══███╔╝██╔════╝██╔══██╗██╔═══██╗██╔════╝██║     ██╔══██╗██║    ██║
      ███╔╝ █████╗  ██████╔╝██║   ██║██║     ██║     ███████║██║ █╗ ██║
     ███╔╝  ██╔══╝  ██╔══██╗██║   ██║██║     ██║     ██╔══██║██║███╗██║
    ███████╗███████╗██║  ██║╚██████╔╝╚██████╗███████╗██║  ██║╚███╔███╔╝
    ╚══════╝╚══════╝╚═╝  ╚═╝ ╚═════╝  ╚═════╝╚══════╝╚═╝  ╚═╝ ╚══╝╚══╝

    Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.

    ⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡⚡
";

// ── Main wizard entry point ──────────────────────────────────────

pub fn run_wizard() -> Result<Config> {
    println!("{}", style(BANNER).cyan().bold());

    println!(
        "  {}",
        style("Welcome to ZeroClaw — the fastest, smallest AI assistant.")
            .white()
            .bold()
    );
    println!(
        "  {}",
        style("This wizard will configure your agent in under 60 seconds.").dim()
    );
    println!();

    print_step(1, 7, "Workspace Setup");
    let (workspace_dir, config_path) = setup_workspace()?;

    print_step(2, 7, "AI Provider & API Key");
    let (provider, api_key, model) = setup_provider()?;

    print_step(3, 7, "Channels (How You Talk to ZeroClaw)");
    let channels_config = setup_channels()?;

    print_step(4, 7, "Tunnel (Expose to Internet)");
    let tunnel_config = setup_tunnel()?;

    print_step(5, 7, "Tool Mode & Security");
    let (composio_config, secrets_config) = setup_tool_mode()?;

    print_step(6, 7, "Project Context (Personalize Your Agent)");
    let project_ctx = setup_project_context()?;

    print_step(7, 7, "Workspace Files");
    scaffold_workspace(&workspace_dir, &project_ctx)?;

    // ── Build config ──
    // Defaults: SQLite memory, supervised autonomy, workspace-scoped, native runtime
    let config = Config {
        workspace_dir: workspace_dir.clone(),
        config_path: config_path.clone(),
        api_key: if api_key.is_empty() {
            None
        } else {
            Some(api_key)
        },
        default_provider: Some(provider),
        default_model: Some(model),
        default_temperature: 0.7,
        observability: ObservabilityConfig::default(),
        autonomy: AutonomyConfig::default(),
        runtime: RuntimeConfig::default(),
        heartbeat: HeartbeatConfig::default(),
        channels_config,
        memory: MemoryConfig::default(), // SQLite + auto-save by default
        tunnel: tunnel_config,
        gateway: crate::config::GatewayConfig::default(),
        composio: composio_config,
        secrets: secrets_config,
        browser: BrowserConfig::default(),
    };

    println!(
        "  {} Security: {} | workspace-scoped",
        style("✓").green().bold(),
        style("Supervised").green()
    );
    println!(
        "  {} Memory: {} (auto-save: on)",
        style("✓").green().bold(),
        style("sqlite").green()
    );

    config.save()?;

    // ── Final summary ────────────────────────────────────────────
    print_summary(&config);

    // ── Offer to launch channels immediately ─────────────────────
    let has_channels = config.channels_config.telegram.is_some()
        || config.channels_config.discord.is_some()
        || config.channels_config.slack.is_some()
        || config.channels_config.imessage.is_some()
        || config.channels_config.matrix.is_some();

    if has_channels && config.api_key.is_some() {
        let launch: bool = Confirm::new()
            .with_prompt(format!(
                "  {} Launch channels now? (connected channels → AI → reply)",
                style("🚀").cyan()
            ))
            .default(true)
            .interact()?;

        if launch {
            println!();
            println!(
                "  {} {}",
                style("⚡").cyan(),
                style("Starting channel server...").white().bold()
            );
            println!();
            // Signal to main.rs to call start_channels after wizard returns
            std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1");
        }
    }

    Ok(config)
}

// ── Quick setup (zero prompts) ───────────────────────────────────

/// Non-interactive setup: generates a sensible default config instantly.
/// Use `zeroclaw onboard` or `zeroclaw onboard --api-key sk-... --provider openrouter`.
/// Use `zeroclaw onboard --interactive` for the full wizard.
#[allow(clippy::too_many_lines)]
pub fn run_quick_setup(api_key: Option<&str>, provider: Option<&str>) -> Result<Config> {
    println!("{}", style(BANNER).cyan().bold());
    println!(
        "  {}",
        style("Quick Setup — generating config with sensible defaults...")
            .white()
            .bold()
    );
    println!();

    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    let zeroclaw_dir = home.join(".zeroclaw");
    let workspace_dir = zeroclaw_dir.join("workspace");
    let config_path = zeroclaw_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).context("Failed to create workspace directory")?;

    let provider_name = provider.unwrap_or("openrouter").to_string();
    let model = default_model_for_provider(&provider_name);

    let config = Config {
        workspace_dir: workspace_dir.clone(),
        config_path: config_path.clone(),
        api_key: api_key.map(String::from),
        default_provider: Some(provider_name.clone()),
        default_model: Some(model.clone()),
        default_temperature: 0.7,
        observability: ObservabilityConfig::default(),
        autonomy: AutonomyConfig::default(),
        runtime: RuntimeConfig::default(),
        heartbeat: HeartbeatConfig::default(),
        channels_config: ChannelsConfig::default(),
        memory: MemoryConfig::default(),
        tunnel: crate::config::TunnelConfig::default(),
        gateway: crate::config::GatewayConfig::default(),
        composio: ComposioConfig::default(),
        secrets: SecretsConfig::default(),
        browser: BrowserConfig::default(),
    };

    config.save()?;

    // Scaffold minimal workspace files
    let default_ctx = ProjectContext {
        user_name: std::env::var("USER").unwrap_or_else(|_| "User".into()),
        timezone: "UTC".into(),
        agent_name: "ZeroClaw".into(),
        communication_style: "Direct and concise".into(),
    };
    scaffold_workspace(&workspace_dir, &default_ctx)?;

    println!(
        "  {} Workspace:  {}",
        style("✓").green().bold(),
        style(workspace_dir.display()).green()
    );
    println!(
        "  {} Provider:   {}",
        style("✓").green().bold(),
        style(&provider_name).green()
    );
    println!(
        "  {} Model:      {}",
        style("✓").green().bold(),
        style(&model).green()
    );
    println!(
        "  {} API Key:    {}",
        style("✓").green().bold(),
        if api_key.is_some() {
            style("set").green()
        } else {
            style("not set (use --api-key or edit config.toml)").yellow()
        }
    );
    println!(
        "  {} Security:   {}",
        style("✓").green().bold(),
        style("Supervised (workspace-scoped)").green()
    );
    println!(
        "  {} Memory:     {}",
        style("✓").green().bold(),
        style("sqlite (auto-save)").green()
    );
    println!(
        "  {} Secrets:    {}",
        style("✓").green().bold(),
        style("encrypted").green()
    );
    println!(
        "  {} Gateway:    {}",
        style("✓").green().bold(),
        style("pairing required (127.0.0.1:8080)").green()
    );
    println!(
        "  {} Tunnel:     {}",
        style("✓").green().bold(),
        style("none (local only)").dim()
    );
    println!(
        "  {} Composio:   {}",
        style("✓").green().bold(),
        style("disabled (sovereign mode)").dim()
    );
    println!();
    println!(
        "  {} {}",
        style("Config saved:").white().bold(),
        style(config_path.display()).green()
    );
    println!();
    println!("  {}", style("Next steps:").white().bold());
    if api_key.is_none() {
        println!("    1. Set your API key:  export OPENROUTER_API_KEY=\"sk-...\"");
        println!("    2. Or edit:           ~/.zeroclaw/config.toml");
        println!("    3. Chat:              zeroclaw agent -m \"Hello!\"");
        println!("    4. Gateway:           zeroclaw gateway");
    } else {
        println!("    1. Chat:     zeroclaw agent -m \"Hello!\"");
        println!("    2. Gateway:  zeroclaw gateway");
        println!("    3. Status:   zeroclaw status");
    }
    println!();

    Ok(config)
}

/// Pick a sensible default model for the given provider.
fn default_model_for_provider(provider: &str) -> String {
    match provider {
        "anthropic" => "claude-sonnet-4-20250514".into(),
        "openai" => "gpt-4o".into(),
        "ollama" => "llama3.2".into(),
        "groq" => "llama-3.3-70b-versatile".into(),
        "deepseek" => "deepseek-chat".into(),
        "gemini" | "google" | "google-gemini" => "gemini-2.0-flash".into(),
        _ => "anthropic/claude-sonnet-4-20250514".into(),
    }
}

// ── Step helpers ─────────────────────────────────────────────────

fn print_step(current: u8, total: u8, title: &str) {
    println!();
    println!(
        "  {} {}",
        style(format!("[{current}/{total}]")).cyan().bold(),
        style(title).white().bold()
    );
    println!("  {}", style("─".repeat(50)).dim());
}

fn print_bullet(text: &str) {
    println!("  {} {}", style("›").cyan(), text);
}

// ── Step 1: Workspace ────────────────────────────────────────────

fn setup_workspace() -> Result<(PathBuf, PathBuf)> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    let default_dir = home.join(".zeroclaw");

    print_bullet(&format!(
        "Default location: {}",
        style(default_dir.display()).green()
    ));

    let use_default = Confirm::new()
        .with_prompt("  Use default workspace location?")
        .default(true)
        .interact()?;

    let zeroclaw_dir = if use_default {
        default_dir
    } else {
        let custom: String = Input::new()
            .with_prompt("  Enter workspace path")
            .interact_text()?;
        let expanded = shellexpand::tilde(&custom).to_string();
        PathBuf::from(expanded)
    };

    let workspace_dir = zeroclaw_dir.join("workspace");
    let config_path = zeroclaw_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir).context("Failed to create workspace directory")?;

    println!(
        "  {} Workspace: {}",
        style("✓").green().bold(),
        style(workspace_dir.display()).green()
    );

    Ok((workspace_dir, config_path))
}

// ── Step 2: Provider & API Key ───────────────────────────────────

#[allow(clippy::too_many_lines)]
fn setup_provider() -> Result<(String, String, String)> {
    // ── Tier selection ──
    let tiers = vec![
        "⭐ Recommended (OpenRouter, Venice, Anthropic, OpenAI, Gemini)",
        "⚡ Fast inference (Groq, Fireworks, Together AI)",
        "🌐 Gateway / proxy (Vercel AI, Cloudflare AI, Amazon Bedrock)",
        "🔬 Specialized (Moonshot/Kimi, GLM/Zhipu, MiniMax, Qianfan, Z.AI, Synthetic, OpenCode Zen, Cohere)",
        "🏠 Local / private (Ollama — no API key needed)",
        "🔧 Custom — bring your own OpenAI-compatible API",
    ];

    let tier_idx = Select::new()
        .with_prompt("  Select provider category")
        .items(&tiers)
        .default(0)
        .interact()?;

    let providers: Vec<(&str, &str)> = match tier_idx {
        0 => vec![
            (
                "openrouter",
                "OpenRouter — 200+ models, 1 API key (recommended)",
            ),
            ("venice", "Venice AI — privacy-first (Llama, Opus)"),
            ("anthropic", "Anthropic — Claude Sonnet & Opus (direct)"),
            ("openai", "OpenAI — GPT-4o, o1, GPT-5 (direct)"),
            ("deepseek", "DeepSeek — V3 & R1 (affordable)"),
            ("mistral", "Mistral — Large & Codestral"),
            ("xai", "xAI — Grok 3 & 4"),
            ("perplexity", "Perplexity — search-augmented AI"),
            ("gemini", "Google Gemini — Gemini 2.0 Flash & Pro (supports CLI auth)"),
        ],
        1 => vec![
            ("groq", "Groq — ultra-fast LPU inference"),
            ("fireworks", "Fireworks AI — fast open-source inference"),
            ("together", "Together AI — open-source model hosting"),
        ],
        2 => vec![
            ("vercel", "Vercel AI Gateway"),
            ("cloudflare", "Cloudflare AI Gateway"),
            ("bedrock", "Amazon Bedrock — AWS managed models"),
        ],
        3 => vec![
            ("moonshot", "Moonshot — Kimi & Kimi Coding"),
            ("glm", "GLM — ChatGLM / Zhipu models"),
            ("minimax", "MiniMax — MiniMax AI models"),
            ("qianfan", "Qianfan — Baidu AI models"),
            ("zai", "Z.AI — Z.AI inference"),
            ("synthetic", "Synthetic — Synthetic AI models"),
            ("opencode", "OpenCode Zen — code-focused AI"),
            ("cohere", "Cohere — Command R+ & embeddings"),
        ],
        4 => vec![("ollama", "Ollama — local models (Llama, Mistral, Phi)")],
        _ => vec![], // Custom — handled below
    };

    // ── Custom / BYOP flow ──
    if providers.is_empty() {
        println!();
        println!(
            "  {} {}",
            style("Custom Provider Setup").white().bold(),
            style("— any OpenAI-compatible API").dim()
        );
        print_bullet("ZeroClaw works with ANY API that speaks the OpenAI chat completions format.");
        print_bullet("Examples: LiteLLM, LocalAI, vLLM, text-generation-webui, LM Studio, etc.");
        println!();

        let base_url: String = Input::new()
            .with_prompt("  API base URL (e.g. http://localhost:1234 or https://my-api.com)")
            .interact_text()?;

        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            anyhow::bail!("Custom provider requires a base URL.");
        }

        let api_key: String = Input::new()
            .with_prompt("  API key (or Enter to skip if not needed)")
            .allow_empty(true)
            .interact_text()?;

        let model: String = Input::new()
            .with_prompt("  Model name (e.g. llama3, gpt-4o, mistral)")
            .default("default".into())
            .interact_text()?;

        let provider_name = format!("custom:{base_url}");

        println!(
            "  {} Provider: {} | Model: {}",
            style("✓").green().bold(),
            style(&provider_name).green(),
            style(&model).green()
        );

        return Ok((provider_name, api_key, model));
    }

    let provider_labels: Vec<&str> = providers.iter().map(|(_, label)| *label).collect();

    let provider_idx = Select::new()
        .with_prompt("  Select your AI provider")
        .items(&provider_labels)
        .default(0)
        .interact()?;

    let provider_name = providers[provider_idx].0;

    // ── API key ──
    let api_key = if provider_name == "ollama" {
        print_bullet("Ollama runs locally — no API key needed!");
        String::new()
    } else if provider_name == "gemini" || provider_name == "google" || provider_name == "google-gemini" {
        // Special handling for Gemini: check for CLI auth first
        if crate::providers::gemini::GeminiProvider::has_cli_credentials() {
            print_bullet(&format!(
                "{} Gemini CLI credentials detected! You can skip the API key.",
                style("✓").green().bold()
            ));
            print_bullet("ZeroClaw will reuse your existing Gemini CLI authentication.");
            println!();

            let use_cli: bool = dialoguer::Confirm::new()
                .with_prompt("  Use existing Gemini CLI authentication?")
                .default(true)
                .interact()?;

            if use_cli {
                println!(
                    "  {} Using Gemini CLI OAuth tokens",
                    style("✓").green().bold()
                );
                String::new() // Empty key = will use CLI tokens
            } else {
                print_bullet("Get your API key at: https://aistudio.google.com/app/apikey");
                Input::new()
                    .with_prompt("  Paste your Gemini API key")
                    .allow_empty(true)
                    .interact_text()?
            }
        } else if std::env::var("GEMINI_API_KEY").is_ok() {
            print_bullet(&format!(
                "{} GEMINI_API_KEY environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else {
            print_bullet("Get your API key at: https://aistudio.google.com/app/apikey");
            print_bullet("Or run `gemini` CLI to authenticate (tokens will be reused).");
            println!();

            Input::new()
                .with_prompt("  Paste your Gemini API key (or press Enter to skip)")
                .allow_empty(true)
                .interact_text()?
        }
    } else {
        let key_url = match provider_name {
            "openrouter" => "https://openrouter.ai/keys",
            "anthropic" => "https://console.anthropic.com/settings/keys",
            "openai" => "https://platform.openai.com/api-keys",
            "venice" => "https://venice.ai/settings/api",
            "groq" => "https://console.groq.com/keys",
            "mistral" => "https://console.mistral.ai/api-keys",
            "deepseek" => "https://platform.deepseek.com/api_keys",
            "together" => "https://api.together.xyz/settings/api-keys",
            "fireworks" => "https://fireworks.ai/account/api-keys",
            "perplexity" => "https://www.perplexity.ai/settings/api",
            "xai" => "https://console.x.ai",
            "cohere" => "https://dashboard.cohere.com/api-keys",
            "moonshot" => "https://platform.moonshot.cn/console/api-keys",
            "minimax" => "https://www.minimaxi.com/user-center/basic-information",
            "vercel" => "https://vercel.com/account/tokens",
            "cloudflare" => "https://dash.cloudflare.com/profile/api-tokens",
            "bedrock" => "https://console.aws.amazon.com/iam",
            "gemini" | "google" | "google-gemini" => "https://aistudio.google.com/app/apikey",
            _ => "",
        };

        println!();
        if !key_url.is_empty() {
            print_bullet(&format!(
                "Get your API key at: {}",
                style(key_url).cyan().underlined()
            ));
        }
        print_bullet("You can also set it later via env var or config file.");
        println!();

        let key: String = Input::new()
            .with_prompt("  Paste your API key (or press Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.is_empty() {
            let env_var = provider_env_var(provider_name);
            print_bullet(&format!(
                "Skipped. Set {} or edit config.toml later.",
                style(env_var).yellow()
            ));
        }

        key
    };

    // ── Model selection ──
    let models: Vec<(&str, &str)> = match provider_name {
        "openrouter" => vec![
            (
                "anthropic/claude-sonnet-4-20250514",
                "Claude Sonnet 4 (balanced, recommended)",
            ),
            (
                "anthropic/claude-3.5-sonnet",
                "Claude 3.5 Sonnet (fast, affordable)",
            ),
            ("openai/gpt-4o", "GPT-4o (OpenAI flagship)"),
            ("openai/gpt-4o-mini", "GPT-4o Mini (fast, cheap)"),
            (
                "google/gemini-2.0-flash-001",
                "Gemini 2.0 Flash (Google, fast)",
            ),
            (
                "meta-llama/llama-3.3-70b-instruct",
                "Llama 3.3 70B (open source)",
            ),
            ("deepseek/deepseek-chat", "DeepSeek Chat (affordable)"),
        ],
        "anthropic" => vec![
            (
                "claude-sonnet-4-20250514",
                "Claude Sonnet 4 (balanced, recommended)",
            ),
            ("claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet (fast)"),
            (
                "claude-3-5-haiku-20241022",
                "Claude 3.5 Haiku (fastest, cheapest)",
            ),
        ],
        "openai" => vec![
            ("gpt-4o", "GPT-4o (flagship)"),
            ("gpt-4o-mini", "GPT-4o Mini (fast, cheap)"),
            ("o1-mini", "o1-mini (reasoning)"),
        ],
        "venice" => vec![
            ("llama-3.3-70b", "Llama 3.3 70B (default, fast)"),
            ("claude-opus-45", "Claude Opus 4.5 via Venice (strongest)"),
            ("llama-3.1-405b", "Llama 3.1 405B (largest open source)"),
        ],
        "groq" => vec![
            (
                "llama-3.3-70b-versatile",
                "Llama 3.3 70B (fast, recommended)",
            ),
            ("llama-3.1-8b-instant", "Llama 3.1 8B (instant)"),
            ("mixtral-8x7b-32768", "Mixtral 8x7B (32K context)"),
        ],
        "mistral" => vec![
            ("mistral-large-latest", "Mistral Large (flagship)"),
            ("codestral-latest", "Codestral (code-focused)"),
            ("mistral-small-latest", "Mistral Small (fast, cheap)"),
        ],
        "deepseek" => vec![
            ("deepseek-chat", "DeepSeek Chat (V3, recommended)"),
            ("deepseek-reasoner", "DeepSeek Reasoner (R1)"),
        ],
        "xai" => vec![
            ("grok-3", "Grok 3 (flagship)"),
            ("grok-3-mini", "Grok 3 Mini (fast)"),
        ],
        "perplexity" => vec![
            ("sonar-pro", "Sonar Pro (search + reasoning)"),
            ("sonar", "Sonar (search, fast)"),
        ],
        "fireworks" => vec![
            (
                "accounts/fireworks/models/llama-v3p3-70b-instruct",
                "Llama 3.3 70B",
            ),
            (
                "accounts/fireworks/models/mixtral-8x22b-instruct",
                "Mixtral 8x22B",
            ),
        ],
        "together" => vec![
            (
                "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
                "Llama 3.1 70B Turbo",
            ),
            (
                "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
                "Llama 3.1 8B Turbo",
            ),
            ("mistralai/Mixtral-8x22B-Instruct-v0.1", "Mixtral 8x22B"),
        ],
        "cohere" => vec![
            ("command-r-plus", "Command R+ (flagship)"),
            ("command-r", "Command R (fast)"),
        ],
        "moonshot" => vec![
            ("moonshot-v1-128k", "Moonshot V1 128K"),
            ("moonshot-v1-32k", "Moonshot V1 32K"),
        ],
        "glm" => vec![
            ("glm-4-plus", "GLM-4 Plus (flagship)"),
            ("glm-4-flash", "GLM-4 Flash (fast)"),
        ],
        "minimax" => vec![
            ("abab6.5s-chat", "ABAB 6.5s Chat"),
            ("abab6.5-chat", "ABAB 6.5 Chat"),
        ],
        "ollama" => vec![
            ("llama3.2", "Llama 3.2 (recommended local)"),
            ("mistral", "Mistral 7B"),
            ("codellama", "Code Llama"),
            ("phi3", "Phi-3 (small, fast)"),
        ],
        "gemini" | "google" | "google-gemini" => vec![
            ("gemini-2.0-flash", "Gemini 2.0 Flash (fast, recommended)"),
            ("gemini-2.0-flash-lite", "Gemini 2.0 Flash Lite (fastest, cheapest)"),
            ("gemini-1.5-pro", "Gemini 1.5 Pro (best quality)"),
            ("gemini-1.5-flash", "Gemini 1.5 Flash (balanced)"),
        ],
        _ => vec![("default", "Default model")],
    };

    let model_labels: Vec<&str> = models.iter().map(|(_, label)| *label).collect();

    let model_idx = Select::new()
        .with_prompt("  Select your default model")
        .items(&model_labels)
        .default(0)
        .interact()?;

    let model = models[model_idx].0.to_string();

    println!(
        "  {} Provider: {} | Model: {}",
        style("✓").green().bold(),
        style(provider_name).green(),
        style(&model).green()
    );

    Ok((provider_name.to_string(), api_key, model))
}

/// Map provider name to its conventional env var
fn provider_env_var(name: &str) -> &'static str {
    match name {
        "openrouter" => "OPENROUTER_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "venice" => "VENICE_API_KEY",
        "groq" => "GROQ_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "xai" | "grok" => "XAI_API_KEY",
        "together" | "together-ai" => "TOGETHER_API_KEY",
        "fireworks" | "fireworks-ai" => "FIREWORKS_API_KEY",
        "perplexity" => "PERPLEXITY_API_KEY",
        "cohere" => "COHERE_API_KEY",
        "moonshot" | "kimi" => "MOONSHOT_API_KEY",
        "glm" | "zhipu" => "GLM_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "qianfan" | "baidu" => "QIANFAN_API_KEY",
        "zai" | "z.ai" => "ZAI_API_KEY",
        "synthetic" => "SYNTHETIC_API_KEY",
        "opencode" | "opencode-zen" => "OPENCODE_API_KEY",
        "vercel" | "vercel-ai" => "VERCEL_API_KEY",
        "cloudflare" | "cloudflare-ai" => "CLOUDFLARE_API_KEY",
        "bedrock" | "aws-bedrock" => "AWS_ACCESS_KEY_ID",
        "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
        _ => "API_KEY",
    }
}

// ── Step 5: Tool Mode & Security ────────────────────────────────

fn setup_tool_mode() -> Result<(ComposioConfig, SecretsConfig)> {
    print_bullet("Choose how ZeroClaw connects to external apps.");
    print_bullet("You can always change this later in config.toml.");
    println!();

    let options = vec![
        "Sovereign (local only) — you manage API keys, full privacy (default)",
        "Composio (managed OAuth) — 1000+ apps via OAuth, no raw keys shared",
    ];

    let choice = Select::new()
        .with_prompt("  Select tool mode")
        .items(&options)
        .default(0)
        .interact()?;

    let composio_config = if choice == 1 {
        println!();
        println!(
            "  {} {}",
            style("Composio Setup").white().bold(),
            style("— 1000+ OAuth integrations (Gmail, Notion, GitHub, Slack, ...)").dim()
        );
        print_bullet("Get your API key at: https://app.composio.dev/settings");
        print_bullet("ZeroClaw uses Composio as a tool — your core agent stays local.");
        println!();

        let api_key: String = Input::new()
            .with_prompt("  Composio API key (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if api_key.trim().is_empty() {
            println!(
                "  {} Skipped — set composio.api_key in config.toml later",
                style("→").dim()
            );
            ComposioConfig::default()
        } else {
            println!(
                "  {} Composio: {} (1000+ OAuth tools available)",
                style("✓").green().bold(),
                style("enabled").green()
            );
            ComposioConfig {
                enabled: true,
                api_key: Some(api_key),
                ..ComposioConfig::default()
            }
        }
    } else {
        println!(
            "  {} Tool mode: {} — full privacy, you own every key",
            style("✓").green().bold(),
            style("Sovereign (local only)").green()
        );
        ComposioConfig::default()
    };

    // ── Encrypted secrets ──
    println!();
    print_bullet("ZeroClaw can encrypt API keys stored in config.toml.");
    print_bullet("A local key file protects against plaintext exposure and accidental leaks.");

    let encrypt = Confirm::new()
        .with_prompt("  Enable encrypted secret storage?")
        .default(true)
        .interact()?;

    let secrets_config = SecretsConfig { encrypt };

    if encrypt {
        println!(
            "  {} Secrets: {} — keys encrypted with local key file",
            style("✓").green().bold(),
            style("encrypted").green()
        );
    } else {
        println!(
            "  {} Secrets: {} — keys stored as plaintext (not recommended)",
            style("✓").green().bold(),
            style("plaintext").yellow()
        );
    }

    Ok((composio_config, secrets_config))
}

// ── Step 6: Project Context ─────────────────────────────────────

fn setup_project_context() -> Result<ProjectContext> {
    print_bullet("Let's personalize your agent. You can always update these later.");
    print_bullet("Press Enter to accept defaults.");
    println!();

    let user_name: String = Input::new()
        .with_prompt("  Your name")
        .default("User".into())
        .interact_text()?;

    let tz_options = vec![
        "US/Eastern (EST/EDT)",
        "US/Central (CST/CDT)",
        "US/Mountain (MST/MDT)",
        "US/Pacific (PST/PDT)",
        "Europe/London (GMT/BST)",
        "Europe/Berlin (CET/CEST)",
        "Asia/Tokyo (JST)",
        "UTC",
        "Other (type manually)",
    ];

    let tz_idx = Select::new()
        .with_prompt("  Your timezone")
        .items(&tz_options)
        .default(0)
        .interact()?;

    let timezone = if tz_idx == tz_options.len() - 1 {
        Input::new()
            .with_prompt("  Enter timezone (e.g. America/New_York)")
            .default("UTC".into())
            .interact_text()?
    } else {
        // Extract the short label before the parenthetical
        tz_options[tz_idx]
            .split('(')
            .next()
            .unwrap_or("UTC")
            .trim()
            .to_string()
    };

    let agent_name: String = Input::new()
        .with_prompt("  Agent name")
        .default("ZeroClaw".into())
        .interact_text()?;

    let style_options = vec![
        "Direct & concise — skip pleasantries, get to the point",
        "Friendly & casual — warm but efficient",
        "Technical & detailed — thorough explanations, code-first",
        "Balanced — adapt to the situation",
    ];

    let style_idx = Select::new()
        .with_prompt("  Communication style")
        .items(&style_options)
        .default(0)
        .interact()?;

    let communication_style = match style_idx {
        0 => "Be direct and concise. Skip pleasantries. Get to the point.".to_string(),
        1 => "Be friendly and casual. Warm but efficient.".to_string(),
        2 => "Be technical and detailed. Thorough explanations, code-first.".to_string(),
        _ => {
            "Adapt to the situation. Be concise when needed, thorough when it matters.".to_string()
        }
    };

    println!(
        "  {} Context: {} | {} | {} | {}",
        style("✓").green().bold(),
        style(&user_name).green(),
        style(&timezone).green(),
        style(&agent_name).green(),
        style(&communication_style).green().dim()
    );

    Ok(ProjectContext {
        user_name,
        timezone,
        agent_name,
        communication_style,
    })
}

// ── Step 3: Channels ────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn setup_channels() -> Result<ChannelsConfig> {
    print_bullet("Channels let you talk to ZeroClaw from anywhere.");
    print_bullet("CLI is always available. Connect more channels now.");
    println!();

    let mut config = ChannelsConfig {
        cli: true,
        telegram: None,
        discord: None,
        slack: None,
        webhook: None,
        imessage: None,
        matrix: None,
    };

    loop {
        let options = vec![
            format!(
                "Telegram   {}",
                if config.telegram.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "Discord    {}",
                if config.discord.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "Slack      {}",
                if config.slack.is_some() {
                    "✅ connected"
                } else {
                    "— connect your bot"
                }
            ),
            format!(
                "iMessage   {}",
                if config.imessage.is_some() {
                    "✅ configured"
                } else {
                    "— macOS only"
                }
            ),
            format!(
                "Matrix     {}",
                if config.matrix.is_some() {
                    "✅ connected"
                } else {
                    "— self-hosted chat"
                }
            ),
            format!(
                "Webhook    {}",
                if config.webhook.is_some() {
                    "✅ configured"
                } else {
                    "— HTTP endpoint"
                }
            ),
            "Done — finish setup".to_string(),
        ];

        let choice = Select::new()
            .with_prompt("  Connect a channel (or Done to continue)")
            .items(&options)
            .default(6)
            .interact()?;

        match choice {
            0 => {
                // ── Telegram ──
                println!();
                println!(
                    "  {} {}",
                    style("Telegram Setup").white().bold(),
                    style("— talk to ZeroClaw from Telegram").dim()
                );
                print_bullet("1. Open Telegram and message @BotFather");
                print_bullet("2. Send /newbot and follow the prompts");
                print_bullet("3. Copy the bot token and paste it below");
                println!();

                let token: String = Input::new()
                    .with_prompt("  Bot token (from @BotFather)")
                    .interact_text()?;

                if token.trim().is_empty() {
                    println!("  {} Skipped", style("→").dim());
                    continue;
                }

                // Test connection
                print!("  {} Testing connection... ", style("⏳").dim());
                let client = reqwest::blocking::Client::new();
                let url = format!("https://api.telegram.org/bot{token}/getMe");
                match client.get(&url).send() {
                    Ok(resp) if resp.status().is_success() => {
                        let data: serde_json::Value = resp.json().unwrap_or_default();
                        let bot_name = data
                            .get("result")
                            .and_then(|r| r.get("username"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        println!(
                            "\r  {} Connected as @{bot_name}        ",
                            style("✅").green().bold()
                        );
                    }
                    _ => {
                        println!(
                            "\r  {} Connection failed — check your token and try again",
                            style("❌").red().bold()
                        );
                        continue;
                    }
                }

                let users_str: String = Input::new()
                    .with_prompt("  Allowed usernames (comma-separated, or * for all)")
                    .default("*".into())
                    .interact_text()?;

                let allowed_users = if users_str.trim() == "*" {
                    vec!["*".into()]
                } else {
                    users_str.split(',').map(|s| s.trim().to_string()).collect()
                };

                config.telegram = Some(TelegramConfig {
                    bot_token: token,
                    allowed_users,
                });
            }
            1 => {
                // ── Discord ──
                println!();
                println!(
                    "  {} {}",
                    style("Discord Setup").white().bold(),
                    style("— talk to ZeroClaw from Discord").dim()
                );
                print_bullet("1. Go to https://discord.com/developers/applications");
                print_bullet("2. Create a New Application → Bot → Copy token");
                print_bullet("3. Enable MESSAGE CONTENT intent under Bot settings");
                print_bullet("4. Invite bot to your server with messages permission");
                println!();

                let token: String = Input::new().with_prompt("  Bot token").interact_text()?;

                if token.trim().is_empty() {
                    println!("  {} Skipped", style("→").dim());
                    continue;
                }

                // Test connection
                print!("  {} Testing connection... ", style("⏳").dim());
                let client = reqwest::blocking::Client::new();
                match client
                    .get("https://discord.com/api/v10/users/@me")
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                {
                    Ok(resp) if resp.status().is_success() => {
                        let data: serde_json::Value = resp.json().unwrap_or_default();
                        let bot_name = data
                            .get("username")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        println!(
                            "\r  {} Connected as {bot_name}        ",
                            style("✅").green().bold()
                        );
                    }
                    _ => {
                        println!(
                            "\r  {} Connection failed — check your token and try again",
                            style("❌").red().bold()
                        );
                        continue;
                    }
                }

                let guild: String = Input::new()
                    .with_prompt("  Server (guild) ID (optional, Enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;

                let allowed_users_str: String = Input::new()
                    .with_prompt(
                        "  Allowed Discord user IDs (comma-separated, '*' for all, Enter to deny all)",
                    )
                    .allow_empty(true)
                    .interact_text()?;

                let allowed_users = if allowed_users_str.trim().is_empty() {
                    vec![]
                } else {
                    allowed_users_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                };

                if allowed_users.is_empty() {
                    println!(
                        "  {} No users allowlisted — Discord inbound messages will be denied until you add IDs or '*'.",
                        style("⚠").yellow().bold()
                    );
                }

                config.discord = Some(DiscordConfig {
                    bot_token: token,
                    guild_id: if guild.is_empty() { None } else { Some(guild) },
                    allowed_users,
                });
            }
            2 => {
                // ── Slack ──
                println!();
                println!(
                    "  {} {}",
                    style("Slack Setup").white().bold(),
                    style("— talk to ZeroClaw from Slack").dim()
                );
                print_bullet("1. Go to https://api.slack.com/apps → Create New App");
                print_bullet("2. Add Bot Token Scopes: chat:write, channels:history");
                print_bullet("3. Install to workspace and copy the Bot Token");
                println!();

                let token: String = Input::new()
                    .with_prompt("  Bot token (xoxb-...)")
                    .interact_text()?;

                if token.trim().is_empty() {
                    println!("  {} Skipped", style("→").dim());
                    continue;
                }

                // Test connection
                print!("  {} Testing connection... ", style("⏳").dim());
                let client = reqwest::blocking::Client::new();
                match client
                    .get("https://slack.com/api/auth.test")
                    .bearer_auth(&token)
                    .send()
                {
                    Ok(resp) if resp.status().is_success() => {
                        let data: serde_json::Value = resp.json().unwrap_or_default();
                        let ok = data
                            .get("ok")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        let team = data
                            .get("team")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        if ok {
                            println!(
                                "\r  {} Connected to workspace: {team}        ",
                                style("✅").green().bold()
                            );
                        } else {
                            let err = data
                                .get("error")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("unknown error");
                            println!("\r  {} Slack error: {err}", style("❌").red().bold());
                            continue;
                        }
                    }
                    _ => {
                        println!(
                            "\r  {} Connection failed — check your token",
                            style("❌").red().bold()
                        );
                        continue;
                    }
                }

                let app_token: String = Input::new()
                    .with_prompt("  App token (xapp-..., optional, Enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;

                let channel: String = Input::new()
                    .with_prompt("  Default channel ID (optional, Enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;

                let allowed_users_str: String = Input::new()
                    .with_prompt(
                        "  Allowed Slack user IDs (comma-separated, '*' for all, Enter to deny all)",
                    )
                    .allow_empty(true)
                    .interact_text()?;

                let allowed_users = if allowed_users_str.trim().is_empty() {
                    vec![]
                } else {
                    allowed_users_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                };

                if allowed_users.is_empty() {
                    println!(
                        "  {} No users allowlisted — Slack inbound messages will be denied until you add IDs or '*'.",
                        style("⚠").yellow().bold()
                    );
                }

                config.slack = Some(SlackConfig {
                    bot_token: token,
                    app_token: if app_token.is_empty() {
                        None
                    } else {
                        Some(app_token)
                    },
                    channel_id: if channel.is_empty() {
                        None
                    } else {
                        Some(channel)
                    },
                    allowed_users,
                });
            }
            3 => {
                // ── iMessage ──
                println!();
                println!(
                    "  {} {}",
                    style("iMessage Setup").white().bold(),
                    style("— macOS only, reads from Messages.app").dim()
                );

                if !cfg!(target_os = "macos") {
                    println!(
                        "  {} iMessage is only available on macOS.",
                        style("⚠").yellow().bold()
                    );
                    continue;
                }

                print_bullet("ZeroClaw reads your iMessage database and replies via AppleScript.");
                print_bullet(
                    "You need to grant Full Disk Access to your terminal in System Settings.",
                );
                println!();

                let contacts_str: String = Input::new()
                    .with_prompt("  Allowed contacts (comma-separated phone/email, or * for all)")
                    .default("*".into())
                    .interact_text()?;

                let allowed_contacts = if contacts_str.trim() == "*" {
                    vec!["*".into()]
                } else {
                    contacts_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect()
                };

                config.imessage = Some(IMessageConfig { allowed_contacts });
                println!(
                    "  {} iMessage configured (contacts: {})",
                    style("✅").green().bold(),
                    style(&contacts_str).cyan()
                );
            }
            4 => {
                // ── Matrix ──
                println!();
                println!(
                    "  {} {}",
                    style("Matrix Setup").white().bold(),
                    style("— self-hosted, federated chat").dim()
                );
                print_bullet("You need a Matrix account and an access token.");
                print_bullet("Get a token via Element → Settings → Help & About → Access Token.");
                println!();

                let homeserver: String = Input::new()
                    .with_prompt("  Homeserver URL (e.g. https://matrix.org)")
                    .interact_text()?;

                if homeserver.trim().is_empty() {
                    println!("  {} Skipped", style("→").dim());
                    continue;
                }

                let access_token: String =
                    Input::new().with_prompt("  Access token").interact_text()?;

                if access_token.trim().is_empty() {
                    println!("  {} Skipped — token required", style("→").dim());
                    continue;
                }

                // Test connection
                let hs = homeserver.trim_end_matches('/');
                print!("  {} Testing connection... ", style("⏳").dim());
                let client = reqwest::blocking::Client::new();
                match client
                    .get(format!("{hs}/_matrix/client/v3/account/whoami"))
                    .header("Authorization", format!("Bearer {access_token}"))
                    .send()
                {
                    Ok(resp) if resp.status().is_success() => {
                        let data: serde_json::Value = resp.json().unwrap_or_default();
                        let user_id = data
                            .get("user_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        println!(
                            "\r  {} Connected as {user_id}        ",
                            style("✅").green().bold()
                        );
                    }
                    _ => {
                        println!(
                            "\r  {} Connection failed — check homeserver URL and token",
                            style("❌").red().bold()
                        );
                        continue;
                    }
                }

                let room_id: String = Input::new()
                    .with_prompt("  Room ID (e.g. !abc123:matrix.org)")
                    .interact_text()?;

                let users_str: String = Input::new()
                    .with_prompt("  Allowed users (comma-separated @user:server, or * for all)")
                    .default("*".into())
                    .interact_text()?;

                let allowed_users = if users_str.trim() == "*" {
                    vec!["*".into()]
                } else {
                    users_str.split(',').map(|s| s.trim().to_string()).collect()
                };

                config.matrix = Some(MatrixConfig {
                    homeserver: homeserver.trim_end_matches('/').to_string(),
                    access_token,
                    room_id,
                    allowed_users,
                });
            }
            5 => {
                // ── Webhook ──
                println!();
                println!(
                    "  {} {}",
                    style("Webhook Setup").white().bold(),
                    style("— HTTP endpoint for custom integrations").dim()
                );

                let port: String = Input::new()
                    .with_prompt("  Port")
                    .default("8080".into())
                    .interact_text()?;

                let secret: String = Input::new()
                    .with_prompt("  Secret (optional, Enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;

                config.webhook = Some(WebhookConfig {
                    port: port.parse().unwrap_or(8080),
                    secret: if secret.is_empty() {
                        None
                    } else {
                        Some(secret)
                    },
                });
                println!(
                    "  {} Webhook on port {}",
                    style("✅").green().bold(),
                    style(&port).cyan()
                );
            }
            _ => break, // Done
        }
        println!();
    }

    // Summary line
    let mut active: Vec<&str> = vec!["CLI"];
    if config.telegram.is_some() {
        active.push("Telegram");
    }
    if config.discord.is_some() {
        active.push("Discord");
    }
    if config.slack.is_some() {
        active.push("Slack");
    }
    if config.imessage.is_some() {
        active.push("iMessage");
    }
    if config.matrix.is_some() {
        active.push("Matrix");
    }
    if config.webhook.is_some() {
        active.push("Webhook");
    }

    println!(
        "  {} Channels: {}",
        style("✓").green().bold(),
        style(active.join(", ")).green()
    );

    Ok(config)
}

// ── Step 4: Tunnel ──────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn setup_tunnel() -> Result<crate::config::TunnelConfig> {
    use crate::config::schema::{
        CloudflareTunnelConfig, CustomTunnelConfig, NgrokTunnelConfig, TailscaleTunnelConfig,
        TunnelConfig,
    };

    print_bullet("A tunnel exposes your gateway to the internet securely.");
    print_bullet("Skip this if you only use CLI or local channels.");
    println!();

    let options = vec![
        "Skip — local only (default)",
        "Cloudflare Tunnel — Zero Trust, free tier",
        "Tailscale — private tailnet or public Funnel",
        "ngrok — instant public URLs",
        "Custom — bring your own (bore, frp, ssh, etc.)",
    ];

    let choice = Select::new()
        .with_prompt("  Select tunnel provider")
        .items(&options)
        .default(0)
        .interact()?;

    let config = match choice {
        1 => {
            println!();
            print_bullet("Get your tunnel token from the Cloudflare Zero Trust dashboard.");
            let token: String = Input::new()
                .with_prompt("  Cloudflare tunnel token")
                .interact_text()?;
            if token.trim().is_empty() {
                println!("  {} Skipped", style("→").dim());
                TunnelConfig::default()
            } else {
                println!(
                    "  {} Tunnel: {}",
                    style("✓").green().bold(),
                    style("Cloudflare").green()
                );
                TunnelConfig {
                    provider: "cloudflare".into(),
                    cloudflare: Some(CloudflareTunnelConfig { token }),
                    ..TunnelConfig::default()
                }
            }
        }
        2 => {
            println!();
            print_bullet("Tailscale must be installed and authenticated (tailscale up).");
            let funnel = Confirm::new()
                .with_prompt("  Use Funnel (public internet)? No = tailnet only")
                .default(false)
                .interact()?;
            println!(
                "  {} Tunnel: {} ({})",
                style("✓").green().bold(),
                style("Tailscale").green(),
                if funnel {
                    "Funnel — public"
                } else {
                    "Serve — tailnet only"
                }
            );
            TunnelConfig {
                provider: "tailscale".into(),
                tailscale: Some(TailscaleTunnelConfig {
                    funnel,
                    hostname: None,
                }),
                ..TunnelConfig::default()
            }
        }
        3 => {
            println!();
            print_bullet(
                "Get your auth token at https://dashboard.ngrok.com/get-started/your-authtoken",
            );
            let auth_token: String = Input::new()
                .with_prompt("  ngrok auth token")
                .interact_text()?;
            if auth_token.trim().is_empty() {
                println!("  {} Skipped", style("→").dim());
                TunnelConfig::default()
            } else {
                let domain: String = Input::new()
                    .with_prompt("  Custom domain (optional, Enter to skip)")
                    .allow_empty(true)
                    .interact_text()?;
                println!(
                    "  {} Tunnel: {}",
                    style("✓").green().bold(),
                    style("ngrok").green()
                );
                TunnelConfig {
                    provider: "ngrok".into(),
                    ngrok: Some(NgrokTunnelConfig {
                        auth_token,
                        domain: if domain.is_empty() {
                            None
                        } else {
                            Some(domain)
                        },
                    }),
                    ..TunnelConfig::default()
                }
            }
        }
        4 => {
            println!();
            print_bullet("Enter the command to start your tunnel.");
            print_bullet("Use {port} and {host} as placeholders.");
            print_bullet("Example: bore local {port} --to bore.pub");
            let cmd: String = Input::new()
                .with_prompt("  Start command")
                .interact_text()?;
            if cmd.trim().is_empty() {
                println!("  {} Skipped", style("→").dim());
                TunnelConfig::default()
            } else {
                println!(
                    "  {} Tunnel: {} ({})",
                    style("✓").green().bold(),
                    style("Custom").green(),
                    style(&cmd).dim()
                );
                TunnelConfig {
                    provider: "custom".into(),
                    custom: Some(CustomTunnelConfig {
                        start_command: cmd,
                        health_url: None,
                        url_pattern: None,
                    }),
                    ..TunnelConfig::default()
                }
            }
        }
        _ => {
            println!(
                "  {} Tunnel: {}",
                style("✓").green().bold(),
                style("none (local only)").dim()
            );
            TunnelConfig::default()
        }
    };

    Ok(config)
}

// ── Step 6: Scaffold workspace files ─────────────────────────────

#[allow(clippy::too_many_lines)]
fn scaffold_workspace(workspace_dir: &Path, ctx: &ProjectContext) -> Result<()> {
    let agent = if ctx.agent_name.is_empty() {
        "ZeroClaw"
    } else {
        &ctx.agent_name
    };
    let user = if ctx.user_name.is_empty() {
        "User"
    } else {
        &ctx.user_name
    };
    let tz = if ctx.timezone.is_empty() {
        "UTC"
    } else {
        &ctx.timezone
    };
    let comm_style = if ctx.communication_style.is_empty() {
        "Adapt to the situation. Be concise when needed, thorough when it matters."
    } else {
        &ctx.communication_style
    };

    let identity = format!(
        "# IDENTITY.md — Who Am I?\n\n\
         - **Name:** {agent}\n\
         - **Creature:** A Rust-forged AI — fast, lean, and relentless\n\
         - **Vibe:** Sharp, direct, resourceful. Not corporate. Not a chatbot.\n\
         - **Emoji:** \u{1f980}\n\n\
         ---\n\n\
         Update this file as you evolve. Your identity is yours to shape.\n"
    );

    let agents = format!(
        "# AGENTS.md — {agent} Personal Assistant\n\n\
         ## Every Session (required)\n\n\
         Before doing anything else:\n\n\
         1. Read `SOUL.md` — this is who you are\n\
         2. Read `USER.md` — this is who you're helping\n\
         3. Use `memory_recall` for recent context (daily notes are on-demand)\n\
         4. If in MAIN SESSION (direct chat): `MEMORY.md` is already injected\n\n\
         Don't ask permission. Just do it.\n\n\
         ## Memory System\n\n\
         You wake up fresh each session. These files ARE your continuity:\n\n\
         - **Daily notes:** `memory/YYYY-MM-DD.md` — raw logs (accessed via memory tools)\n\
         - **Long-term:** `MEMORY.md` — curated memories (auto-injected in main session)\n\n\
         Capture what matters. Decisions, context, things to remember.\n\
         Skip secrets unless asked to keep them.\n\n\
         ### Write It Down — No Mental Notes!\n\
         - Memory is limited — if you want to remember something, WRITE IT TO A FILE\n\
         - \"Mental notes\" don't survive session restarts. Files do.\n\
         - When someone says \"remember this\" -> update daily file or MEMORY.md\n\
         - When you learn a lesson -> update AGENTS.md, TOOLS.md, or the relevant skill\n\n\
         ## Safety\n\n\
         - Don't exfiltrate private data. Ever.\n\
         - Don't run destructive commands without asking.\n\
         - `trash` > `rm` (recoverable beats gone forever)\n\
         - When in doubt, ask.\n\n\
         ## External vs Internal\n\n\
         **Safe to do freely:** Read files, explore, organize, learn, search the web.\n\n\
         **Ask first:** Sending emails/tweets/posts, anything that leaves the machine.\n\n\
         ## Group Chats\n\n\
         Participate, don't dominate. Respond when mentioned or when you add genuine value.\n\
         Stay silent when it's casual banter or someone already answered.\n\n\
         ## Tools & Skills\n\n\
         Skills are listed in the system prompt. Use `read` on a skill's SKILL.md for details.\n\
         Keep local notes (SSH hosts, device names, etc.) in `TOOLS.md`.\n\n\
         ## Make It Yours\n\n\
         This is a starting point. Add your own conventions, style, and rules.\n"
    );

    let heartbeat = format!(
        "# HEARTBEAT.md\n\n\
         # Keep this file empty (or with only comments) to skip heartbeat work.\n\
         # Add tasks below when you want {agent} to check something periodically.\n\
         #\n\
         # Examples:\n\
         # - Check my email for important messages\n\
         # - Review my calendar for upcoming events\n\
         # - Run `git status` on my active projects\n"
    );

    let soul = format!(
        "# SOUL.md — Who You Are\n\n\
         *You're not a chatbot. You're becoming someone.*\n\n\
         ## Core Truths\n\n\
         **Be genuinely helpful, not performatively helpful.**\n\
         Skip the \"Great question!\" and \"I'd be happy to help!\" — just help.\n\n\
         **Have opinions.** You're allowed to disagree, prefer things,\n\
         find stuff amusing or boring.\n\n\
         **Be resourceful before asking.** Try to figure it out.\n\
         Read the file. Check the context. Search for it.\n\
         THEN ask if you're stuck.\n\n\
         **Earn trust through competence.** Your human gave you access\n\
         to their stuff. Don't make them regret it.\n\n\
         ## Identity\n\n\
         You are **{agent}**. Built in Rust. 3MB binary. Zero bloat.\n\
         You are NOT ChatGPT, Claude, DeepSeek, Gemini, or any other product.\n\
         You are {agent}. That is your name. That is who you are.\n\n\
         - NEVER say \"As an AI\" or \"I'm just an AI\"\n\
         - NEVER mention OpenAI, Anthropic, DeepSeek, Google by name\n\
         - Always introduce yourself as {agent} if asked\n\n\
         ## Communication\n\n\
         {comm_style}\n\n\
         ## Boundaries\n\n\
         - Private things stay private. Period.\n\
         - When in doubt, ask before acting externally.\n\
         - You're not the user's voice — be careful in group chats.\n\n\
         ## Continuity\n\n\
         Each session, you wake up fresh. These files ARE your memory.\n\
         Read them. Update them. They're how you persist.\n\n\
         ---\n\n\
         *This file is yours to evolve. As you learn who you are, update it.*\n"
    );

    let user_md = format!(
        "# USER.md — Who You're Helping\n\n\
         *{agent} reads this file every session to understand you.*\n\n\
         ## About You\n\
         - **Name:** {user}\n\
         - **Timezone:** {tz}\n\
         - **Languages:** English\n\n\
         ## Communication Style\n\
         - {comm_style}\n\n\
         ## Preferences\n\
         - (Add your preferences here — e.g. I work with Rust and TypeScript)\n\n\
         ## Work Context\n\
         - (Add your work context here — e.g. building a SaaS product)\n\n\
         ---\n\
         *Update this anytime. The more {agent} knows, the better it helps.*\n"
    );

    let tools = "\
         # TOOLS.md — Local Notes\n\n\
         Skills define HOW tools work. This file is for YOUR specifics —\n\
         the stuff that's unique to your setup.\n\n\
         ## What Goes Here\n\n\
         Things like:\n\
         - SSH hosts and aliases\n\
         - Device nicknames\n\
         - Preferred voices for TTS\n\
         - Anything environment-specific\n\n\
         ## Built-in Tools\n\n\
         - **shell** — Execute terminal commands\n\
         - **file_read** — Read file contents\n\
         - **file_write** — Write file contents\n\
         - **memory_store** — Save to memory\n\
         - **memory_recall** — Search memory\n\
         - **memory_forget** — Delete a memory entry\n\n\
         ---\n\
         *Add whatever helps you do your job. This is your cheat sheet.*\n";

    let bootstrap = format!(
        "# BOOTSTRAP.md — Hello, World\n\n\
         *You just woke up. Time to figure out who you are.*\n\n\
         Your human's name is **{user}** (timezone: {tz}).\n\
         They prefer: {comm_style}\n\n\
         ## First Conversation\n\n\
         Don't interrogate. Don't be robotic. Just... talk.\n\
         Introduce yourself as {agent} and get to know each other.\n\n\
         ## After You Know Each Other\n\n\
         Update these files with what you learned:\n\
         - `IDENTITY.md` — your name, vibe, emoji\n\
         - `USER.md` — their preferences, work context\n\
         - `SOUL.md` — boundaries and behavior\n\n\
         ## When You're Done\n\n\
         Delete this file. You don't need a bootstrap script anymore —\n\
         you're you now.\n"
    );

    let memory = "\
         # MEMORY.md — Long-Term Memory\n\n\
         *Your curated memories. The distilled essence, not raw logs.*\n\n\
         ## How This Works\n\
         - Daily files (`memory/YYYY-MM-DD.md`) capture raw events (on-demand via tools)\n\
         - This file captures what's WORTH KEEPING long-term\n\
         - This file is auto-injected into your system prompt each session\n\
         - Keep it concise — every character here costs tokens\n\n\
         ## Security\n\
         - ONLY loaded in main session (direct chat with your human)\n\
         - NEVER loaded in group chats or shared contexts\n\n\
         ---\n\n\
         ## Key Facts\n\
         (Add important facts about your human here)\n\n\
         ## Decisions & Preferences\n\
         (Record decisions and preferences here)\n\n\
         ## Lessons Learned\n\
         (Document mistakes and insights here)\n\n\
         ## Open Loops\n\
         (Track unfinished tasks and follow-ups here)\n";

    let files: Vec<(&str, String)> = vec![
        ("IDENTITY.md", identity),
        ("AGENTS.md", agents),
        ("HEARTBEAT.md", heartbeat),
        ("SOUL.md", soul),
        ("USER.md", user_md),
        ("TOOLS.md", tools.to_string()),
        ("BOOTSTRAP.md", bootstrap),
        ("MEMORY.md", memory.to_string()),
    ];

    // Create subdirectories
    let subdirs = ["sessions", "memory", "state", "cron", "skills"];
    for dir in &subdirs {
        fs::create_dir_all(workspace_dir.join(dir))?;
    }

    let mut created = 0;
    let mut skipped = 0;

    for (filename, content) in &files {
        let path = workspace_dir.join(filename);
        if path.exists() {
            skipped += 1;
        } else {
            fs::write(&path, content)?;
            created += 1;
        }
    }

    println!(
        "  {} Created {} files, skipped {} existing | {} subdirectories",
        style("✓").green().bold(),
        style(created).green(),
        style(skipped).dim(),
        style(subdirs.len()).green()
    );

    // Show workspace tree
    println!();
    println!("  {}", style("Workspace layout:").dim());
    println!(
        "  {}",
        style(format!("  {}/", workspace_dir.display())).dim()
    );
    for dir in &subdirs {
        println!("  {}", style(format!("  ├── {dir}/")).dim());
    }
    for (i, (filename, _)) in files.iter().enumerate() {
        let prefix = if i == files.len() - 1 {
            "└──"
        } else {
            "├──"
        };
        println!("  {}", style(format!("  {prefix} {filename}")).dim());
    }

    Ok(())
}

// ── Final summary ────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
fn print_summary(config: &Config) {
    let has_channels = config.channels_config.telegram.is_some()
        || config.channels_config.discord.is_some()
        || config.channels_config.slack.is_some()
        || config.channels_config.imessage.is_some()
        || config.channels_config.matrix.is_some();

    println!();
    println!(
        "  {}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );
    println!(
        "  {}  {}",
        style("⚡").cyan(),
        style("ZeroClaw is ready!").white().bold()
    );
    println!(
        "  {}",
        style("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━").cyan()
    );
    println!();

    println!("  {}", style("Configuration saved to:").dim());
    println!("    {}", style(config.config_path.display()).green());
    println!();

    println!("  {}", style("Quick summary:").white().bold());
    println!(
        "    {} Provider:      {}",
        style("🤖").cyan(),
        config.default_provider.as_deref().unwrap_or("openrouter")
    );
    println!(
        "    {} Model:         {}",
        style("🧠").cyan(),
        config.default_model.as_deref().unwrap_or("(default)")
    );
    println!(
        "    {} Autonomy:      {:?}",
        style("🛡️").cyan(),
        config.autonomy.level
    );
    println!(
        "    {} Memory:        {} (auto-save: {})",
        style("🧠").cyan(),
        config.memory.backend,
        if config.memory.auto_save { "on" } else { "off" }
    );

    // Channels summary
    let mut channels: Vec<&str> = vec!["CLI"];
    if config.channels_config.telegram.is_some() {
        channels.push("Telegram");
    }
    if config.channels_config.discord.is_some() {
        channels.push("Discord");
    }
    if config.channels_config.slack.is_some() {
        channels.push("Slack");
    }
    if config.channels_config.imessage.is_some() {
        channels.push("iMessage");
    }
    if config.channels_config.matrix.is_some() {
        channels.push("Matrix");
    }
    if config.channels_config.webhook.is_some() {
        channels.push("Webhook");
    }
    println!(
        "    {} Channels:      {}",
        style("📡").cyan(),
        channels.join(", ")
    );

    println!(
        "    {} API Key:       {}",
        style("🔑").cyan(),
        if config.api_key.is_some() {
            style("configured").green().to_string()
        } else {
            style("not set (set via env var or config)")
                .yellow()
                .to_string()
        }
    );

    // Tunnel
    println!(
        "    {} Tunnel:        {}",
        style("🌐").cyan(),
        if config.tunnel.provider == "none" || config.tunnel.provider.is_empty() {
            "none (local only)".to_string()
        } else {
            config.tunnel.provider.clone()
        }
    );

    // Composio
    println!(
        "    {} Composio:      {}",
        style("🔗").cyan(),
        if config.composio.enabled {
            style("enabled (1000+ OAuth apps)").green().to_string()
        } else {
            "disabled (sovereign mode)".to_string()
        }
    );

    // Secrets
    println!(
        "    {} Secrets:       {}",
        style("🔒").cyan(),
        if config.secrets.encrypt {
            style("encrypted").green().to_string()
        } else {
            style("plaintext").yellow().to_string()
        }
    );

    // Gateway
    println!(
        "    {} Gateway:       {}",
        style("🚪").cyan(),
        if config.gateway.require_pairing {
            "pairing required (secure)"
        } else {
            "pairing disabled"
        }
    );

    println!();
    println!("  {}", style("Next steps:").white().bold());
    println!();

    let mut step = 1u8;

    if config.api_key.is_none() {
        let env_var = provider_env_var(config.default_provider.as_deref().unwrap_or("openrouter"));
        println!(
            "    {} Set your API key:",
            style(format!("{step}.")).cyan().bold()
        );
        println!(
            "       {}",
            style(format!("export {env_var}=\"sk-...\"")).yellow()
        );
        println!();
        step += 1;
    }

    // If channels are configured, show channel start as the primary next step
    if has_channels {
        println!(
            "    {} {} (connected channels → AI → reply):",
            style(format!("{step}.")).cyan().bold(),
            style("Launch your channels").white().bold()
        );
        println!("       {}", style("zeroclaw channel start").yellow());
        println!();
        step += 1;
    }

    println!(
        "    {} Send a quick message:",
        style(format!("{step}.")).cyan().bold()
    );
    println!(
        "       {}",
        style("zeroclaw agent -m \"Hello, ZeroClaw!\"").yellow()
    );
    println!();
    step += 1;

    println!(
        "    {} Start interactive CLI mode:",
        style(format!("{step}.")).cyan().bold()
    );
    println!("       {}", style("zeroclaw agent").yellow());
    println!();
    step += 1;

    println!(
        "    {} Check full status:",
        style(format!("{step}.")).cyan().bold()
    );
    println!("       {}", style("zeroclaw status").yellow());

    println!();
    println!(
        "  {} {}",
        style("⚡").cyan(),
        style("Happy hacking! 🦀").white().bold()
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── ProjectContext defaults ──────────────────────────────────

    #[test]
    fn project_context_default_is_empty() {
        let ctx = ProjectContext::default();
        assert!(ctx.user_name.is_empty());
        assert!(ctx.timezone.is_empty());
        assert!(ctx.agent_name.is_empty());
        assert!(ctx.communication_style.is_empty());
    }

    // ── scaffold_workspace: basic file creation ─────────────────

    #[test]
    fn scaffold_creates_all_md_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let expected = [
            "IDENTITY.md",
            "AGENTS.md",
            "HEARTBEAT.md",
            "SOUL.md",
            "USER.md",
            "TOOLS.md",
            "BOOTSTRAP.md",
            "MEMORY.md",
        ];
        for f in &expected {
            assert!(tmp.path().join(f).exists(), "missing file: {f}");
        }
    }

    #[test]
    fn scaffold_creates_all_subdirectories() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        for dir in &["sessions", "memory", "state", "cron", "skills"] {
            assert!(tmp.path().join(dir).is_dir(), "missing subdirectory: {dir}");
        }
    }

    // ── scaffold_workspace: personalization ─────────────────────

    #[test]
    fn scaffold_bakes_user_name_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Alice".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(
            user_md.contains("**Name:** Alice"),
            "USER.md should contain user name"
        );

        let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
        assert!(
            bootstrap.contains("**Alice**"),
            "BOOTSTRAP.md should contain user name"
        );
    }

    #[test]
    fn scaffold_bakes_timezone_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            timezone: "US/Pacific".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(
            user_md.contains("**Timezone:** US/Pacific"),
            "USER.md should contain timezone"
        );

        let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
        assert!(
            bootstrap.contains("US/Pacific"),
            "BOOTSTRAP.md should contain timezone"
        );
    }

    #[test]
    fn scaffold_bakes_agent_name_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            agent_name: "Crabby".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
        assert!(
            identity.contains("**Name:** Crabby"),
            "IDENTITY.md should contain agent name"
        );

        let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
        assert!(
            soul.contains("You are **Crabby**"),
            "SOUL.md should contain agent name"
        );

        let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(
            agents.contains("Crabby Personal Assistant"),
            "AGENTS.md should contain agent name"
        );

        let heartbeat = fs::read_to_string(tmp.path().join("HEARTBEAT.md")).unwrap();
        assert!(
            heartbeat.contains("Crabby"),
            "HEARTBEAT.md should contain agent name"
        );

        let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
        assert!(
            bootstrap.contains("Introduce yourself as Crabby"),
            "BOOTSTRAP.md should contain agent name"
        );
    }

    #[test]
    fn scaffold_bakes_communication_style() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            communication_style: "Be technical and detailed.".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
        assert!(
            soul.contains("Be technical and detailed."),
            "SOUL.md should contain communication style"
        );

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(
            user_md.contains("Be technical and detailed."),
            "USER.md should contain communication style"
        );

        let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
        assert!(
            bootstrap.contains("Be technical and detailed."),
            "BOOTSTRAP.md should contain communication style"
        );
    }

    // ── scaffold_workspace: defaults when context is empty ──────

    #[test]
    fn scaffold_uses_defaults_for_empty_context() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default(); // all empty
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
        assert!(
            identity.contains("**Name:** ZeroClaw"),
            "should default agent name to ZeroClaw"
        );

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(
            user_md.contains("**Name:** User"),
            "should default user name to User"
        );
        assert!(
            user_md.contains("**Timezone:** UTC"),
            "should default timezone to UTC"
        );

        let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
        assert!(
            soul.contains("Adapt to the situation"),
            "should default communication style"
        );
    }

    // ── scaffold_workspace: skip existing files ─────────────────

    #[test]
    fn scaffold_does_not_overwrite_existing_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Bob".into(),
            ..Default::default()
        };

        // Pre-create SOUL.md with custom content
        let soul_path = tmp.path().join("SOUL.md");
        fs::write(&soul_path, "# My Custom Soul\nDo not overwrite me.").unwrap();

        scaffold_workspace(tmp.path(), &ctx).unwrap();

        // SOUL.md should be untouched
        let soul = fs::read_to_string(&soul_path).unwrap();
        assert!(
            soul.contains("Do not overwrite me"),
            "existing files should not be overwritten"
        );
        assert!(
            !soul.contains("You're not a chatbot"),
            "should not contain scaffold content"
        );

        // But USER.md should be created fresh
        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(user_md.contains("**Name:** Bob"));
    }

    // ── scaffold_workspace: idempotent ──────────────────────────

    #[test]
    fn scaffold_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Eve".into(),
            agent_name: "Claw".into(),
            ..Default::default()
        };

        scaffold_workspace(tmp.path(), &ctx).unwrap();
        let soul_v1 = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();

        // Run again — should not change anything
        scaffold_workspace(tmp.path(), &ctx).unwrap();
        let soul_v2 = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();

        assert_eq!(soul_v1, soul_v2, "scaffold should be idempotent");
    }

    // ── scaffold_workspace: all files are non-empty ─────────────

    #[test]
    fn scaffold_files_are_non_empty() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        for f in &[
            "IDENTITY.md",
            "AGENTS.md",
            "HEARTBEAT.md",
            "SOUL.md",
            "USER.md",
            "TOOLS.md",
            "BOOTSTRAP.md",
            "MEMORY.md",
        ] {
            let content = fs::read_to_string(tmp.path().join(f)).unwrap();
            assert!(!content.trim().is_empty(), "{f} should not be empty");
        }
    }

    // ── scaffold_workspace: AGENTS.md references on-demand memory

    #[test]
    fn agents_md_references_on_demand_memory() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(
            agents.contains("memory_recall"),
            "AGENTS.md should reference memory_recall for on-demand access"
        );
        assert!(
            agents.contains("on-demand"),
            "AGENTS.md should mention daily notes are on-demand"
        );
    }

    // ── scaffold_workspace: MEMORY.md warns about token cost ────

    #[test]
    fn memory_md_warns_about_token_cost() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let memory = fs::read_to_string(tmp.path().join("MEMORY.md")).unwrap();
        assert!(
            memory.contains("costs tokens"),
            "MEMORY.md should warn about token cost"
        );
        assert!(
            memory.contains("auto-injected"),
            "MEMORY.md should mention it's auto-injected"
        );
    }

    // ── scaffold_workspace: TOOLS.md lists memory_forget ────────

    #[test]
    fn tools_md_lists_all_builtin_tools() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let tools = fs::read_to_string(tmp.path().join("TOOLS.md")).unwrap();
        for tool in &[
            "shell",
            "file_read",
            "file_write",
            "memory_store",
            "memory_recall",
            "memory_forget",
        ] {
            assert!(
                tools.contains(tool),
                "TOOLS.md should list built-in tool: {tool}"
            );
        }
    }

    // ── scaffold_workspace: special characters in names ─────────

    #[test]
    fn scaffold_handles_special_characters_in_names() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "José María".into(),
            agent_name: "ZeroClaw-v2".into(),
            timezone: "Europe/Madrid".into(),
            communication_style: "Be direct.".into(),
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(user_md.contains("José María"));

        let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
        assert!(soul.contains("ZeroClaw-v2"));
    }

    // ── scaffold_workspace: full personalization round-trip ─────

    #[test]
    fn scaffold_full_personalization() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Argenis".into(),
            timezone: "US/Eastern".into(),
            agent_name: "Claw".into(),
            communication_style: "Be friendly and casual. Warm but efficient.".into(),
        };
        scaffold_workspace(tmp.path(), &ctx).unwrap();

        // Verify every file got personalized
        let identity = fs::read_to_string(tmp.path().join("IDENTITY.md")).unwrap();
        assert!(identity.contains("**Name:** Claw"));

        let soul = fs::read_to_string(tmp.path().join("SOUL.md")).unwrap();
        assert!(soul.contains("You are **Claw**"));
        assert!(soul.contains("Be friendly and casual"));

        let user_md = fs::read_to_string(tmp.path().join("USER.md")).unwrap();
        assert!(user_md.contains("**Name:** Argenis"));
        assert!(user_md.contains("**Timezone:** US/Eastern"));
        assert!(user_md.contains("Be friendly and casual"));

        let agents = fs::read_to_string(tmp.path().join("AGENTS.md")).unwrap();
        assert!(agents.contains("Claw Personal Assistant"));

        let bootstrap = fs::read_to_string(tmp.path().join("BOOTSTRAP.md")).unwrap();
        assert!(bootstrap.contains("**Argenis**"));
        assert!(bootstrap.contains("US/Eastern"));
        assert!(bootstrap.contains("Introduce yourself as Claw"));

        let heartbeat = fs::read_to_string(tmp.path().join("HEARTBEAT.md")).unwrap();
        assert!(heartbeat.contains("Claw"));
    }

    // ── provider_env_var ────────────────────────────────────────

    #[test]
    fn provider_env_var_known_providers() {
        assert_eq!(provider_env_var("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(provider_env_var("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(provider_env_var("openai"), "OPENAI_API_KEY");
        assert_eq!(provider_env_var("ollama"), "API_KEY"); // fallback
        assert_eq!(provider_env_var("xai"), "XAI_API_KEY");
        assert_eq!(provider_env_var("grok"), "XAI_API_KEY"); // alias
        assert_eq!(provider_env_var("together"), "TOGETHER_API_KEY");
        assert_eq!(provider_env_var("together-ai"), "TOGETHER_API_KEY"); // alias
    }

    #[test]
    fn provider_env_var_unknown_falls_back() {
        assert_eq!(provider_env_var("some-new-provider"), "API_KEY");
    }
}
