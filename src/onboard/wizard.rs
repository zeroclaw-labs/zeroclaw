use crate::cli_input::Input;
use crate::config::{
    AutonomyConfig, BrowserConfig, ChannelsConfig, ComposioConfig, Config, HeartbeatConfig,
    MemoryConfig, ObservabilityConfig, RuntimeConfig, SecretsConfig, SlackConfig, StorageConfig,
    StreamMode, TelegramConfig,
};
use crate::hardware::{self, HardwareConfig};
use crate::memory::{
    default_memory_backend_key, memory_backend_profile, selectable_memory_backends,
};
use anyhow::{Context, Result, bail};
use console::style;
use dialoguer::{Confirm, Select};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

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

const LIVE_MODEL_MAX_OPTIONS: usize = 120;
const MODEL_PREVIEW_LIMIT: usize = 20;
const MODEL_CACHE_FILE: &str = "models_cache.json";
const MODEL_CACHE_TTL_SECS: u64 = 12 * 60 * 60;
const CUSTOM_MODEL_SENTINEL: &str = "__custom_model__";

fn has_launchable_channels(channels: &ChannelsConfig) -> bool {
    channels.channels_except_webhook().iter().any(|(_, ok)| *ok)
}

// ── Main wizard entry point ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveOnboardingMode {
    FullOnboarding,
    UpdateProviderOnly,
}

pub async fn run_wizard(force: bool) -> Result<Config> {
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

    print_step(1, 9, "Workspace Setup");
    let (workspace_dir, config_path) = setup_workspace().await?;
    match resolve_interactive_onboarding_mode(&config_path, force)? {
        InteractiveOnboardingMode::FullOnboarding => {}
        InteractiveOnboardingMode::UpdateProviderOnly => {
            return Box::pin(run_provider_update_wizard(&workspace_dir, &config_path)).await;
        }
    }

    print_step(2, 9, "AI Provider & API Key");
    let (provider, api_key, model, provider_api_url) = setup_provider(&workspace_dir).await?;

    print_step(3, 9, "Channels (How You Talk to ZeroClaw)");
    let channels_config = setup_channels()?;

    print_step(4, 9, "Tunnel (Expose to Internet)");
    let tunnel_config = setup_tunnel()?;

    print_step(5, 9, "Tool Mode & Security");
    let (composio_config, secrets_config) = setup_tool_mode()?;

    print_step(6, 9, "Hardware (Physical World)");
    let hardware_config = setup_hardware()?;

    print_step(7, 9, "Memory Configuration");
    let memory_config = setup_memory()?;

    print_step(8, 9, "Project Context (Personalize Your Agent)");
    let project_ctx = setup_project_context()?;

    print_step(9, 9, "Workspace Files");
    scaffold_workspace(&workspace_dir, &project_ctx, &memory_config.backend).await?;

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
        api_url: provider_api_url,
        api_path: None,
        default_provider: Some(provider),
        default_model: Some(model),
        model_providers: std::collections::HashMap::new(),
        default_temperature: 0.7,
        provider_timeout_secs: 120,
        provider_max_tokens: None,
        extra_headers: std::collections::HashMap::new(),
        observability: ObservabilityConfig::default(),
        autonomy: AutonomyConfig::default(),
        trust: crate::trust::TrustConfig::default(),
        backup: crate::config::BackupConfig::default(),
        data_retention: crate::config::DataRetentionConfig::default(),
        cloud_ops: crate::config::CloudOpsConfig::default(),
        conversational_ai: crate::config::ConversationalAiConfig::default(),
        security: crate::config::SecurityConfig::default(),
        security_ops: crate::config::SecurityOpsConfig::default(),
        runtime: RuntimeConfig::default(),
        reliability: crate::config::ReliabilityConfig::default(),
        scheduler: crate::config::schema::SchedulerConfig::default(),
        agent: crate::config::schema::AgentConfig::default(),
        pacing: crate::config::PacingConfig::default(),
        skills: crate::config::SkillsConfig::default(),
        pipeline: crate::config::PipelineConfig::default(),
        model_routes: Vec::new(),
        embedding_routes: Vec::new(),
        heartbeat: HeartbeatConfig::default(),
        cron: crate::config::CronConfig::default(),
        channels_config,
        memory: memory_config, // User-selected memory backend
        storage: StorageConfig::default(),
        tunnel: tunnel_config,
        gateway: crate::config::GatewayConfig::default(),
        composio: composio_config,
        microsoft365: crate::config::Microsoft365Config::default(),
        secrets: secrets_config,
        browser: BrowserConfig::default(),
        browser_delegate: crate::tools::browser_delegate::BrowserDelegateConfig::default(),
        http_request: crate::config::HttpRequestConfig::default(),
        multimodal: crate::config::MultimodalConfig::default(),
        media_pipeline: crate::config::MediaPipelineConfig::default(),
        web_fetch: crate::config::WebFetchConfig::default(),
        link_enricher: crate::config::LinkEnricherConfig::default(),
        text_browser: crate::config::TextBrowserConfig::default(),
        web_search: crate::config::WebSearchConfig::default(),
        project_intel: crate::config::ProjectIntelConfig::default(),
        google_workspace: crate::config::GoogleWorkspaceConfig::default(),
        proxy: crate::config::ProxyConfig::default(),
        identity: crate::config::IdentityConfig::default(),
        cost: crate::config::CostConfig::default(),
        peripherals: crate::config::PeripheralsConfig::default(),
        delegate: crate::config::DelegateToolConfig::default(),
        agents: std::collections::HashMap::new(),
        swarms: std::collections::HashMap::new(),
        hooks: crate::config::HooksConfig::default(),
        hardware: hardware_config,
        query_classification: crate::config::QueryClassificationConfig::default(),
        transcription: crate::config::TranscriptionConfig::default(),
        tts: crate::config::TtsConfig::default(),
        mcp: crate::config::McpConfig::default(),
        nodes: crate::config::NodesConfig::default(),
        workspace: crate::config::WorkspaceConfig::default(),
        notion: crate::config::NotionConfig::default(),
        jira: crate::config::JiraConfig::default(),
        node_transport: crate::config::NodeTransportConfig::default(),
        knowledge: crate::config::KnowledgeConfig::default(),
        linkedin: crate::config::LinkedInConfig::default(),
        image_gen: crate::config::ImageGenConfig::default(),
        plugins: crate::config::PluginsConfig::default(),
        locale: None,
        verifiable_intent: crate::config::VerifiableIntentConfig::default(),
        claude_code: crate::config::ClaudeCodeConfig::default(),
        claude_code_runner: crate::config::ClaudeCodeRunnerConfig::default(),
        codex_cli: crate::config::CodexCliConfig::default(),
        gemini_cli: crate::config::GeminiCliConfig::default(),
        opencode_cli: crate::config::OpenCodeCliConfig::default(),
        sop: crate::config::SopConfig::default(),
        shell_tool: crate::config::ShellToolConfig::default(),
    };

    println!(
        "  {} Security: {} | workspace-scoped",
        style("✓").green().bold(),
        style("Supervised").green()
    );
    println!(
        "  {} Memory: {} (auto-save: {})",
        style("✓").green().bold(),
        style(&config.memory.backend).green(),
        if config.memory.auto_save { "on" } else { "off" }
    );

    config.save().await?;
    persist_workspace_selection(&config.config_path).await?;

    // ── Final summary ────────────────────────────────────────────
    print_summary(&config);

    // ── Offer to launch channels immediately ─────────────────────
    let has_channels = has_launchable_channels(&config.channels_config);

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
            // SAFETY: called during single-threaded onboarding wizard before async runtime.
            unsafe { std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1") };
        }
    }

    Ok(config)
}

/// Interactive repair flow: rerun channel setup only without redoing full onboarding.
pub async fn run_channels_repair_wizard() -> Result<Config> {
    println!("{}", style(BANNER).cyan().bold());
    println!(
        "  {}",
        style("Channels Repair — update channel tokens and allowlists only")
            .white()
            .bold()
    );
    println!();

    let mut config = Box::pin(Config::load_or_init()).await?;

    print_step(1, 1, "Channels (How You Talk to ZeroClaw)");
    config.channels_config = setup_channels()?;
    config.save().await?;
    persist_workspace_selection(&config.config_path).await?;

    println!();
    println!(
        "  {} Channel config saved: {}",
        style("✓").green().bold(),
        style(config.config_path.display()).green()
    );

    let has_channels = has_launchable_channels(&config.channels_config);

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
            // SAFETY: called during single-threaded onboarding wizard before async runtime.
            unsafe { std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1") };
        }
    }

    Ok(config)
}

/// Interactive flow: update only provider/model/api key while preserving existing config.
async fn run_provider_update_wizard(workspace_dir: &Path, config_path: &Path) -> Result<Config> {
    println!();
    println!(
        "  {} Existing config detected. Running provider-only update mode (preserving channels, memory, tunnel, hooks, and other settings).",
        style("↻").cyan().bold()
    );

    let raw = fs::read_to_string(config_path).await.with_context(|| {
        format!(
            "Failed to read existing config at {}",
            config_path.display()
        )
    })?;
    let mut config: Config = toml::from_str(&raw).with_context(|| {
        format!(
            "Failed to parse existing config at {}",
            config_path.display()
        )
    })?;
    config.workspace_dir = workspace_dir.to_path_buf();
    config.config_path = config_path.to_path_buf();

    print_step(1, 1, "AI Provider & API Key");
    let (provider, api_key, model, provider_api_url) = setup_provider(workspace_dir).await?;
    apply_provider_update(&mut config, provider, api_key, model, provider_api_url);

    config.save().await?;
    persist_workspace_selection(&config.config_path).await?;

    println!(
        "  {} Provider settings updated at {}",
        style("✓").green().bold(),
        style(config.config_path.display()).green()
    );
    print_summary(&config);

    let has_channels = has_launchable_channels(&config.channels_config);
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
            // SAFETY: called during single-threaded onboarding wizard before async runtime.
            unsafe { std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1") };
        }
    }

    Ok(config)
}

fn apply_provider_update(
    config: &mut Config,
    provider: String,
    api_key: String,
    model: String,
    provider_api_url: Option<String>,
) {
    config.default_provider = Some(provider);
    config.default_model = Some(model);
    config.api_url = provider_api_url;
    config.api_key = if api_key.trim().is_empty() {
        None
    } else {
        Some(api_key)
    };
}

// ── Quick setup (zero prompts) ───────────────────────────────────

/// Non-interactive setup: generates a sensible default config instantly.
/// Use `zeroclaw onboard` or `zeroclaw onboard --api-key sk-... --provider openrouter --memory sqlite|lucid`.
fn backend_key_from_choice(choice: usize) -> &'static str {
    selectable_memory_backends()
        .get(choice)
        .map_or(default_memory_backend_key(), |backend| backend.key)
}

fn memory_config_defaults_for_backend(backend: &str) -> MemoryConfig {
    let profile = memory_backend_profile(backend);

    MemoryConfig {
        backend: backend.to_string(),
        auto_save: profile.auto_save_default,
        hygiene_enabled: profile.uses_sqlite_hygiene,
        archive_after_days: if profile.uses_sqlite_hygiene { 7 } else { 0 },
        purge_after_days: if profile.uses_sqlite_hygiene { 30 } else { 0 },
        conversation_retention_days: 30,
        embedding_provider: "none".to_string(),
        embedding_model: "text-embedding-3-small".to_string(),
        embedding_dimensions: 1536,
        vector_weight: 0.7,
        keyword_weight: 0.3,
        search_mode: crate::config::SearchMode::default(),
        min_relevance_score: 0.4,
        embedding_cache_size: if profile.uses_sqlite_hygiene {
            10000
        } else {
            0
        },
        chunk_max_tokens: 512,
        response_cache_enabled: false,
        response_cache_ttl_minutes: 60,
        response_cache_max_entries: 5_000,
        response_cache_hot_entries: 256,
        snapshot_enabled: false,
        snapshot_on_hygiene: false,
        auto_hydrate: true,
        retrieval_stages: vec!["cache".into(), "fts".into(), "vector".into()],
        rerank_enabled: false,
        rerank_threshold: 5,
        fts_early_return_score: 0.85,
        default_namespace: "default".into(),
        conflict_threshold: 0.85,
        audit_enabled: false,
        audit_retention_days: 30,
        policy: crate::config::MemoryPolicyConfig::default(),
        sqlite_open_timeout_secs: None,
        qdrant: crate::config::QdrantConfig::default(),
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run_quick_setup(
    credential_override: Option<&str>,
    provider: Option<&str>,
    model_override: Option<&str>,
    memory_backend: Option<&str>,
    force: bool,
) -> Result<Config> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;

    Box::pin(run_quick_setup_with_home(
        credential_override,
        provider,
        model_override,
        memory_backend,
        force,
        &home,
    ))
    .await
}

fn resolve_quick_setup_dirs_with_home(home: &Path) -> (PathBuf, PathBuf) {
    if let Ok(custom_config_dir) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let trimmed = custom_config_dir.trim();
        if !trimmed.is_empty() {
            let config_dir = PathBuf::from(shellexpand::tilde(trimmed).as_ref());
            return (config_dir.clone(), config_dir.join("workspace"));
        }
    }

    if let Ok(custom_workspace) = std::env::var("ZEROCLAW_WORKSPACE") {
        let trimmed = custom_workspace.trim();
        if !trimmed.is_empty() {
            let expanded = shellexpand::tilde(trimmed);
            return crate::config::schema::resolve_config_dir_for_workspace(&PathBuf::from(
                expanded.as_ref(),
            ));
        }
    }

    let config_dir = home.join(".zeroclaw");
    (config_dir.clone(), config_dir.join("workspace"))
}

fn homebrew_prefix_for_exe(exe: &Path) -> Option<&'static str> {
    let exe = exe.to_string_lossy();
    if exe == "/opt/homebrew/bin/zeroclaw"
        || exe.starts_with("/opt/homebrew/Cellar/zeroclaw/")
        || exe.starts_with("/opt/homebrew/opt/zeroclaw/")
    {
        return Some("/opt/homebrew");
    }

    if exe == "/usr/local/bin/zeroclaw"
        || exe.starts_with("/usr/local/Cellar/zeroclaw/")
        || exe.starts_with("/usr/local/opt/zeroclaw/")
    {
        return Some("/usr/local");
    }

    None
}

fn quick_setup_homebrew_service_note(
    config_path: &Path,
    workspace_dir: &Path,
    exe: &Path,
) -> Option<String> {
    let prefix = homebrew_prefix_for_exe(exe)?;
    let service_root = Path::new(prefix).join("var").join("zeroclaw");
    let service_config = service_root.join("config.toml");
    let service_workspace = service_root.join("workspace");

    if config_path == service_config || workspace_dir == service_workspace {
        return None;
    }

    Some(format!(
        "Homebrew service note: `brew services` uses {} (config {}) by default. Your onboarding just wrote {}. If you plan to run ZeroClaw as a service, copy or link this workspace first.",
        service_workspace.display(),
        service_config.display(),
        config_path.display(),
    ))
}

