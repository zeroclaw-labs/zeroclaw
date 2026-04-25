#![warn(clippy::all, clippy::pedantic)]
#![forbid(unsafe_code)]
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
    clippy::must_use_candidate,
    clippy::new_without_default,
    clippy::needless_pass_by_value,
    clippy::needless_raw_string_hashes,
    clippy::redundant_closure_for_method_calls,
    clippy::return_self_not_must_use,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::struct_field_names,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_literal_bound,
    clippy::unnecessary_map_or,
    clippy::unused_self,
    clippy::cast_precision_loss,
    clippy::unnecessary_wraps,
    clippy::large_stack_arrays,
    dead_code
)]

use clap::Subcommand;
use serde::{Deserialize, Serialize};

pub mod advisor;
pub mod agent;
pub(crate) mod approval;
pub(crate) mod auth;
pub mod billing;
pub mod categories;
pub mod channels;
pub mod coding;
pub mod config;
pub mod coordination;
pub(crate) mod cost;
pub(crate) mod cron;
pub(crate) mod daemon;
pub mod dispatch;
pub(crate) mod doctor;
pub mod economic;
pub mod gatekeeper;
pub mod gateway;
pub mod goals;
pub(crate) mod hardware;
pub(crate) mod health;
// `host_probe` (host hardware capabilities for Gemma 4 tier auto-selection).
// Distinct from `hardware` (USB peripheral discovery).
pub mod host_probe;
pub(crate) mod heartbeat;
pub mod hooks;
pub(crate) mod identity;
// Intentionally unused re-export — public API surface for plugin authors.
pub(crate) mod integrations;
// `local_llm` (on-device Gemma 4 fallback: daemon health, model pull, config).
// Distinct from `providers::ollama` which handles inference (chat/completion).
pub mod local_llm;
pub mod memory;
pub(crate) mod migration;
pub(crate) mod multimodal;
pub mod observability;
pub(crate) mod onboard;
pub mod ontology;
pub mod peripherals;
pub mod phone;
#[allow(unused_imports)]
pub(crate) mod plugins;
pub mod providers;
pub mod rag;
pub mod runtime;
pub mod sandbox;
pub(crate) mod security;
pub(crate) mod service;
pub mod services;
pub(crate) mod session_search;
pub(crate) mod skills;
pub(crate) mod storage;
pub mod sync;
pub mod task_category;
pub mod telemetry;
pub mod tools;
pub(crate) mod tunnel;
pub mod update;
pub(crate) mod user_model;
pub(crate) mod util;
pub mod vault;
pub mod voice;
pub mod workflow;

pub use config::Config;

/// Service management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceCommands {
    /// Install daemon service unit for auto-start and restart
    Install,
    /// Start daemon service
    Start,
    /// Stop daemon service
    Stop,
    /// Restart daemon service to apply latest config
    Restart,
    /// Check daemon service status
    Status,
    /// Uninstall daemon service unit
    Uninstall,
}

/// Channel management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelCommands {
    /// List all configured channels
    List,
    /// Start all configured channels (handled in main.rs for async)
    Start,
    /// Run health checks for configured channels (handled in main.rs for async)
    Doctor,
    /// Add a new channel configuration
    #[command(long_about = "\
Add a new channel configuration.

Provide the channel type and a JSON object with the required \
configuration keys for that channel type.

Supported types: telegram, discord, slack, whatsapp, github, matrix, imessage, email.

Examples:
  zeroclaw channel add telegram '{\"bot_token\":\"...\",\"name\":\"my-bot\"}'
  zeroclaw channel add discord '{\"bot_token\":\"...\",\"name\":\"my-discord\"}'")]
    Add {
        /// Channel type (telegram, discord, slack, whatsapp, github, matrix, imessage, email)
        channel_type: String,
        /// Optional configuration as JSON
        config: String,
    },
    /// Remove a channel configuration
    Remove {
        /// Channel name to remove
        name: String,
    },
    /// Bind a Telegram identity (username or numeric user ID) into allowlist
    #[command(long_about = "\
