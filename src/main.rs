#![warn(clippy::all, clippy::pedantic)]
#![forbid(unsafe_code)]
#![recursion_limit = "256"]
#![allow(
    clippy::assigning_clones,
    clippy::bool_to_int_with_if,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::cast_possible_wrap,
    clippy::doc_markdown,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::implicit_clone,
    clippy::items_after_statements,
    clippy::map_unwrap_or,
    clippy::manual_let_else,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    clippy::needless_raw_string_hashes,
    clippy::redundant_closure_for_method_calls,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_field_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unused_self,
    clippy::cast_precision_loss,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_map_or,
    clippy::unnecessary_wraps,
    dead_code
)]

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use dialoguer::{Input, Password};
use serde::{Deserialize, Serialize};
use std::io::Write;
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

const PROFILE_MISMATCH_PREFIX: &str = "Pending login profile mismatch:";
const ZEROCLAW_BUILD_VERSION: &str = env!("ZEROCLAW_BUILD_VERSION");

#[derive(Debug, Clone, ValueEnum)]
enum QuotaFormat {
    Text,
    Json,
}

fn parse_temperature(s: &str) -> std::result::Result<f64, String> {
    let t: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if !(0.0..=2.0).contains(&t) {
        return Err("temperature must be between 0.0 and 2.0".to_string());
    }
    Ok(t)
}

mod agent;
mod approval;
mod auth;
mod channels;
mod config;
mod coordination;
mod cost;
mod cron;
mod daemon;
mod doctor;
mod gateway;
mod goals;
mod hardware;
mod health;
mod heartbeat;
mod hooks;
mod identity;
mod integrations;
mod memory;
mod migration;
mod multimodal;
mod observability;
mod onboard;
mod peripherals;
mod plugins;
mod providers;
mod rag;
mod runtime;
mod security;
mod service;
mod skillforge;
mod skills;
mod tools;
mod tunnel;
mod update;
mod util;

use config::Config;

// Re-export so binary modules can use crate::<CommandEnum> while keeping a single source of truth.
pub use zeroclaw::{
    ChannelCommands, CronCommands, HardwareCommands, IntegrationCommands, MigrateCommands,
    PeripheralCommands, ServiceCommands, SkillCommands,
};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum CompletionShell {
    #[value(name = "bash")]
    Bash,
    #[value(name = "fish")]
    Fish,
    #[value(name = "zsh")]
    Zsh,
    #[value(name = "powershell")]
    PowerShell,
    #[value(name = "elvish")]
    Elvish,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum EstopLevelArg {
    #[value(name = "kill-all")]
    KillAll,
    #[value(name = "network-kill")]
    NetworkKill,
    #[value(name = "domain-block")]
    DomainBlock,
    #[value(name = "tool-freeze")]
    ToolFreeze,
}

/// `ZeroClaw` - Zero overhead. Zero compromise. 100% Rust.
#[derive(Parser, Debug)]
#[command(name = "zeroclaw")]
#[command(author = "theonlyhennygod")]
#[command(version = ZEROCLAW_BUILD_VERSION)]
#[command(about = "The fastest, smallest AI assistant.", long_about = None)]
struct Cli {
    #[arg(long, global = true)]
    config_dir: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize your workspace and configuration
    Onboard {
        /// Run the full interactive wizard (default is quick setup)
        #[arg(long)]
        interactive: bool,

        /// Overwrite existing config without confirmation
        #[arg(long)]
        force: bool,

        /// Reconfigure channels only (fast repair flow)
        #[arg(long)]
        channels_only: bool,

        /// API key (used in quick mode, ignored with --interactive)
        #[arg(long)]
        api_key: Option<String>,

        /// Provider name (used in quick mode, default: openrouter)
        #[arg(long)]
        provider: Option<String>,
        /// Model ID override (used in quick mode)
        #[arg(long)]
        model: Option<String>,
        /// Memory backend (sqlite, lucid, markdown, none) - used in quick mode, default: sqlite
        #[arg(long)]
        memory: Option<String>,

        /// Disable OTP in quick setup (not recommended)
        #[arg(long)]
        no_totp: bool,

        /// Merge-migrate data from OpenClaw during onboarding
        #[arg(long)]
        migrate_openclaw: bool,

        /// Optional OpenClaw workspace path (defaults to ~/.openclaw/workspace)
        #[arg(long)]
        openclaw_source: Option<std::path::PathBuf>,

        /// Optional OpenClaw config path (defaults to ~/.openclaw/openclaw.json)
        #[arg(long)]
        openclaw_config: Option<std::path::PathBuf>,
    },

    /// Start the AI agent loop
    #[command(long_about = "\
Start the AI agent loop.

Launches an interactive chat session with the configured AI provider. \
Use --message for single-shot queries without entering interactive mode.

Examples:
  zeroclaw agent                              # interactive session
  zeroclaw agent -m \"Summarize today's logs\"  # single message
  zeroclaw agent -p anthropic --model claude-sonnet-4-20250514
  zeroclaw agent --peripheral nucleo-f401re:/dev/ttyACM0
  zeroclaw agent --autonomy-level full --max-actions-per-hour 100
  zeroclaw agent -m \"quick task\" --memory-backend none --compact-context")]
    Agent {
        /// Single message mode (don't enter interactive mode)
        #[arg(short, long)]
        message: Option<String>,

        /// Provider to use (openrouter, anthropic, openai, openai-codex)
        #[arg(short, long)]
        provider: Option<String>,

        /// Model to use
        #[arg(long)]
        model: Option<String>,

        /// Temperature (0.0 - 2.0)
        #[arg(short, long, default_value = "0.7", value_parser = parse_temperature)]
        temperature: f64,

        /// Attach a peripheral (board:path, e.g. nucleo-f401re:/dev/ttyACM0)
        #[arg(long)]
        peripheral: Vec<String>,

        /// Autonomy level (read_only, supervised, full)
        #[arg(long, value_parser = clap::value_parser!(security::AutonomyLevel))]
        autonomy_level: Option<security::AutonomyLevel>,

        /// Maximum shell/tool actions per hour
        #[arg(long)]
        max_actions_per_hour: Option<u32>,

        /// Maximum tool-call iterations per message
        #[arg(long)]
        max_tool_iterations: Option<usize>,

        /// Maximum conversation history messages
        #[arg(long)]
        max_history_messages: Option<usize>,

        /// Enable compact context mode (smaller prompts for limited models)
        #[arg(long)]
        compact_context: bool,

        /// Memory backend (sqlite, markdown, none)
        #[arg(long)]
        memory_backend: Option<String>,
    },

    /// Start the gateway server (webhooks, websockets)
    #[command(long_about = "\
Start the gateway server (webhooks, websockets).

Runs the HTTP/WebSocket gateway that accepts incoming webhook events \
and WebSocket connections. Bind address defaults to the values in \
your config file (gateway.host / gateway.port).

Examples:
  zeroclaw gateway                  # use config defaults
  zeroclaw gateway -p 8080          # listen on port 8080
  zeroclaw gateway --host 0.0.0.0   # bind to all interfaces
  zeroclaw gateway -p 0             # random available port
  zeroclaw gateway --new-pairing    # clear tokens and generate fresh pairing code")]
    Gateway {
        /// Port to listen on (use 0 for random available port); defaults to config gateway.port
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to; defaults to config gateway.host
        #[arg(long)]
        host: Option<String>,

        /// Clear all paired tokens and generate a fresh pairing code
        #[arg(long)]
        new_pairing: bool,
    },

    /// Start long-running autonomous runtime (gateway + channels + heartbeat + scheduler)
    #[command(long_about = "\
Start the long-running autonomous daemon.

Launches the full ZeroClaw runtime: gateway server, all configured \
channels (Telegram, Discord, Slack, etc.), heartbeat monitor, and \
the cron scheduler. This is the recommended way to run ZeroClaw in \
production or as an always-on assistant.

Use 'zeroclaw service install' to register the daemon as an OS \
service (systemd/launchd) for auto-start on boot.

Examples:
  zeroclaw daemon                   # use config defaults
  zeroclaw daemon -p 9090           # gateway on port 9090
  zeroclaw daemon --host 127.0.0.1  # localhost only")]
    Daemon {
        /// Port to listen on (use 0 for random available port); defaults to config gateway.port
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to; defaults to config gateway.host
        #[arg(long)]
        host: Option<String>,
    },

    /// Manage OS service lifecycle (launchd/systemd user service)
    Service {
        /// Init system to use: auto (detect), systemd, or openrc
        #[arg(long, default_value = "auto", value_parser = ["auto", "systemd", "openrc"])]
        service_init: String,

        #[command(subcommand)]
        service_command: ServiceCommands,
    },

    /// Run diagnostics for daemon/scheduler/channel freshness
    Doctor {
        #[command(subcommand)]
        doctor_command: Option<DoctorCommands>,
    },

    /// Show system status (full details)
    Status,

    /// Self-update ZeroClaw to the latest version
    #[command(long_about = "\
Self-update ZeroClaw to the latest release from GitHub.

Downloads the appropriate pre-built binary for your platform and
replaces the current executable. Requires write permissions to
the binary location.