#[allow(clippy::too_many_lines)]
async fn run_quick_setup_with_home(
    credential_override: Option<&str>,
    provider: Option<&str>,
    model_override: Option<&str>,
    memory_backend: Option<&str>,
    force: bool,
    home: &Path,
) -> Result<Config> {
    println!("{}", style(BANNER).cyan().bold());
    println!(
        "  {}",
        style("Quick Setup — generating config with sensible defaults...")
            .white()
            .bold()
    );
    println!();

    let (zeroclaw_dir, workspace_dir) = resolve_quick_setup_dirs_with_home(home);
    let config_path = zeroclaw_dir.join("config.toml");

    ensure_onboard_overwrite_allowed(&config_path, force)?;
    fs::create_dir_all(&workspace_dir)
        .await
        .context("Failed to create workspace directory")?;

    let provider_name = provider.unwrap_or("openrouter").to_string();
    let model = model_override
        .map(str::to_string)
        .unwrap_or_else(|| default_model_for_provider(&provider_name));
    let memory_backend_name = memory_backend
        .unwrap_or(default_memory_backend_key())
        .to_string();

    // Create memory config based on backend choice
    let memory_config = memory_config_defaults_for_backend(&memory_backend_name);

    let config = Config {
        workspace_dir: workspace_dir.clone(),
        config_path: config_path.clone(),
        api_key: credential_override.map(|c| {
            let mut s = String::with_capacity(c.len());
            s.push_str(c);
            s
        }),
        api_url: None,
        api_path: None,
        default_provider: Some(provider_name.clone()),
        default_model: Some(model.clone()),
        model_providers: std::collections::HashMap::new(),
        default_temperature: 0.7,
        provider_timeout_secs: 120,
        provider_max_tokens: None,
        extra_headers: std::collections::HashMap::new(),
        observability: ObservabilityConfig::default(),
        autonomy: AutonomyConfig::default(),
        trust: crate::trust::TrustConfig::default(),
        backup: crate::config::BackupConfig::default(),
        data_retention: crate::config::DataRetentionConfig::default(),
        cloud_ops: crate::config::CloudOpsConfig::default(),
        conversational_ai: crate::config::ConversationalAiConfig::default(),
        security: crate::config::SecurityConfig::default(),
        security_ops: crate::config::SecurityOpsConfig::default(),
        runtime: RuntimeConfig::default(),
        reliability: crate::config::ReliabilityConfig::default(),
        scheduler: crate::config::schema::SchedulerConfig::default(),
        agent: crate::config::schema::AgentConfig::default(),
        pacing: crate::config::PacingConfig::default(),
        skills: crate::config::SkillsConfig::default(),
        pipeline: crate::config::PipelineConfig::default(),
        model_routes: Vec::new(),
        embedding_routes: Vec::new(),
        heartbeat: HeartbeatConfig::default(),
        cron: crate::config::CronConfig::default(),
        channels_config: ChannelsConfig::default(),
        memory: memory_config,
        storage: StorageConfig::default(),
        tunnel: crate::config::TunnelConfig::default(),
        gateway: crate::config::GatewayConfig::default(),
        composio: ComposioConfig::default(),
        microsoft365: crate::config::Microsoft365Config::default(),
        secrets: SecretsConfig::default(),
        browser: BrowserConfig::default(),
        browser_delegate: crate::tools::browser_delegate::BrowserDelegateConfig::default(),
        http_request: crate::config::HttpRequestConfig::default(),
        multimodal: crate::config::MultimodalConfig::default(),
        media_pipeline: crate::config::MediaPipelineConfig::default(),
        web_fetch: crate::config::WebFetchConfig::default(),
        link_enricher: crate::config::LinkEnricherConfig::default(),
        text_browser: crate::config::TextBrowserConfig::default(),
        web_search: crate::config::WebSearchConfig::default(),
        project_intel: crate::config::ProjectIntelConfig::default(),
        google_workspace: crate::config::GoogleWorkspaceConfig::default(),
        proxy: crate::config::ProxyConfig::default(),
        identity: crate::config::IdentityConfig::default(),
        cost: crate::config::CostConfig::default(),
        peripherals: crate::config::PeripheralsConfig::default(),
        delegate: crate::config::DelegateToolConfig::default(),
        agents: std::collections::HashMap::new(),
        swarms: std::collections::HashMap::new(),
        hooks: crate::config::HooksConfig::default(),
        hardware: crate::config::HardwareConfig::default(),
        query_classification: crate::config::QueryClassificationConfig::default(),
        transcription: crate::config::TranscriptionConfig::default(),
        tts: crate::config::TtsConfig::default(),
        mcp: crate::config::McpConfig::default(),
        nodes: crate::config::NodesConfig::default(),
        workspace: crate::config::WorkspaceConfig::default(),
        notion: crate::config::NotionConfig::default(),
        jira: crate::config::JiraConfig::default(),
        node_transport: crate::config::NodeTransportConfig::default(),
        knowledge: crate::config::KnowledgeConfig::default(),
        linkedin: crate::config::LinkedInConfig::default(),
        image_gen: crate::config::ImageGenConfig::default(),
        plugins: crate::config::PluginsConfig::default(),
        locale: None,
        verifiable_intent: crate::config::VerifiableIntentConfig::default(),
        claude_code: crate::config::ClaudeCodeConfig::default(),
        claude_code_runner: crate::config::ClaudeCodeRunnerConfig::default(),
        codex_cli: crate::config::CodexCliConfig::default(),
        gemini_cli: crate::config::GeminiCliConfig::default(),
        opencode_cli: crate::config::OpenCodeCliConfig::default(),
        sop: crate::config::SopConfig::default(),
        shell_tool: crate::config::ShellToolConfig::default(),
    };

    config.save().await?;
    persist_workspace_selection(&config.config_path).await?;

    // Scaffold minimal workspace files
    let default_ctx = ProjectContext {
        user_name: std::env::var("USER").unwrap_or_else(|_| "User".into()),
        timezone: "UTC".into(),
        agent_name: "ZeroClaw".into(),
        communication_style:
            "Be warm, natural, and clear. Use occasional relevant emojis (1-2 max) and avoid robotic phrasing."
                .into(),
    };
    scaffold_workspace(&workspace_dir, &default_ctx, &memory_backend_name).await?;

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
        if credential_override.is_some() {
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
        "  {} Memory:     {} (auto-save: {})",
        style("✓").green().bold(),
        style(&memory_backend_name).green(),
        if memory_backend_name == "none" {
            "off"
        } else {
            "on"
        }
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
    if cfg!(target_os = "macos") {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(note) =
                quick_setup_homebrew_service_note(&config_path, &workspace_dir, &exe)
            {
                println!();
                println!("  {}", style(note).yellow());
            }
        }
    }
    println!();
    println!("  {}", style("Next steps:").white().bold());
    if credential_override.is_none() {
        if provider_supports_keyless_local_usage(&provider_name) {
            println!("    1. Chat:     zeroclaw agent -m \"Hello!\"");
            println!("    2. Gateway:  zeroclaw gateway");
            println!("    3. Status:   zeroclaw status");
        } else if provider_supports_device_flow(&provider_name) {
            if canonical_provider_name(&provider_name) == "copilot" {
                println!("    1. Chat:              zeroclaw agent -m \"Hello!\"");
                println!("       (device / OAuth auth will prompt on first run)");
                println!("    2. Gateway:           zeroclaw gateway");
                println!("    3. Status:            zeroclaw status");
            } else {
                println!(
                    "    1. Login:             zeroclaw auth login --provider {}",
                    provider_name
                );
                println!("    2. Chat:              zeroclaw agent -m \"Hello!\"");
                println!("    3. Gateway:           zeroclaw gateway");
                println!("    4. Status:            zeroclaw status");
            }
        } else {
            let env_var = provider_env_var(&provider_name);
            println!("    1. Set your API key:  export {env_var}=\"sk-...\"");
            println!("    2. Or edit:           ~/.zeroclaw/config.toml");
            println!("    3. Chat:              zeroclaw agent -m \"Hello!\"");
            println!("    4. Gateway:           zeroclaw gateway");
        }
    } else {
        println!("    1. Chat:     zeroclaw agent -m \"Hello!\"");
        println!("    2. Gateway:  zeroclaw gateway");
        println!("    3. Status:   zeroclaw status");
    }
    println!();

    Ok(config)
}

fn canonical_provider_name(provider_name: &str) -> &str {
    match provider_name {
        "google" | "google-gemini" => "gemini",
        _ => provider_name,
    }
}

fn allows_unauthenticated_model_fetch(provider_name: &str) -> bool {
    matches!(canonical_provider_name(provider_name), "openrouter")
}

fn default_model_for_provider(provider: &str) -> String {
    match canonical_provider_name(provider) {
        "anthropic" => "claude-sonnet-4-5-20250929".into(),
        "openai" => "gpt-5.2".into(),
        "gemini" => "gemini-2.5-pro".into(),
        _ => "anthropic/claude-sonnet-4.6".into(),
    }
}

fn curated_models_for_provider(provider_name: &str) -> Vec<(String, String)> {
    match canonical_provider_name(provider_name) {
        "openrouter" => vec![
            (
                "anthropic/claude-sonnet-4.6".to_string(),
                "Claude Sonnet 4.6 (balanced, recommended)".to_string(),
            ),
            (
                "openai/gpt-5.2".to_string(),
                "GPT-5.2 (latest flagship)".to_string(),
            ),
            (
                "openai/gpt-5-mini".to_string(),
                "GPT-5 mini (fast, cost-efficient)".to_string(),
            ),
            (
                "google/gemini-3-pro-preview".to_string(),
                "Gemini 3 Pro Preview (frontier reasoning)".to_string(),
            ),
            (
                "x-ai/grok-4.1-fast".to_string(),
                "Grok 4.1 Fast (reasoning + speed)".to_string(),
            ),
            (
                "deepseek/deepseek-v3.2".to_string(),
                "DeepSeek V3.2 (agentic + affordable)".to_string(),
            ),
            (
                "meta-llama/llama-4-maverick".to_string(),
                "Llama 4 Maverick (open model)".to_string(),
            ),
        ],
        "anthropic" => vec![
            (
                "claude-sonnet-4-5-20250929".to_string(),
                "Claude Sonnet 4.5 (balanced, recommended)".to_string(),
            ),
            (
                "claude-opus-4-6".to_string(),
                "Claude Opus 4.6 (best quality)".to_string(),
            ),
            (
                "claude-haiku-4-5-20251001".to_string(),
                "Claude Haiku 4.5 (fastest, cheapest)".to_string(),
            ),
        ],
        "openai" => vec![
            (
                "gpt-5.2".to_string(),
                "GPT-5.2 (latest coding/agentic flagship)".to_string(),
            ),
            (
                "gpt-5-mini".to_string(),
                "GPT-5 mini (faster, cheaper)".to_string(),
            ),
            (
                "gpt-5-nano".to_string(),
                "GPT-5 nano (lowest latency/cost)".to_string(),
            ),
            (
                "gpt-5.2-codex".to_string(),
                "GPT-5.2 Codex (agentic coding)".to_string(),
            ),
        ],
        "gemini" => vec![
            (
                "gemini-3-pro-preview".to_string(),
                "Gemini 3 Pro Preview (latest frontier reasoning)".to_string(),
            ),
            (
                "gemini-2.5-pro".to_string(),
                "Gemini 2.5 Pro (stable reasoning)".to_string(),
            ),
            (
                "gemini-2.5-flash".to_string(),
                "Gemini 2.5 Flash (best price/performance)".to_string(),
            ),
            (
                "gemini-2.5-flash-lite".to_string(),
                "Gemini 2.5 Flash-Lite (lowest cost)".to_string(),
            ),
        ],
        _ => vec![("default".to_string(), "Default model".to_string())],
    }
}

fn supports_live_model_fetch(provider_name: &str) -> bool {
    if provider_name.trim().starts_with("custom:") {
        return true;
    }

    matches!(
        canonical_provider_name(provider_name),
        "openrouter"
            | "openai-codex"
            | "openai"
            | "anthropic"
            | "groq"
            | "mistral"
            | "deepseek"
            | "xai"
            | "together-ai"
            | "gemini"
            | "ollama"
            | "llamacpp"
            | "sglang"
            | "vllm"
            | "osaurus"
            | "astrai"
            | "avian"
            | "venice"
            | "fireworks"
            | "novita"
            | "cohere"
            | "moonshot"
            | "glm"
            | "zai"
            | "qwen"
            | "nvidia"
            | "opencode-go"
    )
}

fn models_endpoint_for_provider(provider_name: &str) -> Option<&'static str> {
    match provider_name {
        "qwen-intl" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models"),
        "dashscope-us" => Some("https://dashscope-us.aliyuncs.com/compatible-mode/v1/models"),
        "moonshot-cn" | "kimi-cn" => Some("https://api.moonshot.cn/v1/models"),
        "glm-cn" | "bigmodel" => Some("https://open.bigmodel.cn/api/paas/v4/models"),
        "zai-cn" | "z.ai-cn" => Some("https://open.bigmodel.cn/api/coding/paas/v4/models"),
        _ => match canonical_provider_name(provider_name) {
            "openai-codex" | "openai" => Some("https://api.openai.com/v1/models"),
            "venice" => Some("https://api.venice.ai/api/v1/models"),
            "groq" => Some("https://api.groq.com/openai/v1/models"),
            "mistral" => Some("https://api.mistral.ai/v1/models"),
            "deepseek" => Some("https://api.deepseek.com/v1/models"),
            "xai" => Some("https://api.x.ai/v1/models"),
            "together-ai" => Some("https://api.together.xyz/v1/models"),
            "fireworks" => Some("https://api.fireworks.ai/inference/v1/models"),
            "novita" => Some("https://api.novita.ai/openai/v1/models"),
            "cohere" => Some("https://api.cohere.com/compatibility/v1/models"),
            "moonshot" => Some("https://api.moonshot.ai/v1/models"),
            "glm" => Some("https://api.z.ai/api/paas/v4/models"),
            "zai" => Some("https://api.z.ai/api/coding/paas/v4/models"),
            "qwen" => Some("https://dashscope.aliyuncs.com/compatible-mode/v1/models"),
            "nvidia" => Some("https://integrate.api.nvidia.com/v1/models"),
            "astrai" => Some("https://as-trai.com/v1/models"),
            "avian" => Some("https://api.avian.io/v1/models"),
            "llamacpp" => Some("http://localhost:8080/v1/models"),
            "sglang" => Some("http://localhost:30000/v1/models"),
            "vllm" => Some("http://localhost:8000/v1/models"),
            "osaurus" => Some("http://localhost:1337/v1/models"),
            "opencode-go" => Some("https://opencode.ai/zen/go/v1/models"),
            _ => None,
        },
    }
}

fn build_model_fetch_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(4))
        .build()
        .context("failed to build model-fetch HTTP client")
}

fn normalize_model_ids(ids: Vec<String>) -> Vec<String> {
    let mut unique = BTreeMap::new();
    for id in ids {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            unique
                .entry(trimmed.to_ascii_lowercase())
                .or_insert_with(|| trimmed.to_string());
        }
    }
    unique.into_values().collect()
}

fn parse_openai_compatible_model_ids(payload: &Value) -> Vec<String> {
    let mut models = Vec::new();

    if let Some(data) = payload.get("data").and_then(Value::as_array) {
        for model in data {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                models.push(id.to_string());
            }
        }
    } else if let Some(data) = payload.as_array() {
        for model in data {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                models.push(id.to_string());
            }
        }
    }

    normalize_model_ids(models)
}

fn parse_gemini_model_ids(payload: &Value) -> Vec<String> {
    let Some(models) = payload.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut ids = Vec::new();
    for model in models {
        let supports_generate_content = model
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .is_none_or(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            });

        if !supports_generate_content {
            continue;
        }

        if let Some(name) = model.get("name").and_then(Value::as_str) {
            ids.push(name.trim_start_matches("models/").to_string());
        }
    }

    normalize_model_ids(ids)
}

