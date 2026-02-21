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

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use dialoguer::{Input, Password};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use tracing_subscriber::{fmt, EnvFilter};

mod agent;
mod approval;
mod auth;
mod channels;
mod rag {
    pub use zeroclaw::rag::*;
}
mod config;
mod cron;
mod daemon;
mod doctor;
mod gateway;
mod hardware;
mod health;
mod heartbeat;
mod identity;
mod integrations;
mod memory;
mod migration;
mod observability;
mod onboard;
mod peripherals;
mod presets;
mod providers;
mod runtime;
mod security;
mod service;
mod skillforge;
mod skills;
mod tools;
mod tunnel;
mod updater;
mod util;

use config::Config;

// Re-export so binary's hardware/peripherals modules can use crate::HardwareCommands etc.
pub use zeroclaw::{HardwareCommands, PeripheralCommands};

/// `ZeroClaw` - Zero overhead. Zero compromise. 100% Rust.
#[derive(Parser, Debug)]
#[command(name = "zeroclaw")]
#[command(author = "theonlyhennygod")]
#[command(version = "0.1.0")]
#[command(about = "The fastest, smallest AI assistant.", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum ServiceCommands {
    /// Install daemon service unit for auto-start and restart
    Install,
    /// Start daemon service
    Start,
    /// Stop daemon service
    Stop,
    /// Check daemon service status
    Status,
    /// Uninstall daemon service unit
    Uninstall,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize your workspace and configuration
    Onboard {
        /// Run the full interactive wizard (default is quick setup)
        #[arg(long)]
        interactive: bool,

        /// Reconfigure channels only (fast repair flow)
        #[arg(long)]
        channels_only: bool,

        /// API key (used in quick mode, ignored with --interactive)
        #[arg(long)]
        api_key: Option<String>,

        /// Provider name (used in quick mode, default: openrouter)
        #[arg(long)]
        provider: Option<String>,

        /// Memory backend (sqlite, lucid, markdown, none) - used in quick mode, default: sqlite
        #[arg(long)]
        memory: Option<String>,

        /// Official preset ID (used in quick mode, default: default)
        #[arg(long)]
        preset: Option<String>,

        /// Extra pack ID to add on top of selected preset (repeatable)
        #[arg(long = "pack")]
        pack: Vec<String>,

        /// Security profile for quick onboarding (default: strict)
        #[arg(long = "security-profile", value_enum)]
        security_profile: Option<SecurityProfileArg>,

        /// Confirm using a non-strict security profile in quick onboarding
        #[arg(long = "yes-security-risk")]
        yes_security_risk: bool,
    },

    /// Start the AI agent loop
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
        #[arg(short, long, default_value = "0.7")]
        temperature: f64,

        /// Attach a peripheral (board:path, e.g. nucleo-f401re:/dev/ttyACM0)
        #[arg(long)]
        peripheral: Vec<String>,
    },

    /// Check and apply `zeroclaw` binary updates from GitHub Releases
    Update {
        /// Apply update (default action is check-only when this flag is omitted)
        #[arg(long)]
        apply: bool,

        /// Specific release version to target (e.g. 0.1.0 or v0.1.0); defaults to latest release
        #[arg(long)]
        version: Option<String>,

        /// Install path for updated binary (defaults to the currently running executable path)
        #[arg(long)]
        install_path: Option<std::path::PathBuf>,

        /// Preview update steps without downloading/installing
        #[arg(long)]
        dry_run: bool,

        /// Confirm binary replacement for update apply
        #[arg(long)]
        yes: bool,
    },

    /// Start the gateway server (webhooks, websockets)
    Gateway {
        /// Port to listen on (use 0 for random available port); defaults to config gateway.port
        #[arg(short, long)]
        port: Option<u16>,

        /// Host to bind to; defaults to config gateway.host
        #[arg(long)]
        host: Option<String>,
    },

    /// Start long-running autonomous runtime (gateway + channels + heartbeat + scheduler)
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

    /// Configure and manage scheduled tasks
    Cron {
        #[command(subcommand)]
        cron_command: CronCommands,
    },

    /// Manage provider model catalogs
    Models {
        #[command(subcommand)]
        model_command: ModelCommands,
    },

    /// Manage preset compositions, import/export, and intent-driven planning
    Preset {
        #[command(subcommand)]
        preset_command: PresetCommands,
    },

    /// Inspect and change security/autonomy profile
    Security {
        #[command(subcommand)]
        security_command: SecurityCommands,
    },

    /// List supported AI providers
    Providers,

    /// Manage channels (telegram, discord, slack)
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
    Hardware {
        #[command(subcommand)]
        hardware_command: zeroclaw::HardwareCommands,
    },

    /// Manage hardware peripherals (STM32, RPi GPIO, etc.)
    Peripheral {
        #[command(subcommand)]
        peripheral_command: zeroclaw::PeripheralCommands,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCommands {
    /// Login with OpenAI Codex OAuth
    Login {
        /// Provider (`openai-codex`)
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
enum MigrateCommands {
    /// Import memory from an `OpenClaw` workspace into this `ZeroClaw` workspace
    Openclaw {
        /// Optional path to `OpenClaw` workspace (defaults to ~/.openclaw/workspace)
        #[arg(long)]
        source: Option<std::path::PathBuf>,

        /// Validate and preview migration without writing any data
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
enum CronCommands {
    /// List all scheduled tasks
    List,
    /// Add a new scheduled task
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
    AddAt {
        /// One-shot timestamp in RFC3339 format
        at: String,
        /// Command to run
        command: String,
    },
    /// Add a fixed-interval scheduled task
    AddEvery {
        /// Interval in milliseconds
        every_ms: u64,
        /// Command to run
        command: String,
    },
    /// Add a one-shot delayed task (e.g. "30m", "2h", "1d")
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

#[derive(Subcommand, Debug)]
enum ModelCommands {
    /// Refresh and cache provider models
    Refresh {
        /// Provider name (defaults to configured default provider)
        #[arg(long)]
        provider: Option<String>,

        /// Force live refresh and ignore fresh cache
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum PresetCommands {
    /// List official presets and available packs
    List,
    /// Show details for an official preset
    Show {
        /// Official preset id
        id: String,
    },
    /// Show current workspace preset selection
    Current,
    /// Apply preset and pack changes to current workspace
    Apply {
        /// Base preset id (if omitted, starts from current selection or default)
        #[arg(long)]
        preset: Option<String>,

        /// Add a pack (repeatable)
        #[arg(long = "pack")]
        pack: Vec<String>,

        /// Remove a pack (repeatable)
        #[arg(long = "remove-pack")]
        remove_pack: Vec<String>,

        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,

        /// Approve applying risky packs
        #[arg(long)]
        yes_risky: bool,

        /// Rebuild binary after applying selection
        #[arg(long)]
        rebuild: bool,

        /// Confirm rebuild execution
        #[arg(long)]
        yes_rebuild: bool,
    },
    /// Build a preset plan from natural language intent
    Intent {
        /// Natural language intent text
        text: String,

        /// Extra capability graph file(s) to merge (repeatable)
        #[arg(long = "capabilities-file")]
        capabilities_file: Vec<std::path::PathBuf>,

        /// Apply the planned selection to workspace
        #[arg(long)]
        apply: bool,

        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,

        /// Approve applying risky packs
        #[arg(long)]
        yes_risky: bool,

        /// Rebuild binary after applying selection
        #[arg(long)]
        rebuild: bool,

        /// Confirm rebuild execution
        #[arg(long)]
        yes_rebuild: bool,

        /// Print orchestration-friendly JSON report (plan + security recommendation + generated next commands)
        #[arg(long)]
        json: bool,

        /// Write a shell orchestration script (template only, not executed)
        #[arg(long = "emit-shell")]
        emit_shell: Option<std::path::PathBuf>,
    },
    /// Export preset payload JSON (share/import format)
    Export {
        /// Output file path
        path: std::path::PathBuf,

        /// Export an official preset id instead of current workspace selection
        #[arg(long)]
        preset: Option<String>,
    },
    /// Import preset payload JSON into current workspace selection
    Import {
        /// Input file path
        path: std::path::PathBuf,

        /// Import mode: overwrite, merge, or fill
        #[arg(long, value_enum, default_value_t = presets::PresetImportMode::Merge)]
        mode: presets::PresetImportMode,

        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,

        /// Approve applying risky packs
        #[arg(long)]
        yes_risky: bool,

        /// Rebuild binary after applying selection
        #[arg(long)]
        rebuild: bool,

        /// Confirm rebuild execution
        #[arg(long)]
        yes_rebuild: bool,
    },
    /// Validate preset payload JSON files/directories
    Validate {
        /// Input file or directory path (repeatable)
        paths: Vec<std::path::PathBuf>,

        /// Allow unknown pack IDs (useful for external/private registries)
        #[arg(long)]
        allow_unknown_packs: bool,

        /// Print machine-readable JSON report
        #[arg(long)]
        json: bool,
    },
    /// Rebuild binary from current workspace preset selection
    Rebuild {
        /// Preview command only
        #[arg(long)]
        dry_run: bool,

        /// Confirm rebuild execution
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum SecurityProfileArg {
    Strict,
    Balanced,
    Flexible,
    Full,
}

impl SecurityProfileArg {
    fn as_profile_id(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Flexible => "flexible",
            Self::Full => "full",
        }
    }

    fn is_non_strict(self) -> bool {
        !matches!(self, Self::Strict)
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum NonCliApprovalArg {
    Manual,
    Auto,
}

impl NonCliApprovalArg {
    fn allows_auto_approval(self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[derive(Subcommand, Debug)]
enum SecurityCommands {
    /// Show current security profile and guardrails
    Show,
    /// Manage named security profiles
    Profile {
        #[command(subcommand)]
        profile_command: SecurityProfileCommands,
    },
}

#[derive(Subcommand, Debug)]
enum SecurityProfileCommands {
    /// Set workspace security profile
    Set {
        /// Target profile: strict, balanced, flexible, full
        #[arg(value_enum)]
        level: SecurityProfileArg,

        /// Non-CLI approval mode override: manual (default) or auto
        #[arg(long = "non-cli-approval", value_enum)]
        non_cli_approval: Option<NonCliApprovalArg>,

        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,

        /// Confirm setting a non-strict profile
        #[arg(long = "yes-risk")]
        yes_risk: bool,

        /// Print structured JSON change report
        #[arg(long)]
        json: bool,

        /// Export change report to a JSON file
        #[arg(long = "export-diff")]
        export_diff: Option<std::path::PathBuf>,
    },
    /// Recommend a security profile from natural-language intent
    Recommend {
        /// Natural language intent text
        intent: String,

        /// Extra capability graph file(s) to merge (repeatable)
        #[arg(long = "capabilities-file")]
        capabilities_file: Vec<std::path::PathBuf>,

        /// Evaluate recommendation as if this preset were the base (does not write)
        #[arg(long = "from-preset")]
        from_preset: Option<String>,

        /// Add pack(s) on top of the planned selection (repeatable, does not write)
        #[arg(long = "pack")]
        pack: Vec<String>,

        /// Remove pack(s) from the planned selection (repeatable, does not write)
        #[arg(long = "remove-pack")]
        remove_pack: Vec<String>,

        /// Print structured JSON recommendation report
        #[arg(long)]
        json: bool,
    },
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
}

#[derive(Subcommand, Debug)]
enum ChannelCommands {
    /// List configured channels
    List,
    /// Start all configured channels (Telegram, Discord, Slack)
    Start,
    /// Run health checks for configured channels
    Doctor,
    /// Add a new channel
    Add {
        /// Channel type
        channel_type: String,
        /// Configuration JSON
        config: String,
    },
    /// Remove a channel
    Remove {
        /// Channel name
        name: String,
    },
    /// Bind a Telegram identity (username or numeric user ID) into allowlist
    BindTelegram {
        /// Telegram identity to allow (username without '@' or numeric user ID)
        identity: String,
    },
}

#[derive(Subcommand, Debug)]
enum SkillCommands {
    /// List installed skills
    List,
    /// Install a skill from a GitHub URL or local path
    Install {
        /// GitHub URL or local path
        source: String,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum IntegrationCommands {
    /// Show details about a specific integration
    Info {
        /// Integration name
        name: String,
    },
}

fn print_selection(selection: &presets::WorkspacePresetSelection) {
    println!("Preset: {}", selection.preset_id);
    println!("Packs:  {}", selection.packs.join(", "));
    if !selection.added_packs.is_empty() {
        println!("Added:  {}", selection.added_packs.join(", "));
    }
}

fn print_selection_diff(diff: &presets::SelectionDiff) {
    if let Some(before) = &diff.before_preset_id {
        if before != &diff.after_preset_id {
            println!("Preset: {before} -> {}", diff.after_preset_id);
        } else {
            println!("Preset: {}", diff.after_preset_id);
        }
    } else {
        println!("Preset: {}", diff.after_preset_id);
    }

    if diff.added_packs.is_empty() && diff.removed_packs.is_empty() {
        println!("Packs:  no changes");
        return;
    }

    if !diff.added_packs.is_empty() {
        println!("Add:    {}", diff.added_packs.join(", "));
    }
    if !diff.removed_packs.is_empty() {
        println!("Remove: {}", diff.removed_packs.join(", "));
    }
}

async fn maybe_rebuild_selection(
    selection: &presets::WorkspacePresetSelection,
    rebuild: bool,
    dry_run: bool,
    approved: bool,
) -> Result<()> {
    if !rebuild {
        return Ok(());
    }
    if !dry_run && !approved {
        bail!(
            "Refusing to run rebuild without confirmation. Re-run with `--yes-rebuild`, or use `--dry-run`."
        );
    }

    let cwd = std::env::current_dir()?;
    let plan = presets::rebuild_plan_for_selection(selection, &cwd)?;
    println!();
    println!("Rebuild command:");
    println!("  cargo {}", plan.args.join(" "));
    println!("  (working directory: {})", plan.manifest_dir.display());

    if dry_run {
        println!("Rebuild dry-run: no command executed.");
        return Ok(());
    }

    let plan_clone = plan.clone();
    tokio::task::spawn_blocking(move || presets::execute_rebuild_plan(&plan_clone))
        .await
        .map_err(|error| anyhow::anyhow!("rebuild task failed: {error}"))??;
    println!("Rebuild completed.");
    Ok(())
}

fn print_security_profile_summary(config: &Config) {
    let label = onboard::security_profile_label(&config.autonomy);
    println!("Security profile: {label}");
    println!(
        "Guardrails: workspace_only={}, medium_approval={}, high_risk_block={}, non_cli_approval={}",
        config.autonomy.workspace_only,
        config.autonomy.require_approval_for_medium_risk,
        config.autonomy.block_high_risk_commands,
        non_cli_approval_mode(config.autonomy.allow_non_cli_auto_approval)
    );
    println!(
        "Limits: max_actions_per_hour={}, max_cost_per_day=${:.2}",
        config.autonomy.max_actions_per_hour,
        config.autonomy.max_cost_per_day_cents as f32 / 100.0
    );
}

#[derive(Debug, Serialize)]
struct SecurityProfileSnapshot {
    profile_id: String,
    label: String,
    level: String,
    workspace_only: bool,
    require_approval_for_medium_risk: bool,
    block_high_risk_commands: bool,
    allow_non_cli_auto_approval: bool,
    non_cli_approval_mode: String,
    max_actions_per_hour: u32,
    max_cost_per_day_cents: u32,
    max_cost_per_day_usd: String,
}

#[derive(Debug, Serialize)]
struct SecurityFieldChange {
    field: String,
    from: String,
    to: String,
}

#[derive(Debug, Serialize)]
struct SecurityProfileChangeReport {
    current: SecurityProfileSnapshot,
    target: SecurityProfileSnapshot,
    changes: Vec<SecurityFieldChange>,
    requires_explicit_risk_consent: bool,
    dry_run: bool,
    rollback_command: String,
}

#[derive(Debug, Serialize)]
struct SecurityProfileIntentRecommendationReport {
    intent: String,
    current_profile: SecurityProfileSnapshot,
    recommended_profile: onboard::SecurityProfileRecommendation,
    base_override_preset: Option<String>,
    manual_add_packs: Vec<String>,
    manual_remove_packs: Vec<String>,
    current_selection: Option<presets::WorkspacePresetSelection>,
    planned_selection: presets::WorkspacePresetSelection,
    risky_packs: Vec<String>,
    capability_sources: Vec<String>,
    plan_confidence: f32,
    plan_reasons: Vec<String>,
    apply_command: String,
}

#[derive(Debug, Clone, Serialize)]
struct GeneratedNextCommand {
    id: String,
    description: String,
    command: String,
    requires_explicit_consent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    consent_reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PresetIntentOrchestrationReport {
    intent: String,
    capability_sources: Vec<String>,
    plan: presets::IntentPlan,
    planned_selection: presets::WorkspacePresetSelection,
    risky_packs: Vec<String>,
    security_recommendation: onboard::SecurityProfileRecommendation,
    security_apply_command: String,
    next_commands: Vec<GeneratedNextCommand>,
}

fn shell_quote(raw: &str) -> String {
    let escaped = raw.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn build_preset_intent_command(
    text: &str,
    capabilities_file: &[std::path::PathBuf],
    apply: bool,
    dry_run: bool,
    yes_risky: bool,
    rebuild: bool,
    yes_rebuild: bool,
) -> String {
    let mut parts = vec![
        "zeroclaw".to_string(),
        "preset".to_string(),
        "intent".to_string(),
    ];
    parts.push(shell_quote(text));
    for path in capabilities_file {
        parts.push("--capabilities-file".to_string());
        parts.push(shell_quote(&path.display().to_string()));
    }
    if apply {
        parts.push("--apply".to_string());
    }
    if dry_run {
        parts.push("--dry-run".to_string());
    }
    if yes_risky {
        parts.push("--yes-risky".to_string());
    }
    if rebuild {
        parts.push("--rebuild".to_string());
    }
    if yes_rebuild {
        parts.push("--yes-rebuild".to_string());
    }
    parts.join(" ")
}

fn build_security_apply_command(recommendation: &onboard::SecurityProfileRecommendation) -> String {
    if recommendation.requires_explicit_consent {
        format!(
            "zeroclaw security profile set {} --yes-risk",
            recommendation.profile_id
        )
    } else {
        format!(
            "zeroclaw security profile set {}",
            recommendation.profile_id
        )
    }
}

fn build_preset_apply_consent_reasons(
    risky_packs: &[String],
    dry_run: bool,
    yes_risky: bool,
    rebuild: bool,
    yes_rebuild: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if !risky_packs.is_empty() && !dry_run && !yes_risky {
        reasons.push("risky_pack".to_string());
    }
    if rebuild && !dry_run && !yes_rebuild {
        reasons.push("rebuild".to_string());
    }
    reasons
}

fn build_security_apply_consent_reasons(
    recommendation: &onboard::SecurityProfileRecommendation,
) -> Vec<String> {
    if recommendation.requires_explicit_consent {
        vec!["security_non_strict".to_string()]
    } else {
        Vec::new()
    }
}

fn build_orchestration_shell_script(report: &PresetIntentOrchestrationReport) -> String {
    let mut lines = vec![
        "#!/usr/bin/env bash".to_string(),
        "set -euo pipefail".to_string(),
        "".to_string(),
        format!(
            "# Generated by: zeroclaw preset intent {} --json",
            shell_quote(&report.intent)
        ),
        "# This script is generated only. It is not executed automatically.".to_string(),
        "".to_string(),
        "confirm() {".to_string(),
        "  local prompt=\"$1\"".to_string(),
        "  local reply".to_string(),
        "  read -r -p \"$prompt [y/N]: \" reply".to_string(),
        "  case \"$reply\" in".to_string(),
        "    [yY]|[yY][eE][sS]) return 0 ;;".to_string(),
        "    *) return 1 ;;".to_string(),
        "  esac".to_string(),
        "}".to_string(),
        "".to_string(),
    ];

    for command in &report.next_commands {
        lines.push(format!("# {}: {}", command.id, command.description));
        if command.requires_explicit_consent {
            let reason_label = if command.consent_reasons.is_empty() {
                "manual_confirmation".to_string()
            } else {
                command.consent_reasons.join(",")
            };
            lines.push(format!(
                "if confirm \"Run {} (reasons: {})?\"; then",
                command.id, reason_label
            ));
            lines.push(format!("  {}", command.command));
            lines.push("else".to_string());
            lines.push(format!("  echo \"Skipped {}\"", command.id));
            lines.push("fi".to_string());
        } else {
            lines.push(command.command.clone());
        }
        lines.push("".to_string());
    }

    lines.join("\n")
}

fn emit_orchestration_shell_script(
    path: &std::path::Path,
    report: &PresetIntentOrchestrationReport,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }

    let script = build_orchestration_shell_script(report);
    std::fs::write(path, script).with_context(|| format!("Failed to write {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("Failed to set executable bit on {}", path.display()))?;
    }

    Ok(())
}

fn autonomy_level_id(level: security::AutonomyLevel) -> &'static str {
    match level {
        security::AutonomyLevel::ReadOnly => "read_only",
        security::AutonomyLevel::Supervised => "supervised",
        security::AutonomyLevel::Full => "full",
    }
}

fn non_cli_approval_mode(auto_enabled: bool) -> &'static str {
    if auto_enabled {
        "auto"
    } else {
        "manual"
    }
}

fn cents_to_usd_string(cents: u32) -> String {
    format!("{:.2}", cents as f64 / 100.0)
}

fn build_security_profile_snapshot(
    autonomy: &config::AutonomyConfig,
    profile_id_override: Option<&str>,
) -> SecurityProfileSnapshot {
    let label = onboard::security_profile_label(autonomy).to_string();
    let profile_id = profile_id_override
        .map(str::to_string)
        .unwrap_or_else(|| onboard::security_profile_id_from_autonomy(autonomy).to_string());

    SecurityProfileSnapshot {
        profile_id,
        label,
        level: autonomy_level_id(autonomy.level).to_string(),
        workspace_only: autonomy.workspace_only,
        require_approval_for_medium_risk: autonomy.require_approval_for_medium_risk,
        block_high_risk_commands: autonomy.block_high_risk_commands,
        allow_non_cli_auto_approval: autonomy.allow_non_cli_auto_approval,
        non_cli_approval_mode: non_cli_approval_mode(autonomy.allow_non_cli_auto_approval)
            .to_string(),
        max_actions_per_hour: autonomy.max_actions_per_hour,
        max_cost_per_day_cents: autonomy.max_cost_per_day_cents,
        max_cost_per_day_usd: cents_to_usd_string(autonomy.max_cost_per_day_cents),
    }
}

fn build_security_profile_change_report(
    current: &config::AutonomyConfig,
    target: &config::AutonomyConfig,
    target_profile_id: &str,
    requires_explicit_risk_consent: bool,
    dry_run: bool,
) -> SecurityProfileChangeReport {
    let current_snapshot = build_security_profile_snapshot(current, None);
    let target_snapshot = build_security_profile_snapshot(target, Some(target_profile_id));
    let mut changes = Vec::new();

    if current_snapshot.profile_id != target_snapshot.profile_id {
        changes.push(SecurityFieldChange {
            field: "profile_id".to_string(),
            from: current_snapshot.profile_id.clone(),
            to: target_snapshot.profile_id.clone(),
        });
    }
    if current_snapshot.level != target_snapshot.level {
        changes.push(SecurityFieldChange {
            field: "level".to_string(),
            from: current_snapshot.level.clone(),
            to: target_snapshot.level.clone(),
        });
    }
    if current.workspace_only != target.workspace_only {
        changes.push(SecurityFieldChange {
            field: "workspace_only".to_string(),
            from: current.workspace_only.to_string(),
            to: target.workspace_only.to_string(),
        });
    }
    if current.require_approval_for_medium_risk != target.require_approval_for_medium_risk {
        changes.push(SecurityFieldChange {
            field: "require_approval_for_medium_risk".to_string(),
            from: current.require_approval_for_medium_risk.to_string(),
            to: target.require_approval_for_medium_risk.to_string(),
        });
    }
    if current.block_high_risk_commands != target.block_high_risk_commands {
        changes.push(SecurityFieldChange {
            field: "block_high_risk_commands".to_string(),
            from: current.block_high_risk_commands.to_string(),
            to: target.block_high_risk_commands.to_string(),
        });
    }
    if current.allow_non_cli_auto_approval != target.allow_non_cli_auto_approval {
        changes.push(SecurityFieldChange {
            field: "allow_non_cli_auto_approval".to_string(),
            from: current.allow_non_cli_auto_approval.to_string(),
            to: target.allow_non_cli_auto_approval.to_string(),
        });
        changes.push(SecurityFieldChange {
            field: "non_cli_approval_mode".to_string(),
            from: non_cli_approval_mode(current.allow_non_cli_auto_approval).to_string(),
            to: non_cli_approval_mode(target.allow_non_cli_auto_approval).to_string(),
        });
    }
    if current.max_actions_per_hour != target.max_actions_per_hour {
        changes.push(SecurityFieldChange {
            field: "max_actions_per_hour".to_string(),
            from: current.max_actions_per_hour.to_string(),
            to: target.max_actions_per_hour.to_string(),
        });
    }
    if current.max_cost_per_day_cents != target.max_cost_per_day_cents {
        changes.push(SecurityFieldChange {
            field: "max_cost_per_day_cents".to_string(),
            from: current.max_cost_per_day_cents.to_string(),
            to: target.max_cost_per_day_cents.to_string(),
        });
        changes.push(SecurityFieldChange {
            field: "max_cost_per_day_usd".to_string(),
            from: cents_to_usd_string(current.max_cost_per_day_cents),
            to: cents_to_usd_string(target.max_cost_per_day_cents),
        });
    }

    SecurityProfileChangeReport {
        current: current_snapshot,
        target: target_snapshot,
        changes,
        requires_explicit_risk_consent,
        dry_run,
        rollback_command: "zeroclaw security profile set strict".to_string(),
    }
}

fn print_security_profile_change_report(report: &SecurityProfileChangeReport) {
    println!("Security profile change:");
    println!("- current: {}", report.current.label);
    println!(
        "  guardrails: workspace_only={}, medium_approval={}, high_risk_block={}, non_cli_approval={}, max_actions_per_hour={}, max_cost_per_day=${}",
        report.current.workspace_only,
        report.current.require_approval_for_medium_risk,
        report.current.block_high_risk_commands,
        report.current.non_cli_approval_mode,
        report.current.max_actions_per_hour,
        report.current.max_cost_per_day_usd
    );
    println!("- target: {}", report.target.label);
    println!(
        "  guardrails: workspace_only={}, medium_approval={}, high_risk_block={}, non_cli_approval={}, max_actions_per_hour={}, max_cost_per_day=${}",
        report.target.workspace_only,
        report.target.require_approval_for_medium_risk,
        report.target.block_high_risk_commands,
        report.target.non_cli_approval_mode,
        report.target.max_actions_per_hour,
        report.target.max_cost_per_day_usd
    );

    if report.changes.is_empty() {
        println!("- delta: no effective policy changes");
    } else {
        println!("- delta:");
        for change in &report.changes {
            println!("  {}: {} -> {}", change.field, change.from, change.to);
        }
    }
}

fn handle_security_command(command: SecurityCommands, config: &mut Config) -> Result<()> {
    match command {
        SecurityCommands::Show => {
            print_security_profile_summary(config);
            Ok(())
        }
        SecurityCommands::Profile { profile_command } => match profile_command {
            SecurityProfileCommands::Set {
                level,
                non_cli_approval,
                dry_run,
                yes_risk,
                json,
                export_diff,
            } => {
                let profile_id = level.as_profile_id();
                let current = config.autonomy.clone();
                let mut next = onboard::autonomy_config_for_security_profile_id(profile_id)?;
                if let Some(mode) = non_cli_approval {
                    next.allow_non_cli_auto_approval = mode.allows_auto_approval();
                }

                let enabling_non_cli_auto_approval =
                    !current.allow_non_cli_auto_approval && next.allow_non_cli_auto_approval;
                let requires_explicit_risk_consent =
                    level.is_non_strict() || enabling_non_cli_auto_approval;
                let report = build_security_profile_change_report(
                    &current,
                    &next,
                    profile_id,
                    requires_explicit_risk_consent,
                    dry_run,
                );

                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_security_profile_change_report(&report);
                }

                if let Some(path) = export_diff {
                    let payload = serde_json::to_string_pretty(&report)?;
                    std::fs::write(&path, payload)
                        .with_context(|| format!("Failed to write {}", path.display()))?;
                    println!("Exported security diff: {}", path.display());
                }

                if requires_explicit_risk_consent && !yes_risk && !dry_run {
                    let mut risk_reasons = Vec::new();
                    if level.is_non_strict() {
                        risk_reasons.push(format!("profile '{}' is non-strict", profile_id));
                    }
                    if enabling_non_cli_auto_approval {
                        risk_reasons.push(
                            "non-CLI auto approval removes per-call confirmation on non-CLI channels"
                                .to_string(),
                        );
                    }
                    bail!(
                        "Refusing to apply risk-elevating security changes without explicit consent ({}). Re-run with `--yes-risk`, or use `--dry-run`.",
                        risk_reasons.join("; ")
                    );
                }

                if dry_run {
                    println!("Security profile dry-run: no changes written.");
                    println!("Rollback command: {}", report.rollback_command);
                    return Ok(());
                }

                config.autonomy = next;
                config.save()?;
                println!("Saved config: {}", config.config_path.display());
                println!("Rollback command: {}", report.rollback_command);
                Ok(())
            }
            SecurityProfileCommands::Recommend {
                intent,
                capabilities_file,
                from_preset,
                pack,
                remove_pack,
                json,
            } => {
                let current_selection = presets::load_workspace_selection(config)?;
                let resolved_capabilities =
                    presets::resolve_intent_capabilities(config, &capabilities_file)?;
                let plan = presets::plan_from_intent_with_rules(
                    &intent,
                    current_selection.as_ref(),
                    &resolved_capabilities.rules,
                );
                let planned_selection = if let Some(base_preset_id) = from_preset.as_deref() {
                    let base = presets::from_preset_id(base_preset_id)?;
                    presets::compose_selection(base, &plan.add_packs, &plan.remove_packs)?
                } else {
                    presets::selection_from_plan(&plan, current_selection.as_ref())?
                };
                let planned_selection =
                    presets::compose_selection(planned_selection, &pack, &remove_pack)?;
                let risky_packs = presets::risky_pack_ids(&planned_selection);
                let recommendation =
                    onboard::recommend_security_profile(Some(&intent), &planned_selection.packs);
                let apply_command = build_security_apply_command(&recommendation);

                let report = SecurityProfileIntentRecommendationReport {
                    intent: intent.clone(),
                    current_profile: build_security_profile_snapshot(&config.autonomy, None),
                    recommended_profile: recommendation,
                    base_override_preset: from_preset.clone(),
                    manual_add_packs: pack.clone(),
                    manual_remove_packs: remove_pack.clone(),
                    current_selection,
                    planned_selection,
                    risky_packs,
                    capability_sources: resolved_capabilities.sources,
                    plan_confidence: plan.confidence,
                    plan_reasons: plan.reasons,
                    apply_command,
                };

                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    return Ok(());
                }

                println!("Intent: {}", report.intent);
                println!(
                    "Current profile: {} ({})",
                    report.current_profile.label, report.current_profile.profile_id
                );
                if let Some(base) = report.base_override_preset.as_deref() {
                    println!("Planning base override: {base}");
                }
                println!(
                    "Recommended profile: {} ({})",
                    report.recommended_profile.label, report.recommended_profile.profile_id
                );
                println!("Risk tier: {}", report.recommended_profile.risk_tier);
                println!("Recommendation reasons:");
                for reason in &report.recommended_profile.reasons {
                    println!("- {reason}");
                }
                println!();
                println!("Preset/pack plan used for recommendation:");
                println!("- preset: {}", report.planned_selection.preset_id);
                println!("- packs: {}", report.planned_selection.packs.join(", "));
                if !report.manual_add_packs.is_empty() {
                    println!("- manual add packs: {}", report.manual_add_packs.join(", "));
                }
                if !report.manual_remove_packs.is_empty() {
                    println!(
                        "- manual remove packs: {}",
                        report.manual_remove_packs.join(", ")
                    );
                }
                if report.risky_packs.is_empty() {
                    println!("- risky packs: (none)");
                } else {
                    println!("- risky packs: {}", report.risky_packs.join(", "));
                }
                println!("- plan confidence: {:.2}", report.plan_confidence);
                if !report.capability_sources.is_empty() {
                    println!(
                        "- capability sources: {}",
                        report.capability_sources.join(" -> ")
                    );
                }
                println!();
                println!("No config changes were applied.");
                println!("Apply recommendation:");
                println!("  {}", report.apply_command);
                Ok(())
            }
        },
    }
}

async fn handle_preset_command(command: PresetCommands, config: &Config) -> Result<()> {
    match command {
        PresetCommands::List => {
            println!("Official presets:");
            for preset in onboard::PRESETS {
                println!("- {}: {}", preset.id, preset.description);
                println!("  packs: {}", preset.packs.join(", "));
            }
            println!();
            println!("Available packs:");
            for pack in onboard::FEATURE_PACKS {
                let risk = if pack.requires_confirmation {
                    " [requires confirmation]"
                } else {
                    ""
                };
                let features = if pack.cargo_features.is_empty() {
                    "(no extra cargo features)".to_string()
                } else {
                    pack.cargo_features.join(", ")
                };
                println!("- {}{}: {}", pack.id, risk, pack.description);
                println!("  cargo features: {features}");
            }
            Ok(())
        }
        PresetCommands::Show { id } => {
            let preset =
                onboard::preset_by_id(&id).with_context(|| format!("Unknown preset id '{id}'"))?;
            println!("Preset: {}", preset.id);
            println!("Description: {}", preset.description);
            println!("Packs:");
            for pack_id in preset.packs {
                if let Some(pack) = onboard::feature_pack_by_id(pack_id) {
                    let risk = if pack.requires_confirmation {
                        " [requires confirmation]"
                    } else {
                        ""
                    };
                    println!("- {}{}: {}", pack.id, risk, pack.description);
                } else {
                    println!("- {} (unknown pack reference)", pack_id);
                }
            }
            Ok(())
        }
        PresetCommands::Current => {
            let path = presets::workspace_preset_path(config);
            let current = presets::load_workspace_selection(config)?;
            println!("Workspace preset file: {}", path.display());
            if let Some(selection) = current {
                print_selection(&selection);
            } else {
                println!("No workspace preset selection found yet.");
            }
            Ok(())
        }
        PresetCommands::Apply {
            preset,
            pack,
            remove_pack,
            dry_run,
            yes_risky,
            rebuild,
            yes_rebuild,
        } => {
            let before = presets::load_workspace_selection(config)?;
            let base = if let Some(preset_id) = preset {
                presets::from_preset_id(&preset_id)?
            } else if let Some(current) = before.clone() {
                current
            } else {
                presets::default_selection()?
            };
            let after = presets::compose_selection(base, &pack, &remove_pack)?;
            let diff = presets::selection_diff(before.as_ref(), &after);

            println!("Preset plan:");
            print_selection_diff(&diff);

            let risky = presets::risky_pack_ids(&after);
            if !risky.is_empty() && !yes_risky && !dry_run {
                bail!(
                    "Selection includes risky packs [{}]. Re-run with `--yes-risky`, or use `--dry-run`.",
                    risky.join(", ")
                );
            }
            if !risky.is_empty() {
                println!("Risky packs: {}", risky.join(", "));
            }

            if dry_run {
                println!("Apply dry-run: no changes written.");
                maybe_rebuild_selection(&after, rebuild, true, true).await?;
                return Ok(());
            }

            let path = presets::save_workspace_selection(config, &after)?;
            println!("Saved workspace preset selection: {}", path.display());
            maybe_rebuild_selection(&after, rebuild, false, yes_rebuild).await?;
            Ok(())
        }
        PresetCommands::Intent {
            text,
            capabilities_file,
            apply,
            dry_run,
            yes_risky,
            rebuild,
            yes_rebuild,
            json,
            emit_shell,
        } => {
            if json && apply {
                bail!("`preset intent --json` is plan-only and cannot be combined with `--apply`.");
            }
            if emit_shell.is_some() && apply {
                bail!("`preset intent --emit-shell` is plan-only and cannot be combined with `--apply`.");
            }

            let before = presets::load_workspace_selection(config)?;
            let resolved_capabilities =
                presets::resolve_intent_capabilities(config, &capabilities_file)?;
            let plan = presets::plan_from_intent_with_rules(
                &text,
                before.as_ref(),
                &resolved_capabilities.rules,
            );
            let after = presets::selection_from_plan(&plan, before.as_ref())?;
            let diff = presets::selection_diff(before.as_ref(), &after);
            let risky = presets::risky_pack_ids(&after);
            let security_recommendation =
                onboard::recommend_security_profile(Some(&text), &after.packs);
            let security_apply_command = build_security_apply_command(&security_recommendation);

            let preview_apply_command = build_preset_intent_command(
                &text,
                &capabilities_file,
                true,
                true,
                false,
                rebuild,
                false,
            );
            let apply_command = build_preset_intent_command(
                &text,
                &capabilities_file,
                true,
                dry_run,
                yes_risky,
                rebuild,
                yes_rebuild,
            );
            let preset_apply_consent_reasons = build_preset_apply_consent_reasons(
                &risky,
                dry_run,
                yes_risky,
                rebuild,
                yes_rebuild,
            );
            let security_apply_consent_reasons =
                build_security_apply_consent_reasons(&security_recommendation);

            let mut next_commands = vec![
                GeneratedNextCommand {
                    id: "preset.apply.preview".to_string(),
                    description:
                        "Preview applying this intent plan without mutating workspace state"
                            .to_string(),
                    command: preview_apply_command.clone(),
                    requires_explicit_consent: false,
                    consent_reasons: Vec::new(),
                },
                GeneratedNextCommand {
                    id: "preset.apply".to_string(),
                    description: "Apply this preset composition plan to workspace selection"
                        .to_string(),
                    command: apply_command,
                    requires_explicit_consent: !preset_apply_consent_reasons.is_empty(),
                    consent_reasons: preset_apply_consent_reasons,
                },
                GeneratedNextCommand {
                    id: "security.profile.set".to_string(),
                    description:
                        "Align security profile with the recommended guardrails (manual step)"
                            .to_string(),
                    command: security_apply_command.clone(),
                    requires_explicit_consent: !security_apply_consent_reasons.is_empty(),
                    consent_reasons: security_apply_consent_reasons,
                },
            ];
            if next_commands[0].command == next_commands[1].command {
                next_commands.remove(0);
            }

            let orchestration_report = PresetIntentOrchestrationReport {
                intent: text.clone(),
                capability_sources: resolved_capabilities.sources.clone(),
                plan: plan.clone(),
                planned_selection: after.clone(),
                risky_packs: risky.clone(),
                security_recommendation: security_recommendation.clone(),
                security_apply_command: security_apply_command.clone(),
                next_commands: next_commands.clone(),
            };

            if let Some(path) = emit_shell.as_ref() {
                emit_orchestration_shell_script(path, &orchestration_report)?;
                if json {
                    eprintln!("Wrote orchestration shell script: {}", path.display());
                } else {
                    println!("Wrote orchestration shell script: {}", path.display());
                }
            }

            if json {
                println!("{}", serde_json::to_string_pretty(&orchestration_report)?);
                return Ok(());
            }

            println!("Intent: {}", plan.intent);
            if let Some(base) = plan.base_preset_id.as_deref() {
                println!("Base preset: {base}");
            } else {
                println!("Base preset: current selection/default fallback");
            }
            println!("Confidence: {:.2}", plan.confidence);
            if !resolved_capabilities.sources.is_empty() {
                println!(
                    "Capability sources: {}",
                    resolved_capabilities.sources.join(" -> ")
                );
            }
            println!("Reasons:");
            for reason in &plan.reasons {
                println!("- {reason}");
            }
            if !plan.capability_signals.is_empty() {
                println!();
                println!("Capability graph matches:");
                for signal in &plan.capability_signals {
                    println!(
                        "- {} ({:.2}) — {}",
                        signal.capability_id, signal.weight, signal.rationale
                    );
                    println!("  terms: {}", signal.matched_terms.join(", "));
                }
            }
            if !plan.preset_ranking.is_empty() {
                println!();
                println!("Preset ranking:");
                for rank in plan.preset_ranking.iter().take(3) {
                    println!("- {} ({:.2})", rank.preset_id, rank.score);
                    if !rank.reasons.is_empty() {
                        println!("  signals: {}", rank.reasons.join("; "));
                    }
                }
            }
            println!();
            println!(
                "Confidence breakdown: base {:.2} + signal {:.2} + ranking {:.2} - penalty {:.2} = {:.2}",
                plan.confidence_breakdown.base,
                plan.confidence_breakdown.signal_bonus,
                plan.confidence_breakdown.ranking_bonus,
                plan.confidence_breakdown.contradiction_penalty,
                plan.confidence_breakdown.final_score
            );
            println!();
            println!("Intent directives:");
            if plan.add_packs.is_empty() {
                println!("- add packs: (none)");
            } else {
                println!("- add packs: {}", plan.add_packs.join(", "));
            }
            if plan.remove_packs.is_empty() {
                println!("- remove packs: (none)");
            } else {
                println!("- remove packs: {}", plan.remove_packs.join(", "));
            }
            println!();
            println!("Planned selection:");
            print_selection_diff(&diff);
            println!("Resolved packs: {}", after.packs.join(", "));
            if before.is_none() {
                println!("Current workspace selection: none (first composition run)");
            }
            println!();
            println!(
                "Security recommendation: {} ({})",
                security_recommendation.label, security_recommendation.profile_id
            );
            println!("Risk tier: {}", security_recommendation.risk_tier);
            if let Some(primary_reason) = security_recommendation.reasons.first() {
                println!("Why: {primary_reason}");
            }

            if !apply {
                println!();
                println!("Generated next commands (not executed):");
                for entry in &orchestration_report.next_commands {
                    println!("- {}: {}", entry.id, entry.description);
                    println!(
                        "  consent required: {}",
                        if entry.requires_explicit_consent {
                            "yes"
                        } else {
                            "no"
                        }
                    );
                    if !entry.consent_reasons.is_empty() {
                        println!("  consent reasons: {}", entry.consent_reasons.join(", "));
                    }
                    println!("  {}", entry.command);
                }
                println!();
                println!("Plan only. Re-run with `--apply` to persist this selection.");
                return Ok(());
            }

            if !risky.is_empty() && !yes_risky && !dry_run {
                bail!(
                    "Selection includes risky packs [{}]. Re-run with `--yes-risky`, or use `--dry-run`.",
                    risky.join(", ")
                );
            }
            if !risky.is_empty() {
                println!("Risky packs: {}", risky.join(", "));
            }

            if dry_run {
                println!("Intent apply dry-run: no changes written.");
                maybe_rebuild_selection(&after, rebuild, true, true).await?;
                return Ok(());
            }

            let path = presets::save_workspace_selection(config, &after)?;
            println!("Saved workspace preset selection: {}", path.display());
            maybe_rebuild_selection(&after, rebuild, false, yes_rebuild).await?;
            println!("Recommended follow-up security command:");
            println!("  {security_apply_command}");
            Ok(())
        }
        PresetCommands::Export { path, preset } => {
            let selection = if let Some(preset_id) = preset {
                presets::from_preset_id(&preset_id)?
            } else if let Some(current) = presets::load_workspace_selection(config)? {
                current
            } else {
                presets::default_selection()?
            };
            let document = presets::selection_to_document(&selection);
            presets::export_document_to_path(&path, &document)?;
            println!("Exported preset payload to {}", path.display());
            Ok(())
        }
        PresetCommands::Import {
            path,
            mode,
            dry_run,
            yes_risky,
            rebuild,
            yes_rebuild,
        } => {
            let result = presets::import_selection_from_path(config, &path, mode)?;
            println!("Import mode: {}", result.mode);
            print_selection_diff(&presets::selection_diff(
                result.before.as_ref(),
                &result.after,
            ));

            let risky = presets::risky_pack_ids(&result.after);
            if !risky.is_empty() && !yes_risky && !dry_run {
                bail!(
                    "Selection includes risky packs [{}]. Re-run with `--yes-risky`, or use `--dry-run`.",
                    risky.join(", ")
                );
            }
            if !risky.is_empty() {
                println!("Risky packs: {}", risky.join(", "));
            }

            if dry_run {
                println!("Import dry-run: no changes written.");
                maybe_rebuild_selection(&result.after, rebuild, true, true).await?;
                return Ok(());
            }

            let saved = presets::save_workspace_selection(config, &result.after)?;
            println!("Saved workspace preset selection: {}", saved.display());
            maybe_rebuild_selection(&result.after, rebuild, false, yes_rebuild).await?;
            Ok(())
        }
        PresetCommands::Validate {
            paths,
            allow_unknown_packs,
            json,
        } => {
            let report = presets::validate_preset_paths(&paths, allow_unknown_packs)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Preset validation summary: {} checked, {} failed",
                    report.files_checked, report.files_failed
                );
                println!(
                    "Unknown packs allowed: {}",
                    if report.allow_unknown_packs {
                        "yes"
                    } else {
                        "no"
                    }
                );
                for result in &report.results {
                    if result.ok {
                        println!("- [ok] {} ({})", result.path, result.format);
                    } else {
                        println!("- [failed] {} ({})", result.path, result.format);
                        for error in &result.errors {
                            println!("  - {error}");
                        }
                    }
                }
            }

            if report.files_failed > 0 {
                bail!(
                    "Preset validation failed for {} of {} files.",
                    report.files_failed,
                    report.files_checked
                );
            }
            Ok(())
        }
        PresetCommands::Rebuild { dry_run, yes } => {
            let selection = if let Some(current) = presets::load_workspace_selection(config)? {
                current
            } else {
                presets::default_selection()?
            };
            maybe_rebuild_selection(&selection, true, dry_run, yes).await
        }
    }
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

    // Initialize logging - respects RUST_LOG env var, defaults to INFO
    let subscriber = fmt::Subscriber::builder()
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
        channels_only,
        api_key,
        provider,
        memory,
        preset,
        pack,
        security_profile,
        yes_security_risk,
    } = &cli.command
    {
        let interactive = *interactive;
        let channels_only = *channels_only;
        let api_key = api_key.clone();
        let provider = provider.clone();
        let memory = memory.clone();
        let preset = preset.clone();
        let pack = pack.clone();
        let security_profile = *security_profile;
        let yes_security_risk = *yes_security_risk;

        if interactive && channels_only {
            bail!("Use either --interactive or --channels-only, not both");
        }
        if interactive && (security_profile.is_some() || yes_security_risk) {
            bail!(
                "--interactive does not accept `--security-profile` or `--yes-security-risk`; pick profile in the wizard."
            );
        }
        if channels_only
            && (api_key.is_some()
                || provider.is_some()
                || memory.is_some()
                || preset.is_some()
                || !pack.is_empty()
                || security_profile.is_some()
                || yes_security_risk)
        {
            bail!("--channels-only does not accept quick-setup override flags");
        }
        if !interactive
            && !channels_only
            && security_profile.is_some_and(SecurityProfileArg::is_non_strict)
            && !yes_security_risk
        {
            bail!(
                "Refusing non-strict quick onboarding profile without explicit consent. \
                 Re-run with `--yes-security-risk`, or use `--interactive` to confirm in wizard."
            );
        }

        let config = tokio::task::spawn_blocking(move || {
            if channels_only {
                onboard::run_channels_repair_wizard()
            } else if interactive {
                onboard::run_wizard()
            } else {
                onboard::run_quick_setup(
                    api_key.as_deref(),
                    provider.as_deref(),
                    memory.as_deref(),
                    preset.as_deref(),
                    &pack,
                    security_profile.map(SecurityProfileArg::as_profile_id),
                )
            }
        })
        .await??;
        // Auto-start channels if user said yes during wizard
        if std::env::var("ZEROCLAW_AUTOSTART_CHANNELS").as_deref() == Ok("1") {
            channels::start_channels(config).await?;
        }
        return Ok(());
    }

    // All other commands need config loaded first
    let mut config = Config::load_or_init()?;
    config.apply_env_overrides();

    match cli.command {
        Commands::Onboard { .. } => unreachable!(),

        Commands::Agent {
            message,
            provider,
            model,
            temperature,
            peripheral,
        } => agent::run(config, message, provider, model, temperature, peripheral)
            .await
            .map(|_| ()),

        Commands::Update {
            apply,
            version,
            install_path,
            dry_run,
            yes,
        } => {
            if apply {
                if !yes && !dry_run {
                    bail!(
                        "Refusing to replace the running binary without explicit confirmation. \
                         Re-run with `--yes`, or add `--dry-run` to preview."
                    );
                }

                let result = updater::apply_update(updater::UpdateApplyOptions {
                    target_version: version,
                    install_path,
                    dry_run,
                })
                .await?;

                if result.dry_run {
                    println!("Update dry-run complete.");
                    println!("  from:          {}", result.from_version);
                    println!("  to:            {}", result.to_version);
                    println!("  target:        {}", result.target);
                    println!("  release asset: {}", result.asset_name);
                    println!("  install path:  {}", result.install_path.display());
                    if let Some(url) = result.release_url {
                        println!("  release:       {url}");
                    }
                } else {
                    println!(
                        "Updated zeroclaw from {} to {}",
                        result.from_version, result.to_version
                    );
                    println!("Installed binary: {}", result.install_path.display());
                }
                Ok(())
            } else {
                let result =
                    updater::check_for_updates(env!("CARGO_PKG_VERSION"), version.as_deref())
                        .await?;

                println!("Current version: {}", result.current_version);
                println!("Latest version:  {}", result.latest_version);
                if result.update_available {
                    println!("Update available: yes");
                    if let Some(url) = &result.release.html_url {
                        println!("Release URL:      {url}");
                    }
                    println!("Run: zeroclaw update --apply --yes");
                } else {
                    println!("Update available: no");
                }

                Ok(())
            }
        }

        Commands::Gateway { port, host } => {
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
            println!("Version:     {}", env!("CARGO_PKG_VERSION"));
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
                "  Non-CLI approval:  {}",
                non_cli_approval_mode(config.autonomy.allow_non_cli_auto_approval)
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
            println!();
            println!("Channels:");
            println!("  CLI:      ✅ always");
            for (name, configured) in [
                ("Telegram", config.channels_config.telegram.is_some()),
                ("Discord", config.channels_config.discord.is_some()),
                ("Slack", config.channels_config.slack.is_some()),
                ("Webhook", config.channels_config.webhook.is_some()),
            ] {
                println!(
                    "  {name:9} {}",
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

        Commands::Cron { cron_command } => cron::handle_command(cron_command, &config),

        Commands::Models { model_command } => match model_command {
            ModelCommands::Refresh { provider, force } => {
                let config_for_refresh = config.clone();
                tokio::task::spawn_blocking(move || {
                    onboard::run_models_refresh(&config_for_refresh, provider.as_deref(), force)
                })
                .await
                .map_err(|e| anyhow::anyhow!("models refresh task failed: {e}"))?
            }
        },

        Commands::Preset { preset_command } => handle_preset_command(preset_command, &config).await,

        Commands::Security { security_command } => {
            handle_security_command(security_command, &mut config)
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

        Commands::Service { service_command } => service::handle_command(&service_command, &config),

        Commands::Doctor { doctor_command } => match doctor_command {
            Some(DoctorCommands::Models {
                provider,
                use_cache,
            }) => {
                let config_for_models = config.clone();
                tokio::task::spawn_blocking(move || {
                    doctor::run_models(&config_for_models, provider.as_deref(), use_cache)
                })
                .await
                .map_err(|e| anyhow::anyhow!("doctor models task failed: {e}"))?
            }
            None => doctor::run(&config),
        },

        Commands::Channel { channel_command } => match channel_command {
            ChannelCommands::Start => channels::start_channels(config).await,
            ChannelCommands::Doctor => channels::doctor_channels(config).await,
            other => channels::handle_command(other, &config),
        },

        Commands::Integrations {
            integration_command,
        } => integrations::handle_command(integration_command, &config),

        Commands::Skills { skill_command } => {
            skills::handle_command(skill_command, &config.workspace_dir)
        }

        Commands::Migrate { migrate_command } => {
            migration::handle_command(migrate_command, &config).await
        }

        Commands::Auth { auth_command } => handle_auth_command(auth_command, &config).await,

        Commands::Hardware { hardware_command } => {
            hardware::handle_command(hardware_command.clone(), &config)
        }

        Commands::Peripheral { peripheral_command } => {
            peripherals::handle_command(peripheral_command.clone(), &config)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOpenAiLogin {
    profile: String,
    code_verifier: String,
    state: String,
    created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOpenAiLoginFile {
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_verifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_code_verifier: Option<String>,
    state: String,
    created_at: String,
}

fn pending_openai_login_path(config: &Config) -> std::path::PathBuf {
    auth::state_dir_from_config(config).join("auth-openai-pending.json")
}

fn pending_openai_secret_store(config: &Config) -> security::secrets::SecretStore {
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

fn save_pending_openai_login(config: &Config, pending: &PendingOpenAiLogin) -> Result<()> {
    let path = pending_openai_login_path(config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let secret_store = pending_openai_secret_store(config);
    let encrypted_code_verifier = secret_store.encrypt(&pending.code_verifier)?;
    let persisted = PendingOpenAiLoginFile {
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

fn load_pending_openai_login(config: &Config) -> Result<Option<PendingOpenAiLogin>> {
    let path = pending_openai_login_path(config);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let persisted: PendingOpenAiLoginFile = serde_json::from_slice(&bytes)?;
    let secret_store = pending_openai_secret_store(config);
    let code_verifier = if let Some(encrypted) = persisted.encrypted_code_verifier {
        secret_store.decrypt(&encrypted)?
    } else if let Some(plaintext) = persisted.code_verifier {
        plaintext
    } else {
        bail!("Pending OpenAI login is missing code verifier");
    };
    Ok(Some(PendingOpenAiLogin {
        profile: persisted.profile,
        code_verifier,
        state: persisted.state,
        created_at: persisted.created_at,
    }))
}

fn clear_pending_openai_login(config: &Config) {
    let path = pending_openai_login_path(config);
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
            if provider != "openai-codex" {
                bail!("`auth login` currently supports only --provider openai-codex");
            }

            let client = reqwest::Client::new();

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
                            auth::openai_oauth::poll_device_code_tokens(&client, &device).await?;
                        let account_id =
                            extract_openai_account_id_for_profile(&token_set.access_token);

                        auth_service.store_openai_tokens(&profile, token_set, account_id, true)?;
                        clear_pending_openai_login(config);

                        println!("Saved profile {profile}");
                        println!("Active profile for openai-codex: {profile}");
                        return Ok(());
                    }
                    Err(e) => {
                        println!(
                            "Device-code flow unavailable: {e}. Falling back to browser/paste flow."
                        );
                    }
                }
            }

            let pkce = auth::openai_oauth::generate_pkce_state();
            let pending = PendingOpenAiLogin {
                profile: profile.clone(),
                code_verifier: pkce.code_verifier.clone(),
                state: pkce.state.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            save_pending_openai_login(config, &pending)?;

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

            auth_service.store_openai_tokens(&profile, token_set, account_id, true)?;
            clear_pending_openai_login(config);

            println!("Saved profile {profile}");
            println!("Active profile for openai-codex: {profile}");
            Ok(())
        }

        AuthCommands::PasteRedirect {
            provider,
            profile,
            input,
        } => {
            let provider = auth::normalize_provider(&provider)?;
            if provider != "openai-codex" {
                bail!("`auth paste-redirect` currently supports only --provider openai-codex");
            }

            let pending = load_pending_openai_login(config)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "No pending OpenAI login found. Run `zeroclaw auth login --provider openai-codex` first."
                )
            })?;

            if pending.profile != profile {
                bail!(
                    "Pending login profile mismatch: pending={}, requested={}",
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
                auth::openai_oauth::exchange_code_for_tokens(&client, &code, &pkce).await?;
            let account_id = extract_openai_account_id_for_profile(&token_set.access_token);

            auth_service.store_openai_tokens(&profile, token_set, account_id, true)?;
            clear_pending_openai_login(config);

            println!("Saved profile {profile}");
            println!("Active profile for openai-codex: {profile}");
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

            auth_service.store_provider_token(&provider, &profile, &token, metadata, true)?;
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

            auth_service.store_provider_token(&provider, &profile, &token, metadata, true)?;
            println!("Saved profile {profile}");
            println!("Active profile for {provider}: {profile}");
            Ok(())
        }

        AuthCommands::Refresh { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            if provider != "openai-codex" {
                bail!("`auth refresh` currently supports only --provider openai-codex");
            }

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

        AuthCommands::Logout { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            let removed = auth_service.remove_profile(&provider, &profile)?;
            if removed {
                println!("Removed auth profile {provider}:{profile}");
            } else {
                println!("Auth profile not found: {provider}:{profile}");
            }
            Ok(())
        }

        AuthCommands::Use { provider, profile } => {
            let provider = auth::normalize_provider(&provider)?;
            auth_service.set_active_profile(&provider, &profile)?;
            println!("Active profile for {provider}: {profile}");
            Ok(())
        }

        AuthCommands::List => {
            let data = auth_service.load_profiles()?;
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
            let data = auth_service.load_profiles()?;
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
    fn security_profile_set_parses_non_cli_approval_override() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "security",
            "profile",
            "set",
            "strict",
            "--non-cli-approval",
            "auto",
            "--yes-risk",
        ])
        .expect("cli parse should succeed");

        match cli.command {
            Commands::Security { security_command } => match security_command {
                SecurityCommands::Profile { profile_command } => match profile_command {
                    SecurityProfileCommands::Set {
                        non_cli_approval, ..
                    } => {
                        assert_eq!(non_cli_approval, Some(NonCliApprovalArg::Auto));
                    }
                    _ => panic!("expected security profile set command"),
                },
                _ => panic!("expected security profile command"),
            },
            _ => panic!("expected security command"),
        }
    }

    #[test]
    fn security_change_report_includes_non_cli_approval_delta() {
        let mut current = config::AutonomyConfig::default();
        current.allow_non_cli_auto_approval = false;
        let mut target = current.clone();
        target.allow_non_cli_auto_approval = true;
        let report = build_security_profile_change_report(&current, &target, "strict", true, true);

        assert!(report
            .changes
            .iter()
            .any(|change| change.field == "allow_non_cli_auto_approval"));
        assert!(report
            .changes
            .iter()
            .any(|change| change.field == "non_cli_approval_mode"));
        assert_eq!(report.current.non_cli_approval_mode, "manual");
        assert_eq!(report.target.non_cli_approval_mode, "auto");
    }

    #[test]
    fn non_cli_approval_mode_labels_are_stable() {
        assert_eq!(non_cli_approval_mode(false), "manual");
        assert_eq!(non_cli_approval_mode(true), "auto");
    }

    #[test]
    fn preset_apply_consent_reasons_capture_risky_and_rebuild() {
        let risky = vec!["tools-update".to_string()];
        let reasons = build_preset_apply_consent_reasons(&risky, false, false, true, false);
        assert_eq!(
            reasons,
            vec!["risky_pack".to_string(), "rebuild".to_string()]
        );
    }

    #[test]
    fn security_apply_consent_reasons_for_non_strict_profile() {
        let recommendation = onboard::SecurityProfileRecommendation {
            profile_id: "balanced".to_string(),
            label: "Balanced supervised".to_string(),
            risk_tier: "medium".to_string(),
            requires_explicit_consent: true,
            reasons: vec!["test".to_string()],
        };
        let reasons = build_security_apply_consent_reasons(&recommendation);
        assert_eq!(reasons, vec!["security_non_strict".to_string()]);
    }

    #[test]
    fn orchestration_shell_script_includes_confirmation_for_guarded_commands() {
        let report = PresetIntentOrchestrationReport {
            intent: "need unattended browser automation".to_string(),
            capability_sources: vec!["builtin".to_string()],
            plan: presets::IntentPlan {
                intent: "need unattended browser automation".to_string(),
                base_preset_id: Some("automation".to_string()),
                add_packs: vec!["browser-native".to_string()],
                remove_packs: Vec::new(),
                confidence: 0.7,
                reasons: vec!["test".to_string()],
                capability_signals: Vec::new(),
                preset_ranking: Vec::new(),
                confidence_breakdown: presets::ConfidenceBreakdown {
                    base: 0.42,
                    signal_bonus: 0.08,
                    ranking_bonus: 0.1,
                    contradiction_penalty: 0.0,
                    final_score: 0.6,
                },
            },
            planned_selection: presets::WorkspacePresetSelection {
                schema_version: 1,
                preset_id: "automation".to_string(),
                packs: vec!["core-agent".to_string(), "tools-update".to_string()],
                added_packs: Vec::new(),
            },
            risky_packs: vec!["tools-update".to_string()],
            security_recommendation: onboard::SecurityProfileRecommendation {
                profile_id: "balanced".to_string(),
                label: "Balanced supervised".to_string(),
                risk_tier: "medium".to_string(),
                requires_explicit_consent: true,
                reasons: vec!["test".to_string()],
            },
            security_apply_command: "zeroclaw security profile set balanced --yes-risk".to_string(),
            next_commands: vec![
                GeneratedNextCommand {
                    id: "preset.apply.preview".to_string(),
                    description: "preview".to_string(),
                    command: "zeroclaw preset intent 'x' --apply --dry-run".to_string(),
                    requires_explicit_consent: false,
                    consent_reasons: Vec::new(),
                },
                GeneratedNextCommand {
                    id: "preset.apply".to_string(),
                    description: "apply".to_string(),
                    command: "zeroclaw preset intent 'x' --apply".to_string(),
                    requires_explicit_consent: true,
                    consent_reasons: vec!["risky_pack".to_string()],
                },
            ],
        };

        let script = build_orchestration_shell_script(&report);
        assert!(script.contains("confirm()"));
        assert!(script.contains("zeroclaw preset intent 'x' --apply --dry-run"));
        assert!(script.contains("Run preset.apply (reasons: risky_pack)?"));
    }
}