Examples:
  zeroclaw update              # Update to latest version
  zeroclaw update --check      # Check for updates without installing
  zeroclaw update --instructions # Show install-method-specific update instructions
  zeroclaw update --force      # Reinstall even if already up to date")]
    Update {
        /// Check for updates without installing
        #[arg(long, conflicts_with_all = ["force", "instructions"])]
        check: bool,

        /// Force update even if already at latest version
        #[arg(long, conflicts_with = "instructions")]
        force: bool,

        /// Show human-friendly update instructions for your installation method
        #[arg(long, conflicts_with_all = ["check", "force"])]
        instructions: bool,
    },

    /// Engage, inspect, and resume emergency-stop states.
    ///
    /// Examples:
    /// - `zeroclaw estop`
    /// - `zeroclaw estop --level network-kill`
    /// - `zeroclaw estop --level domain-block --domain "*.chase.com"`
    /// - `zeroclaw estop --level tool-freeze --tool shell --tool browser`
    /// - `zeroclaw estop status`
    /// - `zeroclaw estop resume --network`
    /// - `zeroclaw estop resume --domain "*.chase.com"`
    /// - `zeroclaw estop resume --tool shell`
    Estop {
        #[command(subcommand)]
        estop_command: Option<EstopSubcommands>,

        /// Level used when engaging estop from `zeroclaw estop`.
        #[arg(long, value_enum)]
        level: Option<EstopLevelArg>,

        /// Domain pattern(s) for `domain-block` (repeatable).
        #[arg(long = "domain")]
        domains: Vec<String>,

        /// Tool name(s) for `tool-freeze` (repeatable).
        #[arg(long = "tool")]
        tools: Vec<String>,
    },

    /// Configure and manage scheduled tasks
    #[command(long_about = "\
Configure and manage scheduled tasks.

Schedule recurring, one-shot, or interval-based tasks using cron \
expressions, RFC 3339 timestamps, durations, or fixed intervals.

Cron expressions use the standard 5-field format: \
'min hour day month weekday'. Timezones default to UTC; \
override with --tz and an IANA timezone name.

Examples:
  zeroclaw cron list
  zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York
  zeroclaw cron add '*/30 * * * *' 'Check system health'
  zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder'
  zeroclaw cron add-every 60000 'Ping heartbeat'
  zeroclaw cron once 30m 'Run backup in 30 minutes'
  zeroclaw cron pause <task-id>
  zeroclaw cron update <task-id> --expression '0 8 * * *' --tz Europe/London")]
    Cron {
        #[command(subcommand)]
        cron_command: CronCommands,
    },

    /// Manage provider model catalogs
    Models {
        #[command(subcommand)]
        model_command: ModelCommands,
    },

    /// List supported AI providers
    Providers,

    /// Show provider quota and rate limit status
    #[command(
        name = "providers-quota",
        long_about = "\
Show provider quota and rate limit status.

Displays quota remaining, rate limit resets, circuit breaker state, \
and per-profile breakdown for all configured providers. Helps diagnose \
quota exhaustion and rate limiting issues.

Examples:
  zeroclaw providers-quota                    # text output, all providers
  zeroclaw providers-quota --format json      # JSON output
  zeroclaw providers-quota --provider gemini  # filter by provider"
    )]
    ProvidersQuota {
        /// Filter by provider name (optional, shows all if omitted)
        #[arg(long)]
        provider: Option<String>,

        /// Output format (text or json)
        #[arg(long, value_enum, default_value_t = QuotaFormat::Text)]
        format: QuotaFormat,
    },
    /// Manage channels (telegram, discord, slack, github)
    #[command(long_about = "\
Manage communication channels.

Add, remove, list, and health-check channels that connect ZeroClaw \
to messaging platforms. Supported channel types: telegram, discord, \
slack, whatsapp, github, matrix, imessage, email.

Examples:
  zeroclaw channel list
  zeroclaw channel doctor
  zeroclaw channel add telegram '{\"bot_token\":\"...\",\"name\":\"my-bot\"}'
  zeroclaw channel remove my-bot
  zeroclaw channel bind-telegram zeroclaw_user")]
    Channel {
        #[command(subcommand)]
        channel_command: ChannelCommands,
    },

    /// Browse 50+ integrations
    Integrations {
        #[command(subcommand)]
        integration_command: IntegrationCommands,
    },

    /// Manage skills (user-defined capabilities)
    #[command(name = "skill", alias = "skills")]
    Skills {
        #[command(subcommand)]
        skill_command: SkillCommands,
    },

    /// Migrate data from other agent runtimes
    Migrate {
        #[command(subcommand)]
        migrate_command: MigrateCommands,
    },

    /// Manage provider subscription authentication profiles
    Auth {
        #[command(subcommand)]
        auth_command: AuthCommands,
    },

    /// Discover and introspect USB hardware
    #[command(long_about = "\
Discover and introspect USB hardware.

Enumerate connected USB devices, identify known development boards \
(STM32 Nucleo, Arduino, ESP32), and retrieve chip information via \
probe-rs / ST-Link.

Examples:
  zeroclaw hardware discover
  zeroclaw hardware introspect /dev/ttyACM0
  zeroclaw hardware info --chip STM32F401RETx")]
    Hardware {
        #[command(subcommand)]
        hardware_command: zeroclaw::HardwareCommands,
    },

    /// Manage hardware peripherals (STM32, RPi GPIO, etc.)
    #[command(long_about = "\
Manage hardware peripherals.

Add, list, flash, and configure hardware boards that expose tools \
to the agent (GPIO, sensors, actuators). Supported boards: \
nucleo-f401re, rpi-gpio, esp32, arduino-uno.

Examples:
  zeroclaw peripheral list
  zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
  zeroclaw peripheral add rpi-gpio native
  zeroclaw peripheral flash --port /dev/cu.usbmodem12345
  zeroclaw peripheral flash-nucleo")]
    Peripheral {
        #[command(subcommand)]
        peripheral_command: zeroclaw::PeripheralCommands,
    },

    /// Manage agent memory (list, get, stats, clear)
    #[command(long_about = "\
Manage agent memory entries.

List, inspect, and clear memory entries stored by the agent. \
Supports filtering by category and session, pagination, and \
batch clearing with confirmation.

Examples:
  zeroclaw memory stats
  zeroclaw memory list
  zeroclaw memory list --category core --limit 10
  zeroclaw memory get <key>
  zeroclaw memory clear --category conversation --yes")]
    Memory {
        #[command(subcommand)]
        memory_command: MemoryCommands,
    },

    /// Manage configuration
    #[command(long_about = "\
Manage ZeroClaw configuration.

Inspect, query, and modify configuration settings.

Examples:
  zeroclaw config show                        # show effective config (secrets masked)
  zeroclaw config get gateway.port            # query a specific value by dot-path
  zeroclaw config set gateway.port 8080       # update a value and save to config.toml
  zeroclaw config schema                      # print full JSON Schema to stdout")]
    Config {
        #[command(subcommand)]
        config_command: ConfigCommands,
    },

    /// Generate shell completion script to stdout
    #[command(long_about = "\
Generate shell completion scripts for `zeroclaw`.

The script is printed to stdout so it can be sourced directly:

Examples:
  source <(zeroclaw completions bash)
  zeroclaw completions zsh > ~/.zfunc/_zeroclaw
  zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish")]
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: CompletionShell,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Show the current effective configuration (secrets masked)
    Show,
    /// Get a specific configuration value by dot-path (e.g. "gateway.port")
    Get {
        /// Dot-separated config path, e.g. "security.estop.enabled"
        key: String,
    },
    /// Set a configuration value and save to config.toml
    Set {
        /// Dot-separated config path, e.g. "gateway.port"
        key: String,
        /// New value (string, number, boolean, or JSON for objects/arrays)
        value: String,
    },
    /// Dump the full configuration JSON Schema to stdout
    Schema,
}