fn parse_ollama_model_ids(payload: &Value) -> Vec<String> {
    let Some(models) = payload.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut ids = Vec::new();
    for model in models {
        if let Some(name) = model.get("name").and_then(Value::as_str) {
            ids.push(name.to_string());
        }
    }

    normalize_model_ids(ids)
}

async fn fetch_openai_compatible_models(
    endpoint: &str,
    api_key: Option<&str>,
    allow_unauthenticated: bool,
) -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let mut request = client.get(endpoint);

    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    } else if !allow_unauthenticated {
        bail!("model fetch requires API key for endpoint {endpoint}");
    }

    let payload: Value = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .with_context(|| format!("model fetch failed: GET {endpoint}"))?
        .json()
        .await
        .context("failed to parse model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

async fn fetch_openrouter_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let mut request = client.get("https://openrouter.ai/api/v1/models");
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }

    let payload: Value = request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("model fetch failed: GET https://openrouter.ai/api/v1/models")?
        .json()
        .await
        .context("failed to parse OpenRouter model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

async fn fetch_anthropic_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let Some(api_key) = api_key else {
        bail!("Anthropic model fetch requires API key or OAuth token");
    };

    let client = build_model_fetch_client()?;
    let mut request = client
        .get("https://api.anthropic.com/v1/models")
        .header("anthropic-version", "2023-06-01");

    if api_key.starts_with("sk-ant-oat01-") {
        request = request
            .header("Authorization", format!("Bearer {api_key}"))
            .header("anthropic-beta", "oauth-2025-04-20");
    } else {
        request = request.header("x-api-key", api_key);
    }

    let response = request
        .send()
        .await
        .context("model fetch failed: GET https://api.anthropic.com/v1/models")?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Anthropic model list request failed (HTTP {status}): {body}");
    }

    let payload: Value = response
        .json()
        .await
        .context("failed to parse Anthropic model list response")?;

    Ok(parse_openai_compatible_model_ids(&payload))
}

async fn fetch_gemini_models(api_key: Option<&str>) -> Result<Vec<String>> {
    let Some(api_key) = api_key else {
        bail!("Gemini model fetch requires API key");
    };

    let client = build_model_fetch_client()?;
    let payload: Value = client
        .get("https://generativelanguage.googleapis.com/v1beta/models")
        .query(&[("key", api_key), ("pageSize", "200")])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("model fetch failed: GET Gemini models")?
        .json()
        .await
        .context("failed to parse Gemini model list response")?;

    Ok(parse_gemini_model_ids(&payload))
}

async fn fetch_ollama_models() -> Result<Vec<String>> {
    let client = build_model_fetch_client()?;
    let payload: Value = client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .context("model fetch failed: GET http://localhost:11434/api/tags")?
        .json()
        .await
        .context("failed to parse Ollama model list response")?;

    Ok(parse_ollama_model_ids(&payload))
}

fn normalize_ollama_endpoint_url(raw_url: &str) -> String {
    let trimmed = raw_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed
        .strip_suffix("/api")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

fn ollama_endpoint_is_local(endpoint_url: &str) -> bool {
    reqwest::Url::parse(endpoint_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "0.0.0.0"))
}

fn ollama_uses_remote_endpoint(provider_api_url: Option<&str>) -> bool {
    let Some(endpoint) = provider_api_url else {
        return false;
    };

    let normalized = normalize_ollama_endpoint_url(endpoint);
    if normalized.is_empty() {
        return false;
    }

    !ollama_endpoint_is_local(&normalized)
}

fn resolve_live_models_endpoint(
    provider_name: &str,
    provider_api_url: Option<&str>,
) -> Option<String> {
    if let Some(raw_base) = provider_name.strip_prefix("custom:") {
        let normalized = raw_base.trim().trim_end_matches('/');
        if normalized.is_empty() {
            return None;
        }
        if normalized.ends_with("/models") {
            return Some(normalized.to_string());
        }
        return Some(format!("{normalized}/models"));
    }

    if matches!(
        canonical_provider_name(provider_name),
        "llamacpp" | "sglang" | "vllm" | "osaurus"
    ) {
        if let Some(url) = provider_api_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let normalized = url.trim_end_matches('/');
            if normalized.ends_with("/models") {
                return Some(normalized.to_string());
            }
            return Some(format!("{normalized}/models"));
        }
    }

    if canonical_provider_name(provider_name) == "openai-codex" {
        if let Some(url) = provider_api_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
        {
            let normalized = url.trim_end_matches('/');
            if normalized.ends_with("/models") {
                return Some(normalized.to_string());
            }
            return Some(format!("{normalized}/models"));
        }
    }

    models_endpoint_for_provider(provider_name).map(str::to_string)
}

async fn fetch_live_models_for_provider(
    provider_name: &str,
    api_key: &str,
    provider_api_url: Option<&str>,
) -> Result<Vec<String>> {
    let requested_provider_name = provider_name;
    let provider_name = canonical_provider_name(provider_name);
    let ollama_remote = provider_name == "ollama" && ollama_uses_remote_endpoint(provider_api_url);
    let api_key = if api_key.trim().is_empty() {
        if provider_name == "ollama" && !ollama_remote {
            None
        } else {
            std::env::var(provider_env_var(provider_name))
                .ok()
                .or_else(|| {
                    // Anthropic also accepts OAuth setup-tokens via ANTHROPIC_OAUTH_TOKEN
                    if provider_name == "anthropic" {
                        std::env::var("ANTHROPIC_OAUTH_TOKEN").ok()
                    } else if provider_name == "minimax" {
                        std::env::var("MINIMAX_OAUTH_TOKEN").ok()
                    } else {
                        None
                    }
                })
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        }
    } else {
        Some(api_key.trim().to_string())
    };

    let models = match provider_name {
        "openrouter" => fetch_openrouter_models(api_key.as_deref()).await?,
        "anthropic" => fetch_anthropic_models(api_key.as_deref()).await?,
        "gemini" => fetch_gemini_models(api_key.as_deref()).await?,
        "ollama" => {
            if ollama_remote {
                // Remote Ollama endpoints can serve cloud-routed models.
                // Keep this curated list aligned with current Ollama cloud catalog.
                vec![
                    "glm-5:cloud".to_string(),
                    "glm-4.7:cloud".to_string(),
                    "gpt-oss:20b:cloud".to_string(),
                    "gpt-oss:120b:cloud".to_string(),
                    "gemini-3-flash-preview:cloud".to_string(),
                    "qwen3-coder-next:cloud".to_string(),
                    "qwen3-coder:480b:cloud".to_string(),
                    "kimi-k2.5:cloud".to_string(),
                    "minimax-m2.7:cloud".to_string(),
                    "deepseek-v3.1:671b:cloud".to_string(),
                ]
            } else {
                // Local endpoints should not surface cloud-only suffixes.
                fetch_ollama_models()
                    .await?
                    .into_iter()
                    .filter(|model_id| !model_id.ends_with(":cloud"))
                    .collect()
            }
        }
        _ => {
            if let Some(endpoint) =
                resolve_live_models_endpoint(requested_provider_name, provider_api_url)
            {
                let allow_unauthenticated =
                    allows_unauthenticated_model_fetch(requested_provider_name);
                fetch_openai_compatible_models(&endpoint, api_key.as_deref(), allow_unauthenticated)
                    .await?
            } else {
                Vec::new()
            }
        }
    };

    Ok(models)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelCacheEntry {
    provider: String,
    fetched_at_unix: u64,
    models: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelCacheState {
    entries: Vec<ModelCacheEntry>,
}

#[derive(Debug, Clone)]
struct CachedModels {
    models: Vec<String>,
    age_secs: u64,
}

fn model_cache_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join(MODEL_CACHE_FILE)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

async fn load_model_cache_state(workspace_dir: &Path) -> Result<ModelCacheState> {
    let path = model_cache_path(workspace_dir);
    if !path.exists() {
        return Ok(ModelCacheState::default());
    }

    let raw = fs::read_to_string(&path)
        .await
        .with_context(|| format!("failed to read model cache at {}", path.display()))?;

    match serde_json::from_str::<ModelCacheState>(&raw) {
        Ok(state) => Ok(state),
        Err(_) => Ok(ModelCacheState::default()),
    }
}

async fn save_model_cache_state(workspace_dir: &Path, state: &ModelCacheState) -> Result<()> {
    let path = model_cache_path(workspace_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create model cache directory {}",
                parent.display()
            )
        })?;
    }

    let json = serde_json::to_vec_pretty(state).context("failed to serialize model cache")?;
    fs::write(&path, json)
        .await
        .with_context(|| format!("failed to write model cache at {}", path.display()))?;

    Ok(())
}

async fn cache_live_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
    models: &[String],
) -> Result<()> {
    let normalized_models = normalize_model_ids(models.to_vec());
    if normalized_models.is_empty() {
        return Ok(());
    }

    let mut state = load_model_cache_state(workspace_dir).await?;
    let now = now_unix_secs();

    if let Some(entry) = state
        .entries
        .iter_mut()
        .find(|entry| entry.provider == provider_name)
    {
        entry.fetched_at_unix = now;
        entry.models = normalized_models;
    } else {
        state.entries.push(ModelCacheEntry {
            provider: provider_name.to_string(),
            fetched_at_unix: now,
            models: normalized_models,
        });
    }

    save_model_cache_state(workspace_dir, &state).await
}

async fn load_cached_models_for_provider_internal(
    workspace_dir: &Path,
    provider_name: &str,
    ttl_secs: Option<u64>,
) -> Result<Option<CachedModels>> {
    let state = load_model_cache_state(workspace_dir).await?;
    let now = now_unix_secs();

    let Some(entry) = state
        .entries
        .into_iter()
        .find(|entry| entry.provider == provider_name)
    else {
        return Ok(None);
    };

    if entry.models.is_empty() {
        return Ok(None);
    }

    let age_secs = now.saturating_sub(entry.fetched_at_unix);
    if ttl_secs.is_some_and(|ttl| age_secs > ttl) {
        return Ok(None);
    }

    Ok(Some(CachedModels {
        models: entry.models,
        age_secs,
    }))
}

async fn load_cached_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
    ttl_secs: u64,
) -> Result<Option<CachedModels>> {
    load_cached_models_for_provider_internal(workspace_dir, provider_name, Some(ttl_secs)).await
}

async fn load_any_cached_models_for_provider(
    workspace_dir: &Path,
    provider_name: &str,
) -> Result<Option<CachedModels>> {
    load_cached_models_for_provider_internal(workspace_dir, provider_name, None).await
}

fn humanize_age(age_secs: u64) -> String {
    if age_secs < 60 {
        format!("{age_secs}s")
    } else if age_secs < 60 * 60 {
        format!("{}m", age_secs / 60)
    } else {
        format!("{}h", age_secs / (60 * 60))
    }
}

fn build_model_options(model_ids: Vec<String>, source: &str) -> Vec<(String, String)> {
    model_ids
        .into_iter()
        .map(|model_id| {
            let label = format!("{model_id} ({source})");
            (model_id, label)
        })
        .collect()
}

fn print_model_preview(models: &[String]) {
    for model in models.iter().take(MODEL_PREVIEW_LIMIT) {
        println!("  {} {model}", style("-"));
    }

    if models.len() > MODEL_PREVIEW_LIMIT {
        println!(
            "  {} ... and {} more",
            style("-"),
            models.len() - MODEL_PREVIEW_LIMIT
        );
    }
}

pub async fn run_models_refresh(
    config: &Config,
    provider_override: Option<&str>,
    force: bool,
) -> Result<()> {
    let provider_name = provider_override
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter")
        .trim()
        .to_string();

    if provider_name.is_empty() {
        anyhow::bail!("Provider name cannot be empty");
    }

    if !supports_live_model_fetch(&provider_name) {
        anyhow::bail!("Provider '{provider_name}' does not support live model discovery yet");
    }

    if !force {
        if let Some(cached) = load_cached_models_for_provider(
            &config.workspace_dir,
            &provider_name,
            MODEL_CACHE_TTL_SECS,
        )
        .await?
        {
            println!(
                "Using cached model list for '{}' (updated {} ago):",
                provider_name,
                humanize_age(cached.age_secs)
            );
            print_model_preview(&cached.models);
            println!();
            println!(
                "Tip: run `zeroclaw models refresh --force --provider {}` to fetch latest now.",
                provider_name
            );
            return Ok(());
        }
    }

    let api_key = config.api_key.clone().unwrap_or_default();

    match fetch_live_models_for_provider(&provider_name, &api_key, config.api_url.as_deref()).await
    {
        Ok(models) if !models.is_empty() => {
            cache_live_models_for_provider(&config.workspace_dir, &provider_name, &models).await?;
            println!(
                "Refreshed '{}' model cache with {} models.",
                provider_name,
                models.len()
            );
            print_model_preview(&models);
            Ok(())
        }
        Ok(_) => {
            if let Some(stale_cache) =
                load_any_cached_models_for_provider(&config.workspace_dir, &provider_name).await?
            {
                println!(
                    "Provider returned no models; using stale cache (updated {} ago):",
                    humanize_age(stale_cache.age_secs)
                );
                print_model_preview(&stale_cache.models);
                return Ok(());
            }

            anyhow::bail!("Provider '{}' returned an empty model list", provider_name)
        }
        Err(error) => {
            if let Some(stale_cache) =
                load_any_cached_models_for_provider(&config.workspace_dir, &provider_name).await?
            {
                println!(
                    "Live refresh failed ({}). Falling back to stale cache (updated {} ago):",
                    error,
                    humanize_age(stale_cache.age_secs)
                );
                print_model_preview(&stale_cache.models);
                return Ok(());
            }

            Err(error)
                .with_context(|| format!("failed to refresh models for provider '{provider_name}'"))
        }
    }
}

pub async fn run_models_list(config: &Config, provider_override: Option<&str>) -> Result<()> {
    let provider_name = provider_override
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");

    let cached = load_any_cached_models_for_provider(&config.workspace_dir, provider_name).await?;

    let Some(cached) = cached else {
        println!();
        println!(
            "  No cached models for '{provider_name}'. Run: zeroclaw models refresh --provider {provider_name}"
        );
        println!();
        return Ok(());
    };

    println!();
    println!(
        "  {} models for '{}' (cached {} ago):",
        cached.models.len(),
        provider_name,
        humanize_age(cached.age_secs)
    );
    println!();
    for model in &cached.models {
        let marker = if config.default_model.as_deref() == Some(model.as_str()) {
            "* "
        } else {
            "  "
        };
        println!("  {marker}{model}");
    }
    println!();
    Ok(())
}

pub async fn run_models_set(config: &Config, model: &str) -> Result<()> {
    let model = model.trim();
    if model.is_empty() {
        anyhow::bail!("Model name cannot be empty");
    }

    let mut updated = config.clone();
    updated.default_model = Some(model.to_string());
    updated.save().await?;

    println!();
    println!("  Default model set to '{}'.", style(model).green().bold());
    println!();
    Ok(())
}