Bind a Telegram identity into the allowlist.

Adds a Telegram username (without the '@' prefix) or numeric user \
ID to the channel allowlist so the agent will respond to messages \
from that identity.

Examples:
  zeroclaw channel bind-telegram zeroclaw_user
  zeroclaw channel bind-telegram 123456789")]
    BindTelegram {
        /// Telegram identity to allow (username without '@' or numeric user ID)
        identity: String,
    },
}

/// Skills management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillCommands {
    /// List all installed skills
    List,
    /// Scaffold a new skill project from a template
    New {
        /// Skill name (snake_case recommended, e.g. my_weather_tool)
        name: String,
        /// Template language: typescript, rust, go, python
        #[arg(long, short, default_value = "typescript")]
        template: String,
    },
    /// Run a skill tool locally for testing (reads args from --args or stdin)
    Test {
        /// Path to the skill directory or installed skill name
        path: String,
        /// Optional tool name inside the skill (defaults to first tool found)
        #[arg(long)]
        tool: Option<String>,
        /// JSON arguments to pass to the tool, e.g. '{"city":"Hanoi"}'
        #[arg(long, short)]
        args: Option<String>,
    },
    /// Audit a skill source directory or installed skill name
    Audit {
        /// Skill path or installed skill name
        source: String,
    },
    /// Install a new skill from a local path, git URL, or registry (namespace/name)
    Install {
        /// Source: local path, git URL, or registry package (e.g. acme/my-tool)
        source: String,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
    },
    /// List all available skill templates
    Templates,
}

/// Migration subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrateCommands {
    /// Import OpenClaw data into this ZeroClaw workspace (memory, config, agents)
    Openclaw {
        /// Optional path to `OpenClaw` workspace (defaults to ~/.openclaw/workspace)
        #[arg(long)]
        source: Option<std::path::PathBuf>,

        /// Optional path to `OpenClaw` config file (defaults to ~/.openclaw/openclaw.json)
        #[arg(long)]
        source_config: Option<std::path::PathBuf>,

        /// Validate and preview migration without writing any data
        #[arg(long)]
        dry_run: bool,

        /// Skip memory migration
        #[arg(long)]
        no_memory: bool,

        /// Skip configuration and agents migration
        #[arg(long)]
        no_config: bool,
    },
}

/// Cron subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CronCommands {
    /// List all scheduled tasks
    List,
    /// Add a new scheduled task
    #[command(long_about = "\
Add a new recurring scheduled task.

Uses standard 5-field cron syntax: 'min hour day month weekday'. \
Times are evaluated in UTC by default; use --tz with an IANA \
timezone name to override.

Examples:
  zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York
  zeroclaw cron add '*/30 * * * *' 'Check system health'")]
    Add {
        /// Cron expression
        expression: String,
        /// Optional IANA timezone (e.g. America/Los_Angeles)
        #[arg(long)]
        tz: Option<String>,
        /// Command to run
        command: String,
    },
    /// Add a one-shot scheduled task at an RFC3339 timestamp
    #[command(long_about = "\
Add a one-shot task that fires at a specific UTC timestamp.

The timestamp must be in RFC 3339 format (e.g. 2025-01-15T14:00:00Z).

Examples:
  zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder'
  zeroclaw cron add-at 2025-12-31T23:59:00Z 'Happy New Year!'")]
    AddAt {
        /// One-shot timestamp in RFC3339 format
        at: String,
        /// Command to run
        command: String,
    },
    /// Add a fixed-interval scheduled task
    #[command(long_about = "\
Add a task that repeats at a fixed interval.

Interval is specified in milliseconds. For example, 60000 = 1 minute.