#[derive(Subcommand, Debug)]
enum EstopSubcommands {
    /// Print current estop status.
    Status,
    /// Resume from an engaged estop level.
    Resume {
        /// Resume only network kill.
        #[arg(long)]
        network: bool,
        /// Resume one or more blocked domain patterns.
        #[arg(long = "domain")]
        domains: Vec<String>,
        /// Resume one or more frozen tools.
        #[arg(long = "tool")]
        tools: Vec<String>,
        /// OTP code. If omitted and OTP is required, a prompt is shown.
        #[arg(long)]
        otp: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCommands {
    /// Login with OAuth (OpenAI Codex or Gemini)
    Login {
        /// Provider (`openai-codex` or `gemini`)
        #[arg(long)]
        provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Use OAuth device-code flow
        #[arg(long)]
        device_code: bool,
    },
    /// Complete OAuth by pasting redirect URL or auth code
    PasteRedirect {
        /// Provider (`openai-codex`)
        #[arg(long)]
        provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Full redirect URL or raw OAuth code
        #[arg(long)]
        input: Option<String>,
    },
    /// Paste setup token / auth token (for Anthropic subscription auth)
    PasteToken {
        /// Provider (`anthropic`)
        #[arg(long)]
        provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Token value (if omitted, read interactively)
        #[arg(long)]
        token: Option<String>,
        /// Auth kind override (`authorization` or `api-key`)
        #[arg(long)]
        auth_kind: Option<String>,
    },
    /// Alias for `paste-token` (interactive by default)
    SetupToken {
        /// Provider (`anthropic`)
        #[arg(long)]
        provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Refresh OpenAI Codex access token using refresh token
    Refresh {
        /// Provider (`openai-codex`)
        #[arg(long)]
        provider: String,
        /// Profile name or profile id
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove auth profile
    Logout {
        /// Provider
        #[arg(long)]
        provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Set active profile for a provider
    Use {
        /// Provider
        #[arg(long)]
        provider: String,
        /// Profile name or full profile id
        #[arg(long)]
        profile: String,
    },
    /// List auth profiles
    List,
    /// Show auth status with active profile and token expiry info
    Status,
}

#[derive(Subcommand, Debug)]
enum ModelCommands {
    /// Refresh and cache provider models
    Refresh {
        /// Provider name (defaults to configured default provider)
        #[arg(long)]
        provider: Option<String>,

        /// Refresh all providers that support live model discovery
        #[arg(long)]
        all: bool,

        /// Force live refresh and ignore fresh cache
        #[arg(long)]
        force: bool,
    },
    /// List cached models for a provider
    List {
        /// Provider name (defaults to configured default provider)
        #[arg(long)]
        provider: Option<String>,
    },
    /// Set the default model in config
    Set {
        /// Model name to set as default
        model: String,
    },
    /// Show current model configuration and cache status
    Status,
}

#[derive(Subcommand, Debug)]
enum DoctorCommands {
    /// Probe model catalogs across providers and report availability
    Models {
        /// Probe a specific provider only (default: all known providers)
        #[arg(long)]
        provider: Option<String>,

        /// Prefer cached catalogs when available (skip forced live refresh)
        #[arg(long)]
        use_cache: bool,
    },
    /// Query runtime trace events (tool diagnostics and model replies)
    Traces {
        /// Show a specific trace event by id
        #[arg(long)]
        id: Option<String>,
        /// Filter list output by event type
        #[arg(long)]
        event: Option<String>,
        /// Case-insensitive text match across message/payload
        #[arg(long)]
        contains: Option<String>,
        /// Maximum number of events to display
        #[arg(long, default_value = "20")]
        limit: usize,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCommands {
    /// List memory entries with optional filters
    List {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Get a specific memory entry by key
    Get { key: String },
    /// Show memory backend statistics and health
    Stats,
    /// Clear memories by category, by key, or clear all
    Clear {
        /// Delete a single entry by key (supports prefix match)
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        category: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Rebuild embeddings for all memories (use after changing embedding model)
    Reindex {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Show progress during reindex
        #[arg(long, default_value = "true")]
        progress: bool,
    },
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<()> {
    // Install default crypto provider for Rustls TLS.
    // This prevents the error: "could not automatically determine the process-level CryptoProvider"
    // when both aws-lc-rs and ring features are available (or neither is explicitly selected).
    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        eprintln!("Warning: Failed to install default crypto provider: {e:?}");
    }

    let cli = Cli::parse();

    if let Some(config_dir) = &cli.config_dir {
        if config_dir.trim().is_empty() {
            bail!("--config-dir cannot be empty");
        }
        std::env::set_var("ZEROCLAW_CONFIG_DIR", config_dir);
    }

    // Completions must remain stdout-only and should not load config or initialize logging.
    // This avoids warnings/log lines corrupting sourced completion scripts.
    if let Commands::Completions { shell } = &cli.command {
        let mut stdout = std::io::stdout().lock();
        write_shell_completion(*shell, &mut stdout)?;
        return Ok(());
    }

    // Initialize logging - respects RUST_LOG env var, defaults to INFO
    let subscriber = fmt::Subscriber::builder()
        .with_timer(tracing_subscriber::fmt::time::ChronoLocal::rfc_3339())
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    // Onboard runs quick setup by default, or the interactive wizard with --interactive.
    // The onboard wizard uses reqwest::blocking internally, which creates its own
    // Tokio runtime. To avoid "Cannot drop a runtime in a context where blocking is
    // not allowed", we run the wizard on a blocking thread via spawn_blocking.
    if let Commands::Onboard {
        interactive,
        force,
        channels_only,
        api_key,
        provider,
        model,
        memory,
        no_totp,
        migrate_openclaw,
        openclaw_source,
        openclaw_config,
    } = &cli.command
    {
        let interactive = *interactive;
        let force = *force;
        let channels_only = *channels_only;
        let api_key = api_key.clone();
        let provider = provider.clone();
        let model = model.clone();
        let memory = memory.clone();
        let no_totp = *no_totp;
        let migrate_openclaw = *migrate_openclaw;
        let openclaw_source = openclaw_source.clone();
        let openclaw_config = openclaw_config.clone();
        let openclaw_migration_enabled =
            migrate_openclaw || openclaw_source.is_some() || openclaw_config.is_some();

        if interactive && channels_only {
            bail!("Use either --interactive or --channels-only, not both");
        }
        if channels_only
            && (api_key.is_some()
                || provider.is_some()
                || model.is_some()
                || memory.is_some()
                || no_totp
                || migrate_openclaw
                || openclaw_source.is_some()
                || openclaw_config.is_some())
        {
            bail!(
                "--channels-only does not accept --api-key, --provider, --model, --memory, --no-totp, or OpenClaw migration flags"
            );
        }
        if channels_only && force {
            bail!("--channels-only does not accept --force");
        }
        let config = if channels_only {
            Box::pin(onboard::run_channels_repair_wizard()).await
        } else if interactive {
            Box::pin(onboard::run_wizard_with_migration(
                force,
                onboard::OpenClawOnboardMigrationOptions {
                    enabled: openclaw_migration_enabled,
                    source_workspace: openclaw_source,
                    source_config: openclaw_config,
                },
            ))
            .await
        } else {
            onboard::run_quick_setup_with_migration(
                api_key.as_deref(),
                provider.as_deref(),
                model.as_deref(),
                memory.as_deref(),
                force,
                no_totp,
                onboard::OpenClawOnboardMigrationOptions {
                    enabled: openclaw_migration_enabled,
                    source_workspace: openclaw_source,
                    source_config: openclaw_config,
                },
            )
            .await
        }?;
        // Auto-start channels if user said yes during wizard
        if std::env::var("ZEROCLAW_AUTOSTART_CHANNELS").as_deref() == Ok("1") {
            Box::pin(channels::start_channels(config)).await?;
        }
        return Ok(());
    }

    // All other commands need config loaded first
    let mut config = Config::load_or_init().await?;
    config.apply_env_overrides();
    observability::runtime_trace::init_from_config(&config.observability, &config.workspace_dir);
    if config.security.otp.enabled {
        let config_dir = config
            .config_path
            .parent()
            .context("Config path must have a parent directory")?;
        let store = security::SecretStore::new(config_dir, config.secrets.encrypt);
        let (_validator, enrollment_uri) =
            security::OtpValidator::from_config(&config.security.otp, config_dir, &store)?;
        if let Some(uri) = enrollment_uri {
            println!("Initialized OTP secret for ZeroClaw.");
            println!("Enrollment URI: {uri}");
        }
    }

    match cli.command {
        Commands::Onboard { .. } | Commands::Completions { .. } => unreachable!(),

        Commands::Agent {
            message,
            provider,
            model,
            temperature,
            peripheral,
            autonomy_level,
            max_actions_per_hour,
            max_tool_iterations,
            max_history_messages,
            compact_context,
            memory_backend,
        } => {
            if let Some(level) = autonomy_level {
                config.autonomy.level = level;
            }
            if let Some(n) = max_actions_per_hour {
                config.autonomy.max_actions_per_hour = n;
            }
            if let Some(n) = max_tool_iterations {
                config.agent.max_tool_iterations = n;
            }
            if let Some(n) = max_history_messages {
                config.agent.max_history_messages = n;
            }
            if compact_context {
                config.agent.compact_context = true;
            }
            if let Some(ref backend) = memory_backend {
                config.memory.backend = backend.clone();
            }
            // interactive=true only when no --message flag (real REPL session).
            // Single-shot mode (-m) runs non-interactively: no TTY approval prompt,
            // so tools are not denied by a stdin read returning EOF.
            let interactive = message.is_none();
            Box::pin(agent::run(
                config,
                message,
                provider,
                model,
                temperature,
                peripheral,
                interactive,
                None,
            ))
            .await
            .map(|_| ())
        }

        Commands::Gateway {
            port,
            host,
            new_pairing,
        } => {
            if new_pairing {
                // Persist token reset from raw config so env-derived overrides are not written to disk.
                let mut persisted_config = Config::load_or_init().await?;
                persisted_config.gateway.paired_tokens.clear();
                persisted_config.save().await?;
                config.gateway.paired_tokens.clear();
                info!("🔐 Cleared paired tokens — a fresh pairing code will be generated");
            }
            let port = port.unwrap_or(config.gateway.port);
            let host = host.unwrap_or_else(|| config.gateway.host.clone());
            if port == 0 {
                info!("🚀 Starting ZeroClaw Gateway on {host} (random port)");
            } else {
                info!("🚀 Starting ZeroClaw Gateway on {host}:{port}");
            }
            gateway::run_gateway(&host, port, config).await
        }

        Commands::Daemon { port, host } => {
            let port = port.unwrap_or(config.gateway.port);
            let host = host.unwrap_or_else(|| config.gateway.host.clone());
            if port == 0 {
                info!("🧠 Starting ZeroClaw Daemon on {host} (random port)");
            } else {
                info!("🧠 Starting ZeroClaw Daemon on {host}:{port}");
            }
            daemon::run(config, host, port).await
        }

        Commands::Status => {
            println!("🦀 ZeroClaw Status");
            println!();
            println!("Version:     {}", ZEROCLAW_BUILD_VERSION);
            println!("Workspace:   {}", config.workspace_dir.display());
            println!("Config:      {}", config.config_path.display());
            println!();
            println!(
                "🤖 Provider:      {}",
                config.default_provider.as_deref().unwrap_or("openrouter")
            );
            println!(
                "   Model:         {}",
                config.default_model.as_deref().unwrap_or("(default)")
            );
            println!("📊 Observability:  {}", config.observability.backend);
            println!(
                "🧾 Trace storage:  {} ({})",
                config.observability.runtime_trace_mode, config.observability.runtime_trace_path
            );
            println!("🛡️  Autonomy:      {:?}", config.autonomy.level);
            println!("⚙️  Runtime:       {}", config.runtime.kind);
            let effective_memory_backend = memory::effective_memory_backend_name(
                &config.memory.backend,
                Some(&config.storage.provider.config),
            );
            println!(
                "💓 Heartbeat:      {}",
                if config.heartbeat.enabled {
                    format!("every {}min", config.heartbeat.interval_minutes)
                } else {
                    "disabled".into()
                }
            );
            println!(
                "🧠 Memory:         {} (auto-save: {})",
                effective_memory_backend,
                if config.memory.auto_save { "on" } else { "off" }
            );

            println!();
            println!("Security:");
            println!("  Workspace only:    {}", config.autonomy.workspace_only);
            println!(
                "  Allowed roots:     {}",
                if config.autonomy.allowed_roots.is_empty() {
                    "(none)".to_string()
                } else {
                    config.autonomy.allowed_roots.join(", ")
                }
            );
            println!(
                "  Allowed commands:  {}",
                config.autonomy.allowed_commands.join(", ")
            );
            println!(
                "  Max actions/hour:  {}",
                config.autonomy.max_actions_per_hour
            );
            println!(
                "  Max cost/day:      ${:.2}",
                f64::from(config.autonomy.max_cost_per_day_cents) / 100.0
            );
            println!("  OTP enabled:       {}", config.security.otp.enabled);
            println!("  E-stop enabled:    {}", config.security.estop.enabled);
            println!();
            println!("Channels:");
            println!("  CLI:      ✅ always");
            for (channel, configured) in config.channels_config.channels() {
                println!(
                    "  {:9} {}",
                    channel.name(),
                    if configured {
                        "✅ configured"
                    } else {
                        "❌ not configured"
                    }
                );
            }
            println!();
            println!("Peripherals:");
            println!(
                "  Enabled:   {}",
                if config.peripherals.enabled {
                    "yes"
                } else {
                    "no"
                }
            );
            println!("  Boards:    {}", config.peripherals.boards.len());

            Ok(())
        }

        Commands::Update {
            check,
            force,
            instructions,
        } => {
            if instructions {
                update::print_update_instructions()?;
                Ok(())
            } else {
                update::self_update(force, check).await?;
                Ok(())
            }
        }

        Commands::Estop {
            estop_command,
            level,
            domains,
            tools,
        } => handle_estop_command(&config, estop_command, level, domains, tools),

        Commands::Cron { cron_command } => cron::handle_command(cron_command, &config),

        Commands::Models { model_command } => match model_command {
            ModelCommands::Refresh {
                provider,
                all,
                force,
            } => {
                if all {
                    if provider.is_some() {
                        bail!("`models refresh --all` cannot be combined with --provider");
                    }
                    onboard::run_models_refresh_all(&config, force).await
                } else {
                    onboard::run_models_refresh(&config, provider.as_deref(), force).await
                }
            }
            ModelCommands::List { provider } => {
                onboard::run_models_list(&config, provider.as_deref()).await
            }
            ModelCommands::Set { model } => onboard::run_models_set(&config, &model).await,
            ModelCommands::Status => onboard::run_models_status(&config).await,
        },

        Commands::ProvidersQuota { provider, format } => {
            let format_str = match format {
                QuotaFormat::Text => "text",
                QuotaFormat::Json => "json",
            };
            providers::quota_cli::run(&config, provider.as_deref(), format_str).await
        }

        Commands::Providers => {
            let providers = providers::list_providers();
            let current = config
                .default_provider
                .as_deref()
                .unwrap_or("openrouter")
                .trim()
                .to_ascii_lowercase();
            println!("Supported providers ({} total):\n", providers.len());
            println!("  ID (use in config)  DESCRIPTION");
            println!("  ─────────────────── ───────────");
            for p in &providers {
                let is_active = p.name.eq_ignore_ascii_case(&current)
                    || p.aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(&current));
                let marker = if is_active { " (active)" } else { "" };
                let local_tag = if p.local { " [local]" } else { "" };
                let aliases = if p.aliases.is_empty() {
                    String::new()
                } else {
                    format!("  (aliases: {})", p.aliases.join(", "))
                };
                println!(
                    "  {:<19} {}{}{}{}",
                    p.name, p.display_name, local_tag, marker, aliases
                );
            }
            println!("\n  custom:<URL>   Any OpenAI-compatible endpoint");
            println!("  anthropic-custom:<URL>  Any Anthropic-compatible endpoint");
            Ok(())
        }

        Commands::Service {
            service_command,
            service_init,
        } => {
            let init_system = service_init.parse()?;
            service::handle_command(&service_command, &config, init_system)
        }

        Commands::Doctor { doctor_command } => match doctor_command {
            Some(DoctorCommands::Models {
                provider,
                use_cache,
            }) => doctor::run_models(&config, provider.as_deref(), use_cache).await,
            Some(DoctorCommands::Traces {
                id,
                event,
                contains,
                limit,
            }) => doctor::run_traces(
                &config,
                id.as_deref(),
                event.as_deref(),
                contains.as_deref(),
                limit,
            ),
            None => doctor::run(&config),
        },

        Commands::Channel { channel_command } => match channel_command {
            ChannelCommands::Start => Box::pin(channels::start_channels(config)).await,
            ChannelCommands::Doctor => Box::pin(channels::doctor_channels(config)).await,
            other => channels::handle_command(other, &config).await,
        },

        Commands::Integrations {
            integration_command,
        } => integrations::handle_command(integration_command, &config),

        Commands::Skills { skill_command } => skills::handle_command(skill_command, &config),

        Commands::Migrate { migrate_command } => {
            migration::handle_command(migrate_command, &config).await
        }

        Commands::Memory { memory_command } => {
            memory::cli::handle_command(memory_command, &config).await
        }

        Commands::Auth { auth_command } => handle_auth_command(auth_command, &config).await,

        Commands::Hardware { hardware_command } => {
            hardware::handle_command(hardware_command.clone(), &config)
        }

        Commands::Peripheral { peripheral_command } => {
            peripherals::handle_command(peripheral_command.clone(), &config).await
        }

        Commands::Config { config_command } => match config_command {
            ConfigCommands::Show => {
                let mut json =
                    serde_json::to_value(&config).context("Failed to serialize config")?;
                redact_config_secrets(&mut json);
                println!("{}", serde_json::to_string_pretty(&json)?);
                Ok(())
            }
            ConfigCommands::Get { key } => {
                let mut json =
                    serde_json::to_value(&config).context("Failed to serialize config")?;
                redact_config_secrets(&mut json);

                let mut current = &json;
                for segment in key.split('.') {
                    current = current
                        .get(segment)
                        .with_context(|| format!("Config path not found: {key}"))?;
                }

                match current {
                    serde_json::Value::String(s) => println!("{s}"),
                    serde_json::Value::Bool(b) => println!("{b}"),
                    serde_json::Value::Number(n) => println!("{n}"),
                    serde_json::Value::Null => println!("null"),
                    _ => println!("{}", serde_json::to_string_pretty(current)?),
                }
                Ok(())
            }
            ConfigCommands::Set { key, value } => {
                let mut json =
                    serde_json::to_value(&config).context("Failed to serialize config")?;

                // Parse the new value: try bool, then integer, then float, then JSON, then string
                let new_value = if value == "true" {
                    serde_json::Value::Bool(true)
                } else if value == "false" {
                    serde_json::Value::Bool(false)
                } else if value == "null" {
                    serde_json::Value::Null
                } else if let Ok(n) = value.parse::<i64>() {
                    serde_json::json!(n)
                } else if let Ok(n) = value.parse::<f64>() {
                    serde_json::json!(n)
                } else if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&value) {
                    // JSON object/array (e.g. '["a","b"]' or '{"key":"val"}')
                    parsed
                } else {
                    serde_json::Value::String(value.clone())
                };

                // Navigate to the parent and set the leaf
                let segments: Vec<&str> = key.split('.').collect();
                if segments.is_empty() {
                    bail!("Config key cannot be empty");
                }
                let (parents, leaf) = segments.split_at(segments.len() - 1);

                let mut target = &mut json;
                for segment in parents {
                    target = target
                        .get_mut(*segment)
                        .with_context(|| format!("Config path not found: {key}"))?;
                }

                let leaf_key = leaf[0];
                if target.get(leaf_key).is_none() {
                    bail!("Config path not found: {key}");
                }
                target[leaf_key] = new_value.clone();

                // Deserialize back to Config and save.
                // Preserve runtime-only fields lost during JSON round-trip (#[serde(skip)]).
                let config_path = config.config_path.clone();
                let workspace_dir = config.workspace_dir.clone();
                config = serde_json::from_value(json)
                    .context("Invalid value for this config key — type mismatch")?;
                config.config_path = config_path;
                config.workspace_dir = workspace_dir;
                config.save().await?;

                // Show the saved value
                let display = match &new_value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                println!("Set {key} = {display}");
                Ok(())
            }
            ConfigCommands::Schema => {
                let schema = schemars::schema_for!(config::Config);
                println!(
                    "{}",
                    serde_json::to_string_pretty(&schema).expect("failed to serialize JSON Schema")
                );
                Ok(())
            }
        },
    }
}

/// Keys whose values are masked in `config show` / `config get` output.
const REDACTED_CONFIG_KEYS: &[&str] = &[
    "api_key",
    "api_keys",
    "bot_token",
    "paired_tokens",
    "db_url",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "secret_key",
    "webhook_secret",
];

fn redact_config_secrets(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if REDACTED_CONFIG_KEYS.contains(&k.as_str()) {
                    match v {
                        serde_json::Value::String(s) if !s.is_empty() => {
                            *v = serde_json::Value::String("***REDACTED***".to_string());
                        }
                        serde_json::Value::Array(arr) if !arr.is_empty() => {
                            *v = serde_json::json!(["***REDACTED***"]);
                        }
                        _ => {}
                    }
                } else {
                    redact_config_secrets(v);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                redact_config_secrets(item);
            }
        }
        _ => {}
    }
}

fn handle_estop_command(
    config: &Config,
    estop_command: Option<EstopSubcommands>,
    level: Option<EstopLevelArg>,
    domains: Vec<String>,
    tools: Vec<String>,
) -> Result<()> {
    if !config.security.estop.enabled {
        bail!("Emergency stop is disabled. Enable [security.estop].enabled = true in config.toml");
    }

    let config_dir = config
        .config_path
        .parent()
        .context("Config path must have a parent directory")?;
    let mut manager = security::EstopManager::load(&config.security.estop, config_dir)?;

    match estop_command {
        Some(EstopSubcommands::Status) => {
            print_estop_status(&manager.status());
            Ok(())
        }
        Some(EstopSubcommands::Resume {
            network,
            domains,
            tools,
            otp,
        }) => {
            let selector = build_resume_selector(network, domains, tools)?;
            let mut otp_code = otp;
            let otp_validator = if config.security.estop.require_otp_to_resume {
                if !config.security.otp.enabled {
                    bail!(
                        "security.estop.require_otp_to_resume=true but security.otp.enabled=false"
                    );
                }
                if otp_code.is_none() {
                    let entered = Password::new()
                        .with_prompt("Enter OTP code")
                        .allow_empty_password(false)
                        .interact()?;
                    otp_code = Some(entered);
                }

                let store = security::SecretStore::new(config_dir, config.secrets.encrypt);
                let (validator, enrollment_uri) =
                    security::OtpValidator::from_config(&config.security.otp, config_dir, &store)?;
                if let Some(uri) = enrollment_uri {
                    println!("Initialized OTP secret for ZeroClaw.");
                    println!("Enrollment URI: {uri}");
                }
                Some(validator)
            } else {
                None
            };

            manager.resume(selector, otp_code.as_deref(), otp_validator.as_ref())?;
            println!("Estop resume completed.");
            print_estop_status(&manager.status());
            Ok(())
        }
        None => {
            let engage_level = build_engage_level(level, domains, tools)?;
            manager.engage(engage_level)?;
            println!("Estop engaged.");
            print_estop_status(&manager.status());
            Ok(())
        }
    }
}

fn build_engage_level(
    level: Option<EstopLevelArg>,
    domains: Vec<String>,
    tools: Vec<String>,
) -> Result<security::EstopLevel> {
    let requested = level.unwrap_or(EstopLevelArg::KillAll);
    match requested {
        EstopLevelArg::KillAll => {
            if !domains.is_empty() || !tools.is_empty() {
                bail!("--domain/--tool are only valid with --level domain-block/tool-freeze");
            }
            Ok(security::EstopLevel::KillAll)
        }
        EstopLevelArg::NetworkKill => {
            if !domains.is_empty() || !tools.is_empty() {
                bail!("--domain/--tool are not valid with --level network-kill");
            }
            Ok(security::EstopLevel::NetworkKill)
        }
        EstopLevelArg::DomainBlock => {
            if domains.is_empty() {
                bail!("--level domain-block requires at least one --domain");
            }
            if !tools.is_empty() {
                bail!("--tool is not valid with --level domain-block");
            }
            Ok(security::EstopLevel::DomainBlock(domains))
        }
        EstopLevelArg::ToolFreeze => {
            if tools.is_empty() {
                bail!("--level tool-freeze requires at least one --tool");
            }
            if !domains.is_empty() {
                bail!("--domain is not valid with --level tool-freeze");
            }
            Ok(security::EstopLevel::ToolFreeze(tools))
        }
    }
}

fn build_resume_selector(
    network: bool,
    domains: Vec<String>,
    tools: Vec<String>,
) -> Result<security::ResumeSelector> {
    let selected =
        usize::from(network) + usize::from(!domains.is_empty()) + usize::from(!tools.is_empty());
    if selected > 1 {
        bail!("Use only one of --network, --domain, or --tool for estop resume");
    }
    if network {
        return Ok(security::ResumeSelector::Network);
    }
    if !domains.is_empty() {
        return Ok(security::ResumeSelector::Domains(domains));
    }
    if !tools.is_empty() {
        return Ok(security::ResumeSelector::Tools(tools));
    }
    Ok(security::ResumeSelector::KillAll)
}

fn print_estop_status(state: &security::EstopState) {
    println!("Estop status:");
    println!(
        "  engaged:        {}",
        if state.is_engaged() { "yes" } else { "no" }
    );
    println!(
        "  kill_all:       {}",
        if state.kill_all { "active" } else { "inactive" }
    );
    println!(
        "  network_kill:   {}",
        if state.network_kill {
            "active"
        } else {
            "inactive"
        }
    );
    if state.blocked_domains.is_empty() {
        println!("  domain_blocks:  (none)");
    } else {
        println!("  domain_blocks:  {}", state.blocked_domains.join(", "));
    }
    if state.frozen_tools.is_empty() {
        println!("  tool_freeze:    (none)");
    } else {
        println!("  tool_freeze:    {}", state.frozen_tools.join(", "));
    }
    if let Some(updated_at) = &state.updated_at {
        println!("  updated_at:     {updated_at}");
    }
}

fn write_shell_completion<W: Write>(shell: CompletionShell, writer: &mut W) -> Result<()> {
    use clap_complete::generate;
    use clap_complete::shells;

    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();

    match shell {
        CompletionShell::Bash => generate(shells::Bash, &mut cmd, bin_name.clone(), writer),
        CompletionShell::Fish => generate(shells::Fish, &mut cmd, bin_name.clone(), writer),
        CompletionShell::Zsh => generate(shells::Zsh, &mut cmd, bin_name.clone(), writer),
        CompletionShell::PowerShell => {
            generate(shells::PowerShell, &mut cmd, bin_name.clone(), writer);
        }
        CompletionShell::Elvish => generate(shells::Elvish, &mut cmd, bin_name, writer),
    }

    writer.flush()?;
    Ok(())
}

// ─── Generic Pending OAuth Login ────────────────────────────────────────────

/// Generic pending OAuth login state, shared across providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOAuthLogin {
    provider: String,
    profile: String,
    code_verifier: String,
    state: String,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOAuthLoginFile {
    #[serde(default)]
    provider: Option<String>,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_code_verifier: Option<String>,
    state: String,
    created_at: String,
}

fn pending_oauth_login_path(config: &Config, provider: &str) -> std::path::PathBuf {
    let filename = format!("auth-{}-pending.json", provider);
    auth::state_dir_from_config(config).join(filename)
}

fn pending_oauth_secret_store(config: &Config) -> security::secrets::SecretStore {
    security::secrets::SecretStore::new(
        &auth::state_dir_from_config(config),
        config.secrets.encrypt,
    )
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Check if a pending OAuth login is stale (older than 24 hours).
fn is_pending_login_stale(pending: &PendingOAuthLogin) -> bool {
    if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&pending.created_at) {
        let age = chrono::Utc::now().signed_duration_since(created);
        age > chrono::Duration::hours(24)
    } else {
        // If we can't parse the timestamp, consider it stale
        true
    }
}

fn save_pending_oauth_login(config: &Config, pending: &PendingOAuthLogin) -> Result<()> {
    let path = pending_oauth_login_path(config, &pending.provider);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secret_store = pending_oauth_secret_store(config);
    let encrypted_code_verifier = secret_store.encrypt(&pending.code_verifier)?;
    let persisted = PendingOAuthLoginFile {
        provider: Some(pending.provider.clone()),
        profile: pending.profile.clone(),
        code_verifier: None,
        encrypted_code_verifier: Some(encrypted_code_verifier),
        state: pending.state.clone(),
        created_at: pending.created_at.clone(),
    };
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let json = serde_json::to_vec_pretty(&persisted)?;
    std::fs::write(&tmp, json)?;
    set_owner_only_permissions(&tmp)?;
    std::fs::rename(tmp, &path)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

fn load_pending_oauth_login(config: &Config, provider: &str) -> Result<Option<PendingOAuthLogin>> {
    let path = pending_oauth_login_path(config, provider);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let persisted: PendingOAuthLoginFile = serde_json::from_slice(&bytes)?;
    let secret_store = pending_oauth_secret_store(config);
    let code_verifier = if let Some(encrypted) = persisted.encrypted_code_verifier {
        secret_store.decrypt(&encrypted)?
    } else if let Some(plaintext) = persisted.code_verifier {
        plaintext
    } else {
        bail!("Pending {} login is missing code verifier", provider);
    };

    let pending = PendingOAuthLogin {
        provider: persisted.provider.unwrap_or_else(|| provider.to_string()),
        profile: persisted.profile,
        code_verifier,
        state: persisted.state,
        created_at: persisted.created_at,
    };

    // Auto-cleanup if stale (older than 24 hours)
    if is_pending_login_stale(&pending) {
        println!("ℹ️  Removing stale pending auth file (older than 24h)");
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }

    Ok(Some(pending))
}

fn clear_pending_oauth_login(config: &Config, provider: &str) {
    let path = pending_oauth_login_path(config, provider);
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&path) {
        let _ = file.set_len(0);
        let _ = file.sync_all();
    }
    let _ = std::fs::remove_file(path);
}

fn read_auth_input(prompt: &str) -> Result<String> {
    let input = Password::new()
        .with_prompt(prompt)
        .allow_empty_password(false)
        .interact()?;
    Ok(input.trim().to_string())
}

fn read_plain_input(prompt: &str) -> Result<String> {
    let input: String = Input::new().with_prompt(prompt).interact_text()?;
    Ok(input.trim().to_string())
}

fn extract_openai_account_id_for_profile(access_token: &str) -> Option<String> {
    let account_id = auth::openai_oauth::extract_account_id_from_jwt(access_token);
    if account_id.is_none() {
        warn!(
            "Could not extract OpenAI account id from OAuth access token; \
             requests may fail until re-authentication."
        );
    }
    account_id
}

fn format_expiry(profile: &auth::profiles::AuthProfile) -> String {
    match profile
        .token_set
        .as_ref()
        .and_then(|token_set| token_set.expires_at)
    {
        Some(ts) => {
            let now = chrono::Utc::now();
            if ts <= now {
                format!("expired at {}", ts.to_rfc3339())
            } else {
                let mins = (ts - now).num_minutes();
                format!("expires in {mins}m ({})", ts.to_rfc3339())
            }
        }
        None => "n/a".to_string(),
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_auth_command(auth_command: AuthCommands, config: &Config) -> Result<()> {
    let auth_service = auth::AuthService::from_config(config);

    match auth_command {
        AuthCommands::Login {
            provider,
            profile,
            device_code,
        } => {
            let provider = auth::normalize_provider(&provider)?;
            let client = reqwest::Client::new();

            match provider.as_str() {
                "gemini" => {
                    // Gemini OAuth flow
                    if device_code {
                        match auth::gemini_oauth::start_device_code_flow(&client).await {
                            Ok(device) => {
                                println!("Google/Gemini device-code login started.");
                                println!("Visit: {}", device.verification_uri);
                                println!("Code:  {}", device.user_code);
                                if let Some(uri_complete) = &device.verification_uri_complete {
                                    println!("Fast link: {uri_complete}");
                                }

                                let token_set =
                                    auth::gemini_oauth::poll_device_code_tokens(&client, &device)
                                        .await?;
                                let account_id = token_set.id_token.as_deref().and_then(
                                    auth::gemini_oauth::extract_account_email_from_id_token,
                                );

                                auth_service
                                    .store_gemini_tokens(&profile, token_set, account_id, true)
                                    .await?;

                                println!("Saved profile {profile}");
                                println!("Active profile for gemini: {profile}");
                                return Ok(());
                            }
                            Err(e) => {
                                let err_msg = e.to_string();
                                if err_msg.contains("403")
                                    || err_msg.contains("Forbidden")
                                    || err_msg.contains("Cloudflare")
                                {
                                    println!(
                                        "ℹ️  Device-code flow is blocked by Cloudflare protection."
                                    );
                                    println!("   This is normal for server environments.");
                                    println!("   Switching to browser authorization flow...");
                                } else if err_msg.contains("invalid_client") {
                                    println!("⚠️  OAuth client configuration error: {}", err_msg);
                                    println!("   Check your GEMINI_OAUTH_CLIENT_ID and GEMINI_OAUTH_CLIENT_SECRET");
                                } else {
                                    println!("ℹ️  Device-code flow unavailable: {}", err_msg);
                                    println!("   Falling back to browser flow.");
                                }
                            }
                        }
                    }

                    let pkce = auth::gemini_oauth::generate_pkce_state();
                    let authorize_url = auth::gemini_oauth::build_authorize_url(&pkce)?;

                    // Save pending login for paste-redirect fallback
                    let pending = PendingOAuthLogin {
                        provider: "gemini".to_string(),
                        profile: profile.clone(),
                        code_verifier: pkce.code_verifier.clone(),
                        state: pkce.state.clone(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    };
                    save_pending_oauth_login(config, &pending)?;

                    println!("Open this URL in your browser and authorize access:");
                    println!("{authorize_url}");
                    println!();

                    let code = match auth::gemini_oauth::receive_loopback_code(
                        &pkce.state,
                        std::time::Duration::from_secs(180),
                    )
                    .await
                    {
                        Ok(code) => {
                            clear_pending_oauth_login(config, "gemini");
                            code
                        }
                        Err(e) => {
                            println!("Callback capture failed: {e}");
                            println!(
                                "Run `zeroclaw auth paste-redirect --provider gemini --profile {profile}`"
                            );
                            return Ok(());
                        }
                    };

                    let token_set =
                        auth::gemini_oauth::exchange_code_for_tokens(&client, &code, &pkce).await?;
                    let account_id = token_set
                        .id_token
                        .as_deref()
                        .and_then(auth::gemini_oauth::extract_account_email_from_id_token);

                    auth_service
                        .store_gemini_tokens(&profile, token_set, account_id, true)
                        .await?;

                    println!("Saved profile {profile}");
                    println!("Active profile for gemini: {profile}");
                    Ok(())
                }
                "openai-codex" => {
                    // OpenAI Codex OAuth flow
                    if device_code {
                        match auth::openai_oauth::start_device_code_flow(&client).await {
                            Ok(device) => {
                                println!("OpenAI device-code login started.");
                                println!("Visit: {}", device.verification_uri);
                                println!("Code:  {}", device.user_code);
                                if let Some(uri_complete) = &device.verification_uri_complete {
                                    println!("Fast link: {uri_complete}");
                                }
                                if let Some(message) = &device.message {
                                    println!("{message}");
                                }

                                let token_set =
                                    auth::openai_oauth::poll_device_code_tokens(&client, &device)
                                        .await?;
                                let account_id =
                                    extract_openai_account_id_for_profile(&token_set.access_token);

                                auth_service
                                    .store_openai_tokens(&profile, token_set, account_id, true)
                                    .await?;
                                clear_pending_oauth_login(config, "openai");

                                println!("Saved profile {profile}");
                                println!("Active profile for openai-codex: {profile}");
                                return Ok(());
                            }
                            Err(e) => {
                                let err_msg = e.to_string();
                                if err_msg.contains("403")
                                    || err_msg.contains("Forbidden")
                                    || err_msg.contains("Cloudflare")
                                {
                                    println!(
                                        "ℹ️  Device-code flow is blocked by Cloudflare protection."
                                    );
                                    println!("   This is normal for server environments.");
                                    println!("   Switching to browser authorization flow...");
                                } else {
                                    println!("ℹ️  Device-code flow unavailable: {}", err_msg);
                                    println!("   Falling back to browser flow.");
                                }
                            }
                        }
                    }

                    let pkce = auth::openai_oauth::generate_pkce_state();
                    let pending = PendingOAuthLogin {
                        provider: "openai".to_string(),
                        profile: profile.clone(),
                        code_verifier: pkce.code_verifier.clone(),
                        state: pkce.state.clone(),
                        created_at: chrono::Utc::now().to_rfc3339(),
                    };
                    save_pending_oauth_login(config, &pending)?;

                    let authorize_url = auth::openai_oauth::build_authorize_url(&pkce);
                    println!("Open this URL in your browser and authorize access:");
                    println!("{authorize_url}");
                    println!();
                    println!("Waiting for callback at http://localhost:1455/auth/callback ...");

                    let code = match auth::openai_oauth::receive_loopback_code(
                        &pkce.state,
                        std::time::Duration::from_secs(180),
                    )
                    .await
                    {
                        Ok(code) => code,
                        Err(e) => {
                            println!("Callback capture failed: {e}");
                            println!(
                                "Run `zeroclaw auth paste-redirect --provider openai-codex --profile {profile}`"
                            );
                            return Ok(());
                        }
                    };

                    let token_set =
                        auth::openai_oauth::exchange_code_for_tokens(&client, &code, &pkce).await?;
                    let account_id = extract_openai_account_id_for_profile(&token_set.access_token);

                    auth_service
                        .store_openai_tokens(&profile, token_set, account_id, true)
                        .await?;
                    clear_pending_oauth_login(config, "openai");

                    println!("Saved profile {profile}");
                    println!("Active profile for openai-codex: {profile}");
                    Ok(())
                }
                _ => {
                    bail!(
                        "`auth login` supports --provider openai-codex or gemini, got: {provider}"
                    );
                }
            }
        }

        AuthCommands::PasteRedirect {
            provider,
            profile,
            input,
        } => {
            let provider = auth::normalize_provider(&provider)?;

            match provider.as_str() {
                "openai-codex" => {
                    let result = async {
                        let pending =
                            load_pending_oauth_login(config, "openai")?.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No pending OpenAI login found.\n\n\
                                💡 Please start the login flow first:\n   \
                                zeroclaw auth login --provider openai-codex --profile {}\n\n\
                                Then paste the callback URL or code here.",
                                    profile
                                )
                            })?;

                        if pending.profile != profile {
                            bail!(
                                "{} pending={}, requested={}",
                                PROFILE_MISMATCH_PREFIX,
                                pending.profile,
                                profile
                            );
                        }

                        let redirect_input = match input {
                            Some(value) => value,
                            None => read_plain_input("Paste redirect URL or OAuth code")?,
                        };

                        let code = auth::openai_oauth::parse_code_from_redirect(
                            &redirect_input,
                            Some(&pending.state),
                        )?;

                        let pkce = auth::openai_oauth::PkceState {
                            code_verifier: pending.code_verifier.clone(),
                            code_challenge: String::new(),
                            state: pending.state.clone(),
                        };

                        let client = reqwest::Client::new();
                        let token_set =
                            auth::openai_oauth::exchange_code_for_tokens(&client, &code, &pkce)
                                .await?;
                        let account_id =
                            extract_openai_account_id_for_profile(&token_set.access_token);

                        auth_service
                            .store_openai_tokens(&profile, token_set, account_id, true)
                            .await?;
                        clear_pending_oauth_login(config, "openai");

                        println!("Saved profile {profile}");
                        println!("Active profile for openai-codex: {profile}");
                        Ok(())
                    }
                    .await;

                    if let Err(e) = result {
                        // Cleanup pending file on error
                        if e.to_string().starts_with(PROFILE_MISMATCH_PREFIX) {
                            clear_pending_oauth_login(config, "openai");
                            eprintln!("❌ {}", e);
                            eprintln!(
                                "\n💡 Tip: A previous login attempt was for a different profile."
                            );
                            eprintln!("   The pending auth file has been cleared.");
                            eprintln!("   Please start fresh with:");
                            eprintln!(
                                "   zeroclaw auth login --provider openai-codex --profile {}",
                                profile
                            );
                            std::process::exit(1);
                        }
                        return Err(e);
                    }
                }
                "gemini" => {
                    let result = async {
                        let pending =
                            load_pending_oauth_login(config, "gemini")?.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "No pending Gemini login found.\n\n\
                                💡 Please start the login flow first:\n   \
                                zeroclaw auth login --provider gemini --profile {}\n\n\
                                Then paste the callback URL or code here.",
                                    profile
                                )
                            })?;

                        if pending.profile != profile {
                            bail!(
                                "{} pending={}, requested={}",
                                PROFILE_MISMATCH_PREFIX,
                                pending.profile,
                                profile
                            );
                        }

                        let redirect_input = match input {
                            Some(value) => value,
                            None => read_plain_input("Paste redirect URL or OAuth code")?,
                        };

                        let code = auth::gemini_oauth::parse_code_from_redirect(
                            &redirect_input,
                            Some(&pending.state),
                        )?;

                        let pkce = auth::gemini_oauth::PkceState {
                            code_verifier: pending.code_verifier.clone(),
                            code_challenge: String::new(),
                            state: pending.state.clone(),
                        };

                        let client = reqwest::Client::new();
                        let token_set =
                            auth::gemini_oauth::exchange_code_for_tokens(&client, &code, &pkce)
                                .await?;
                        let account_id = token_set
                            .id_token
                            .as_deref()
                            .and_then(auth::gemini_oauth::extract_account_email_from_id_token);

                        auth_service
                            .store_gemini_tokens(&profile, token_set, account_id, true)
                            .await?;
                        clear_pending_oauth_login(config, "gemini");

                        println!("Saved profile {profile}");
                        println!("Active profile for gemini: {profile}");
                        Ok(())
                    }
                    .await;

                    if let Err(e) = result {
                        // Cleanup pending file on error
                        if e.to_string().starts_with(PROFILE_MISMATCH_PREFIX) {
                            clear_pending_oauth_login(config, "gemini");
                            eprintln!("❌ {}", e);
                            eprintln!(
                                "\n💡 Tip: A previous login attempt was for a different profile."
                            );
                            eprintln!("   The pending auth file has been cleared.");
                            eprintln!("   Please start fresh with:");
                            eprintln!(
                                "   zeroclaw auth login --provider gemini --profile {}",
                                profile
                            );
                            std::process::exit(1);
                        }
                        return Err(e);
                    }
                }
                _ => {
                    bail!("`auth paste-redirect` supports --provider openai-codex or gemini");
                }
            }
            Ok(())
        }

        AuthCommands::PasteToken {
            provider,
            profile,
            token,
            auth_kind,
        } => {
            let provider = auth::normalize_provider(&provider)?;
            let token = match token {
                Some(token) => token.trim().to_string(),
                None => read_auth_input("Paste token")?,
            };
            if token.is_empty() {
                bail!("Token cannot be empty");
            }

            let kind = auth::anthropic_token::detect_auth_kind(&token, auth_kind.as_deref());
            let mut metadata = std::collections::HashMap::new();
            metadata.insert(
                "auth_kind".to_string(),
                kind.as_metadata_value().to_string(),
            );

            auth_service
                .store_provider_token(&provider, &profile, &token, metadata, true)
                .await?;
            println!("Saved profile {profile}");
            println!("Active profile for {provider}: {profile}");
            Ok(())
        }

        AuthCommands::SetupToken { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            let token = read_auth_input("Paste token")?;
            if token.is_empty() {
                bail!("Token cannot be empty");
            }

            let kind = auth::anthropic_token::detect_auth_kind(&token, Some("authorization"));
            let mut metadata = std::collections::HashMap::new();
            metadata.insert(
                "auth_kind".to_string(),
                kind.as_metadata_value().to_string(),
            );

            auth_service
                .store_provider_token(&provider, &profile, &token, metadata, true)
                .await?;
            println!("Saved profile {profile}");
            println!("Active profile for {provider}: {profile}");
            Ok(())
        }

        AuthCommands::Refresh { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;

            match provider.as_str() {
                "openai-codex" => {
                    match auth_service
                        .get_valid_openai_access_token(profile.as_deref())
                        .await?
                    {
                        Some(_) => {
                            println!("OpenAI Codex token is valid (refresh completed if needed).");
                            Ok(())
                        }
                        None => {
                            bail!(
                                "No OpenAI Codex auth profile found. Run `zeroclaw auth login --provider openai-codex`."
                            )
                        }
                    }
                }
                "gemini" => {
                    match auth_service
                        .get_valid_gemini_access_token(profile.as_deref())
                        .await?
                    {
                        Some(_) => {
                            let profile_name = profile.as_deref().unwrap_or("default");
                            println!("✓ Gemini token refreshed successfully");
                            println!("  Profile: gemini:{}", profile_name);
                            Ok(())
                        }
                        None => {
                            bail!(
                                "No Gemini auth profile found. Run `zeroclaw auth login --provider gemini`."
                            )
                        }
                    }
                }
                _ => bail!("`auth refresh` supports --provider openai-codex or gemini"),
            }
        }

        AuthCommands::Logout { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            let removed = auth_service.remove_profile(&provider, &profile).await?;
            if removed {
                println!("Removed auth profile {provider}:{profile}");
            } else {
                println!("Auth profile not found: {provider}:{profile}");
            }
            Ok(())
        }

        AuthCommands::Use { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            auth_service.set_active_profile(&provider, &profile).await?;
            println!("Active profile for {provider}: {profile}");
            Ok(())
        }

        AuthCommands::List => {
            let data = auth_service.load_profiles().await?;
            if data.profiles.is_empty() {
                println!("No auth profiles configured.");
                return Ok(());
            }

            for (id, profile) in &data.profiles {
                let active = data
                    .active_profiles
                    .get(&profile.provider)
                    .is_some_and(|active_id| active_id == id);
                let marker = if active { "*" } else { " " };
                println!("{marker} {id}");
            }

            Ok(())
        }

        AuthCommands::Status => {
            let data = auth_service.load_profiles().await?;
            if data.profiles.is_empty() {
                println!("No auth profiles configured.");
                return Ok(());
            }

            for (id, profile) in &data.profiles {
                let active = data
                    .active_profiles
                    .get(&profile.provider)
                    .is_some_and(|active_id| active_id == id);
                let marker = if active { "*" } else { " " };
                println!(
                    "{} {} kind={:?} account={} expires={}",
                    marker,
                    id,
                    profile.kind,
                    crate::security::redact(profile.account_id.as_deref().unwrap_or("unknown")),
                    format_expiry(profile)
                );
            }

            println!();
            println!("Active profiles:");
            for (provider, profile_id) in &data.active_profiles {
                println!("  {provider}: {profile_id}");
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_definition_has_no_flag_conflicts() {
        Cli::command().debug_assert();
    }

    #[test]
    fn onboard_help_includes_model_flag() {
        let cmd = Cli::command();
        let onboard = cmd
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "onboard")
            .expect("onboard subcommand must exist");

        let has_model_flag = onboard
            .get_arguments()
            .any(|arg| arg.get_id().as_str() == "model" && arg.get_long() == Some("model"));

        assert!(
            has_model_flag,
            "onboard help should include --model for quick setup overrides"
        );
    }

    #[test]
    fn onboard_cli_accepts_model_provider_and_api_key_in_quick_mode() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "onboard",
            "--provider",
            "openrouter",
            "--model",
            "custom-model-946",
            "--api-key",
            "sk-issue946",
        ])
        .expect("quick onboard invocation should parse");

        match cli.command {
            Commands::Onboard {
                interactive,
                force,
                channels_only,
                api_key,
                provider,
                model,
                ..
            } => {
                assert!(!interactive);
                assert!(!force);
                assert!(!channels_only);
                assert_eq!(provider.as_deref(), Some("openrouter"));
                assert_eq!(model.as_deref(), Some("custom-model-946"));
                assert_eq!(api_key.as_deref(), Some("sk-issue946"));
            }
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    fn completions_cli_parses_supported_shells() {
        for shell in ["bash", "fish", "zsh", "powershell", "elvish"] {
            let cli = Cli::try_parse_from(["zeroclaw", "completions", shell])
                .expect("completions invocation should parse");
            match cli.command {
                Commands::Completions { .. } => {}
                other => panic!("expected completions command, got {other:?}"),
            }
        }
    }

    #[test]
    fn gateway_help_includes_new_pairing_flag() {
        let cmd = Cli::command();
        let gateway = cmd
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "gateway")
            .expect("gateway subcommand must exist");

        let has_new_pairing_flag = gateway.get_arguments().any(|arg| {
            arg.get_id().as_str() == "new_pairing" && arg.get_long() == Some("new-pairing")
        });

        assert!(
            has_new_pairing_flag,
            "gateway help should include --new-pairing"
        );
    }

    #[test]
    fn gateway_cli_accepts_new_pairing_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "gateway", "--new-pairing"])
            .expect("gateway --new-pairing should parse");