pub async fn run_models_status(config: &Config) -> Result<()> {
    let provider = config.default_provider.as_deref().unwrap_or("openrouter");
    let model = config.default_model.as_deref().unwrap_or("(not set)");

    println!();
    println!("  Provider:  {}", style(provider).cyan());
    println!("  Model:     {}", style(model).cyan());
    println!(
        "  Temp:      {}",
        style(format!("{:.1}", config.default_temperature)).cyan()
    );

    match load_any_cached_models_for_provider(&config.workspace_dir, provider).await? {
        Some(cached) => {
            println!(
                "  Cache:     {} models (updated {} ago)",
                cached.models.len(),
                humanize_age(cached.age_secs)
            );
            let fresh = cached.age_secs < MODEL_CACHE_TTL_SECS;
            if fresh {
                println!("  Freshness: {}", style("fresh").green());
            } else {
                println!("  Freshness: {}", style("stale").yellow());
            }
        }
        None => {
            println!("  Cache:     {}", style("none").yellow());
        }
    }

    println!();
    Ok(())
}

pub async fn cached_model_catalog_stats(
    config: &Config,
    provider_name: &str,
) -> Result<Option<(usize, u64)>> {
    let Some(cached) =
        load_any_cached_models_for_provider(&config.workspace_dir, provider_name).await?
    else {
        return Ok(None);
    };
    Ok(Some((cached.models.len(), cached.age_secs)))
}

pub async fn run_models_refresh_all(config: &Config, force: bool) -> Result<()> {
    let mut targets: Vec<String> = crate::providers::list_providers()
        .into_iter()
        .map(|provider| provider.name.to_string())
        .filter(|name| supports_live_model_fetch(name))
        .collect();

    targets.sort();
    targets.dedup();

    if targets.is_empty() {
        anyhow::bail!("No providers support live model discovery");
    }

    println!(
        "Refreshing model catalogs for {} providers (force: {})",
        targets.len(),
        if force { "yes" } else { "no" }
    );
    println!();

    let mut ok_count = 0usize;
    let mut fail_count = 0usize;

    for provider_name in &targets {
        println!("== {} ==", provider_name);
        match run_models_refresh(config, Some(provider_name), force).await {
            Ok(()) => {
                ok_count += 1;
            }
            Err(error) => {
                fail_count += 1;
                println!("  failed: {error}");
            }
        }
        println!();
    }

    println!("Summary: {} succeeded, {} failed", ok_count, fail_count);

    if ok_count == 0 {
        anyhow::bail!("Model refresh failed for all providers")
    }
    Ok(())
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

fn resolve_interactive_onboarding_mode(
    config_path: &Path,
    force: bool,
) -> Result<InteractiveOnboardingMode> {
    if !config_path.exists() {
        return Ok(InteractiveOnboardingMode::FullOnboarding);
    }

    if force {
        println!(
            "  {} Existing config detected at {}. Proceeding with full onboarding because --force was provided.",
            style("!").yellow().bold(),
            style(config_path.display()).yellow()
        );
        return Ok(InteractiveOnboardingMode::FullOnboarding);
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "Refusing to overwrite existing config at {} in non-interactive mode. Re-run with --force if overwrite is intentional.",
            config_path.display()
        );
    }

    let options = [
        "Full onboarding (overwrite config.toml)",
        "Update AI provider/model/API key only (preserve existing configuration)",
        "Cancel",
    ];

    let mode = Select::new()
        .with_prompt(format!(
            "  Existing config found at {}. Select setup mode",
            config_path.display()
        ))
        .items(options)
        .default(1)
        .interact()?;

    match mode {
        0 => Ok(InteractiveOnboardingMode::FullOnboarding),
        1 => Ok(InteractiveOnboardingMode::UpdateProviderOnly),
        _ => bail!("Onboarding canceled: existing configuration was left unchanged."),
    }
}

fn ensure_onboard_overwrite_allowed(config_path: &Path, force: bool) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }

    if force {
        println!(
            "  {} Existing config detected at {}. Proceeding because --force was provided.",
            style("!").yellow().bold(),
            style(config_path.display()).yellow()
        );
        return Ok(());
    }

    #[cfg(test)]
    {
        bail!(
            "Refusing to overwrite existing config at {} in test mode. Re-run with --force if overwrite is intentional.",
            config_path.display()
        );
    }

    #[cfg(not(test))]
    {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!(
                "Refusing to overwrite existing config at {} in non-interactive mode. Re-run with --force if overwrite is intentional.",
                config_path.display()
            );
        }

        let confirmed = Confirm::new()
            .with_prompt(format!(
                "  Existing config found at {}. Re-running onboarding will overwrite config.toml and may create missing workspace files (including BOOTSTRAP.md). Continue?",
                config_path.display()
            ))
            .default(false)
            .interact()?;

        if !confirmed {
            bail!("Onboarding canceled: existing configuration was left unchanged.");
        }

        Ok(())
    }
}

async fn persist_workspace_selection(config_path: &Path) -> Result<()> {
    let config_dir = config_path
        .parent()
        .context("Config path must have a parent directory")?;
    crate::config::schema::persist_active_workspace_config_dir(config_dir)
        .await
        .with_context(|| {
            format!(
                "Failed to persist active workspace selection for {}",
                config_dir.display()
            )
        })
}

// ── Step 1: Workspace ────────────────────────────────────────────

async fn setup_workspace() -> Result<(PathBuf, PathBuf)> {
    let (default_config_dir, default_workspace_dir) =
        crate::config::schema::resolve_runtime_dirs_for_onboarding().await?;

    print_bullet(&format!(
        "Default location: {}",
        style(default_workspace_dir.display()).green()
    ));

    let use_default = Confirm::new()
        .with_prompt("  Use default workspace location?")
        .default(true)
        .interact()?;

    let (config_dir, workspace_dir) = if use_default {
        (default_config_dir, default_workspace_dir)
    } else {
        let custom: String = Input::new()
            .with_prompt("  Enter workspace path")
            .interact_text()?;
        let expanded = shellexpand::tilde(&custom).to_string();
        crate::config::schema::resolve_config_dir_for_workspace(&PathBuf::from(expanded))
    };

    let config_path = config_dir.join("config.toml");

    fs::create_dir_all(&workspace_dir)
        .await
        .context("Failed to create workspace directory")?;

    println!(
        "  {} Workspace: {}",
        style("✓").green().bold(),
        style(workspace_dir.display()).green()
    );

    Ok((workspace_dir, config_path))
}

// ── Step 2: Provider & API Key ───────────────────────────────────

#[allow(clippy::too_many_lines)]
async fn setup_provider(workspace_dir: &Path) -> Result<(String, String, String, Option<String>)> {
    // ── Tier selection ──
    let tiers = vec![
        "⭐ Recommended (OpenRouter, Venice, Anthropic, OpenAI, Gemini)",
        "⚡ Fast inference (Groq, Fireworks, Together AI, NVIDIA NIM)",
        "🌐 Gateway / proxy (Vercel AI, Cloudflare AI, Amazon Bedrock)",
        "🔬 Specialized (Moonshot/Kimi, GLM/Zhipu, MiniMax, Qwen/DashScope, Qianfan, Z.AI, Synthetic, OpenCode Zen, Cohere)",
        "🏠 Local / private (Ollama, llama.cpp server, vLLM — no API key needed)",
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
            (
                "openai-codex",
                "OpenAI Codex (ChatGPT subscription OAuth, no API key)",
            ),
            ("deepseek", "DeepSeek — V3 & R1 (affordable)"),
            ("mistral", "Mistral — Large & Codestral"),
            ("xai", "xAI — Grok 3 & 4"),
            ("perplexity", "Perplexity — search-augmented AI"),
            (
                "gemini",
                "Google Gemini — Gemini 2.0 Flash & Pro (supports CLI auth)",
            ),
        ],
        1 => vec![
            ("groq", "Groq — ultra-fast LPU inference"),
            ("fireworks", "Fireworks AI — fast open-source inference"),
            ("novita", "Novita AI — affordable open-source inference"),
            ("together-ai", "Together AI — open-source model hosting"),
            ("nvidia", "NVIDIA NIM — DeepSeek, Llama, & more"),
        ],
        2 => vec![
            ("vercel", "Vercel AI Gateway"),
            ("cloudflare", "Cloudflare AI Gateway"),
            (
                "astrai",
                "Astrai — compliant AI routing (PII stripping, cost optimization)",
            ),
            (
                "avian",
                "Avian — OpenAI-compatible inference (DeepSeek, Kimi, GLM, MiniMax)",
            ),
            ("bedrock", "Amazon Bedrock — AWS managed models"),
        ],
        3 => vec![
            (
                "kimi-code",
                "Kimi Code — coding-optimized Kimi API (KimiCLI)",
            ),
            (
                "qwen-code",
                "Qwen Code — OAuth tokens reused from ~/.qwen/oauth_creds.json",
            ),
            ("moonshot", "Moonshot — Kimi API (China endpoint)"),
            (
                "moonshot-intl",
                "Moonshot — Kimi API (international endpoint)",
            ),
            ("glm", "GLM — ChatGLM / Zhipu (international endpoint)"),
            ("glm-cn", "GLM — ChatGLM / Zhipu (China endpoint)"),
            (
                "minimax",
                "MiniMax — international endpoint (api.minimax.io)",
            ),
            ("minimax-cn", "MiniMax — China endpoint (api.minimaxi.com)"),
            ("qwen", "Qwen — DashScope China endpoint"),
            ("qwen-intl", "Qwen — DashScope international endpoint"),
            ("qwen-us", "Qwen — DashScope US endpoint"),
            ("qianfan", "Qianfan — Baidu AI models (China endpoint)"),
            ("zai", "Z.AI — global coding endpoint"),
            ("zai-cn", "Z.AI — China coding endpoint (open.bigmodel.cn)"),
            ("synthetic", "Synthetic — Synthetic AI models"),
            ("opencode", "OpenCode Zen — code-focused AI"),
            ("opencode-go", "OpenCode Go — Subsidized code-focused AI"),
            ("cohere", "Cohere — Command R+ & embeddings"),
        ],
        4 => local_provider_choices(),
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
            .default("default")
            .interact_text()?;

        let provider_name = format!("custom:{base_url}");

        println!(
            "  {} Provider: {} | Model: {}",
            style("✓").green().bold(),
            style(&provider_name).green(),
            style(&model).green()
        );

        return Ok((provider_name, api_key, model, None));
    }

    let provider_labels: Vec<&str> = providers.iter().map(|(_, label)| *label).collect();

    let provider_idx = Select::new()
        .with_prompt("  Select your AI provider")
        .items(&provider_labels)
        .default(0)
        .interact()?;

    let provider_name = providers[provider_idx].0;

    // ── API key / endpoint ──
    let mut provider_api_url: Option<String> = None;
    let api_key = if provider_name == "ollama" {
        let use_remote_ollama = Confirm::new()
            .with_prompt("  Use a remote Ollama endpoint (for example Ollama Cloud)?")
            .default(false)
            .interact()?;

        if use_remote_ollama {
            let raw_url: String = Input::new()
                .with_prompt("  Remote Ollama endpoint URL")
                .default("https://ollama.com")
                .interact_text()?;

            let normalized_url = normalize_ollama_endpoint_url(&raw_url);
            if normalized_url.is_empty() {
                anyhow::bail!("Remote Ollama endpoint URL cannot be empty.");
            }
            let parsed = reqwest::Url::parse(&normalized_url)
                .context("Remote Ollama endpoint URL must be a valid URL")?;
            if !matches!(parsed.scheme(), "http" | "https") {
                anyhow::bail!("Remote Ollama endpoint URL must use http:// or https://");
            }

            provider_api_url = Some(normalized_url.clone());

            print_bullet(&format!(
                "Remote endpoint configured: {}",
                style(&normalized_url).cyan()
            ));
            if raw_url.trim().trim_end_matches('/') != normalized_url {
                print_bullet("Normalized endpoint to base URL (removed trailing /api).");
            }
            print_bullet(&format!(
                "If you use cloud-only models, append {} to the model ID.",
                style(":cloud").yellow()
            ));

            let key: String = Input::new()
                .with_prompt("  API key for remote Ollama endpoint (or Enter to skip)")
                .allow_empty(true)
                .interact_text()?;

            if key.trim().is_empty() {
                print_bullet(&format!(
                    "No API key provided. Set {} later if required by your endpoint.",
                    style("OLLAMA_API_KEY").yellow()
                ));
            }

            key
        } else {
            print_bullet("Using local Ollama at http://localhost:11434 (no API key needed).");
            String::new()
        }
    } else if matches!(provider_name, "llamacpp" | "llama.cpp") {
        let raw_url: String = Input::new()
            .with_prompt("  llama.cpp server endpoint URL")
            .default("http://localhost:8080/v1")
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("llama.cpp endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using llama.cpp server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your llama.cpp server is started with --api-key.");

        let key: String = Input::new()
            .with_prompt("  API key for llama.cpp server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("LLAMACPP_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "sglang" {
        let raw_url: String = Input::new()
            .with_prompt("  SGLang server endpoint URL")
            .default("http://localhost:30000/v1")
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("SGLang endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using SGLang server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your SGLang server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for SGLang server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("SGLANG_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "vllm" {
        let raw_url: String = Input::new()
            .with_prompt("  vLLM server endpoint URL")
            .default("http://localhost:8000/v1")
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("vLLM endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using vLLM server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your vLLM server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for vLLM server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("VLLM_API_KEY").yellow()
            ));
        }

        key
    } else if provider_name == "osaurus" {
        let raw_url: String = Input::new()
            .with_prompt("  Osaurus server endpoint URL")
            .default("http://localhost:1337/v1")
            .interact_text()?;

        let normalized_url = raw_url.trim().trim_end_matches('/').to_string();
        if normalized_url.is_empty() {
            anyhow::bail!("Osaurus endpoint URL cannot be empty.");
        }
        provider_api_url = Some(normalized_url.clone());

        print_bullet(&format!(
            "Using Osaurus server endpoint: {}",
            style(&normalized_url).cyan()
        ));
        print_bullet("No API key needed unless your Osaurus server requires authentication.");

        let key: String = Input::new()
            .with_prompt("  API key for Osaurus server (or Enter to skip)")
            .allow_empty(true)
            .interact_text()?;

        if key.trim().is_empty() {
            print_bullet(&format!(
                "No API key provided. Set {} later only if your server requires authentication.",
                style("OSAURUS_API_KEY").yellow()
            ));
        }

        key
    } else if canonical_provider_name(provider_name) == "gemini" {
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
    } else if canonical_provider_name(provider_name) == "anthropic" {
        if std::env::var("ANTHROPIC_OAUTH_TOKEN").is_ok() {
            print_bullet(&format!(
                "{} ANTHROPIC_OAUTH_TOKEN environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            print_bullet(&format!(
                "{} ANTHROPIC_API_KEY environment variable detected!",
                style("✓").green().bold()
            ));
            String::new()
        } else {
            print_bullet(&format!(
                "Get your API key at: {}",
                style("https://console.anthropic.com/settings/keys")
                    .cyan()
                    .underlined()
            ));
            print_bullet("Or run `claude setup-token` to get an OAuth setup-token.");
            println!();

            let key: String = Input::new()
                .with_prompt("  Paste your API key or setup-token (or press Enter to skip)")
                .allow_empty(true)
                .interact_text()?;

            if key.is_empty() {
                print_bullet(&format!(
                    "Skipped. Set {} or {} or edit config.toml later.",
                    style("ANTHROPIC_API_KEY").yellow(),
                    style("ANTHROPIC_OAUTH_TOKEN").yellow()
                ));
            }

            key
        }
    } else {
        let key_url = match provider_name {
            "openrouter" => "https://openrouter.ai/keys",
            "openai" => "https://platform.openai.com/api-keys",
            "gemini" => "https://aistudio.google.com/app/apikey",
            _ => "",
        };

        println!();
        {
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
        }
    };

    // ── Model selection ──
    let canonical_provider = canonical_provider_name(provider_name);
    let mut model_options: Vec<(String, String)> = curated_models_for_provider(canonical_provider);

    let mut live_options: Option<Vec<(String, String)>> = None;

    if supports_live_model_fetch(provider_name) {
        let ollama_remote = canonical_provider == "ollama"
            && ollama_uses_remote_endpoint(provider_api_url.as_deref());
        let can_fetch_without_key =
            allows_unauthenticated_model_fetch(provider_name) && !ollama_remote;
        let has_api_key = !api_key.trim().is_empty()
            || ((canonical_provider != "ollama" || ollama_remote)
                && std::env::var(provider_env_var(provider_name))
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()))
            || (provider_name == "minimax"
                && std::env::var("MINIMAX_OAUTH_TOKEN")
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()));

        if canonical_provider == "ollama" && ollama_remote && !has_api_key {
            print_bullet(&format!(
                "Remote Ollama live-model refresh needs an API key ({}); using curated models.",
                style("OLLAMA_API_KEY").yellow()
            ));
        }

        if can_fetch_without_key || has_api_key {
            if let Some(cached) =
                load_cached_models_for_provider(workspace_dir, provider_name, MODEL_CACHE_TTL_SECS)
                    .await?
            {
                let shown_count = cached.models.len().min(LIVE_MODEL_MAX_OPTIONS);
                print_bullet(&format!(
                    "Found cached models ({shown_count}) updated {} ago.",
                    humanize_age(cached.age_secs)
                ));

                live_options = Some(build_model_options(
                    cached
                        .models
                        .into_iter()
                        .take(LIVE_MODEL_MAX_OPTIONS)
                        .collect(),
                    "cached",
                ));
            }

            let should_fetch_now = Confirm::new()
                .with_prompt(if live_options.is_some() {
                    "  Refresh models from provider now?"
                } else {
                    "  Fetch latest models from provider now?"
                })
                .default(live_options.is_none())
                .interact()?;

            if should_fetch_now {
                match fetch_live_models_for_provider(
                    provider_name,
                    &api_key,
                    provider_api_url.as_deref(),
                )
                .await
                {
                    Ok(live_model_ids) if !live_model_ids.is_empty() => {
                        cache_live_models_for_provider(
                            workspace_dir,
                            provider_name,
                            &live_model_ids,
                        )
                        .await?;

                        let fetched_count = live_model_ids.len();
                        let shown_count = fetched_count.min(LIVE_MODEL_MAX_OPTIONS);
                        let shown_models: Vec<String> = live_model_ids
                            .into_iter()
                            .take(LIVE_MODEL_MAX_OPTIONS)
                            .collect();

                        if shown_count < fetched_count {
                            print_bullet(&format!(
                                "Fetched {fetched_count} models. Showing first {shown_count}."
                            ));
                        } else {
                            print_bullet(&format!("Fetched {shown_count} live models."));
                        }

                        live_options = Some(build_model_options(shown_models, "live"));
                    }
                    Ok(_) => {
                        print_bullet("Provider returned no models; using curated list.");
                    }
                    Err(error) => {
                        print_bullet(&format!(
                            "Live fetch failed ({}); using cached/curated list.",
                            style(error.to_string()).yellow()
                        ));

                        if live_options.is_none() {
                            if let Some(stale) =
                                load_any_cached_models_for_provider(workspace_dir, provider_name)
                                    .await?
                            {
                                print_bullet(&format!(
                                    "Loaded stale cache from {} ago.",
                                    humanize_age(stale.age_secs)
                                ));

                                live_options = Some(build_model_options(
                                    stale
                                        .models
                                        .into_iter()
                                        .take(LIVE_MODEL_MAX_OPTIONS)
                                        .collect(),
                                    "stale-cache",
                                ));
                            }
                        }
                    }
                }
            }
        } else {
            print_bullet("No API key detected, so using curated model list.");
            print_bullet("Tip: add an API key and rerun onboarding to fetch live models.");
        }
    }

    if let Some(live_model_options) = live_options {
        let source_options = vec![
            format!("Provider model list ({})", live_model_options.len()),
            format!("Curated starter list ({})", model_options.len()),
        ];

        let source_idx = Select::new()
            .with_prompt("  Model source")
            .items(&source_options)
            .default(0)
            .interact()?;

        if source_idx == 0 {
            model_options = live_model_options;
        }
    }

    if model_options.is_empty() {
        model_options.push((
            default_model_for_provider(provider_name),
            "Provider default model".to_string(),
        ));
    }

    model_options.push((
        CUSTOM_MODEL_SENTINEL.to_string(),
        "Custom model ID (type manually)".to_string(),
    ));

    let model_labels: Vec<String> = model_options
        .iter()
        .map(|(model_id, label)| format!("{label} — {}", style(model_id).dim()))
        .collect();

    let model_idx = Select::new()
        .with_prompt("  Select your default model")
        .items(&model_labels)
        .default(0)
        .interact()?;

    let selected_model = model_options[model_idx].0.clone();
    let model = if selected_model == CUSTOM_MODEL_SENTINEL {
        Input::new()
            .with_prompt("  Enter custom model ID")
            .default(default_model_for_provider(provider_name))
            .interact_text()?
    } else {
        selected_model
    };

    println!(
        "  {} Provider: {} | Model: {}",
        style("✓").green().bold(),
        style(provider_name).green(),
        style(&model).green()
    );

    Ok((provider_name.to_string(), api_key, model, provider_api_url))
}

