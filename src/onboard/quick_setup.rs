use anyhow::{Context, Result};
use console::style;
use std::fs;

use crate::config::{
    AutonomyConfig, BrowserConfig, ChannelsConfig, ComposioConfig, Config, HeartbeatConfig,
    MemoryConfig, ObservabilityConfig, RuntimeConfig, SecretsConfig,
};
use crate::onboard::common::{BANNER, ProjectContext};
use crate::onboard::provider_setup::default_model_for_provider;
use crate::onboard::workspace_scaffold::scaffold_workspace;

#[allow(clippy::too_many_lines)]
pub fn run_quick_setup(
    api_key: Option<&str>,
    provider: Option<&str>,
    memory_backend: Option<&str>,
) -> Result<Config> {
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
    let memory_backend_name = memory_backend.unwrap_or("sqlite").to_string();

    let memory_config = MemoryConfig {
        backend: memory_backend_name.clone(),
        auto_save: memory_backend_name != "none",
        hygiene_enabled: memory_backend_name == "sqlite",
        archive_after_days: if memory_backend_name == "sqlite" { 7 } else { 0 },
        purge_after_days: if memory_backend_name == "sqlite" { 30 } else { 0 },
        conversation_retention_days: 30,
        embedding_provider: "none".to_string(),
        embedding_model: "text-embedding-3-small".to_string(),
        embedding_dimensions: 1536,
        vector_weight: 0.7,
        keyword_weight: 0.3,
        embedding_cache_size: if memory_backend_name == "sqlite" {
            10000
        } else {
            0
        },
        chunk_max_tokens: 512,
    };

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
        reliability: crate::config::ReliabilityConfig::default(),
        heartbeat: HeartbeatConfig::default(),
        channels_config: ChannelsConfig::default(),
        memory: memory_config,
        tunnel: crate::config::TunnelConfig::default(),
        gateway: crate::config::GatewayConfig::default(),
        composio: ComposioConfig::default(),
        secrets: SecretsConfig::default(),
        browser: BrowserConfig::default(),
        identity: crate::config::IdentityConfig::default(),
    };

    config.save()?;

    let default_ctx = ProjectContext {
        user_name: std::env::var("USER").unwrap_or_else(|_| "User".into()),
        timezone: "UTC".into(),
        agent_name: "ZeroClaw".into(),
        communication_style:
            "Be warm, natural, and clear. Use occasional relevant emojis (1-2 max) and avoid robotic phrasing."
                .into(),
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