        match cli.command {
            Commands::Gateway { new_pairing, .. } => assert!(new_pairing),
            other => panic!("expected gateway command, got {other:?}"),
        }
    }

    #[test]
    fn gateway_cli_defaults_new_pairing_to_false() {
        let cli = Cli::try_parse_from(["zeroclaw", "gateway"]).expect("gateway should parse");

        match cli.command {
            Commands::Gateway { new_pairing, .. } => assert!(!new_pairing),
            other => panic!("expected gateway command, got {other:?}"),
        }
    }

    #[test]
    fn completion_generation_mentions_binary_name() {
        let mut output = Vec::new();
        write_shell_completion(CompletionShell::Bash, &mut output)
            .expect("completion generation should succeed");
        let script = String::from_utf8(output).expect("completion output should be valid utf-8");
        assert!(
            script.contains("zeroclaw"),
            "completion script should reference binary name"
        );
    }

    #[test]
    fn onboard_cli_accepts_force_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--force"])
            .expect("onboard --force should parse");

        match cli.command {
            Commands::Onboard { force, .. } => assert!(force),
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    fn onboard_cli_accepts_no_totp_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--no-totp"])
            .expect("onboard --no-totp should parse");

        match cli.command {
            Commands::Onboard { no_totp, .. } => assert!(no_totp),
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    fn onboard_cli_accepts_openclaw_migration_flags() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "onboard",
            "--migrate-openclaw",
            "--openclaw-source",
            "/tmp/openclaw-workspace",
            "--openclaw-config",
            "/tmp/openclaw.json",
        ])
        .expect("onboard openclaw migration flags should parse");

        match cli.command {
            Commands::Onboard {
                migrate_openclaw,
                openclaw_source,
                openclaw_config,
                ..
            } => {
                assert!(migrate_openclaw);
                assert_eq!(
                    openclaw_source.as_deref(),
                    Some(std::path::Path::new("/tmp/openclaw-workspace"))
                );
                assert_eq!(
                    openclaw_config.as_deref(),
                    Some(std::path::Path::new("/tmp/openclaw.json"))
                );
            }
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    fn migrate_openclaw_cli_accepts_source_and_module_flags() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "migrate",
            "openclaw",
            "--source",
            "/tmp/openclaw-workspace",
            "--source-config",
            "/tmp/openclaw.json",
            "--dry-run",
            "--no-config",
        ])
        .expect("migrate openclaw flags should parse");

        match cli.command {
            Commands::Migrate {
                migrate_command:
                    MigrateCommands::Openclaw {
                        source,
                        source_config,
                        dry_run,
                        no_memory,
                        no_config,
                    },
            } => {
                assert_eq!(
                    source.as_deref(),
                    Some(std::path::Path::new("/tmp/openclaw-workspace"))
                );
                assert_eq!(
                    source_config.as_deref(),
                    Some(std::path::Path::new("/tmp/openclaw.json"))
                );
                assert!(dry_run);
                assert!(!no_memory);
                assert!(no_config);
            }
            other => panic!("expected migrate openclaw command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_estop_default_engage() {
        let cli = Cli::try_parse_from(["zeroclaw", "estop"]).expect("estop command should parse");

        match cli.command {
            Commands::Estop {
                estop_command,
                level,
                domains,
                tools,
            } => {
                assert!(estop_command.is_none());
                assert!(level.is_none());
                assert!(domains.is_empty());
                assert!(tools.is_empty());
            }
            other => panic!("expected estop command, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_estop_resume_domain() {
        let cli = Cli::try_parse_from(["zeroclaw", "estop", "resume", "--domain", "*.chase.com"])
            .expect("estop resume command should parse");

        match cli.command {
            Commands::Estop {
                estop_command: Some(EstopSubcommands::Resume { domains, .. }),
                ..
            } => assert_eq!(domains, vec!["*.chase.com".to_string()]),
            other => panic!("expected estop resume command, got {other:?}"),
        }
    }

    #[test]
    fn config_help_mentions_show_get_set_examples() {
        let cmd = Cli::command();
        let config_cmd = cmd
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "config")
            .expect("config subcommand must exist");

        let mut output = Vec::new();
        config_cmd
            .clone()
            .write_long_help(&mut output)
            .expect("help generation should succeed");
        let help = String::from_utf8(output).expect("help output should be utf-8");
        assert!(help.contains("zeroclaw config show"));
        assert!(help.contains("zeroclaw config get gateway.port"));
        assert!(help.contains("zeroclaw config set gateway.port 8080"));
    }

    #[test]
    fn config_cli_parses_show_get_set_subcommands() {
        let show =
            Cli::try_parse_from(["zeroclaw", "config", "show"]).expect("config show should parse");
        match show.command {
            Commands::Config {
                config_command: ConfigCommands::Show,
            } => {}
            other => panic!("expected config show, got {other:?}"),
        }

        let get = Cli::try_parse_from(["zeroclaw", "config", "get", "gateway.port"])
            .expect("config get should parse");
        match get.command {
            Commands::Config {
                config_command: ConfigCommands::Get { key },
            } => assert_eq!(key, "gateway.port"),
            other => panic!("expected config get, got {other:?}"),
        }

        let set = Cli::try_parse_from(["zeroclaw", "config", "set", "gateway.port", "8080"])
            .expect("config set should parse");
        match set.command {
            Commands::Config {
                config_command: ConfigCommands::Set { key, value },
            } => {
                assert_eq!(key, "gateway.port");
                assert_eq!(value, "8080");
            }
            other => panic!("expected config set, got {other:?}"),
        }
    }

    #[test]
    fn redact_config_secrets_masks_nested_sensitive_values() {
        let mut payload = serde_json::json!({
            "api_key": "sk-test",
            "nested": {
                "bot_token": "token",
                "paired_tokens": ["abc", "def"],
                "non_secret": "ok"
            }
        });
        redact_config_secrets(&mut payload);

        assert_eq!(payload["api_key"], serde_json::json!("***REDACTED***"));
        assert_eq!(
            payload["nested"]["bot_token"],
            serde_json::json!("***REDACTED***")
        );
        assert_eq!(
            payload["nested"]["paired_tokens"],
            serde_json::json!(["***REDACTED***"])
        );
        assert_eq!(payload["nested"]["non_secret"], serde_json::json!("ok"));
    }

    #[test]
    fn update_help_mentions_instructions_flag() {
        let cmd = Cli::command();
        let update_cmd = cmd
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == "update")
            .expect("update subcommand must exist");

        let mut output = Vec::new();
        update_cmd
            .clone()
            .write_long_help(&mut output)
            .expect("help generation should succeed");
        let help = String::from_utf8(output).expect("help output should be utf-8");

        assert!(help.contains("--instructions"));
    }

    #[test]
    fn update_cli_parses_instructions_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "update", "--instructions"])
            .expect("update --instructions should parse");

        match cli.command {
            Commands::Update {
                check,
                force,
                instructions,
            } => {
                assert!(!check);
                assert!(!force);
                assert!(instructions);
            }
            other => panic!("expected update command, got {other:?}"),
        }
    }
}