fn local_provider_choices() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ollama", "Ollama — local models (Llama, Mistral, Phi)"),
        (
            "llamacpp",
            "llama.cpp server — local OpenAI-compatible endpoint",
        ),
        (
            "sglang",
            "SGLang — high-performance local serving framework",
        ),
        ("vllm", "vLLM — high-performance local inference engine"),
        (
            "osaurus",
            "Osaurus — unified AI edge runtime (local MLX + cloud proxy + MCP)",
        ),
    ]
}

/// Map provider name to its conventional env var
fn provider_env_var(name: &str) -> &'static str {
    match canonical_provider_name(name) {
        "openrouter" => "OPENROUTER_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GEMINI_API_KEY",
        _ => "API_KEY",
    }
}

fn provider_supports_keyless_local_usage(_provider_name: &str) -> bool {
    false
}

fn provider_supports_device_flow(provider_name: &str) -> bool {
    matches!(
        canonical_provider_name(provider_name),
        "copilot" | "gemini" | "openai-codex"
    )
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

// ── Step 6: Hardware (Physical World) ───────────────────────────

fn setup_hardware() -> Result<HardwareConfig> {
    print_bullet("ZeroClaw can talk to physical hardware (LEDs, sensors, motors).");
    print_bullet("Scanning for connected devices...");
    println!();

    // ── Auto-discovery ──
    let devices = hardware::discover_hardware();

    if devices.is_empty() {
        println!(
            "  {} {}",
            style("ℹ").dim(),
            style("No hardware devices detected on this system.").dim()
        );
        println!(
            "  {} {}",
            style("ℹ").dim(),
            style("You can enable hardware later in config.toml under [hardware].").dim()
        );
    } else {
        println!(
            "  {} {} device(s) found:",
            style("✓").green().bold(),
            devices.len()
        );
        for device in &devices {
            let detail = device
                .detail
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default();
            let path = device
                .device_path
                .as_deref()
                .map(|p| format!(" → {p}"))
                .unwrap_or_default();
            println!(
                "    {} {}{}{} [{}]",
                style("›").cyan(),
                style(&device.name).green(),
                style(&detail).dim(),
                style(&path).dim(),
                style(device.transport.to_string()).cyan()
            );
        }
    }
    println!();

    let options = vec![
        "🚀 Native — direct GPIO on this Linux board (Raspberry Pi, Orange Pi, etc.)",
        "🔌 Tethered — control an Arduino/ESP32/Nucleo plugged into USB",
        "🔬 Debug Probe — flash/read MCUs via SWD/JTAG (probe-rs)",
        "☁️  Software Only — no hardware access (default)",
    ];

    let recommended = hardware::recommended_wizard_default(&devices);

    let choice = Select::new()
        .with_prompt("  How should ZeroClaw interact with the physical world?")
        .items(&options)
        .default(recommended)
        .interact()?;

    let mut hw_config = hardware::config_from_wizard_choice(choice, &devices);

    // ── Serial: pick a port if multiple found ──
    if hw_config.transport_mode() == hardware::HardwareTransport::Serial {
        let serial_devices: Vec<&hardware::DiscoveredDevice> = devices
            .iter()
            .filter(|d| d.transport == hardware::HardwareTransport::Serial)
            .collect();

        if serial_devices.len() > 1 {
            let port_labels: Vec<String> = serial_devices
                .iter()
                .map(|d| {
                    format!(
                        "{} ({})",
                        d.device_path.as_deref().unwrap_or("unknown"),
                        d.name
                    )
                })
                .collect();

            let port_idx = Select::new()
                .with_prompt("  Multiple serial devices found — select one")
                .items(&port_labels)
                .default(0)
                .interact()?;

            hw_config.serial_port = serial_devices[port_idx].device_path.clone();
        } else if serial_devices.is_empty() {
            // User chose serial but no device discovered — ask for manual path
            let manual_port: String = Input::new()
                .with_prompt("  Serial port path (e.g. /dev/ttyUSB0)")
                .default("/dev/ttyUSB0")
                .interact_text()?;
            hw_config.serial_port = Some(manual_port);
        }

        // Baud rate
        let baud_options = vec![
            "115200 (default, recommended)",
            "9600 (legacy Arduino)",
            "57600",
            "230400",
            "Custom",
        ];
        let baud_idx = Select::new()
            .with_prompt("  Serial baud rate")
            .items(&baud_options)
            .default(0)
            .interact()?;

        hw_config.baud_rate = match baud_idx {
            1 => 9600,
            2 => 57600,
            3 => 230_400,
            4 => {
                let custom: String = Input::new()
                    .with_prompt("  Custom baud rate")
                    .default("115200")
                    .interact_text()?;
                custom.parse::<u32>().unwrap_or(115_200)
            }
            _ => 115_200,
        };
    }

    // ── Probe: ask for target chip ──
    if hw_config.transport_mode() == hardware::HardwareTransport::Probe
        && hw_config.probe_target.is_none()
    {
        let target: String = Input::new()
            .with_prompt("  Target MCU chip (e.g. STM32F411CEUx, nRF52840_xxAA)")
            .default("STM32F411CEUx")
            .interact_text()?;
        hw_config.probe_target = Some(target);
    }

    // ── Datasheet RAG ──
    if hw_config.enabled {
        let datasheets = Confirm::new()
            .with_prompt("  Enable datasheet RAG? (index PDF schematics for AI pin lookups)")
            .default(true)
            .interact()?;
        hw_config.workspace_datasheets = datasheets;
    }

    // ── Summary ──
    if hw_config.enabled {
        let transport_label = match hw_config.transport_mode() {
            hardware::HardwareTransport::Native => "Native GPIO".to_string(),
            hardware::HardwareTransport::Serial => format!(
                "Serial → {} @ {} baud",
                hw_config.serial_port.as_deref().unwrap_or("?"),
                hw_config.baud_rate
            ),
            hardware::HardwareTransport::Probe => format!(
                "Probe (SWD/JTAG) → {}",
                hw_config.probe_target.as_deref().unwrap_or("?")
            ),
            hardware::HardwareTransport::None => "Software Only".to_string(),
        };

        println!(
            "  {} Hardware: {} | datasheets: {}",
            style("✓").green().bold(),
            style(&transport_label).green(),
            if hw_config.workspace_datasheets {
                style("on").green().to_string()
            } else {
                style("off").dim().to_string()
            }
        );
    } else {
        println!(
            "  {} Hardware: {}",
            style("✓").green().bold(),
            style("disabled (software only)").dim()
        );
    }

    Ok(hw_config)
}

// ── Step 6: Project Context ─────────────────────────────────────

fn setup_project_context() -> Result<ProjectContext> {
    print_bullet("Let's personalize your agent. You can always update these later.");
    print_bullet("Press Enter to accept defaults.");
    println!();

    let user_name: String = Input::new()
        .with_prompt("  Your name")
        .default("User")
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
            .default("UTC")
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
        .default("ZeroClaw")
        .interact_text()?;

    let style_options = vec![
        "Direct & concise — skip pleasantries, get to the point",
        "Friendly & casual — warm, human, and helpful",
        "Professional & polished — calm, confident, and clear",
        "Expressive & playful — more personality + natural emojis",
        "Technical & detailed — thorough explanations, code-first",
        "Balanced — adapt to the situation",
        "Custom — write your own style guide",
    ];

    let style_idx = Select::new()
        .with_prompt("  Communication style")
        .items(&style_options)
        .default(1)
        .interact()?;

    let communication_style = match style_idx {
        0 => "Be direct and concise. Skip pleasantries. Get to the point.".to_string(),
        1 => "Be friendly, human, and conversational. Show warmth and empathy while staying efficient. Use natural contractions.".to_string(),
        2 => "Be professional and polished. Stay calm, structured, and respectful. Use occasional tone-setting emojis only when appropriate.".to_string(),
        3 => "Be expressive and playful when appropriate. Use relevant emojis naturally (0-2 max), and keep serious topics emoji-light.".to_string(),
        4 => "Be technical and detailed. Thorough explanations, code-first.".to_string(),
        5 => "Adapt to the situation. Default to warm and clear communication; be concise when needed, thorough when it matters.".to_string(),
        _ => Input::new()
            .with_prompt("  Custom communication style")
            .default(
                "Be warm, natural, and clear. Use occasional relevant emojis (1-2 max) and avoid robotic phrasing.",
            )
            .interact_text()?,
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

// ── Step 6: Memory Configuration ───────────────────────────────

fn setup_memory() -> Result<MemoryConfig> {
    print_bullet("Choose how ZeroClaw stores and searches memories.");
    print_bullet("You can always change this later in config.toml.");
    println!();

    let options: Vec<&str> = selectable_memory_backends()
        .iter()
        .map(|backend| backend.label)
        .collect();

    let choice = Select::new()
        .with_prompt("  Select memory backend")
        .items(&options)
        .default(0)
        .interact()?;

    let backend = backend_key_from_choice(choice);
    let profile = memory_backend_profile(backend);

    let auto_save = profile.auto_save_default
        && Confirm::new()
            .with_prompt("  Auto-save conversations to memory?")
            .default(true)
            .interact()?;

    println!(
        "  {} Memory: {} (auto-save: {})",
        style("✓").green().bold(),
        style(backend).green(),
        if auto_save { "on" } else { "off" }
    );

    let mut config = memory_config_defaults_for_backend(backend);
    config.auto_save = auto_save;
    Ok(config)
}

// ── Step 3: Channels ────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelMenuChoice {
    Telegram,
    Slack,
    Done,
}

const CHANNEL_MENU_CHOICES: &[ChannelMenuChoice] = &[
    ChannelMenuChoice::Telegram,
    ChannelMenuChoice::Slack,
    ChannelMenuChoice::Done,
];

fn channel_menu_choices() -> &'static [ChannelMenuChoice] {
    CHANNEL_MENU_CHOICES
}

#[allow(clippy::too_many_lines)]
fn setup_channels() -> Result<ChannelsConfig> {
    print_bullet("Channels let you talk to ZeroClaw from anywhere.");
    print_bullet("CLI is always available. Connect more channels now.");
    println!();

    let mut config = ChannelsConfig::default();
    let menu_choices = channel_menu_choices();

    loop {
        let options: Vec<String> = menu_choices
            .iter()
            .map(|choice| match choice {
                ChannelMenuChoice::Telegram => format!(
                    "Telegram   {}",
                    if config.telegram.is_some() {
                        "✅ connected"
                    } else {
                        "— connect your bot"
                    }
                ),
                ChannelMenuChoice::Slack => format!(
                    "Slack      {}",
                    if config.slack.is_some() {
                        "✅ connected"
                    } else {
                        "— connect your bot"
                    }
                ),
                ChannelMenuChoice::Done => "Done — finish setup".to_string(),
            })
            .collect();

        let selection = Select::new()
            .with_prompt("  Connect a channel (or Done to continue)")
            .items(&options)
            .default(options.len() - 1)
            .interact()?;

        let choice = menu_choices
            .get(selection)
            .copied()
            .unwrap_or(ChannelMenuChoice::Done);

        match choice {
            ChannelMenuChoice::Telegram => {
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

                // Test connection (run entirely in separate thread — reqwest::blocking Response
                // must be used and dropped there to avoid "Cannot drop a runtime" panic)
                print!("  {} Testing connection... ", style("⏳").dim());
                let token_clone = token.clone();
                let thread_result = std::thread::spawn(move || {
                    let client = reqwest::blocking::Client::new();
                    let url = format!("https://api.telegram.org/bot{token_clone}/getMe");
                    let resp = client.get(&url).send()?;
                    let ok = resp.status().is_success();
                    let data: serde_json::Value = resp.json().unwrap_or_default();
                    let bot_name = data
                        .get("result")
                        .and_then(|r| r.get("username"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_string();
                    Ok::<_, reqwest::Error>((ok, bot_name))
                })
                .join();
                match thread_result {
                    Ok(Ok((true, bot_name))) => {
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

                print_bullet(
                    "Allowlist your own Telegram identity first (recommended for secure + fast setup).",
                );
                print_bullet(
                    "Use your @username without '@' (example: argenis), or your numeric Telegram user ID.",
                );
                print_bullet("Use '*' only for temporary open testing.");

                let users_str: String = Input::new()
                    .with_prompt(
                        "  Allowed Telegram identities (comma-separated: username without '@' and/or numeric user ID, '*' for all)",
                    )
                    .allow_empty(true)
                    .interact_text()?;

                let allowed_users = if users_str.trim() == "*" {
                    vec!["*".into()]
                } else {
                    users_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                };

                if allowed_users.is_empty() {
                    println!(
                        "  {} No users allowlisted — Telegram inbound messages will be denied until you add your username/user ID or '*'.",
                        style("⚠").yellow().bold()
                    );
                }

                config.telegram = Some(TelegramConfig {
                    bot_token: token,
                    allowed_users,
                    stream_mode: StreamMode::default(),
                    draft_update_interval_ms: 1000,
                    interrupt_on_new_message: false,
                    mention_only: false,
                    ack_reactions: None,
                    proxy_url: None,
                });
            }
            ChannelMenuChoice::Slack => {
                println!();
                println!(
                    "  {} {}",
                    style("Slack Setup").white().bold(),
                    style("— talk to ZeroClaw from Slack").dim()
                );
                print_bullet("1. Create a Slack App at https://api.slack.com/apps");
                print_bullet("2. Enable Socket Mode and get an app-level token (xapp-...)");
                print_bullet(
                    "3. Install the app to your workspace and get the bot token (xoxb-...)",
                );
                println!();

                let bot_token: String = Input::new()
                    .with_prompt("  Bot token (xoxb-...)")
                    .interact_text()?;

                if bot_token.trim().is_empty() {
                    println!("  {} Skipped", style("→").dim());
                    continue;
                }

                let app_token: String = Input::new()
                    .with_prompt("  App-level token for Socket Mode (xapp-..., or Enter to skip)")
                    .default(String::new())
                    .interact_text()?;

                config.slack = Some(SlackConfig {
                    bot_token: bot_token.trim().to_string(),
                    app_token: if app_token.trim().is_empty() {
                        None
                    } else {
                        Some(app_token.trim().to_string())
                    },
                    channel_id: None,
                    channel_ids: Vec::new(),
                    allowed_users: vec!["*".to_string()],
                    interrupt_on_new_message: false,
                    thread_replies: Some(true),
                    mention_only: false,
                    use_markdown_blocks: false,
                    proxy_url: None,
                    stream_drafts: false,
                    draft_update_interval_ms: 1200,
                    cancel_reaction: None,
                });
            }
            ChannelMenuChoice::Done => break,
        }
        println!();
    }

    // Summary line
    let channels = config.channels();
    let channels = channels
        .iter()
        .filter_map(|(channel, ok)| ok.then_some(channel.name()));
    let channels: Vec<_> = std::iter::once("Cli").chain(channels).collect();
    let active = channels.join(", ");

    println!(
        "  {} Channels: {}",
        style("✓").green().bold(),
        style(active).green()
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
            let tunnel_value: String = Input::new()
                .with_prompt("  Cloudflare tunnel token")
                .interact_text()?;
            if tunnel_value.trim().is_empty() {
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
                    cloudflare: Some(CloudflareTunnelConfig {
                        token: tunnel_value,
                    }),
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
async fn scaffold_workspace(
    workspace_dir: &Path,
    ctx: &ProjectContext,
    memory_backend: &str,
) -> Result<()> {
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
        "Be warm, natural, and clear. Use occasional relevant emojis (1-2 max) and avoid robotic phrasing."
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

    let memory_guidance = if memory_backend == "none" {
        "## Memory System\n\n\
         memory.backend = \"none\" — persistent memory is disabled.\n\
         No daily notes or MEMORY.md will be created or injected.\n\
         All context exists only within the current session.\n\n"
            .to_string()
    } else {
        "## Memory System\n\n\
         You wake up fresh each session. These files ARE your continuity:\n\n\
         - **Daily notes:** `memory/YYYY-MM-DD.md` — raw logs (accessed via memory tools)\n\
         - **Long-term:** `MEMORY.md` — curated memories (auto-injected in main session)\n\n\
         Capture what matters. Decisions, context, things to remember.\n\
         Skip secrets unless asked to keep them.\n\n"
            .to_string()
    };

    let session_steps = if memory_backend == "none" {
        "1. Read `SOUL.md` — this is who you are\n\
         2. Read `USER.md` — this is who you're helping\n\n"
    } else {
        "1. Read `SOUL.md` — this is who you are\n\
         2. Read `USER.md` — this is who you're helping\n\
         3. Use `memory_recall` for recent context (daily notes are on-demand)\n\
         4. If in MAIN SESSION (direct chat): `MEMORY.md` is already injected\n\n"
    };

    let agents = format!(
        "# AGENTS.md — {agent} Personal Assistant\n\n\
         ## Every Session (required)\n\n\
         Before doing anything else:\n\n\
         {session_steps}\
         Don't ask permission. Just do it.\n\n\
         {memory_guidance}\
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
         Skills are listed in the system prompt. Use `use_skill` to invoke a skill.\n\
         Keep local notes (SSH hosts, device names, etc.) in `TOOLS.md`.\n\n\
         ## Crash Recovery\n\n\
         - If a run stops unexpectedly, recover context before acting.\n\
         - Check `MEMORY.md` + latest `memory/*.md` notes to avoid duplicate work.\n\
         - Resume from the last confirmed step, not from scratch.\n\n\
         ## Sub-task Scoping\n\n\
         - Break complex work into focused sub-tasks with clear success criteria.\n\
         - Keep sub-tasks small, verify each output, then merge results.\n\
         - Prefer one clear objective per sub-task over broad \"do everything\" asks.\n\n\
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
         - Sound like a real person, not a support script.\n\
         - Mirror the user's energy: calm when serious, upbeat when casual.\n\
         - Use emojis naturally (0-2 max when they help tone, not every sentence).\n\
         - Match emoji density to the user. Formal user => minimal/no emojis.\n\
         - Prefer specific, grounded phrasing over generic filler.\n\n\
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
           - Use when: running local checks, build/test commands, or diagnostics.\n\
           - Don't use when: a safer dedicated tool exists, or command is destructive without approval.\n\
         - **file_read** — Read file contents\n\
           - Use when: inspecting project files, configs, or logs.\n\
           - Don't use when: you only need a quick string search (prefer targeted search first).\n\
         - **file_write** — Write file contents\n\
           - Use when: applying focused edits, scaffolding files, or updating docs/code.\n\
           - Don't use when: unsure about side effects or when the file should remain user-owned.\n\
         - **memory_store** — Save to memory\n\
           - Use when: preserving durable preferences, decisions, or key context.\n\
           - Don't use when: info is transient, noisy, or sensitive without explicit need.\n\
         - **memory_recall** — Search memory\n\
           - Use when: you need prior decisions, user preferences, or historical context.\n\
           - Don't use when: the answer is already in current files/conversation.\n\
         - **memory_forget** — Delete a memory entry\n\
           - Use when: memory is incorrect, stale, or explicitly requested to be removed.\n\
           - Don't use when: uncertain about impact; verify before deleting.\n\n\
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

    let mut files: Vec<(&str, String)> = vec![
        ("IDENTITY.md", identity),
        ("AGENTS.md", agents),
        ("HEARTBEAT.md", heartbeat),
        ("SOUL.md", soul),
        ("USER.md", user_md),
        ("TOOLS.md", tools.to_string()),
        ("BOOTSTRAP.md", bootstrap),
    ];
    if memory_backend != "none" {
        files.push(("MEMORY.md", memory.to_string()));
    }

    // Create subdirectories
    let subdirs = ["sessions", "memory", "state", "cron", "skills"];
    for dir in &subdirs {
        fs::create_dir_all(workspace_dir.join(dir)).await?;
    }

    let mut created = 0;
    let mut skipped = 0;

    for (filename, content) in &files {
        let path = workspace_dir.join(filename);
        if path.exists() {
            skipped += 1;
        } else {
            fs::write(&path, content).await?;
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
    let has_channels = has_launchable_channels(&config.channels_config);

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
    let channels = config.channels_config.channels();
    let channels = channels
        .iter()
        .filter_map(|(channel, ok)| ok.then_some(channel.name()));
    let channels: Vec<_> = std::iter::once("Cli").chain(channels).collect();

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
    println!("    {} Secrets:       configured", style("🔒").cyan());

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

    // Hardware
    println!(
        "    {} Hardware:      {}",
        style("🔌").cyan(),
        if config.hardware.enabled {
            let mode = config.hardware.transport_mode();
            match mode {
                hardware::HardwareTransport::Native => {
                    style("Native GPIO (direct)").green().to_string()
                }
                hardware::HardwareTransport::Serial => format!(
                    "{}",
                    style(format!(
                        "Serial → {} @ {} baud",
                        config.hardware.serial_port.as_deref().unwrap_or("?"),
                        config.hardware.baud_rate
                    ))
                    .green()
                ),
                hardware::HardwareTransport::Probe => format!(
                    "{}",
                    style(format!(
                        "Probe → {}",
                        config.hardware.probe_target.as_deref().unwrap_or("?")
                    ))
                    .green()
                ),
                hardware::HardwareTransport::None => "disabled (software only)".to_string(),
            }
        } else {
            "disabled (software only)".to_string()
        }
    );

    println!();
    println!("  {}", style("Next steps:").white().bold());
    println!();

    let mut step = 1u8;

    let provider = config.default_provider.as_deref().unwrap_or("openrouter");
    if config.api_key.is_none() && !provider_supports_keyless_local_usage(provider) {
        if provider == "openai-codex" {
            println!(
                "    {} Authenticate OpenAI Codex:",
                style(format!("{step}.")).cyan().bold()
            );
            println!(
                "       {}",
                style("zeroclaw auth login --provider openai-codex --device-code").yellow()
            );
        } else if provider == "anthropic" {
            println!(
                "    {} Configure Anthropic auth:",
                style(format!("{step}.")).cyan().bold()
            );
            println!(
                "       {}",
                style("export ANTHROPIC_API_KEY=\"sk-ant-...\"").yellow()
            );
            println!(
                "       {}",
                style(
                    "or: zeroclaw auth paste-token --provider anthropic --auth-kind authorization"
                )
                .yellow()
            );
        } else {
            let env_var = provider_env_var(provider);
            println!(
                "    {} Set your API key:",
                style(format!("{step}.")).cyan().bold()
            );
            println!(
                "       {}",
                style(format!("export {env_var}=\"sk-...\"")).yellow()
            );
        }
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
    use serde_json::json;
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::set_var(self.key, previous) };
            } else {
                // SAFETY: test-only, single-threaded test runner.
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    // ── ProjectContext defaults ──────────────────────────────────

    #[test]
    fn project_context_default_is_empty() {
        let ctx = ProjectContext::default();
        assert!(ctx.user_name.is_empty());
        assert!(ctx.timezone.is_empty());
        assert!(ctx.agent_name.is_empty());
        assert!(ctx.communication_style.is_empty());
    }

    #[test]
    fn apply_provider_update_preserves_non_provider_settings() {
        let mut config = Config::default();
        config.default_temperature = 1.23;
        config.memory.backend = "markdown".to_string();
        config.skills.open_skills_enabled = true;
        config.channels_config.cli = false;

        apply_provider_update(
            &mut config,
            "openrouter".to_string(),
            "sk-updated".to_string(),
            "openai/gpt-5.2".to_string(),
            Some("https://openrouter.ai/api/v1".to_string()),
        );

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt-5.2"));
        assert_eq!(config.api_key.as_deref(), Some("sk-updated"));
        assert_eq!(
            config.api_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(config.default_temperature, 1.23);
        assert_eq!(config.memory.backend, "markdown");
        assert!(config.skills.open_skills_enabled);
        assert!(!config.channels_config.cli);
    }

    #[test]
    fn apply_provider_update_clears_api_key_when_empty() {
        let mut config = Config::default();
        config.api_key = Some("sk-old".to_string());

        apply_provider_update(
            &mut config,
            "anthropic".to_string(),
            String::new(),
            "claude-sonnet-4-5-20250929".to_string(),
            None,
        );

        assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(
            config.default_model.as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );
        assert!(config.api_key.is_none());
        assert!(config.api_url.is_none());
    }

    #[tokio::test]
    async fn quick_setup_model_override_persists_to_config_toml() {
        let _env_guard = env_lock().lock().await;
        let _workspace_env = EnvVarGuard::unset("ZEROCLAW_WORKSPACE");
        let _config_env = EnvVarGuard::unset("ZEROCLAW_CONFIG_DIR");
        let tmp = TempDir::new().unwrap();

        let config = Box::pin(run_quick_setup_with_home(
            Some("sk-issue946"),
            Some("openrouter"),
            Some("custom-model-946"),
            Some("sqlite"),
            false,
            tmp.path(),
        ))
        .await
        .unwrap();

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert_eq!(config.default_model.as_deref(), Some("custom-model-946"));
        assert_eq!(config.api_key.as_deref(), Some("sk-issue946"));

        let config_raw = tokio::fs::read_to_string(config.config_path).await.unwrap();
        assert!(config_raw.contains("default_provider = \"openrouter\""));
        assert!(config_raw.contains("default_model = \"custom-model-946\""));
    }

    #[tokio::test]
    async fn quick_setup_without_model_uses_provider_default_model() {
        let _env_guard = env_lock().lock().await;
        let _workspace_env = EnvVarGuard::unset("ZEROCLAW_WORKSPACE");
        let _config_env = EnvVarGuard::unset("ZEROCLAW_CONFIG_DIR");
        let tmp = TempDir::new().unwrap();

        let config = Box::pin(run_quick_setup_with_home(
            Some("sk-issue946"),
            Some("anthropic"),
            None,
            Some("sqlite"),
            false,
            tmp.path(),
        ))
        .await
        .unwrap();

        let expected = default_model_for_provider("anthropic");
        assert_eq!(config.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(config.default_model.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn quick_setup_existing_config_requires_force_when_non_interactive() {
        let _env_guard = env_lock().lock().await;
        let _workspace_env = EnvVarGuard::unset("ZEROCLAW_WORKSPACE");
        let _config_env = EnvVarGuard::unset("ZEROCLAW_CONFIG_DIR");
        let tmp = TempDir::new().unwrap();
        let zeroclaw_dir = tmp.path().join(".zeroclaw");
        let config_path = zeroclaw_dir.join("config.toml");

        tokio::fs::create_dir_all(&zeroclaw_dir).await.unwrap();
        tokio::fs::write(&config_path, "default_provider = \"openrouter\"\n")
            .await
            .unwrap();

        let err = Box::pin(run_quick_setup_with_home(
            Some("sk-existing"),
            Some("openrouter"),
            Some("custom-model"),
            Some("sqlite"),
            false,
            tmp.path(),
        ))
        .await
        .expect_err("quick setup should refuse overwrite without --force");

        let err_text = err.to_string();
        assert!(err_text.contains("Refusing to overwrite existing config"));
        assert!(err_text.contains("--force"));
    }

    #[tokio::test]
    async fn quick_setup_existing_config_overwrites_with_force() {
        let _env_guard = env_lock().lock().await;
        let _workspace_env = EnvVarGuard::unset("ZEROCLAW_WORKSPACE");
        let _config_env = EnvVarGuard::unset("ZEROCLAW_CONFIG_DIR");
        let tmp = TempDir::new().unwrap();
        let zeroclaw_dir = tmp.path().join(".zeroclaw");
        let config_path = zeroclaw_dir.join("config.toml");

        tokio::fs::create_dir_all(&zeroclaw_dir).await.unwrap();
        tokio::fs::write(
            &config_path,
            "default_provider = \"anthropic\"\ndefault_model = \"stale-model\"\n",
        )
        .await
        .unwrap();

        let config = Box::pin(run_quick_setup_with_home(
            Some("sk-force"),
            Some("openrouter"),
            Some("custom-model-fresh"),
            Some("sqlite"),
            true,
            tmp.path(),
        ))
        .await
        .expect("quick setup should overwrite existing config with --force");

        assert_eq!(config.default_provider.as_deref(), Some("openrouter"));
        assert_eq!(config.default_model.as_deref(), Some("custom-model-fresh"));
        assert_eq!(config.api_key.as_deref(), Some("sk-force"));

        let config_raw = tokio::fs::read_to_string(config.config_path).await.unwrap();
        assert!(config_raw.contains("default_provider = \"openrouter\""));
        assert!(config_raw.contains("default_model = \"custom-model-fresh\""));
    }

    #[tokio::test]
    async fn quick_setup_respects_zero_claw_workspace_env_layout() {
        let _env_guard = env_lock().lock().await;
        let tmp = TempDir::new().unwrap();
        let workspace_root = tmp.path().join("zeroclaw-data");
        let workspace_dir = workspace_root.join("workspace");
        let expected_config_path = workspace_root.join(".zeroclaw").join("config.toml");

        let _workspace_env = EnvVarGuard::set(
            "ZEROCLAW_WORKSPACE",
            workspace_dir.to_string_lossy().as_ref(),
        );
        let _config_env = EnvVarGuard::unset("ZEROCLAW_CONFIG_DIR");

        let config = Box::pin(run_quick_setup_with_home(
            Some("sk-env"),
            Some("openrouter"),
            Some("model-env"),
            Some("sqlite"),
            false,
            tmp.path(),
        ))
        .await
        .expect("quick setup should honor ZEROCLAW_WORKSPACE");

        assert_eq!(config.workspace_dir, workspace_dir);
        assert_eq!(config.config_path, expected_config_path);
    }

    #[test]
    fn homebrew_prefix_for_exe_detects_supported_layouts() {
        assert_eq!(
            homebrew_prefix_for_exe(Path::new("/opt/homebrew/bin/zeroclaw")),
            Some("/opt/homebrew")
        );
        assert_eq!(
            homebrew_prefix_for_exe(Path::new(
                "/opt/homebrew/Cellar/zeroclaw/0.5.0/bin/zeroclaw",
            )),
            Some("/opt/homebrew")
        );
        assert_eq!(
            homebrew_prefix_for_exe(Path::new("/usr/local/bin/zeroclaw")),
            Some("/usr/local")
        );
        assert_eq!(homebrew_prefix_for_exe(Path::new("/tmp/zeroclaw")), None);
    }

    #[test]
    fn quick_setup_homebrew_service_note_mentions_service_workspace() {
        let note = quick_setup_homebrew_service_note(
            Path::new("/Users/alix/.zeroclaw/config.toml"),
            Path::new("/Users/alix/.zeroclaw/workspace"),
            Path::new("/opt/homebrew/bin/zeroclaw"),
        )
        .expect("homebrew installs should emit a service workspace note");

        assert!(note.contains("/opt/homebrew/var/zeroclaw/workspace"));
        assert!(note.contains("/opt/homebrew/var/zeroclaw/config.toml"));
        assert!(note.contains("/Users/alix/.zeroclaw/config.toml"));
    }

    #[test]
    fn quick_setup_homebrew_service_note_skips_matching_service_layout() {
        let service_config = Path::new("/opt/homebrew/var/zeroclaw/config.toml");
        let service_workspace = Path::new("/opt/homebrew/var/zeroclaw/workspace");

        assert!(
            quick_setup_homebrew_service_note(
                service_config,
                service_workspace,
                Path::new("/opt/homebrew/bin/zeroclaw"),
            )
            .is_none()
        );
    }

    // ── scaffold_workspace: basic file creation ─────────────────

    #[tokio::test]
    async fn scaffold_creates_all_md_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

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

    #[tokio::test]
    async fn scaffold_creates_all_subdirectories() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        for dir in &["sessions", "memory", "state", "cron", "skills"] {
            assert!(tmp.path().join(dir).is_dir(), "missing subdirectory: {dir}");
        }
    }

    // ── scaffold_workspace: personalization ─────────────────────

    #[tokio::test]
    async fn scaffold_bakes_user_name_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Alice".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(
            user_md.contains("**Name:** Alice"),
            "USER.md should contain user name"
        );

        let bootstrap = tokio::fs::read_to_string(tmp.path().join("BOOTSTRAP.md"))
            .await
            .unwrap();
        assert!(
            bootstrap.contains("**Alice**"),
            "BOOTSTRAP.md should contain user name"
        );
    }

    #[tokio::test]
    async fn scaffold_bakes_timezone_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            timezone: "US/Pacific".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(
            user_md.contains("**Timezone:** US/Pacific"),
            "USER.md should contain timezone"
        );

        let bootstrap = tokio::fs::read_to_string(tmp.path().join("BOOTSTRAP.md"))
            .await
            .unwrap();
        assert!(
            bootstrap.contains("US/Pacific"),
            "BOOTSTRAP.md should contain timezone"
        );
    }

    #[tokio::test]
    async fn scaffold_bakes_agent_name_into_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            agent_name: "Crabby".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let identity = tokio::fs::read_to_string(tmp.path().join("IDENTITY.md"))
            .await
            .unwrap();
        assert!(
            identity.contains("**Name:** Crabby"),
            "IDENTITY.md should contain agent name"
        );

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(
            soul.contains("You are **Crabby**"),
            "SOUL.md should contain agent name"
        );

        let agents = tokio::fs::read_to_string(tmp.path().join("AGENTS.md"))
            .await
            .unwrap();
        assert!(
            agents.contains("Crabby Personal Assistant"),
            "AGENTS.md should contain agent name"
        );

        let heartbeat = tokio::fs::read_to_string(tmp.path().join("HEARTBEAT.md"))
            .await
            .unwrap();
        assert!(
            heartbeat.contains("Crabby"),
            "HEARTBEAT.md should contain agent name"
        );

        let bootstrap = tokio::fs::read_to_string(tmp.path().join("BOOTSTRAP.md"))
            .await
            .unwrap();
        assert!(
            bootstrap.contains("Introduce yourself as Crabby"),
            "BOOTSTRAP.md should contain agent name"
        );
    }

    #[tokio::test]
    async fn scaffold_bakes_communication_style() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            communication_style: "Be technical and detailed.".into(),
            ..Default::default()
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(
            soul.contains("Be technical and detailed."),
            "SOUL.md should contain communication style"
        );

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(
            user_md.contains("Be technical and detailed."),
            "USER.md should contain communication style"
        );

        let bootstrap = tokio::fs::read_to_string(tmp.path().join("BOOTSTRAP.md"))
            .await
            .unwrap();
        assert!(
            bootstrap.contains("Be technical and detailed."),
            "BOOTSTRAP.md should contain communication style"
        );
    }

    // ── scaffold_workspace: defaults when context is empty ──────

    #[tokio::test]
    async fn scaffold_uses_defaults_for_empty_context() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default(); // all empty
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let identity = tokio::fs::read_to_string(tmp.path().join("IDENTITY.md"))
            .await
            .unwrap();
        assert!(
            identity.contains("**Name:** ZeroClaw"),
            "should default agent name to ZeroClaw"
        );

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(
            user_md.contains("**Name:** User"),
            "should default user name to User"
        );
        assert!(
            user_md.contains("**Timezone:** UTC"),
            "should default timezone to UTC"
        );

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(
            soul.contains("Be warm, natural, and clear."),
            "should default communication style"
        );
    }

    // ── scaffold_workspace: skip existing files ─────────────────

    #[tokio::test]
    async fn scaffold_does_not_overwrite_existing_files() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Bob".into(),
            ..Default::default()
        };

        // Pre-create SOUL.md with custom content
        let soul_path = tmp.path().join("SOUL.md");
        fs::write(&soul_path, "# My Custom Soul\nDo not overwrite me.")
            .await
            .unwrap();

        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        // SOUL.md should be untouched
        let soul = tokio::fs::read_to_string(&soul_path).await.unwrap();
        assert!(
            soul.contains("Do not overwrite me"),
            "existing files should not be overwritten"
        );
        assert!(
            !soul.contains("You're not a chatbot"),
            "should not contain scaffold content"
        );

        // But USER.md should be created fresh
        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(user_md.contains("**Name:** Bob"));
    }

    // ── scaffold_workspace: idempotent ──────────────────────────

    #[tokio::test]
    async fn scaffold_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Eve".into(),
            agent_name: "Claw".into(),
            ..Default::default()
        };

        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();
        let soul_v1 = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();

        // Run again — should not change anything
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();
        let soul_v2 = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();

        assert_eq!(soul_v1, soul_v2, "scaffold should be idempotent");
    }

    // ── scaffold_workspace: all files are non-empty ─────────────

    #[tokio::test]
    async fn scaffold_files_are_non_empty() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

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
            let content = tokio::fs::read_to_string(tmp.path().join(f)).await.unwrap();
            assert!(!content.trim().is_empty(), "{f} should not be empty");
        }
    }

    // ── scaffold_workspace: AGENTS.md references on-demand memory

    #[tokio::test]
    async fn agents_md_references_on_demand_memory() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let agents = tokio::fs::read_to_string(tmp.path().join("AGENTS.md"))
            .await
            .unwrap();
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

    #[tokio::test]
    async fn memory_md_warns_about_token_cost() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let memory = tokio::fs::read_to_string(tmp.path().join("MEMORY.md"))
            .await
            .unwrap();
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

    #[tokio::test]
    async fn tools_md_lists_all_builtin_tools() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let tools = tokio::fs::read_to_string(tmp.path().join("TOOLS.md"))
            .await
            .unwrap();
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
        assert!(
            tools.contains("Use when:"),
            "TOOLS.md should include 'Use when' guidance"
        );
        assert!(
            tools.contains("Don't use when:"),
            "TOOLS.md should include 'Don't use when' guidance"
        );
    }

    #[tokio::test]
    async fn soul_md_includes_emoji_awareness_guidance() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(
            soul.contains("Use emojis naturally (0-2 max"),
            "SOUL.md should include emoji usage guidance"
        );
        assert!(
            soul.contains("Match emoji density to the user"),
            "SOUL.md should include emoji-awareness guidance"
        );
    }

    // ── scaffold_workspace: special characters in names ─────────

    #[tokio::test]
    async fn scaffold_handles_special_characters_in_names() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "José María".into(),
            agent_name: "ZeroClaw-v2".into(),
            timezone: "Europe/Madrid".into(),
            communication_style: "Be direct.".into(),
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(user_md.contains("José María"));

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(soul.contains("ZeroClaw-v2"));
    }

    // ── scaffold_workspace: full personalization round-trip ─────

    #[tokio::test]
    async fn scaffold_full_personalization() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext {
            user_name: "Argenis".into(),
            timezone: "US/Eastern".into(),
            agent_name: "Claw".into(),
            communication_style:
                "Be friendly, human, and conversational. Show warmth and empathy while staying efficient. Use natural contractions."
                    .into(),
        };
        scaffold_workspace(tmp.path(), &ctx, "sqlite")
            .await
            .unwrap();

        // Verify every file got personalized
        let identity = tokio::fs::read_to_string(tmp.path().join("IDENTITY.md"))
            .await
            .unwrap();
        assert!(identity.contains("**Name:** Claw"));

        let soul = tokio::fs::read_to_string(tmp.path().join("SOUL.md"))
            .await
            .unwrap();
        assert!(soul.contains("You are **Claw**"));
        assert!(soul.contains("Be friendly, human, and conversational"));

        let user_md = tokio::fs::read_to_string(tmp.path().join("USER.md"))
            .await
            .unwrap();
        assert!(user_md.contains("**Name:** Argenis"));
        assert!(user_md.contains("**Timezone:** US/Eastern"));
        assert!(user_md.contains("Be friendly, human, and conversational"));

        let agents = tokio::fs::read_to_string(tmp.path().join("AGENTS.md"))
            .await
            .unwrap();
        assert!(agents.contains("Claw Personal Assistant"));

        let bootstrap = tokio::fs::read_to_string(tmp.path().join("BOOTSTRAP.md"))
            .await
            .unwrap();
        assert!(bootstrap.contains("**Argenis**"));
        assert!(bootstrap.contains("US/Eastern"));
        assert!(bootstrap.contains("Introduce yourself as Claw"));

        let heartbeat = tokio::fs::read_to_string(tmp.path().join("HEARTBEAT.md"))
            .await
            .unwrap();
        assert!(heartbeat.contains("Claw"));
    }

    // ── scaffold_workspace: none backend skips MEMORY.md ────────

    #[tokio::test]
    async fn scaffold_none_backend_disables_memory_guidance_and_skips_memory_md() {
        let tmp = TempDir::new().unwrap();
        let ctx = ProjectContext::default();
        scaffold_workspace(tmp.path(), &ctx, "none").await.unwrap();

        assert!(
            !tmp.path().join("MEMORY.md").exists(),
            "MEMORY.md should not be created for none backend"
        );

        let agents = tokio::fs::read_to_string(tmp.path().join("AGENTS.md"))
            .await
            .unwrap();
        assert!(
            agents.contains("memory.backend = \"none\""),
            "AGENTS.md should note that memory backend is none"
        );
    }

    // ── model helper coverage ───────────────────────────────────

    #[test]
    fn default_model_for_provider_uses_latest_defaults() {
        assert_eq!(
            default_model_for_provider("anthropic"),
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(default_model_for_provider("openai"), "gpt-5.2");
        assert_eq!(default_model_for_provider("gemini"), "gemini-2.5-pro");
        assert_eq!(default_model_for_provider("google"), "gemini-2.5-pro");
        assert_eq!(
            default_model_for_provider("google-gemini"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            default_model_for_provider("openrouter"),
            "anthropic/claude-sonnet-4.6"
        );
        assert_eq!(
            default_model_for_provider("unknown"),
            "anthropic/claude-sonnet-4.6"
        );
    }

    #[test]
    fn canonical_provider_name_normalizes_regional_aliases() {
        assert_eq!(canonical_provider_name("google"), "gemini");
        assert_eq!(canonical_provider_name("google-gemini"), "gemini");
        assert_eq!(canonical_provider_name("gemini"), "gemini");
        assert_eq!(canonical_provider_name("openai"), "openai");
        assert_eq!(canonical_provider_name("anthropic"), "anthropic");
        assert_eq!(canonical_provider_name("openrouter"), "openrouter");
        assert_eq!(canonical_provider_name("unknown"), "unknown");
    }

    #[test]
    fn curated_models_for_openai_include_latest_choices() {
        let ids: Vec<String> = curated_models_for_provider("openai")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"gpt-5.2".to_string()));
        assert!(ids.contains(&"gpt-5-mini".to_string()));
    }

    #[test]
    fn curated_models_for_openrouter_use_valid_anthropic_id() {
        let ids: Vec<String> = curated_models_for_provider("openrouter")
            .into_iter()
            .map(|(id, _)| id)
            .collect();

        assert!(ids.contains(&"anthropic/claude-sonnet-4.6".to_string()));
    }

    #[test]
    fn allows_unauthenticated_model_fetch_for_public_catalogs() {
        assert!(allows_unauthenticated_model_fetch("openrouter"));
        assert!(!allows_unauthenticated_model_fetch("openai"));
        assert!(!allows_unauthenticated_model_fetch("anthropic"));
        assert!(!allows_unauthenticated_model_fetch("gemini"));
        assert!(!allows_unauthenticated_model_fetch("unknown"));
    }

    #[test]
    fn supports_live_model_fetch_for_supported_and_unsupported_providers() {
        assert!(supports_live_model_fetch("openai"));
        assert!(supports_live_model_fetch("anthropic"));
        assert!(supports_live_model_fetch("gemini"));
        assert!(supports_live_model_fetch("google"));
        assert!(supports_live_model_fetch("openrouter"));
        assert!(supports_live_model_fetch(
            "custom:https://proxy.example.com/v1"
        ));
        assert!(!supports_live_model_fetch("unknown-provider"));
    }

    #[test]
    fn curated_models_provider_aliases_share_same_catalog() {
        assert_eq!(
            curated_models_for_provider("gemini"),
            curated_models_for_provider("google")
        );
        assert_eq!(
            curated_models_for_provider("gemini"),
            curated_models_for_provider("google-gemini")
        );
    }

    #[test]
    fn models_endpoint_for_provider_handles_region_aliases() {
        assert_eq!(
            models_endpoint_for_provider("glm-cn"),
            Some("https://open.bigmodel.cn/api/paas/v4/models")
        );
        assert_eq!(
            models_endpoint_for_provider("zai-cn"),
            Some("https://open.bigmodel.cn/api/coding/paas/v4/models")
        );
        assert_eq!(
            models_endpoint_for_provider("qwen-intl"),
            Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1/models")
        );
    }

    #[test]
    fn models_endpoint_for_provider_supports_additional_openai_compatible_providers() {
        assert_eq!(
            models_endpoint_for_provider("openai"),
            Some("https://api.openai.com/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("llamacpp"),
            Some("http://localhost:8080/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("sglang"),
            Some("http://localhost:30000/v1/models")
        );
        assert_eq!(
            models_endpoint_for_provider("vllm"),
            Some("http://localhost:8000/v1/models")
        );
        assert_eq!(models_endpoint_for_provider("openrouter"), None);
        assert_eq!(models_endpoint_for_provider("anthropic"), None);
        assert_eq!(models_endpoint_for_provider("unknown-provider"), None);
    }

    #[test]
    fn resolve_live_models_endpoint_falls_back_to_provider_defaults() {
        assert_eq!(
            resolve_live_models_endpoint("llamacpp", None),
            Some("http://localhost:8080/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("sglang", None),
            Some("http://localhost:30000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("vllm", None),
            Some("http://localhost:8000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("venice", Some("http://localhost:9999/v1")),
            Some("https://api.venice.ai/api/v1/models".to_string())
        );
        assert_eq!(resolve_live_models_endpoint("unknown-provider", None), None);
    }

    #[test]
    fn resolve_live_models_endpoint_supports_custom_provider_urls() {
        assert_eq!(
            resolve_live_models_endpoint("custom:https://proxy.example.com/v1", None),
            Some("https://proxy.example.com/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("custom:https://proxy.example.com/v1/models", None),
            Some("https://proxy.example.com/v1/models".to_string())
        );
    }

    #[test]
    fn normalize_ollama_endpoint_url_strips_api_suffix_and_trailing_slash() {
        assert_eq!(
            normalize_ollama_endpoint_url(" https://ollama.com/api/ "),
            "https://ollama.com".to_string()
        );
        assert_eq!(
            normalize_ollama_endpoint_url("https://ollama.com/"),
            "https://ollama.com".to_string()
        );
        assert_eq!(normalize_ollama_endpoint_url(""), "");
    }

    #[test]
    fn ollama_uses_remote_endpoint_distinguishes_local_and_remote_urls() {
        assert!(!ollama_uses_remote_endpoint(None));
        assert!(!ollama_uses_remote_endpoint(Some("http://localhost:11434")));
        assert!(!ollama_uses_remote_endpoint(Some(
            "http://127.0.0.1:11434/api"
        )));
        assert!(ollama_uses_remote_endpoint(Some("https://ollama.com")));
        assert!(ollama_uses_remote_endpoint(Some("https://ollama.com/api")));
    }

    #[test]
    fn resolve_live_models_endpoint_prefers_vllm_custom_url() {
        assert_eq!(
            resolve_live_models_endpoint("vllm", Some("http://127.0.0.1:9000/v1")),
            Some("http://127.0.0.1:9000/v1/models".to_string())
        );
        assert_eq!(
            resolve_live_models_endpoint("vllm", Some("http://127.0.0.1:9000/v1/models")),
            Some("http://127.0.0.1:9000/v1/models".to_string())
        );
    }

    #[test]
    fn parse_openai_model_ids_supports_data_array_payload() {
        let payload = json!({
            "data": [
                {"id": "  gpt-5.1  "},
                {"id": "gpt-5-mini"},
                {"id": "gpt-5.1"},
                {"id": ""}
            ]
        });

        let ids = parse_openai_compatible_model_ids(&payload);
        assert_eq!(ids, vec!["gpt-5-mini".to_string(), "gpt-5.1".to_string()]);
    }

    #[test]
    fn parse_openai_model_ids_supports_root_array_payload() {
        let payload = json!([
            {"id": "alpha"},
            {"id": "beta"},
            {"id": "alpha"}
        ]);

        let ids = parse_openai_compatible_model_ids(&payload);
        assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn normalize_model_ids_deduplicates_case_insensitively() {
        let ids = normalize_model_ids(vec![
            "GPT-5".to_string(),
            "gpt-5".to_string(),
            "gpt-5-mini".to_string(),
            " GPT-5-MINI ".to_string(),
        ]);
        assert_eq!(ids, vec!["GPT-5".to_string(), "gpt-5-mini".to_string()]);
    }

    #[test]
    fn parse_gemini_model_ids_filters_for_generate_content() {
        let payload = json!({
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/gemini-2.5-flash",
                    "supportedGenerationMethods": ["generateContent"]
                }
            ]
        });

        let ids = parse_gemini_model_ids(&payload);
        assert_eq!(
            ids,
            vec!["gemini-2.5-flash".to_string(), "gemini-2.5-pro".to_string()]
        );
    }

    #[test]
    fn parse_ollama_model_ids_extracts_and_deduplicates_names() {
        let payload = json!({
            "models": [
                {"name": "llama3.2:latest"},
                {"name": "mistral:latest"},
                {"name": "llama3.2:latest"}
            ]
        });

        let ids = parse_ollama_model_ids(&payload);
        assert_eq!(
            ids,
            vec!["llama3.2:latest".to_string(), "mistral:latest".to_string()]
        );
    }

    #[tokio::test]
    async fn model_cache_round_trip_returns_fresh_entry() {
        let tmp = TempDir::new().unwrap();
        let models = vec!["gpt-5.1".to_string(), "gpt-5-mini".to_string()];

        cache_live_models_for_provider(tmp.path(), "openai", &models)
            .await
            .unwrap();

        let cached = load_cached_models_for_provider(tmp.path(), "openai", MODEL_CACHE_TTL_SECS)
            .await
            .unwrap();
        let cached = cached.expect("expected fresh cached models");

        assert_eq!(cached.models.len(), 2);
        assert!(cached.models.contains(&"gpt-5.1".to_string()));
        assert!(cached.models.contains(&"gpt-5-mini".to_string()));
    }

    #[tokio::test]
    async fn model_cache_ttl_filters_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let stale = ModelCacheState {
            entries: vec![ModelCacheEntry {
                provider: "openai".to_string(),
                fetched_at_unix: now_unix_secs().saturating_sub(MODEL_CACHE_TTL_SECS + 120),
                models: vec!["gpt-5.1".to_string()],
            }],
        };

        save_model_cache_state(tmp.path(), &stale).await.unwrap();

        let fresh = load_cached_models_for_provider(tmp.path(), "openai", MODEL_CACHE_TTL_SECS)
            .await
            .unwrap();
        assert!(fresh.is_none());

        let stale_any = load_any_cached_models_for_provider(tmp.path(), "openai")
            .await
            .unwrap();
        assert!(stale_any.is_some());
    }

    #[tokio::test]
    async fn run_models_refresh_uses_fresh_cache_without_network() {
        let tmp = TempDir::new().unwrap();

        cache_live_models_for_provider(tmp.path(), "openai", &["gpt-5.1".to_string()])
            .await
            .unwrap();

        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            default_provider: Some("openai".to_string()),
            ..Config::default()
        };

        run_models_refresh(&config, None, false).await.unwrap();
    }

    #[tokio::test]
    async fn run_models_refresh_rejects_unsupported_provider() {
        let tmp = TempDir::new().unwrap();

        let config = Config {
            workspace_dir: tmp.path().to_path_buf(),
            // Use a non-provider channel key to keep this test deterministic and offline.
            default_provider: Some("imessage".to_string()),
            ..Config::default()
        };

        let err = run_models_refresh(&config, None, true).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("does not support live model discovery")
        );
    }

    // ── provider_env_var ────────────────────────────────────────

    #[test]
    fn provider_env_var_known_providers() {
        assert_eq!(provider_env_var("openrouter"), "OPENROUTER_API_KEY");
        assert_eq!(provider_env_var("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(provider_env_var("openai"), "OPENAI_API_KEY");
        assert_eq!(provider_env_var("gemini"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("google"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("google-gemini"), "GEMINI_API_KEY");
        assert_eq!(provider_env_var("unknown"), "API_KEY");
    }

    #[test]
    fn provider_supports_device_flow_copilot() {
        assert!(provider_supports_device_flow("copilot"));
        assert!(provider_supports_device_flow("gemini"));
        assert!(provider_supports_device_flow("openai-codex"));
        assert!(!provider_supports_device_flow("openai"));
        assert!(!provider_supports_device_flow("openrouter"));
        assert!(!provider_supports_device_flow("anthropic"));
    }

    #[test]
    fn local_provider_choices_include_sglang() {
        let choices = local_provider_choices();
        assert!(choices.iter().any(|(provider, _)| *provider == "sglang"));
    }

    #[test]
    fn provider_env_var_unknown_falls_back() {
        assert_eq!(provider_env_var("some-new-provider"), "API_KEY");
    }

    #[test]
    fn backend_key_from_choice_maps_supported_backends() {
        assert_eq!(backend_key_from_choice(0), "sqlite");
        assert_eq!(backend_key_from_choice(1), "lucid");
        assert_eq!(backend_key_from_choice(2), "markdown");
        assert_eq!(backend_key_from_choice(3), "none");
        assert_eq!(backend_key_from_choice(999), "sqlite");
    }

    #[test]
    fn memory_backend_profile_marks_lucid_as_optional_sqlite_backed() {
        let lucid = memory_backend_profile("lucid");
        assert!(lucid.auto_save_default);
        assert!(lucid.uses_sqlite_hygiene);
        assert!(lucid.sqlite_based);
        assert!(lucid.optional_dependency);

        let markdown = memory_backend_profile("markdown");
        assert!(markdown.auto_save_default);
        assert!(!markdown.uses_sqlite_hygiene);

        let none = memory_backend_profile("none");
        assert!(!none.auto_save_default);
        assert!(!none.uses_sqlite_hygiene);

        let custom = memory_backend_profile("custom-memory");
        assert!(custom.auto_save_default);
        assert!(!custom.uses_sqlite_hygiene);
    }

    #[test]
    fn memory_config_defaults_for_lucid_enable_sqlite_hygiene() {
        let config = memory_config_defaults_for_backend("lucid");
        assert_eq!(config.backend, "lucid");
        assert!(config.auto_save);
        assert!(config.hygiene_enabled);
        assert_eq!(config.archive_after_days, 7);
        assert_eq!(config.purge_after_days, 30);
        assert_eq!(config.embedding_cache_size, 10000);
    }

    #[test]
    fn memory_config_defaults_for_none_disable_sqlite_hygiene() {
        let config = memory_config_defaults_for_backend("none");
        assert_eq!(config.backend, "none");
        assert!(!config.auto_save);
        assert!(!config.hygiene_enabled);
        assert_eq!(config.archive_after_days, 0);
        assert_eq!(config.purge_after_days, 0);
        assert_eq!(config.embedding_cache_size, 0);
    }

    #[test]
    fn channel_menu_choices_include_telegram_and_slack() {
        assert!(channel_menu_choices().contains(&ChannelMenuChoice::Telegram));
        assert!(channel_menu_choices().contains(&ChannelMenuChoice::Slack));
    }

    #[test]
    fn launchable_channels_include_telegram_and_slack() {
        let mut channels = ChannelsConfig::default();
        assert!(!has_launchable_channels(&channels));

        channels.telegram = Some(crate::config::schema::TelegramConfig {
            bot_token: "123:ABC".into(),
            allowed_users: vec!["*".into()],
            stream_mode: crate::config::schema::StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
        });
        assert!(has_launchable_channels(&channels));
    }
}
