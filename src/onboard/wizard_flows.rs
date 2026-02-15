use anyhow::Result;
use console::style;
use dialoguer::Confirm;

use crate::config::{
    AutonomyConfig, BrowserConfig, Config, HeartbeatConfig, ObservabilityConfig, RuntimeConfig,
};
use crate::onboard::channel_setup::setup_channels;
use crate::onboard::common::{print_step, BANNER};
use crate::onboard::memory_setup::setup_memory;
use crate::onboard::project_context_setup::setup_project_context;
use crate::onboard::provider_setup::setup_provider;
use crate::onboard::summary::print_summary;
use crate::onboard::tool_mode_setup::setup_tool_mode;
use crate::onboard::tunnel_setup::setup_tunnel;
use crate::onboard::workspace_scaffold::scaffold_workspace;
use crate::onboard::workspace_setup::setup_workspace;

pub(crate) fn maybe_offer_channel_autostart(config: &Config) -> Result<()> {
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
            std::env::set_var("ZEROCLAW_AUTOSTART_CHANNELS", "1");
        }
    }

    Ok(())
}

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

    print_step(1, 8, "Workspace Setup");
    let (workspace_dir, config_path) = setup_workspace()?;

    print_step(2, 8, "AI Provider & API Key");
    let (provider, api_key, model) = setup_provider()?;

    print_step(3, 8, "Channels (How You Talk to ZeroClaw)");
    let channels_config = setup_channels()?;

    print_step(4, 8, "Tunnel (Expose to Internet)");
    let tunnel_config = setup_tunnel()?;

    print_step(5, 8, "Tool Mode & Security");
    let (composio_config, secrets_config) = setup_tool_mode()?;

    print_step(6, 8, "Memory Configuration");
    let memory_config = setup_memory()?;

    print_step(7, 8, "Project Context (Personalize Your Agent)");
    let project_ctx = setup_project_context()?;

    print_step(8, 8, "Workspace Files");
    scaffold_workspace(&workspace_dir, &project_ctx)?;

    let config = Config {
        workspace_dir: workspace_dir.clone(),
        config_path: config_path.clone(),
        api_key: if api_key.is_empty() { None } else { Some(api_key) },
        default_provider: Some(provider),
        default_model: Some(model),
        default_temperature: 0.7,
        observability: ObservabilityConfig::default(),
        autonomy: AutonomyConfig::default(),
        runtime: RuntimeConfig::default(),
        reliability: crate::config::ReliabilityConfig::default(),
        heartbeat: HeartbeatConfig::default(),
        channels_config,
        memory: memory_config,
        tunnel: tunnel_config,
        gateway: crate::config::GatewayConfig::default(),
        composio: composio_config,
        secrets: secrets_config,
        browser: BrowserConfig::default(),
        identity: crate::config::IdentityConfig::default(),
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

    config.save()?;
    print_summary(&config);
    maybe_offer_channel_autostart(&config)?;

    Ok(config)
}

pub fn run_channels_repair_wizard() -> Result<Config> {
    println!("{}", style(BANNER).cyan().bold());
    println!(
        "  {}",
        style("Channels Repair — update channel tokens and allowlists only")
            .white()
            .bold()
    );
    println!();

    let mut config = Config::load_or_init()?;

    print_step(1, 1, "Channels (How You Talk to ZeroClaw)");
    config.channels_config = setup_channels()?;
    config.save()?;

    println!();
    println!(
        "  {} Channel config saved: {}",
        style("✓").green().bold(),
        style(config.config_path.display()).green()
    );

    maybe_offer_channel_autostart(&config)?;

    Ok(config)
}