Examples:
  zeroclaw cron add-every 60000 'Ping heartbeat'     # every minute
  zeroclaw cron add-every 3600000 'Hourly report'    # every hour")]
    AddEvery {
        /// Interval in milliseconds
        every_ms: u64,
        /// Command to run
        command: String,
    },
    /// Add a one-shot delayed task (e.g. "30m", "2h", "1d")
    #[command(long_about = "\
Add a one-shot task that fires after a delay from now.

Accepts human-readable durations: s (seconds), m (minutes), \
h (hours), d (days).

Examples:
  zeroclaw cron once 30m 'Run backup in 30 minutes'
  zeroclaw cron once 2h 'Follow up on deployment'
  zeroclaw cron once 1d 'Daily check'")]
    Once {
        /// Delay duration
        delay: String,
        /// Command to run
        command: String,
    },
    /// Remove a scheduled task
    Remove {
        /// Task ID
        id: String,
    },
    /// Update a scheduled task
    #[command(long_about = "\
Update one or more fields of an existing scheduled task.

Only the fields you specify are changed; others remain unchanged.

Examples:
  zeroclaw cron update <task-id> --expression '0 8 * * *'
  zeroclaw cron update <task-id> --tz Europe/London --name 'Morning check'
  zeroclaw cron update <task-id> --command 'Updated message'")]
    Update {
        /// Task ID
        id: String,
        /// New cron expression
        #[arg(long)]
        expression: Option<String>,
        /// New IANA timezone
        #[arg(long)]
        tz: Option<String>,
        /// New command to run
        #[arg(long)]
        command: Option<String>,
        /// New job name
        #[arg(long)]
        name: Option<String>,
    },
    /// Pause a scheduled task
    Pause {
        /// Task ID
        id: String,
    },
    /// Resume a paused task
    Resume {
        /// Task ID
        id: String,
    },
}

