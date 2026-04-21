#![warn(clippy::all, clippy::pedantic)]
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

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::{fmt, EnvFilter};

use lightwave_sys::channels;
use lightwave_sys::config::Config;
use lightwave_sys::daemon;
use lightwave_sys::memory;
#[cfg(feature = "createos")]
use lightwave_sys::CreateOsCommands;
use lightwave_sys::{BrainCommands, ChannelCommands, MemoryCommands};

/// LightWave Augusta — local AI agent runtime for macOS.
#[derive(Parser, Debug)]
#[command(name = "augusta")]
#[command(author = "LightWave Media")]
#[command(version)]
#[command(about = "Local AI agent runtime for macOS.", long_about = None)]
struct Cli {
    #[arg(long, global = true)]
    config_dir: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the interactive agent (CLI channel)
    #[command(long_about = "\
Start the interactive AI agent loop.

Launches an interactive chat session with the configured AI provider.
Use --message for single-shot queries without entering interactive mode.

Examples:
  augusta agent                                # interactive session
  augusta agent -m \"Summarize today's logs\"    # single message
  augusta agent -p anthropic --model claude-sonnet-4-6")]
    Agent {
        /// Single message mode (don't enter interactive mode)
        #[arg(short, long)]
        message: Option<String>,

        /// Provider to use (anthropic, openai, ollama)
        #[arg(short, long)]
        provider: Option<String>,

        /// Model to use
        #[arg(long)]
        model: Option<String>,

        /// Temperature (0.0 - 2.0)
        #[arg(short, long)]
        temperature: Option<f64>,
    },

    /// Start configured channels
    Channel {
        #[command(subcommand)]
        channel_command: ChannelCommands,
    },

    /// Manage memory
    Memory {
        #[command(subcommand)]
        memory_command: MemoryCommands,
    },

    /// Brain vector DB (index, query, stats, validate)
    #[command(long_about = "\
Manage the Brain vector database for ~/.brain/ knowledge retrieval.

Indexes YAML, Markdown, and Mermaid files with local ONNX embeddings
(BGE-small-en-v1.5) and provides hybrid FTS5 + cosine similarity search.

Examples:
  augusta brain index              # incremental index
  augusta brain index --full       # full rebuild
  augusta brain query \"multi-tenant database\"
  augusta brain query --session backend --budget 4000
  augusta brain stats              # chunk/file counts
  augusta brain validate           # hash verification")]
    Brain {
        #[command(subcommand)]
        brain_command: BrainCommands,
    },

    /// Manage the background daemon
    #[command(long_about = "\
Manage the Augusta background daemon (launchd).

The daemon provides persistent macOS integration: permission brokering,
file system events, service registry, and health monitoring.

Examples:
  augusta daemon start       # start the daemon
  augusta daemon stop        # stop the daemon
  augusta daemon status      # check daemon status
  augusta daemon install     # install launchd plist (auto-start on login)
  augusta daemon uninstall   # remove launchd plist")]
    Daemon {
        #[command(subcommand)]
        action: DaemonCommands,
    },

    /// Show the agent activity feed
    #[command(long_about = "\
Display real-time agent activity feed.

Shows agent status, task progress, and system events.
Use --plain for non-interactive output.

Keybindings (TUI mode):
  j/k   — scroll down/up
  Tab   — cycle panels
  1/2/3 — jump to panel
  p     — toggle problems-only mode
  q     — quit

Examples:
  augusta feed               # interactive TUI
  augusta feed --plain       # plain text output
  augusta feed --problems    # show problems only")]
    Feed {
        /// Plain text output (no TUI)
        #[arg(long)]
        plain: bool,
        /// Show only problems (stuck agents, failures)
        #[arg(long)]
        problems: bool,
    },

    /// Manage createOS pipeline (tasks, epics, sprints)
    #[cfg(feature = "createos")]
    #[command(long_about = "\
Manage the createOS planning pipeline via local SQLite (offline-first).

Syncs bidirectionally with PostgreSQL via PowerSync when connected.
Works fully offline — changes queue locally until sync resumes.

Examples:
  augusta createos tasks                        # list all tasks
  augusta createos tasks --status in_progress   # filter by status
  augusta createos epics                        # list epics
  augusta createos sprints --status active      # active sprints
  augusta createos sync                         # show sync status")]
    #[command(name = "createos")]
    CreateOS {
        #[command(subcommand)]
        createos_command: CreateOsCommands,
    },

    /// Show version and system info
    Version,
}

/// Daemon management subcommands.
#[derive(Subcommand, Debug)]
enum DaemonCommands {
    /// Start the daemon in the foreground
    Start,
    /// Stop a running daemon
    Stop,
    /// Show daemon status
    Status,
    /// Install launchd plist for auto-start on login
    Install,
    /// Remove launchd plist
    Uninstall,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(false).init();

    let cli = Cli::parse();

    // Load config
    let mut config = Config::load_or_init()
        .await
        .context("Failed to load Augusta config")?;

    // Check macOS permissions for desktop automation
    #[cfg(target_os = "macos")]
    {
        let perms = lightwave_macos::permission::check_permissions();
        if !perms.all_granted() {
            for missing in perms.missing_permissions() {
                eprintln!("Warning: Missing permission — {missing}");
            }
            eprintln!("Some desktop automation features will be unavailable.");
            eprintln!();
        }
    }

    match cli.command {
        Commands::Agent {
            message,
            provider,
            model,
            temperature,
        } => {
            // Apply CLI overrides
            if let Some(p) = provider {
                config.default_provider = Some(p);
            }
            if let Some(m) = model {
                config.default_model = Some(m);
            }
            if let Some(t) = temperature {
                config.default_temperature = t;
            }

            let temp = config.default_temperature;
            lightwave_sys::agent::loop_::run(
                config,
                message,
                None, // provider already applied to config above
                None, // model already applied to config above
                temp,
                vec![],
                true, // interactive (enables approval manager)
            )
            .await?;
        }

        Commands::Channel { channel_command } => match channel_command {
            ChannelCommands::List => {
                println!("Configured channels:");
                println!("  - cli (always available)");
                println!("  - imessage (macOS native, requires Full Disk Access)");
            }
            ChannelCommands::Start => {
                channels::start_cli(config).await?;
            }
            ChannelCommands::IMessage => {
                channels::start_imessage(config).await?;
            }
        },

        Commands::Memory { memory_command } => {
            memory::cli::handle_command(memory_command, &config).await?;
        }

        Commands::Brain { brain_command } => {
            lightwave_sys::brain::cli::handle_command(brain_command).await?;
        }

        Commands::Daemon { action } => match action {
            DaemonCommands::Start => {
                let daemon_config = daemon::DaemonConfig::default();
                daemon::run_daemon(daemon_config, Some(&config)).await?;
            }
            DaemonCommands::Stop => {
                let result = daemon::daemon_stop()?;
                println!("{result}");
            }
            DaemonCommands::Status => {
                let status = daemon::daemon_status()?;
                println!("{status}");
            }
            DaemonCommands::Install => {
                daemon::install_launchd()?;
                println!("Augusta daemon installed for auto-start.");
            }
            DaemonCommands::Uninstall => {
                daemon::uninstall_launchd()?;
                println!("Augusta daemon uninstalled.");
            }
        },

        Commands::Feed { plain, problems } => {
            if plain {
                let mut app = lightwave_sys::tui::FeedApp::new(100);
                let event_log = lightwave_sys::tui::event_bus::default_event_log_path();
                let _ = lightwave_sys::tui::event_bus::load_events(&event_log, &mut app);
                if problems {
                    app.toggle_problems();
                }
                let events: Vec<_> = app.visible_events().into_iter().cloned().collect();
                let output = lightwave_sys::tui::feed::render_plain(&events);
                if output.is_empty() {
                    println!("No events to display.");
                } else {
                    println!("{output}");
                }
            } else {
                lightwave_sys::tui::tui_render::run_tui(problems)?;
            }
        }

        #[cfg(feature = "createos")]
        Commands::CreateOS { createos_command } => {
            lightwave_sys::createos::cli::handle_command(createos_command, &config.workspace_dir)
                .await?;
        }

        Commands::Version => {
            println!("LightWave Augusta v{}", env!("CARGO_PKG_VERSION"));
            println!("Runtime: native (macOS)");
            println!(
                "Provider: {}",
                config.default_provider.as_deref().unwrap_or("openrouter")
            );
            println!(
                "Model: {}",
                config
                    .default_model
                    .as_deref()
                    .unwrap_or("anthropic/claude-sonnet-4")
            );
            println!("Memory: {}", config.memory.backend);
            println!("Config: {}", config.config_path.display());
        }
    }

    Ok(())
}