/// Memory management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryCommands {
    /// List memory entries with optional filters
    List {
        /// Filter by category (core, daily, conversation, or custom name)
        #[arg(long)]
        category: Option<String>,
        /// Filter by session ID
        #[arg(long)]
        session: Option<String>,
        /// Maximum number of entries to display
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Number of entries to skip (for pagination)
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    /// Get a specific memory entry by key
    Get {
        /// Memory key to look up
        key: String,
    },
    /// Show memory backend statistics and health
    Stats,
    /// Clear memories by category, by key, or clear all
    Clear {
        /// Delete a single entry by key (supports prefix match)
        #[arg(long)]
        key: Option<String>,
        /// Only clear entries in this category
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

/// Vault (second brain) subcommands.
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VaultCommands {
    /// Legal-domain operations (statute + precedent ingestion, graph stats).
    Legal {
        #[command(subcommand)]
        legal_command: VaultLegalCommands,
    },
    /// Domain corpus lifecycle — install / swap / build / publish the
    /// swappable `domain.db` (Korean legal, future medical, etc.).
    Domain {
        #[command(subcommand)]
        domain_command: VaultDomainCommands,
    },
}

/// `zeroclaw vault domain <subcommand>` — manage the swappable
/// domain corpus DB at `<workspace>/memory/domain.db`.
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VaultDomainCommands {
    /// Show install state, file size, vault_documents/links counts.
    Info,
    /// Migrate legal rows from `brain.db` into `domain.db` (one-time
    /// fixup for v7 → v8 upgrades). Default mode copies and keeps
    /// the source rows intact; pass `--delete` to remove them after
    /// successful copy.
    Extract {
        /// Delete migrated rows from brain.db after successful copy.
        #[arg(long)]
        delete: bool,
    },
    /// Fetch a manifest, verify the bundle SHA-256, and atomic-rename
    /// into `<workspace>/memory/domain.db`. `<from>` may be an
    /// http(s) URL or a local manifest path.
    Install {
        /// Manifest URL or filesystem path.
        #[arg(long)]
        from: String,
    },
    /// Re-fetch from the manifest URL stored in
    /// `MOA_DOMAIN_MANIFEST_URL`. For one-off URLs use `install --from`.
    Update,
    /// Same as `install`, but emits a louder warning that any open
    /// connections holding `domain.db` ATTACHed must reconnect.
    Swap {
        #[arg(long)]
        from: String,
    },
    /// Remove the installed `domain.db` from the workspace. No-op
    /// when no file is present. Caller must ensure no live process
    /// has it ATTACHed.
    Uninstall,
    /// Bake a fresh `domain.db` from a corpus directory of legal
    /// markdown files (statute + precedent). Refuses to overwrite
    /// `--out`.
    Build {
        /// Corpus root (recursively walked).
        corpus_dir: std::path::PathBuf,
        /// Output bundle path (e.g. `./build/korean-legal-2026.01.db`).
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Compute the bundle's SHA-256 + size and write a manifest JSON
    /// next to it. Operators upload both files to their bucket.
    Publish {
        /// Path to the baked bundle.
        bundle: std::path::PathBuf,
        /// Bundle URL as it will appear in the published manifest
        /// (must match where you uploaded the bundle file).
        #[arg(long)]
        url: String,
        /// Corpus name (e.g. `korean-legal`).
        #[arg(long)]
        name: String,
        /// Corpus version (e.g. `2026.01`).
        #[arg(long)]
        version: String,
        /// Manifest output path. Defaults to
        /// `<bundle>.manifest.json` next to the bundle.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
}

/// `zeroclaw vault legal <subcommand>` — ingest Korean statute + precedent
/// markdown into the second-brain graph (vault_documents + vault_links).
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VaultLegalCommands {
    /// Walk a directory of .md files, classify each as statute or case, and upsert.
    ///
    /// Safe to re-run — checksum short-circuits unchanged files.
    Ingest {
        /// Path to a directory (recursively walked) or a single .md file.
        path: std::path::PathBuf,
        /// Parse + report without touching brain.db (in-memory DB).
        #[arg(long)]
        dry_run: bool,
    },
    /// Show legal-graph node/edge counts in brain.db.
    Stats,
    /// Populate `vault_embeddings` for every legal node that doesn't yet
    /// have one. Uses the `memory.embedding_provider/model/dimensions`
    /// config plus the resolved API key. Safe to re-run.
    Embed {
        /// Cap the number of docs embedded in a single run (default: all).
        #[arg(long)]
        limit: Option<usize>,
        /// Batch size for the provider's `embed()` call (default 8, max 32).
        #[arg(long, default_value = "8")]
        batch: usize,
    },
    /// Export a subgraph rooted at a slug to a standalone HTML snapshot
    /// (Cytoscape viewer with data embedded) or graphify-compatible JSON.
    Export {
        /// Root slug (e.g. `statute::근로기준법::36` or `case::2024노3424`).
        #[arg(long)]
        root: String,
        /// Hop limit (1–3).
        #[arg(long, default_value = "2")]
        depth: u32,
        /// Comma-separated kinds filter (`statute,case`). Omit for both.
        #[arg(long)]
        kinds: Option<String>,
        /// Output file path.
        #[arg(long)]
        out: std::path::PathBuf,
        /// Output format. `html` = self-contained viewer (default); `json` =
        /// raw `{nodes, edges, __meta}`, also graphify-compatible.
        #[arg(long, default_value = "html")]
        format: String,
        /// Inline the Cytoscape + dagre JS from the local vendor cache so
        /// the output HTML renders with no CDN calls. Requires a prior
        /// `vault legal vendor-download`. Ignored for `--format json`.
        #[arg(long)]
        offline: bool,
    },
    /// Download Cytoscape.js + dagre + cytoscape-dagre into the workspace
    /// vendor cache so `export --offline` and the gateway can serve them
    /// without CDN calls. Safe to re-run.
    VendorDownload {
        /// Re-download even if the files already exist.
        #[arg(long)]
        force: bool,
    },
}

/// Integration subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntegrationCommands {
    /// List all integrations (optionally filter by category or status)
    List {
        /// Filter by category (e.g. "chat", "ai", "productivity")
        #[arg(long, short)]
        category: Option<String>,
        /// Filter by status: active, available, coming-soon
        #[arg(long, short)]
        status: Option<String>,
    },
    /// Search integrations by keyword (matches name and description)
    Search {
        /// Search query
        query: String,
    },
    /// Show details about a specific integration
    Info {
        /// Integration name
        name: String,
    },
}

/// Hardware discovery subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum HardwareCommands {
    /// Enumerate USB devices (VID/PID) and show known boards
    #[command(long_about = "\
Enumerate USB devices and show known boards.

Scans connected USB devices by VID/PID and matches them against \
known development boards (STM32 Nucleo, Arduino, ESP32).

Examples:
  zeroclaw hardware discover")]
    Discover,
    /// Introspect a device by path (e.g. /dev/ttyACM0)
    #[command(long_about = "\
Introspect a device by its serial or device path.

Opens the specified device path and queries for board information, \
firmware version, and supported capabilities.

Examples:
  zeroclaw hardware introspect /dev/ttyACM0
  zeroclaw hardware introspect COM3")]
    Introspect {
        /// Serial or device path
        path: String,
    },
    /// Get chip info via USB (probe-rs over ST-Link). No firmware needed on target.
    #[command(long_about = "\
Get chip info via USB using probe-rs over ST-Link.

Queries the target MCU directly through the debug probe without \
requiring any firmware on the target board.

Examples:
  zeroclaw hardware info
  zeroclaw hardware info --chip STM32F401RETx")]
    Info {
        /// Chip name (e.g. STM32F401RETx). Default: STM32F401RETx for Nucleo-F401RE
        #[arg(long, default_value = "STM32F401RETx")]
        chip: String,
    },
}

/// Peripheral (hardware) management subcommands
#[derive(Subcommand, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PeripheralCommands {
    /// List configured peripherals
    List,
    /// Add a peripheral (board path, e.g. nucleo-f401re /dev/ttyACM0)
    #[command(long_about = "\
Add a peripheral by board type and transport path.

Registers a hardware board so the agent can use its tools (GPIO, \
sensors, actuators). Use 'native' as path for local GPIO on \
single-board computers like Raspberry Pi.

Supported boards: nucleo-f401re, rpi-gpio, esp32, arduino-uno.

Examples:
  zeroclaw peripheral add nucleo-f401re /dev/ttyACM0
  zeroclaw peripheral add rpi-gpio native
  zeroclaw peripheral add esp32 /dev/ttyUSB0")]
    Add {
        /// Board type (nucleo-f401re, rpi-gpio, esp32)
        board: String,
        /// Path for serial transport (/dev/ttyACM0) or "native" for local GPIO
        path: String,
    },
    /// Flash ZeroClaw firmware to Arduino (creates .ino, installs arduino-cli if needed, uploads)
    #[command(long_about = "\
Flash ZeroClaw firmware to an Arduino board.

Generates the .ino sketch, installs arduino-cli if it is not \
already available, compiles, and uploads the firmware.

Examples:
  zeroclaw peripheral flash
  zeroclaw peripheral flash --port /dev/cu.usbmodem12345
  zeroclaw peripheral flash -p COM3")]
    Flash {
        /// Serial port (e.g. /dev/cu.usbmodem12345). If omitted, uses first arduino-uno from config.
        #[arg(short, long)]
        port: Option<String>,
    },
    /// Setup Arduino Uno Q Bridge app (deploy GPIO bridge for agent control)
    SetupUnoQ {
        /// Uno Q IP (e.g. 192.168.0.48). If omitted, assumes running ON the Uno Q.
        #[arg(long)]
        host: Option<String>,
    },
    /// Flash ZeroClaw firmware to Nucleo-F401RE (builds + probe-rs run)
    FlashNucleo,
}
