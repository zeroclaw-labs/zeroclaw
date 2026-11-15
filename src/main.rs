#![recursion_limit = "256"]
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
    dead_code,
    unused_variables,
    unused_imports
)]

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use dialoguer::{Password, Select};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::io::{BufRead, ErrorKind, Read, Write};

const STDIN_LINE_CAP: usize = 1024 * 1024;

/// Result of [`read_capped_line`].
#[cfg(not(feature = "agent-runtime"))]
#[derive(Debug)]
enum CappedLine {
    /// A full line under the cap, with the trailing `\n` stripped.
    Line(String),
    /// The physical line exceeded `cap`. The remainder has been drained
    /// and must not be used as a prompt.
    Truncated,
    /// EOF with no bytes read.
    Eof,
}

#[cfg(not(feature = "agent-runtime"))]
fn read_capped_line<R: std::io::BufRead>(reader: R, cap: usize) -> std::io::Result<CappedLine> {
    let mut raw = Vec::new();
    let mut limited = reader.take((cap + 1) as u64);
    std::io::BufRead::read_until(&mut limited, b'\n', &mut raw)?;
    let truncated = raw.len() > cap;
    if truncated {
        let mut inner = limited.into_inner();
        discard_until_newline(&mut inner)?;
        return Ok(CappedLine::Truncated);
    } else if raw.last() == Some(&b'\n') {
        raw.pop();
    }
    if raw.is_empty() {
        return Ok(CappedLine::Eof);
    }
    Ok(CappedLine::Line(String::from_utf8_lossy(&raw).into_owned()))
}

/// Truncate `line` in place to at most `cap` bytes, rounding the cut down to a
/// UTF-8 char boundary. `String::truncate` panics when the byte index lands
/// inside a multi-byte character, so a raw `line.truncate(cap)` on piped input
/// is a latent panic. No-op when the string already fits.
fn cap_line_utf8_safe(line: &mut String, cap: usize) {
    if line.len() > cap {
        line.truncate(line.floor_char_boundary(cap));
    }
}

/// Discard bytes from `reader` until the next `\n` or EOF, using only
/// `BufRead::fill_buf` / `consume`. This avoids the unbounded allocation
/// that `read_until(..., &mut Vec::new())` would incur on an oversized
/// physical line, and it stops exactly at the newline so the next line
/// is not consumed.
#[cfg(not(feature = "agent-runtime"))]
fn discard_until_newline<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let buf = reader.fill_buf()?;
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        reader.consume(len);
    }
}

use std::path::{Path, PathBuf};
use std::sync::Arc;

fn parse_temperature(s: &str) -> std::result::Result<f64, String> {
    let t: f64 = s
        .parse()
        .map_err(|e| format!("invalid temperature '{s}': {e}"))?;
    config::schema::validate_temperature(t)
}

/// Flags of `zeroclaw agent` that only take effect with `-m`. Interactive
/// `zeroclaw agent` hands off to `zeroclaw chat`, whose turns run on the
/// gateway with the agent's configured model, so these would be ignored.
/// Returns the flags that were given, in the order they appear in `--help`.
#[cfg_attr(not(feature = "agent-runtime"), allow(dead_code))]
fn interactive_agent_unsupported_flags(
    model_provider: Option<&str>,
    model: Option<&str>,
    temperature: Option<f64>,
) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if model_provider.is_some() {
        flags.push("--model-provider");
    }
    if model.is_some() {
        flags.push("--model");
    }
    if temperature.is_some() {
        flags.push("--temperature");
    }
    flags
}

fn print_no_command_help(cmd: clap::Command) -> Result<()> {
    #[cfg(feature = "agent-runtime")]
    {
        println!(
            "{}",
            crate::i18n::get_cli_string("cli-no-command-provided")
                .as_deref()
                .unwrap_or("No command provided.")
        );
        println!(
            "{}",
            crate::i18n::get_cli_string("cli-try-quickstart")
                .as_deref()
                .unwrap_or("Try `zeroclaw quickstart` to create your first agent.")
        );
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        println!("{}", t("cli-no-command", "No command provided."));
        println!(
            "{}",
            t(
                "cli-try-quickstart",
                "Try `zeroclaw quickstart` to create your first agent."
            )
        );
    }
    println!();

    let mut cmd = cmd;
    cmd.print_help()?;
    println!();

    #[cfg(windows)]
    pause_after_no_command_help();

    Ok(())
}

#[cfg(windows)]
fn pause_after_no_command_help() {
    println!();
    print!("{}", t("cli-press-enter", "Press Enter to exit..."));
    let _ = std::io::stdout().flush();
    // Cap the read so a piped-in flood (e.g. `dir | zeroclaw` with no
    // command) cannot blow up RSS in this trivial one-Enter prompt.
    // See module-level `STDIN_LINE_CAP` for rationale.
    let mut line = String::new();
    let _ = std::io::stdin()
        .lock()
        .take((STDIN_LINE_CAP + 1) as u64)
        .read_line(&mut line);
    if line.len() > STDIN_LINE_CAP {
        // Round down to a UTF-8 char boundary before truncating: a piped
        // multi-byte payload can land the byte cap inside a character, and
        // `String::truncate` panics on a non-boundary index.
        cap_line_utf8_safe(&mut line, STDIN_LINE_CAP);
    }
}

#[cfg(feature = "agent-runtime")]
mod agent;
mod alias_cli;
#[cfg(feature = "agent-runtime")]
mod approval;
#[cfg(feature = "agent-runtime")]
mod auth;
#[cfg(feature = "agent-runtime")]
mod browse;
#[cfg(feature = "agent-runtime")]
mod channels;
#[cfg(feature = "agent-runtime")]
mod cli_input;
mod commands;
mod config;
#[cfg(feature = "agent-runtime")]
mod cost;
#[cfg(feature = "agent-runtime")]
mod cron;
#[cfg(feature = "agent-runtime")]
mod daemon;
#[cfg(feature = "agent-runtime")]
mod doctor;
#[cfg(feature = "gateway")]
mod gateway;
mod gateway_helpers;
#[cfg(feature = "agent-runtime")]
mod health;
#[cfg(feature = "agent-runtime")]
mod heartbeat;
#[cfg(feature = "agent-runtime")]
mod hooks;
#[cfg(feature = "agent-runtime")]
mod i18n;
#[cfg(feature = "agent-runtime")]
mod identity;
#[cfg(feature = "agent-runtime")]
mod integrations;
mod memory;
#[cfg(feature = "agent-runtime")]
mod migration;
#[cfg(feature = "agent-runtime")]
mod multimodal;
#[cfg(feature = "agent-runtime")]
mod observability;
#[cfg(feature = "agent-runtime")]
mod platform;
mod providers;
#[cfg(feature = "agent-runtime")]
mod security;
#[cfg(feature = "agent-runtime")]
mod security_status;
#[cfg(feature = "agent-runtime")]
mod service;
#[cfg(feature = "agent-runtime")]
mod skills;
#[cfg(feature = "agent-runtime")]
mod tools;
#[cfg(feature = "agent-runtime")]
mod tunnel;
#[cfg(feature = "agent-runtime")]
mod util;

use config::Config;

#[cfg(feature = "agent-runtime")]
use gateway_helpers::{
    PaircodeAction, PaircodeResult, fetch_paircode, gateway_admin_url, paircode_no_code_message,
    shutdown_gateway,
};
use gateway_helpers::{log_gateway_start, resolve_gateway_addr};
pub(crate) use gateway_helpers::{t, ta};

// Re-export so binary modules can use crate::<CommandEnum> while keeping a single source of truth.
pub use zeroclaw::{
    AgentsCommands, BridgeCommands, ChannelCommands, ChannelsCommands, CronCommands,
    GatewayCommands, IntegrationCommands, KernelMigrationStoreKind, MigrateCommands,
    ProvidersCommands, ServiceCommands, SkillBundleCommands, SkillCommands,
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
#[command(version)]
// i18n-exempt: clap derive help — framework requires a compile-time literal
#[command(about = "The fastest, smallest AI assistant.", long_about = None)]
struct Cli {
    #[arg(long, global = true)]
    config_dir: Option<String>,

    /// Lowest severity recorded to the runtime trace (and capture
    /// layer). Immutable for the process. Precedence: this flag >
    /// RUST_LOG env > per-command default.
    #[arg(long, global = true, value_enum)]
    log_level: Option<LogLevel>,

    /// Surface recorded logs on the terminal. Off by default: logs go
    /// to the trace file only and the terminal shows just command
    /// output. When on, the terminal shows events down to the recorded
    /// floor. Immutable for the process.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Commands,
}

/// Recording-floor severities, mapped to `RUST_LOG`-style directive
/// fragments. Mirrors `tracing`'s level names so the flag reads the
/// same as the env var it overrides.
#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_directive(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

/// Subcommands for `zeroclaw telegram`.
#[cfg(all(feature = "agent-runtime", feature = "channel-telegram"))]
#[derive(Subcommand, Debug)]
enum TelegramCommands {
    /// Explain migration of the retired Telegram skip command
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
The in-process Telegram listener and its skip markers are retired. \
This compatibility command returns a migration error without writing a marker. \
Telegram intake and cursor recovery belong to zeroclaw-bridge-telegram; \
this command cannot skip a bridge update.")]
    SkipUpdate {
        /// Legacy bot alias (accepted for compatibility)
        #[arg(long)]
        alias: String,

        /// Legacy update ID (accepted for compatibility)
        #[arg(long)]
        update_id: i64,

        /// Legacy skip reason (accepted for compatibility)
        #[arg(long)]
        reason: Option<String>,
    },
}

/// Subcommands for `zeroclaw soul`.
#[cfg(feature = "agent-runtime")]
#[derive(Subcommand, Debug)]
enum SoulCommands {
    /// Check that the Soul behaves the same on the selected models.
    Check {
        /// Agent alias whose Soul to check.
        #[arg(long)]
        agent: String,

        /// Model to test, as `<type>.<alias>` or `<type>.<alias>:<model>`.
        /// Repeat for several. Defaults to the agent's own provider and model.
        #[arg(long = "target", value_name = "PROVIDER[:MODEL]")]
        targets: Vec<String>,

        /// Judge model, as `<type>.<alias>` or `<type>.<alias>:<model>`.
        /// Defaults to the agent's own provider and model.
        #[arg(long, value_name = "PROVIDER[:MODEL]")]
        judge: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: commands::soul::OutputFormat,
    },
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Quickstart — create one working agent end-to-end. Replaces the
    /// section-by-section onboarding flow with a single preset-driven
    /// path. Interactive: the flags below pre-seed checklist selectors
    /// but do not skip them; a terminal is required.
    Quickstart {
        /// Provider type (anthropic / openai / openrouter / ollama).
        #[arg(long)]
        model_provider: Option<String>,

        /// Model id for the new provider entry.
        #[arg(long)]
        model: Option<String>,

        /// API key for the new provider entry (omit for ollama / local).
        #[arg(long)]
        api_key: Option<String>,

        /// Alias for the new agent. Defaults to a sanitized provider name.
        #[arg(long)]
        agent: Option<String>,
    },

    /// Deprecated. Use `zeroclaw quickstart`. Any flags error.
    Onboard {
        /// Configure a specific section only. Omit to run the full flow.
        #[command(subcommand)]
        section: Option<zeroclaw_config::sections::Section>,

        /// Skip interactive prompts; read from --api-key/--model-provider/--model/--memory.
        #[arg(long, hide = true)]
        quick: bool,

        /// Force the dialoguer CLI backend instead of the default ratatui TUI.
        #[arg(long, hide = true)]
        cli: bool,

        /// Deprecated: TUI is now the default. Accepted as a no-op for one release.
        #[arg(long, hide = true)]
        tui: bool,

        /// Don't ask "keep stored secret?" — always re-prompt.
        #[arg(long, hide = true)]
        force: bool,

        /// Back up existing config and start from defaults.
        #[arg(long, hide = true)]
        reinit: bool,

        /// API key for model_provider configuration.
        #[arg(long, hide = true)]
        api_key: Option<String>,

        /// ModelProvider name. Used as the type key for the synthesized
        /// `[providers.models.<type>.default]` entry.
        #[arg(long, hide = true)]
        model_provider: Option<String>,

        /// Model ID override.
        #[arg(long, hide = true)]
        model: Option<String>,

        /// Memory backend (sqlite, lucid, markdown, none).
        #[arg(long, hide = true)]
        memory: Option<String>,

        // Deprecated legacy flags — parsed for one release, each maps to a
        // subcommand with a stderr warning pointing at the new form.
        #[arg(long, hide = true)]
        channels_only: bool,
        #[arg(long, hide = true)]
        providers_only: bool,
        #[arg(long, hide = true)]
        memory_only: bool,
        #[arg(long, hide = true)]
        hardware_only: bool,
        #[arg(long, hide = true)]
        tunnel_only: bool,
    },

    /// Start the AI agent loop
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Start the AI agent loop.

With --message, runs one turn locally and prints the reply. Without it, \
opens `zeroclaw chat` for this agent on this machine's gateway, so the \
conversation is shared with every other client.

Examples:
  zeroclaw agent -a assistant                                          # chat through the gateway
  zeroclaw agent -a assistant -m \"Summarize today's logs\"              # single message
  zeroclaw agent -a assistant -m \"Hi\" -p anthropic --model claude-sonnet-4-20250514")]
    Agent {
        /// Configured agent alias to run as (must match `[agents.<alias>]`).
        /// Required — there is no default agent.
        #[arg(short = 'a', long)]
        agent: String,

        /// Single message mode (don't enter interactive mode)
        #[arg(short, long)]
        message: Option<String>,

        /// Model provider to use (openrouter, anthropic, openai, openai-codex)
        #[arg(short = 'p', long = "model-provider", alias = "provider")]
        model_provider: Option<String>,

        /// Model to use
        #[arg(long)]
        model: Option<String>,

        /// Temperature (0.0 - 2.0, defaults to `providers.models.<type>.<alias>.temperature`)
        #[arg(short, long, value_parser = parse_temperature)]
        temperature: Option<f64>,
    },

    #[cfg(feature = "agent-runtime")]
    /// Chat with an agent through the gateway
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Chat with an agent through the gateway.

Attaches to a session on a running gateway (`zeroclaw daemon` or \
`zeroclaw gateway start`). Every client on the same session, including the \
web dashboard, shares one conversation. Closing the chat leaves a running \
turn going; Ctrl+C or /cancel stops it.

Examples:
  zeroclaw chat -a assistant                          # session \"main\" on this machine's gateway
  zeroclaw chat -a assistant -s work                  # another session
  zeroclaw chat -a assistant -m \"What's on today?\"    # one message, then exit
  zeroclaw chat -a assistant --gateway wss://home.example:42617")]
    Chat {
        /// Agent alias to talk to (must match `[agents.<alias>]` on the gateway)
        #[arg(short = 'a', long)]
        agent: String,

        /// Session to attach to; clients on the same session share one conversation
        #[arg(short = 's', long, default_value = "main")]
        session: String,

        /// Gateway URL (default: this machine's configured gateway)
        #[arg(long)]
        gateway: Option<String>,

        /// Send one message, print the reply, and exit
        #[arg(short, long)]
        message: Option<String>,
    },

    /// Start/manage the gateway server (webhooks, websockets)
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Manage the gateway server (webhooks, websockets).

Start, restart, or inspect the HTTP/WebSocket gateway that accepts \
incoming webhook events and WebSocket connections.

Examples:
  zeroclaw gateway start              # start gateway
  zeroclaw gateway restart            # restart gateway
  zeroclaw gateway get-paircode       # show pairing code")]
    Gateway {
        #[command(subcommand)]
        gateway_command: Option<zeroclaw::GatewayCommands>,
    },

    /// Start ACP (Agent Control Protocol) server over stdio
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Start the ACP server (JSON-RPC 2.0 over stdio).

Launches a JSON-RPC 2.0 server on stdin/stdout for IDE and tool \
integration. Supports session management and streaming agent \
responses as notifications.

Methods: initialize, session/new, session/prompt, session/stop.

Examples:
  zeroclaw acp                        # start ACP server
  zeroclaw acp --max-sessions 5       # limit concurrent sessions")]
    Acp {
        /// Maximum concurrent sessions (default: 10)
        #[arg(long)]
        max_sessions: Option<usize>,

        /// Session inactivity timeout in seconds (default: 3600)
        #[arg(long)]
        session_timeout: Option<u64>,
    },

    /// Start long-running autonomous runtime (gateway + channels + heartbeat + scheduler)
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Start the long-running autonomous daemon.

Launches the full ZeroClaw runtime: gateway server, all configured \
channel surfaces (Telegram bridge, webhook ingress, Email/Discord tools), heartbeat monitor, and \
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

        /// Boot even when security-critical config sections were dropped to
        /// their defaults during load. Without this, the daemon refuses to
        /// start with a weakened posture; with it, the daemon boots so the
        /// operator can reach repair surfaces, emitting a repeating warning.
        #[arg(long)]
        allow_degraded_security: bool,
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
    Status {
        /// Output format: "exit-code" exits 0 if healthy, 1 otherwise (for Docker HEALTHCHECK)
        #[arg(long)]
        format: Option<String>,
    },

    /// Inspect the active security posture derived from local config and host detection
    #[cfg(feature = "agent-runtime")]
    Security {
        #[command(subcommand)]
        security_command: SecurityCommands,
    },

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
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Configure and manage scheduled tasks.

Schedule recurring, one-shot, or interval-based tasks using cron \
expressions, RFC3339 timestamps with explicit Z or offsets, durations, \
or fixed intervals.

Cron expressions use the standard 5-field format: \
'min hour day month weekday'. When --tz is omitted, cron schedules use \
the runtime local timezone. For user-facing schedules, pass --tz with \
an explicit IANA timezone.

Examples:
  zeroclaw cron list
  zeroclaw cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York --agent
  zeroclaw cron add '*/30 * * * *' 'Check system health' --agent
  zeroclaw cron add '*/5 * * * *' 'echo ok'
  zeroclaw cron add-at 2025-01-15T14:00:00Z 'Send reminder' --agent
  zeroclaw cron add-every 60000 'Ping heartbeat'
  zeroclaw cron once 30m 'Run backup in 30 minutes' --agent
  zeroclaw cron pause TASK_ID
  zeroclaw cron update TASK_ID --expression '0 8 * * *' --tz Europe/London")]
    Cron {
        #[command(subcommand)]
        cron_command: CronCommands,
    },

    /// Manage model_provider model catalogs
    Models {
        #[command(subcommand)]
        model_command: ModelCommands,
    },

    Providers {
        #[command(subcommand)]
        providers_command: Option<ProvidersCommands>,
    },

    /// Manage channels (telegram, discord, email, webhook)
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Manage communication channels.

Add, remove, list, send, and health-check channels that connect ZeroClaw \
to messaging surfaces. Live channel types: telegram, discord, email, \
webhook (other legacy sections are retired; see the channels book page).

Examples:
  zeroclaw channel list
  zeroclaw channel doctor
  zeroclaw channel add telegram '{\"bot_token\":\"...\",\"name\":\"my-bot\"}'
  zeroclaw channel remove my-bot
  zeroclaw channel bind-telegram zeroclaw_user
  zeroclaw channel send 'Alert!' --channel-id telegram --recipient 123456789")]
    Channel {
        #[command(subcommand)]
        channel_command: ChannelCommands,
    },

    /// Manage agent aliases (create/list/rename/delete). Distinct from `agent`,
    /// which runs an agent.
    Agents {
        #[command(subcommand)]
        agents_command: AgentsCommands,
    },

    /// Manage channel aliases (create/list/rename/delete)
    Channels {
        #[command(subcommand)]
        channels_command: ChannelsCommands,
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

    /// Browse the shared workspace one directory at a time
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
List children of a directory under `<install>`/shared/. Paths are relative \
to the shared workspace root; `..` traversal that escapes the root is \
rejected. Used by the dashboard's skill-bundle directory picker and by \
operators who want to inspect what's installed.

Examples:
  zeroclaw browse                  # list shared/ root
  zeroclaw browse skills           # list shared/skills/
  zeroclaw browse skills/coding    # list shared/skills/coding/")]
    Browse {
        /// Path relative to `<install>/shared/`. Empty = root.
        #[arg(default_value = "")]
        path: String,
    },

    /// Migrate data from other agent runtimes
    Migrate {
        #[command(subcommand)]
        migrate_command: MigrateCommands,
    },

    /// Manage model_provider subscription authentication profiles
    Auth {
        #[command(subcommand)]
        auth_command: AuthCommands,
    },

    /// Manage agent memory (list, get, stats, clear)
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Manage agent memory entries.

List, inspect, and clear memory entries stored by the agent. \
Supports filtering by category and session, pagination, and \
batch clearing with confirmation.

Examples:
  zeroclaw memory stats
  zeroclaw memory list
  zeroclaw memory list --category core --limit 10
  zeroclaw memory get KEY
  zeroclaw memory clear --category conversation --yes")]
    Memory {
        #[command(subcommand)]
        memory_command: MemoryCommands,
    },

    /// Manage configuration
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Manage ZeroClaw configuration.

View, set, or initialize config properties by dotted path. \
Use 'schema' to dump the full JSON Schema for the config file.

Properties are addressed by dotted path (e.g. channels.matrix.mention-only).
Secret fields (API keys, tokens) automatically use masked input.
Enum fields offer interactive selection when value is omitted.

Examples:
  zeroclaw config list                                  # list all properties
  zeroclaw config list --secrets                        # list only secrets
  zeroclaw config list --filter channels.matrix         # filter by prefix
  zeroclaw config get channels.matrix.mention-only      # get a value
  zeroclaw config set channels.matrix.mention-only true # set a value
  zeroclaw config set channels.matrix.access-token      # secret: masked input
  zeroclaw config set channels.matrix.stream-mode       # enum: interactive select
  zeroclaw config init channels.matrix                  # init section with defaults
  zeroclaw config init risk_profiles.strict             # create a new dynamic-map alias
  zeroclaw config schema                                # print JSON Schema to stdout
  zeroclaw config schema > schema.json

Property path tab completion is included automatically in `zeroclaw completions <shell>`.")]
    Config {
        #[command(subcommand)]
        config_command: ConfigCommands,
    },

    /// Update this fork from source
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
This fork has no verified binary update channel.

Update a checkout of https://github.com/kckylechen1/zeroclaw and run \
./install.sh --source with your existing preset/features.

Legacy --check, --force, --version and --json options remain parseable, \
but this command refuses every binary update without network access.")]
    Update {
        /// Legacy flag; binary update checks are disabled in this fork
        #[arg(long)]
        check: bool,
        /// Legacy flag; cannot override the source-only update policy
        #[arg(long)]
        force: bool,
        /// Legacy target version; binary updates are disabled
        #[arg(long)]
        version: Option<String>,
        /// Legacy output flag; the disabled updater always returns an error
        #[arg(long)]
        json: bool,
    },

    /// Run diagnostic self-tests
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Run diagnostic self-tests to verify the ZeroClaw installation.

By default, runs the full test suite including network checks \
(gateway health, memory round-trip). Use --quick to skip network \
checks for faster offline validation.

Examples:
  zeroclaw self-test             # full suite
  zeroclaw self-test --quick     # quick checks only (no network)")]
    SelfTest {
        /// Run quick checks only (no network)
        #[arg(long)]
        quick: bool,
    },

    #[cfg(feature = "agent-runtime")]
    /// Inspect and check the agent Soul
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Soul diagnostics.

`soul check` sends a fixed set of twelve scenarios to each selected model with \
the agent's real Soul as the system prompt and no tools, then asks a judge \
model to score each reply against the Tone guidance (dials) or a pass/fail \
criterion (honesty, factual correction, ask before acting). It flags dials \
judged two or more levels from their setting, dials two or more levels apart \
between targets, and failed floors.

This makes real model calls (targets x 12 replies, plus one judge call per \
reply). The judge is a model: its scores are a signal, not proof. Nothing is \
stored. Findings do not change the exit status; only setup errors do.

Examples:
  zeroclaw soul check --agent default
  zeroclaw soul check --agent default --target anthropic.work --target openai.work:gpt-5
  zeroclaw soul check --agent default --judge anthropic.work --format json")]
    Soul {
        #[command(subcommand)]
        soul_command: SoulCommands,
    },

    #[cfg(all(feature = "agent-runtime", feature = "channel-telegram"))]
    /// Compatibility diagnostics for retired Telegram operator tooling
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
The in-process Telegram listener is retired. Telegram intake and cursor \
recovery belong to zeroclaw-bridge-telegram.

skip-update remains parseable for compatibility and returns a migration \
error. It does not write a skip marker or advance the bridge cursor.")]
    Telegram {
        #[command(subcommand)]
        telegram_command: TelegramCommands,
    },

    /// Generate shell completion script to stdout
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Generate shell completion scripts for `zeroclaw`.

The script is printed to stdout so it can be sourced directly:

Examples (Unix shells):
  source <(zeroclaw completions bash)
  zeroclaw completions zsh > ~/.zfunc/_zeroclaw
  zeroclaw completions fish > ~/.config/fish/completions/zeroclaw.fish

Examples (Windows PowerShell):
  zeroclaw completions powershell | Out-String | Invoke-Expression
  zeroclaw completions powershell > $PROFILE.CurrentUserAllHosts")]
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: CompletionShell,
    },

    /// Print the full CLI reference as Markdown (used by the docs pipeline).
    #[command(hide = true)]
    MarkdownHelp,

    /// Print the config JSON Schema (used by the docs pipeline).
    #[command(hide = true)]
    MarkdownSchema,

    /// Deprecated: use `zeroclaw config` instead
    #[command(hide = true)]
    Props {
        #[command(subcommand)]
        props_command: DeprecatedPropsCommands,
    },

    /// Restore the translated locale files (FTL) shipped with this build
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Restore this build's translated Fluent (.ftl) catalogues under \
`<config-dir>/data/ftl/<locale>/`, where the runtime loader reads them. \
The compatibility command name is fetch; no network access is needed.

Pass en or zh-CN. By default every catalogue is restored; restrict with \
--catalog (comma-separated): cli, tools.

Examples:
  zeroclaw locales fetch zh-CN
  zeroclaw locales fetch zh-CN --catalog cli,tools")]
    Locales {
        #[command(subcommand)]
        locales_command: LocalesCommands,
    },
}

#[derive(Subcommand, Debug)]
enum LocalesCommands {
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    /// Restore this build's translated FTL files without network access
    Fetch {
        /// Locale code to restore: en or zh-CN.
        locale: String,
        /// Comma-separated catalogues to restore: cli, tools.
        /// Omit to restore all of them.
        #[arg(long)]
        catalog: Option<String>,
    },
}

/// Stub enum that mirrors the old `props` subcommands so clap can still parse
/// `zeroclaw props <anything>` and print a deprecation message.
#[derive(Subcommand, Debug)]
enum DeprecatedPropsCommands {
    #[command(external_subcommand)]
    Any(Vec<String>),
}

#[cfg(feature = "agent-runtime")]
fn runtime_dir_env_is_explicit(name: &str, value: &str) -> bool {
    match name {
        "ZEROCLAW_CONFIG_DIR" | "ZEROCLAW_DATA_DIR" => !value.trim().is_empty(),
        "ZEROCLAW_WORKSPACE" => !value.is_empty(),
        _ => false,
    }
}

#[cfg(feature = "agent-runtime")]
fn resolve_homebrew_onboard_config_dir(
    exe: &Path,
    env_lookup: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let explicit_runtime_dir = [
        "ZEROCLAW_CONFIG_DIR",
        "ZEROCLAW_DATA_DIR",
        "ZEROCLAW_WORKSPACE",
    ]
    .iter()
    .any(|name| env_lookup(name).is_some_and(|value| runtime_dir_env_is_explicit(name, &value)));

    if explicit_runtime_dir {
        return None;
    }

    zeroclaw_runtime::service::homebrew_var_dir_from_exe(exe)
}

#[cfg(feature = "agent-runtime")]
fn apply_homebrew_onboard_config_dir_with(
    exe: &Path,
    env_lookup: impl Fn(&str) -> Option<String>,
    mut set_env: impl FnMut(&'static str, &Path),
) -> Option<PathBuf> {
    let config_dir = resolve_homebrew_onboard_config_dir(exe, env_lookup)?;
    set_env("ZEROCLAW_CONFIG_DIR", &config_dir);
    Some(config_dir)
}

#[cfg(feature = "agent-runtime")]
fn apply_homebrew_onboard_config_dir() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    apply_homebrew_onboard_config_dir_with(
        &exe,
        |name| std::env::var(name).ok(),
        |name, value| {
            // SAFETY: called early in the onboard command path before new threads are spawned.
            unsafe { std::env::set_var(name, value) };
        },
    );
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Dump the full configuration JSON Schema to stdout. With `--path`, returns
    /// the schema fragment for that property only — same payload `OPTIONS
    /// /api/config/prop?path=...` returns over HTTP.
    Schema {
        /// Property path to scope the schema dump (e.g.
        /// `agents.researcher.model_provider`). Without it, dumps the
        /// whole-config schema.
        #[arg(long)]
        path: Option<String>,
    },
    /// List all config properties with current values
    List {
        /// Filter by path prefix (e.g. "channels.telegram")
        #[arg(short, long)]
        filter: Option<String>,
        /// Show only secret (encrypted) fields
        #[arg(long)]
        secrets: bool,
    },
    /// Get a config property value
    Get {
        /// Property path (e.g. channels.telegram.mention-only)
        path: String,
        /// Emit a structured JSON envelope ({path, value} or {path, populated}) instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Set a config property (secret fields auto-prompt for masked input)
    Set {
        /// Property path
        path: String,
        /// New value (omit for secret fields to get masked input)
        value: Option<String>,
        /// Skip interactive prompts — require value on command line, accept raw strings for enums
        #[arg(long)]
        no_interactive: bool,
        /// Optional comment to write alongside the value in TOML (preserves through future edits).
        #[arg(long)]
        comment: Option<String>,
        /// Emit a structured JSON envelope on success.
        #[arg(long)]
        json: bool,
    },
    /// Initialize unconfigured sections with defaults (enabled=false)
    Init {
        /// Section prefix (e.g. channels.matrix), or <section>.<alias> to create a new dynamic-map alias (e.g. risk_profiles.strict). Omit to init all.
        section: Option<String>,
        /// Emit a structured JSON envelope ({initialized: [...]}) instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Migrate the on-disk config to the current schema version (preserves comments)
    Migrate {
        /// Emit a structured JSON envelope ({migrated, backup_path?, schema_version, valid?, error?}) instead of plain text.
        #[arg(long)]
        json: bool,
    },
    /// Apply a JSON Patch (RFC 6902) document atomically. Mirrors `PATCH /api/config`.
    /// Reads operations from the given file, or from stdin when path is `-` or omitted.
    /// Supported ops: `add`, `replace`, `remove`, `test`. `move` and `copy` are rejected.
    Patch {
        /// Path to a JSON Patch document, or `-` for stdin (default).
        input: Option<String>,
        /// Print results as JSON (one object per applied op) instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    Generate {
        /// Target schema version (e.g. 1, 2, 3). Defaults to current.
        version: Option<u32>,
        /// Encrypt secret-bearing string values in the output (api_key,
        /// bot_token, access_token, password, refresh_token, etc.). Works
        /// at every schema version via a key-name-based walker. Uses the
        /// resolved config-dir's `.secret_key` (creates one if missing).
        #[arg(long)]
        encrypt: bool,
    },
    /// Print matching property paths for shell completion (hidden)
    #[command(hide = true)]
    Complete {
        /// Partial path to complete
        partial: Option<String>,
    },
}

#[cfg(feature = "agent-runtime")]
#[derive(Subcommand, Debug)]
enum SecurityCommands {
    /// Show security posture for the default or selected agent risk profile
    Status {
        /// Agent alias whose effective runtime security posture should be inspected.
        #[arg(long)]
        agent: String,

        /// Emit machine-readable JSON instead of human text.
        #[arg(long)]
        json: bool,
    },
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
    /// Login with OAuth (OpenAI Codex, Gemini, or xAI)
    Login {
        /// ModelProvider (`openai-codex`, `gemini`, or `xai`)
        #[arg(long)]
        model_provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Use OAuth device-code flow
        #[arg(long)]
        device_code: bool,
        /// Import an existing auth.json file instead of starting a new login flow.
        /// Supports `openai-codex` (`~/.codex/auth.json`) and `xai` (`~/.grok/auth.json`).
        #[arg(long, value_name = "PATH", conflicts_with = "device_code")]
        import: Option<PathBuf>,
    },
    /// Complete OAuth by pasting redirect URL or auth code
    PasteRedirect {
        /// ModelProvider (`openai-codex`, `gemini`, or `xai`)
        #[arg(long)]
        model_provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
        /// Full redirect URL or raw OAuth code
        #[arg(long)]
        input: Option<String>,
    },
    /// Paste setup token / auth token (for Anthropic subscription auth)
    PasteToken {
        /// ModelProvider (`anthropic`)
        #[arg(long)]
        model_provider: String,
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
        /// ModelProvider (`anthropic`)
        #[arg(long)]
        model_provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Refresh OAuth access token using refresh token
    Refresh {
        /// ModelProvider (`openai-codex`, `gemini`, or `xai`)
        #[arg(long)]
        model_provider: String,
        /// Profile name or profile id
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove auth profile
    Logout {
        /// ModelProvider
        #[arg(long)]
        model_provider: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
    },
    /// Set active profile for a model_provider
    Use {
        /// ModelProvider
        #[arg(long)]
        model_provider: String,
        /// Profile name or full profile id
        #[arg(long)]
        profile: String,
    },
    /// List auth profiles
    List,
    /// Show auth status with active profile and token expiry info
    Status,
    /// Authenticate an email channel via OAuth2 device-code flow
    EmailLogin {
        /// Email channel alias from [channels.email.<alias>] (e.g. 'hotmail')
        #[arg(long)]
        channel: String,
        /// Profile name (default: default)
        #[arg(long, default_value = "default")]
        profile: String,
    },
}

#[derive(Subcommand, Debug)]
enum ModelCommands {
    /// Refresh and cache model_provider models
    Refresh {
        /// ModelProvider name (defaults to configured default model_provider)
        #[arg(long)]
        model_provider: Option<String>,

        /// Refresh all model_providers that support live model discovery
        #[arg(long)]
        all: bool,

        /// Force live refresh and ignore fresh cache
        #[arg(long)]
        force: bool,
    },
    /// List the models configured in config.toml
    List {
        /// ModelProvider name (defaults to all configured entries)
        #[arg(long)]
        model_provider: Option<String>,

        /// Verify each configured model against the provider's live catalog
        #[arg(long)]
        check: bool,
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
    /// Probe model catalogs across model_providers and report availability
    Models {
        /// Probe a specific model_provider only (default: all known model_providers)
        #[arg(long)]
        model_provider: Option<String>,

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
    /// Update context_window in config.toml from provider /models endpoints
    UpdateContextWindows {
        /// Update a specific model_provider only (default: all known model_providers)
        #[arg(long)]
        model_provider: Option<String>,

        /// Show what would be updated without writing to config
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCommands {
    /// Copy a recognized legacy kernel database to a new, verified database.
    MigrateKernel {
        /// Existing standalone memcore database; never modified.
        #[arg(long)]
        source: PathBuf,
        /// New database path; must not exist.
        #[arg(long)]
        destination: PathBuf,
        /// Host-owned store role to admit at the destination.
        #[arg(long, value_enum)]
        store: KernelMigrationStoreKind,
        /// Operator identity recorded in the migration receipt.
        #[arg(long)]
        approved_by: String,
    },
    /// Convert a quiescent native brain.db database into a new, verified Tachi database.
    MigrateNative {
        /// Existing native brain.db database; never modified.
        #[arg(long)]
        source: PathBuf,
        /// New Tachi database path; must not exist.
        #[arg(long)]
        destination: PathBuf,
        /// Operator identity recorded in the migration receipt.
        #[arg(long)]
        approved_by: String,
    },
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
    Get {
        key: String,
    },
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
    Reindex,
}

/// Bootstrap the value of the global `--config-dir` flag before clap renders
/// localized help. The command comes from [`Cli::command`], so clap remains
/// responsible for option ownership, external-subcommand payloads, value
/// parsing, and the option terminator.
fn probe_config_dir(
    command: &clap::Command,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<String> {
    // Help and version normally return display errors before exposing matches.
    // In this bootstrap view, make them ordinary parse boundaries and retain
    // the matches clap accumulated before the boundary.
    let matches = command
        .clone()
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .ignore_errors(true)
        .try_get_matches_from(args)
        .ok()?;

    matches
        .try_get_one::<String>("config_dir")
        .ok()
        .flatten()
        .cloned()
}

fn apply_i18n_to_command(cmd: clap::Command) -> clap::Command {
    #[cfg(feature = "agent-runtime")]
    {
        apply_cmd_translations(cmd, "cli")
    }
    #[cfg(not(feature = "agent-runtime"))]
    cmd
}

#[cfg(feature = "agent-runtime")]
fn apply_cmd_translations(cmd: clap::Command, prefix: &str) -> clap::Command {
    let sub_names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();

    let about_key = format!("{prefix}-about");
    let cmd = match crate::i18n::get_cli_string(&about_key) {
        Some(about) => cmd.about(about),
        None => cmd,
    };

    let long_about_key = format!("{prefix}-long-about");
    let cmd = match crate::i18n::get_cli_string(&long_about_key) {
        Some(long_about) => cmd.long_about(long_about),
        None => cmd,
    };

    let mut cmd = cmd;
    for name in &sub_names {
        let child_prefix = format!("{prefix}-{name}");
        cmd = cmd.mut_subcommand(name, |sub| apply_cmd_translations(sub, &child_prefix));
    }
    cmd
}

#[cfg(feature = "agent-runtime")]
fn validated_locale(locale: &str) -> Result<String> {
    let ok_shape = !locale.is_empty()
        && locale.len() <= 16
        && locale
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !ok_shape {
        bail!("invalid locale code '{locale}'");
    }
    let known = zeroclaw_runtime::i18n::available_locales();
    if !known.iter().any(|o| o.code == locale) {
        let codes: Vec<&str> = known.iter().map(|o| o.code.as_str()).collect();
        bail!(
            "locale '{locale}' is not in the locales.toml registry; known: {}",
            codes.join(", ")
        );
    }
    Ok(locale.to_string())
}

#[cfg(feature = "agent-runtime")]
fn fetch_locales(locale: &str, catalog: Option<&str>) -> Result<()> {
    let locale = validated_locale(locale)?;

    let selected: Vec<&(&str, &str)> = match catalog {
        None => zeroclaw_config::schema::FTL_CATALOGS.iter().collect(),
        Some(list) => {
            let names: Vec<&str> = list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let mut out = Vec::new();
            for name in &names {
                match zeroclaw_config::schema::FTL_CATALOGS
                    .iter()
                    .find(|(n, _)| n == name)
                {
                    Some(entry) => out.push(entry),
                    None => {
                        let valid = zeroclaw_config::schema::FTL_CATALOGS
                            .iter()
                            .map(|(n, _)| *n)
                            .collect::<Vec<_>>()
                            .join(", ");
                        bail!("unknown catalog '{name}'; valid: {valid}");
                    }
                }
            }
            out
        }
    };

    let dest = zeroclaw_config::schema::ftl_locale_dir(&locale)?;
    std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
    // Confinement check: the resolved dest must live under the data-dir FTL root.
    let ftl_root = dest
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| dest.clone());
    let canon_dest = std::fs::canonicalize(&dest).unwrap_or_else(|_| dest.clone());
    let canon_root = std::fs::canonicalize(&ftl_root).unwrap_or(ftl_root);
    if !canon_dest.starts_with(&canon_root) {
        bail!("refusing to write outside the FTL data directory");
    }

    let mut restored = 0u32;

    for (name, out_name) in selected {
        let content =
            zeroclaw_runtime::i18n::builtin_catalog(&locale, name).with_context(|| {
                ta(
                    "cli-locales-missing-builtin",
                    &[("name", name), ("locale", &locale)],
                    "catalogue is not included in this build",
                )
            })?;
        let out_path = dest.join(out_name);
        std::fs::write(&out_path, content)
            .with_context(|| format!("writing {}", out_path.display()))?;
        println!(
            "{}",
            ta(
                "cli-locales-restored",
                &[("name", name), ("path", &out_path.display().to_string())],
                "restored bundled catalogue",
            )
        );
        restored += 1;
    }

    if restored == 0 {
        bail!(crate::i18n::get_required_cli_string(
            "cli-locales-none-selected"
        ));
    }
    println!(
        "{}",
        ta(
            "cli-locales-installed",
            &[
                ("count", &restored.to_string()),
                ("locale", &locale),
                ("dir", &dest.display().to_string())
            ],
            "Installed catalogues",
        )
    );
    Ok(())
}

fn main() -> Result<()> {
    let command = Cli::command();

    // Offline migration needs only its explicit paths and operator identity.
    // Dispatch before config, proxy, logging, OTP or runtime initialization, so
    // a missing or malformed agent config cannot block recovery. Use the OS
    // locale here; resolving the configured locale would read that config.
    if let Ok(matches) = command.clone().try_get_matches_from(std::env::args_os())
        && let Commands::Memory { memory_command } = Cli::from_arg_matches(&matches)?.command
    {
        match memory_command {
            MemoryCommands::MigrateKernel {
                source,
                destination,
                store,
                approved_by,
            } => {
                #[cfg(feature = "agent-runtime")]
                crate::i18n::init(&sys_locale::get_locale().unwrap_or_else(|| "en".to_string()));
                return memory::cli::handle_kernel_migration(
                    &source,
                    &destination,
                    store,
                    &approved_by,
                );
            }
            MemoryCommands::MigrateNative {
                source,
                destination,
                approved_by,
            } => {
                #[cfg(feature = "agent-runtime")]
                crate::i18n::init(&sys_locale::get_locale().unwrap_or_else(|| "en".to_string()));
                return memory::cli::handle_native_migration(&source, &destination, &approved_by);
            }
            _ => {}
        }
    }

    // Locale detection runs while clap builds localized help, so expose the CLI
    // override through the bootstrap env before either i18n or Tokio starts.
    // Empty values remain for clap's canonical parse/validation path below.
    if let Some(config_dir) = probe_config_dir(&command, std::env::args_os())
        && !config_dir.trim().is_empty()
    {
        // SAFETY: this synchronous bootstrap runs before the Tokio runtime (and
        // therefore its worker threads) is constructed.
        unsafe { std::env::set_var("ZEROCLAW_CONFIG_DIR", config_dir) };
    }

    // Startup application of the persisted `[proxy]` config. The
    // model-visible `proxy_config` tool no longer registers, so proxy
    // state changes only through trusted config writes and takes effect
    // here, before any task that consumes proxy state (runtime global for
    // every scope, plus process env when enabled with
    // scope=environment). Must run before the multi-threaded runtime
    // starts: process-env mutation is only sound while no runtime worker
    // or blocking-pool threads exist, so the reading runtime is fully
    // dropped before anything is applied. Daemon reloads refresh the
    // runtime global only, keeping live-env application restart-only.
    // Stdout-only invocations (completions, markdown-help/schema, help,
    // version, parse errors) skip this read; the pre-existing i18n
    // locale detection is unchanged by this gate.
    if invocation_needs_config() {
        let proxy = {
            let pre_runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let proxy = pre_runtime.block_on(config::read_persisted_proxy_for_boot());
            // Drop before applying: shutting the runtime down joins its
            // blocking-pool threads (the resolver may have touched
            // tokio::fs), leaving the process single-threaded again.
            drop(pre_runtime);
            proxy
        };
        if let Some(proxy) = proxy {
            config::apply_persisted_proxy_on_boot(&proxy);
        }
    }

    async_main(command)
}

/// Decide pre-parse whether this invocation will need the config-driven
/// runtime, using a non-exiting trial parse of the canonical (not yet
/// localized) CLI. Parse errors, `--help` and `--version` surface as
/// `Err` and re-parse inside `async_main` for the localized error/help
/// path; the stdout-only subcommands are enumerated explicitly so they
/// skip the proxy bootstrap read. (The pre-existing locale detection in
/// `apply_i18n_to_command` still reads config.toml on every invocation,
/// including these — unchanged by this gate.)
fn invocation_needs_config() -> bool {
    let command = Cli::command();
    let Ok(matches) = command.try_get_matches_from(std::env::args_os()) else {
        return false;
    };
    let config_free = [
        matches.subcommand_matches("completions").is_some(),
        matches.subcommand_matches("markdown-help").is_some(),
        matches.subcommand_matches("markdown-schema").is_some(),
        matches.subcommand_matches("help").is_some(),
    ];
    !config_free.into_iter().any(|excluded| excluded)
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn async_main(command: clap::Command) -> Result<()> {
    // Install default crypto model_provider for Rustls TLS.
    // This prevents the error: "could not automatically determine the process-level CryptoProvider"
    // when both aws-lc-rs and ring features are available (or neither is explicitly selected).
    #[cfg(feature = "agent-runtime")]
    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        eprintln!(
            "{}",
            ta(
                "cli-warn-crypto-provider",
                &[("err", &format!("{e:?}"))],
                "Warning: Failed to install default crypto provider"
            )
        );
    }

    let cmd = apply_i18n_to_command(command);

    if std::env::args_os().len() <= 1 {
        return print_no_command_help(cmd);
    }

    let cli = Cli::from_arg_matches(&cmd.get_matches()).map_err(|e| e.exit())?;

    if let Some(config_dir) = &cli.config_dir
        && config_dir.trim().is_empty()
    {
        bail!("--config-dir cannot be empty");
    }

    #[cfg(feature = "agent-runtime")]
    crate::i18n::init(&crate::i18n::detect_locale());

    // Completions must remain stdout-only and should not load config or initialize logging.
    // This avoids warnings/log lines corrupting sourced completion scripts.
    if let Commands::Completions { shell } = &cli.command {
        let mut stdout = std::io::stdout().lock();
        write_shell_completion(*shell, &mut stdout)?;
        return Ok(());
    }

    // Docs-pipeline subcommands: stdout-only, no config load, no logging init.
    match &cli.command {
        Commands::MarkdownHelp => {
            clap_markdown::print_help_markdown::<Cli>();
            return Ok(());
        }
        Commands::MarkdownSchema => {
            #[cfg(feature = "schema-export")]
            {
                let schema = schemars::schema_for!(config::Config);
                print!("{}", zeroclaw_schema_markdown::generate(&schema.to_value()));
                return Ok(());
            }
            #[cfg(not(feature = "schema-export"))]
            anyhow::bail!("zeroclaw was built without the 'schema-export' feature");
        }
        _ => {}
    }

    let default_floor = match &cli.command {
        Commands::Acp { .. } | Commands::Agent { message: None, .. } => "warn",
        _ => "info",
    };

    // The explicit flag wins over RUST_LOG; without a flag the
    // subscriber honours RUST_LOG and falls back to this default.
    // matrix suppression is appended in both flag and default paths.
    let recording_filter = cli.log_level.map(|level| {
        format!(
            "{},matrix_sdk=warn,matrix_sdk_base=warn,matrix_sdk_crypto=warn",
            level.as_directive()
        )
    });
    let default_filter =
        format!("{default_floor},matrix_sdk=warn,matrix_sdk_base=warn,matrix_sdk_crypto=warn");

    zeroclaw_log::install_global_subscriber(
        recording_filter.as_deref(),
        &default_filter,
        cli.verbose,
    );

    #[cfg(feature = "agent-runtime")]
    if let Commands::Onboard {
        section,
        quick,
        cli: use_cli,
        tui: _,
        force,
        reinit,
        api_key,
        model_provider,
        model,
        memory,
        channels_only,
        providers_only,
        memory_only,
        hardware_only,
        tunnel_only,
    } = &cli.command
    {
        let any_legacy_flag = section.is_some()
            || *quick
            || *use_cli
            || *force
            || *reinit
            || api_key.is_some()
            || model_provider.is_some()
            || model.is_some()
            || memory.is_some()
            || *channels_only
            || *providers_only
            || *memory_only
            || *hardware_only
            || *tunnel_only;
        if any_legacy_flag {
            eprintln!(
                "error: `zeroclaw onboard` is deprecated and its flags no longer apply. \
                 Use `zeroclaw quickstart` to create a new agent, or `zeroclaw config set <path>=<value>` \
                 for headless updates."
            );
            std::process::exit(2);
        }
        eprintln!(
            "{}",
            t(
                "cli-onboard-deprecated",
                "`zeroclaw onboard` is deprecated — use `zeroclaw quickstart`."
            )
        );
        return Ok(());
    }

    #[cfg(feature = "agent-runtime")]
    if let Commands::Service {
        service_command: ServiceCommands::RunLaunchdDaemon,
        ..
    } = &cli.command
    {
        let config_dir = cli
            .config_dir
            .as_deref()
            .map(std::path::Path::new)
            .context("launchd runner requires --config-dir")?;
        return service::run_launchd_daemon(config_dir).await;
    }

    // All other commands need config loaded first
    let mut config = Box::pin(Config::load_or_init()).await?;
    for section in config
        .degraded_sections
        .iter()
        .chain(config.degraded_security.iter())
    {
        eprintln!(
            "{}",
            ta(
                "cli-config-section-degraded",
                &[
                    ("section", section),
                    ("path", &config.config_path.display().to_string()),
                ],
                "warning: config section is malformed and was reset to defaults \
                 for this run. Values in that section are NOT in effect. Run \
                 `zeroclaw config migrate` to see the parse error, then repair \
                 the file."
            )
        );
    }
    #[cfg(feature = "agent-runtime")]
    observability::runtime_trace::init_from_config(&config.observability, &config.data_dir);
    // Note: the persisted [proxy] config (including its process-env
    // broadcast) was applied in the synchronous `main()` bootstrap. The
    // load above can carry ZEROCLAW_PROXY_* env-var overrides the raw
    // bootstrap read could not see, so refresh the runtime global from
    // the fully-loaded config; live-env application stays restart-only.
    config::set_runtime_proxy_config(config.proxy.clone());
    // Must follow the trace sink init above, or the record has no destination.
    // The daemon reload arm calls the same helper against its reloaded config.
    #[cfg(feature = "agent-runtime")]
    warn_verifiable_intent_withheld(&config);
    #[cfg(feature = "agent-runtime")]
    warn_withheld_operator_tools(&config);
    #[cfg(feature = "agent-runtime")]
    if config.security.otp.enabled {
        let config_dir = config
            .config_path
            .parent()
            .context("Config path must have a parent directory")?;
        let store = security::SecretStore::new(config_dir, config.secrets.encrypt);
        let (_validator, enrollment_uri) =
            security::OtpValidator::from_config(&config.security.otp, config_dir, &store)?;
        if let Some(uri) = enrollment_uri {
            println!(
                "{}",
                t(
                    "cli-otp-initialized",
                    "Initialized OTP secret for ZeroClaw."
                )
            );
            println!(
                "{}",
                ta("cli-otp-enrollment-uri", &[("uri", &uri)], "Enrollment URI")
            );
        }
    }

    #[cfg(not(feature = "agent-runtime"))]
    {
        // Kernel-only mode: minimal CLI agent without channels/tools/gateway
        match cli.command {
            Commands::Agent {
                agent: agent_alias,
                message,
                model_provider,
                model,
                temperature,
                ..
            } => {
                if config.agent(&agent_alias).is_none() {
                    anyhow::bail!(
                        "`zeroclaw agent --agent {agent_alias}` is not configured (no [agents.{agent_alias}] entry)"
                    );
                }
                let agent_entry = config.model_provider_for_agent(&agent_alias);
                let final_temperature = temperature
                    .unwrap_or_else(|| agent_entry.and_then(|e| e.temperature).unwrap_or(0.7));
                if let Some(p) = &model_provider {
                    // Parse --model-provider as "type.alias" or bare "type" (use agent alias as alias name).
                    let (type_key, alias_key) =
                        p.split_once('.').unwrap_or((p.as_str(), &agent_alias));
                    let entry = config
                        .providers
                        .models
                        .ensure(type_key, alias_key)
                        .ok_or_else(|| {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"family": type_key})),
                                "ask CLI refused: --model-provider names an unknown family"
                            );
                            anyhow::Error::msg(format!(
                                "Unknown model_provider family: {type_key}. \
                             Configure a provider via `zeroclaw quickstart` or the /config editor."
                            ))
                        })?;
                    if let Some(m) = &model {
                        entry.model = Some(m.clone());
                    }
                    entry.temperature = Some(final_temperature);
                    // Update the agent's model_provider to point to the override
                    if let Some(agent_cfg) = config.agents.get_mut(&agent_alias) {
                        agent_cfg.model_provider = format!("{type_key}.{alias_key}").into();
                    }
                } else if config.model_provider_for_agent(&agent_alias).is_none() {
                    anyhow::bail!(
                        "No model model_provider configured for agent {agent_alias}. \
                         Pass --model-provider <type> or run `zeroclaw quickstart` to configure one."
                    );
                }

                let (provider_name, resolved_entry) = config
                    .resolved_model_provider_for_agent(&agent_alias)
                    .map(|(ty, _alias, entry)| (ty, Some(entry)))
                    .unwrap_or(("openai", None));
                let model_provider = zeroclaw::providers::create_model_provider(
                    provider_name,
                    resolved_entry.and_then(|e| e.api_key.as_deref()),
                )?;
                let model_name = resolved_entry
                    .and_then(|e| e.model.as_deref())
                    .unwrap_or("default");
                match message {
                    Some(msg) => {
                        let response =
                            zeroclaw_providers::ProviderDispatch::from_ref(&*model_provider)
                                .simple_chat(&msg, model_name, Some(final_temperature))
                                .await?;
                        println!("{response}");
                    }
                    None => {
                        loop {
                            eprint!("> ");
                            let line = {
                                let stdin = std::io::stdin().lock();
                                match read_capped_line(stdin, STDIN_LINE_CAP) {
                                    Ok(CappedLine::Eof) => break,
                                    Ok(CappedLine::Line(s)) => s,
                                    Ok(CappedLine::Truncated) => {
                                        // i18n-exempt: no-runtime fallback lacks the Fluent catalogue.
                                        eprintln!(
                                            "\nWarning: input line exceeds {} bytes and was discarded.",
                                            STDIN_LINE_CAP
                                        );
                                        continue;
                                    }
                                    Err(e) => {
                                        // i18n-exempt: no-runtime fallback lacks the Fluent catalogue.
                                        eprintln!("\nError reading input: {e}\n");
                                        break;
                                    }
                                }
                            };
                            let response =
                                zeroclaw_providers::ProviderDispatch::from_ref(&*model_provider)
                                    .simple_chat(line.trim(), model_name, Some(final_temperature))
                                    .await?;
                            println!("{response}");
                        }
                    }
                }
                return Ok(());
            }
            Commands::Completions { .. } | Commands::MarkdownHelp | Commands::MarkdownSchema => {
                unreachable!()
            }
            _ => {
                anyhow::bail!(
                    "This command requires the full runtime. Rebuild with default features:\n  cargo build --release"
                );
            }
        }
    }

    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::cron::scheduler::register_delivery_fn(Box::new(
            |config, channel, target, thread_id, output| {
                Box::pin(async move {
                    channels::deliver_announcement(&config, &channel, &target, thread_id, &output)
                        .await
                })
            },
        ));
    }

    #[cfg(feature = "agent-runtime")]
    match cli.command {
        Commands::Onboard { .. }
        | Commands::Completions { .. }
        | Commands::MarkdownHelp
        | Commands::MarkdownSchema => unreachable!(),

        Commands::Quickstart {
            model_provider,
            model,
            api_key,
            agent,
        } => {
            Box::pin(commands::quickstart::run_quickstart_cli(
                model_provider,
                model,
                api_key,
                agent,
            ))
            .await?;
            Ok(())
        }

        #[cfg(feature = "agent-runtime")]
        Commands::Chat {
            agent,
            session,
            gateway,
            message,
        } => commands::chat::run(&config, agent, session, gateway, message).await,

        Commands::Agent {
            agent: agent_alias,
            message,
            model_provider,
            model,
            temperature,
        } => {
            // Interactive chat is a gateway client, so every device shares
            // one conversation with the agent.
            let Some(message) = message else {
                let unsupported = interactive_agent_unsupported_flags(
                    model_provider.as_deref(),
                    model.as_deref(),
                    temperature,
                );
                if !unsupported.is_empty() {
                    let flags = unsupported.join(", ");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"flags": flags})),
                        "interactive agent refused: flags only apply with --message"
                    );
                    anyhow::bail!(
                        "{}",
                        ta(
                            "cli-agent-interactive-flags-need-message",
                            &[("flags", &flags), ("agent", &agent_alias)],
                            "These flags only apply with -m/--message"
                        )
                    );
                }
                return commands::chat::run(&config, agent_alias, "main".into(), None, None).await;
            };
            let final_temperature: Option<f64> = temperature.or_else(|| {
                config
                    .model_provider_for_agent(&agent_alias)
                    .and_then(|e| e.temperature)
            });

            // Validate up-front: bail with a clear message if the alias
            // isn't configured. The runtime would error too, but this
            // catches typos before any subsystem spins up.
            if config.agent(&agent_alias).is_none() {
                anyhow::bail!(
                    "`zeroclaw agent --agent {agent_alias}` is not configured (no [agents.{agent_alias}] entry)"
                );
            }

            Box::pin(agent::run(
                config,
                &agent_alias,
                Some(message),
                model_provider,
                model,
                final_temperature,
                true,
                None,
                None,
                zeroclaw_api::ingress::TurnOrigin::Interactive,
                zeroclaw_runtime::agent::loop_::AgentRunOverrides::default(),
            ))
            .await
            .map(|_| ())
        }

        Commands::Acp {
            max_sessions,
            session_timeout,
        } => {
            let _ = (max_sessions, session_timeout);
            anyhow::bail!(
                "Legacy in-process `zeroclaw acp` has been retired; use `zeroclaw acp-gateway` \
                 (or the `zeroclaw-acp-bridge` binary) to connect ACP clients to the gateway."
            )
        }

        Commands::Gateway { gateway_command } => {
            match gateway_command {
                Some(zeroclaw::GatewayCommands::Restart {
                    port,
                    host,
                    allow_degraded_security,
                }) => {
                    let _nag = gate_security_posture(&config, allow_degraded_security)?;
                    let (port, host) = resolve_gateway_addr(&config, port, host);
                    let addr = format!("{host}:{port}");
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"addr": addr})),
                        "🔄 Restarting ZeroClaw Gateway on"
                    );

                    // Try to gracefully shutdown existing gateway via admin endpoint
                    match shutdown_gateway(&host, port, config.gateway.path_prefix.as_deref()).await
                    {
                        Ok(()) => {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({"addr": addr})),
                                "✓ Existing gateway on shut down gracefully"
                            );
                            // Poll until the port is free (connection refused) or timeout
                            let deadline =
                                tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
                            loop {
                                match tokio::net::TcpStream::connect(&addr).await {
                                    Err(_) => break, // port is free
                                    Ok(_) if tokio::time::Instant::now() >= deadline => {
                                        ::zeroclaw_log::record!(
                                            WARN,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Note
                                            )
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                            .with_attrs(::serde_json::json!({"port": port})),
                                            "Timed out waiting for port to be released"
                                        );
                                        break;
                                    }
                                    Ok(_) => {
                                        tokio::time::sleep(tokio::time::Duration::from_millis(50))
                                            .await;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "   No existing gateway to shut down"
                            );
                        }
                    }

                    log_gateway_start(&host, port);
                    Box::pin(run_gateway_if_enabled(&host, port, config, None)).await
                }
                Some(zeroclaw::GatewayCommands::GetPaircode {
                    new,
                    rotate,
                    rotate_device,
                    port,
                    host,
                }) => {
                    let (port, host) = resolve_gateway_addr(&config, port, host);

                    let action = if rotate {
                        PaircodeAction::RotateAll
                    } else if let Some(id) = rotate_device {
                        PaircodeAction::RotateDevice(id)
                    } else if new {
                        PaircodeAction::AddClient
                    } else {
                        PaircodeAction::Show
                    };
                    let rotating = action.is_rotation();

                    match fetch_paircode(
                        &host,
                        port,
                        config.gateway.path_prefix.as_deref(),
                        &config.data_dir,
                        &action,
                    )
                    .await
                    {
                        Ok(PaircodeResult::Code { code, message }) => {
                            println!(
                                "{}",
                                t("cli-pairing-enabled", "🔐 Gateway pairing is enabled.")
                            );
                            println!();
                            if let Some(message) = message.as_deref()
                                && rotating
                            {
                                println!("  ✅ {message}");
                                println!();
                            }
                            println!("  ┌──────────────┐");
                            println!("  │  {code}  │");
                            println!("  └──────────────┘");
                            println!();
                            println!(
                                "{}",
                                t(
                                    "cli-pairing-use-code",
                                    "  Use this one-time code to pair a new device:"
                                )
                            );
                            println!(
                                "{}",
                                ta(
                                    "cli-pairing-post",
                                    &[("code", &code)],
                                    "POST /pair with header X-Pairing-Code"
                                )
                            );
                        }
                        Ok(PaircodeResult::NoCode { message }) => {
                            println!(
                                "{}",
                                paircode_no_code_message(
                                    &host,
                                    port,
                                    &config.gateway.host,
                                    config.gateway.port,
                                    &action,
                                    config.gateway.require_pairing,
                                    message.as_deref(),
                                )
                            );
                        }
                        Err(e) => {
                            println!(
                                "❌ Failed to fetch pairing code from gateway at {host}:{port}"
                            );
                            println!(
                                "{}",
                                ta("cli-error-label", &[("err", &e.to_string())], "Error")
                            );
                            println!();
                            println!(
                                "{}",
                                t(
                                    "cli-gateway-running-q",
                                    "   Is the gateway running? Start it with:"
                                )
                            );
                            println!("     zeroclaw gateway start"); // i18n-exempt: literal command/identifier example
                        }
                    }
                    Ok(())
                }
                Some(zeroclaw::GatewayCommands::Bridge { bridge_command }) => {
                    gateway_helpers::bridges::handle_bridge_command(&mut config, bridge_command)
                        .await
                }
                Some(zeroclaw::GatewayCommands::Start {
                    port,
                    host,
                    allow_degraded_security,
                }) => {
                    let _nag = gate_security_posture(&config, allow_degraded_security)?;
                    let (port, host) = resolve_gateway_addr(&config, port, host);
                    log_gateway_start(&host, port);
                    Box::pin(run_gateway_if_enabled(&host, port, config, None)).await
                }
                None => {
                    // Bare `zeroclaw gateway` has no flag, so degraded security
                    // is never auto-allowed here — fail closed.
                    let _nag = gate_security_posture(&config, false)?;
                    let port = config.gateway.port;
                    let host = config.gateway.host.clone();
                    log_gateway_start(&host, port);
                    Box::pin(run_gateway_if_enabled(&host, port, config, None)).await
                }
            }
        }

        Commands::Daemon {
            port,
            host,
            allow_degraded_security,
        } => {
            // Fail closed before any setup work: refuse to serve with a
            // degraded security posture unless explicitly allowed. This branch
            // never spawns the nag (the `!allow` path only bails); the nag is
            // managed per reload-iteration in the loop below.
            if !config.degraded_security.is_empty() && !allow_degraded_security {
                gate_security_posture(&config, allow_degraded_security)?;
            }
            if let Ok(exe) = std::env::current_exe() {
                let under_home = directories::UserDirs::new()
                    .map(|u| u.home_dir().to_path_buf())
                    .is_some_and(|home| exe.starts_with(&home));
                if under_home {
                    let install_hint = if cfg!(windows) {
                        "Consider installing to a system-wide location (e.g. C:\\Program Files\\ZeroClaw) for service use."
                    } else if cfg!(target_os = "macos") {
                        "Consider installing to /usr/local/bin or /opt/homebrew/bin for system-wide service."
                    } else {
                        "Consider installing to /usr/local/bin for system-wide service."
                    };
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "Daemon running from user home directory: {}. {install_hint}",
                            exe.display()
                        )
                    );
                }
            }
            let port = port.unwrap_or(config.gateway.port);
            let host = host.unwrap_or_else(|| config.gateway.host.clone());
            if port == 0 {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"host": host})),
                    "🧠 Starting ZeroClaw Daemon on (random port)"
                );
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"host": host, "port": port})),
                    "🧠 Starting ZeroClaw Daemon on"
                );
            }

            #[cfg(target_os = "linux")]
            {
                use zeroclaw_config::schema::SandboxBackend;
                // Any enabled agent whose risk_profile uses the docker
                // sandbox triggers the warning — we just need to know
                // *some* agent is using it.
                let sandbox_docker = config
                    .agents
                    .iter()
                    .filter(|(_, a)| a.enabled)
                    .filter_map(|(alias, _)| config.risk_profile_for_agent(alias))
                    .any(|p| matches!(p.sandbox_config().backend, SandboxBackend::Docker));
                let runtime_docker_mem = config.runtime.kind
                    == zeroclaw_config::schema::RuntimeKind::Docker
                    && config
                        .runtime
                        .docker
                        .memory_limit_mb
                        .is_some_and(|mb| mb > 0);
                if (sandbox_docker || runtime_docker_mem)
                    && !zeroclaw_runtime::security::linux_memcg_available()
                {
                    let which = match (sandbox_docker, runtime_docker_mem) {
                        (true, true) => {
                            "security.sandbox.backend = \"docker\" and runtime.kind = \"docker\""
                        }
                        (true, false) => "security.sandbox.backend = \"docker\"",
                        _ => "runtime.kind = \"docker\"",
                    };
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"which": which})),
                        "Docker memory limits are configured but the Linux kernel has no memcg support. Affected config: . Consequence: --memory limits are silently ignored; agents can OOM the host. Fix: add 'cgroup_memory=1 cgroup_enable=memory' to /boot/firmware/cmdline.txt (Raspberry Pi) or enable CONFIG_MEMCG in your kernel, then reboot."
                    );
                }
            }

            // Cron delivery is registered earlier (before the command match)
            // so it works for both `daemon` and `gateway start`.

            // Capture the launch command now, before any in-app upgrade can
            // swap the binary on disk (after which `current_exe()` resolves to a
            // "(deleted)" path on Linux). Used by the post-loop self-respawn.
            zeroclaw_runtime::restart::record_launch();

            // Non-destructive disposition for the retired legacy SOP run store:
            // a real install may still carry `<data_dir>/sop/runs.db` (runs,
            // events, claims, proposals) from before the run-side demolition.
            // The file is left exactly in place - nothing reads, migrates, or
            // deletes it - and this once-per-boot WARN names the migration path
            // so the disposition is never silent. Run truth lives Tachi-side as
            // ProcedureRuns through the procedure_v1 seam.
            let legacy_sop_run_store = config.data_dir.join("sop").join("runs.db");
            if legacy_sop_run_store.exists() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "path": legacy_sop_run_store.display().to_string()
                        })),
                    "Legacy SOP run store found and left in place (never read, migrated, or deleted); SOP runs are Tachi-side ProcedureRuns via the procedure_v1 seam - archive or remove the file manually if it is no longer needed"
                );
            }

            // Reload loop. `daemon::run` returns DaemonExit::Shutdown on
            // SIGINT/SIGTERM (loop ends) or DaemonExit::Reload on SIGUSR1
            // (loop re-reads config from disk and re-runs). The PID stays
            // the same across reloads — only the in-process subsystems
            // tear down + re-instantiate.
            let mut current_config = config;
            // Companion store is owned by this loop. Each iteration drops the
            // previous Arc (after daemon::run returned and subsystem clones
            // died) and opens again. Gateway and channels receive clones of
            // the same handle — they must not call the factory themselves.
            let mut companion_store: Option<Arc<zeroclaw_memory::CompanionStore>> = None;
            // Nag task for the degraded-security warning, scoped to the
            // current config. Re-evaluated each reload iteration so a repaired
            // config stops the warning and a freshly-degraded one starts it.
            let mut degraded_nag: Option<tokio::task::JoinHandle<()>> =
                gate_security_posture(&current_config, allow_degraded_security)?;
            loop {
                companion_store =
                    zeroclaw_memory::reload_companion_store(companion_store, &current_config)?;
                let (companion_for_gateway, _companion_for_channels) =
                    zeroclaw_memory::clone_for_subsystems(&companion_store);
                let companion_outbox_observer =
                    spawn_companion_outbox_observer(companion_store.clone());
                if let Some(store) = companion_store.as_ref() {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "path": store.path().display().to_string(),
                            })),
                        "daemon holding companion store for this generation"
                    );
                }
                let mut registry = daemon::DaemonRegistry::new();

                #[cfg(feature = "gateway")]
                registry.register_gateway(Box::new(
                    move |host, port, config, tx, reload_controls| {
                        let companion_store = companion_for_gateway.clone();
                        Box::pin(async move {
                            Box::pin(zeroclaw_gateway::run_gateway(
                                &host,
                                port,
                                config,
                                tx,
                                reload_controls,
                                companion_store,
                            ))
                            .await
                        })
                    },
                ));

                let exit = Box::pin(daemon::run(
                    current_config.clone(),
                    host.clone(),
                    port,
                    registry,
                ))
                .await;
                if let Some(handle) = companion_outbox_observer {
                    handle.abort();
                }
                let exit = exit?;
                match exit {
                    daemon::DaemonExit::Shutdown => break,
                    daemon::DaemonExit::Reload => {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            "🔄 Daemon reload — re-reading config from disk"
                        );
                        current_config = Box::pin(Config::load_or_init()).await?;
                        // A reload refreshes the runtime proxy global from
                        // the re-read config; live process-env application
                        // stays restart-only (see
                        // `apply_persisted_proxy_on_boot`).
                        config::set_runtime_proxy_config(current_config.proxy.clone());
                        #[cfg(feature = "agent-runtime")]
                        observability::runtime_trace::init_from_config(
                            &current_config.observability,
                            &current_config.data_dir,
                        );
                        // A reload applies config the process has not seen, so an
                        // operator who just enabled the section learns why the tool
                        // is still absent without having to restart.
                        #[cfg(feature = "agent-runtime")]
                        warn_verifiable_intent_withheld(&current_config);
                        #[cfg(feature = "agent-runtime")]
                        warn_withheld_operator_tools(&current_config);
                        if let Some(handle) = degraded_nag.take() {
                            handle.abort();
                        }
                        degraded_nag =
                            gate_security_posture(&current_config, allow_degraded_security)?;
                        // Continue loop: fresh subsystems with the new config.
                    }
                }
            }
            if let Some(handle) = degraded_nag.take() {
                handle.abort();
            }
            // Bare-process auto-restart: the daemon has now torn down (the
            // gateway listener is released), so launch the upgraded binary as a
            // detached child before we exit. No-op unless an in-app upgrade
            // requested a self-respawn.
            zeroclaw_runtime::restart::respawn_if_requested();
            Ok(())
        }

        Commands::Status { format } => commands::status::handle(&config, format).await,

        #[cfg(all(feature = "agent-runtime", feature = "channel-telegram"))]
        Commands::Telegram { telegram_command } => match telegram_command {
            TelegramCommands::SkipUpdate {
                alias,
                update_id,
                reason,
            } => commands::telegram::run_skip_update(&alias, update_id, reason),
        },

        #[cfg(feature = "agent-runtime")]
        Commands::Security {
            security_command: SecurityCommands::Status { agent, json },
        } => {
            let report = security_status::build_report(&config, &agent)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                security_status::print_report(&report);
            }
            Ok(())
        }

        Commands::Estop {
            estop_command,
            level,
            domains,
            tools,
        } => handle_estop_command(&config, estop_command, level, domains, tools),

        Commands::Cron { cron_command } => cron::handle_command(cron_command, &config),

        Commands::Models { model_command } => {
            #[cfg(feature = "agent-runtime")]
            {
                dispatch_models_command(model_command, &mut config).await
            }
            #[cfg(not(feature = "agent-runtime"))]
            {
                match model_command {
                    ModelCommands::List {
                        model_provider,
                        check,
                    } => {
                        doctor::run_configured_models(&config, model_provider.as_deref(), check)
                            .await
                    }
                    ModelCommands::Refresh { model_provider, .. } => {
                        doctor::run_models(&config, model_provider.as_deref(), false, false).await
                    }
                    _ => doctor::run_models(&config, None, false, false).await,
                }
            }
        }

        Commands::Providers {
            providers_command: None,
        } => {
            let model_providers = zeroclaw_providers::list_model_providers();
            let configured_types: std::collections::HashSet<&str> = config
                .providers
                .models
                .iter_entries()
                .map(|(ty, _, _)| ty)
                .collect();
            println!(
                "Supported model model_providers ({} total):\n",
                model_providers.len()
            );
            println!("  ID (use in config)  DESCRIPTION"); // i18n-exempt: literal command/identifier example
            println!("  ─────────────────── ───────────");
            for category in zeroclaw_providers::ModelProviderCategory::all() {
                let in_category: Vec<_> = model_providers
                    .iter()
                    .filter(|p| p.category == *category)
                    .collect();
                if in_category.is_empty() {
                    continue;
                }
                println!("\n  {}:", category.as_str());
                for p in in_category {
                    let is_configured = configured_types.contains(p.name);
                    let marker = if is_configured { " (configured)" } else { "" };
                    let local_tag = if p.local { " [local]" } else { "" };
                    println!("  {:<19} {}{}{}", p.name, p.display_name, local_tag, marker);
                }
            }
            println!(
                "\n  Set [providers.models.custom.<alias>] uri = \"<URL>\" for any \
                 OpenAI-compatible endpoint, or [providers.models.anthropic.<alias>] \
                 uri = \"<URL>\" for an Anthropic-compatible endpoint."
            );
            Ok(())
        }

        Commands::Providers {
            providers_command: Some(providers_command),
        } => Box::pin(alias_cli::handle_providers(providers_command, &mut config)).await,

        Commands::Service {
            service_command,
            service_init,
        } => {
            let init_system = service_init.parse()?;
            service::handle_command(&service_command, &config, init_system)
        }

        Commands::Doctor { doctor_command } => match doctor_command {
            Some(DoctorCommands::Models {
                model_provider,
                use_cache: _,
            }) => doctor::run_configured_models(&config, model_provider.as_deref(), true).await,
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
            Some(DoctorCommands::UpdateContextWindows {
                model_provider,
                dry_run,
            }) => {
                Box::pin(doctor::update_context_windows(
                    &mut config,
                    model_provider.as_deref(),
                    dry_run,
                    None,
                ))
                .await?;
                Ok(())
            }
            None => doctor::run(&config).await,
        },

        Commands::Channel { channel_command } => match channel_command {
            ChannelCommands::Start => {
                let cancel = tokio_util::sync::CancellationToken::new();
                Box::pin(channels::start_channels(config, cancel)).await
            }
            ChannelCommands::Doctor => Box::pin(channels::doctor_channels(config)).await,
            other => Box::pin(channels::handle_command(other, &config)).await,
        },

        Commands::Agents { agents_command } => {
            Box::pin(alias_cli::handle_agents(agents_command, &mut config)).await
        }
        Commands::Channels { channels_command } => {
            Box::pin(alias_cli::handle_channels(channels_command, &mut config)).await
        }

        Commands::Integrations {
            integration_command,
        } => integrations::handle_command(integration_command, &config),

        Commands::Skills { skill_command } => skills::handle_command(skill_command, &config).await,

        Commands::Browse { path } => browse::handle_browse(path, &config),

        Commands::Migrate { migrate_command } => {
            migration::handle_command(migrate_command, &config).await
        }

        Commands::Memory { memory_command } => {
            memory::cli::handle_command(memory_command, &config).await
        }

        Commands::Auth { auth_command } => {
            commands::auth::handle_auth_command(auth_command, &config).await
        }

        Commands::Locales { locales_command } => {
            let LocalesCommands::Fetch { locale, catalog } = locales_command;
            fetch_locales(&locale, catalog.as_deref())?;
            Ok(())
        }

        Commands::Update { .. } => commands::update::unavailable(),

        Commands::SelfTest { quick } => {
            let results = if quick {
                commands::self_test::run_quick(&config).await?
            } else {
                commands::self_test::run_full(&config).await?
            };
            commands::self_test::print_results(&results);
            let failed = results.iter().filter(|r| !r.passed).count();
            if failed > 0 {
                std::process::exit(1);
            }
            Ok(())
        }

        #[cfg(feature = "agent-runtime")]
        Commands::Soul { soul_command } => match soul_command {
            SoulCommands::Check {
                agent,
                targets,
                judge,
                format,
            } => {
                let report =
                    commands::soul::check(&config, &agent, &targets, judge.as_deref()).await?;
                commands::soul::print_report(&report, format);
                Ok(())
            }
        },

        Commands::Config { config_command } => {
            commands::config::handle(config_command, &mut config).await
        }

        Commands::Props { .. } => {
            anyhow::bail!(
                "`zeroclaw props` has been renamed to `zeroclaw config`. \
                 Replace `props` with `config` in your command and try again."
            );
        }
    }
}

#[cfg(feature = "agent-runtime")]
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
                    println!(
                        "{}",
                        t(
                            "cli-otp-initialized",
                            "Initialized OTP secret for ZeroClaw."
                        )
                    );
                    println!(
                        "{}",
                        ta("cli-otp-enrollment-uri", &[("uri", &uri)], "Enrollment URI")
                    );
                }
                Some(validator)
            } else {
                None
            };

            manager.resume(selector, otp_code.as_deref(), otp_validator.as_ref())?;
            println!("{}", t("cli-estop-resume-done", "Estop resume completed."));
            print_estop_status(&manager.status());
            Ok(())
        }
        None => {
            let engage_level = build_engage_level(level, domains, tools)?;
            manager.engage(engage_level)?;
            println!("{}", t("cli-estop-engaged", "Estop engaged."));
            print_estop_status(&manager.status());
            Ok(())
        }
    }
}

#[cfg(feature = "agent-runtime")]
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

#[cfg(feature = "agent-runtime")]
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

#[cfg(feature = "agent-runtime")]
fn print_estop_status(state: &security::EstopState) {
    println!("{}", t("cli-estop-status", "Estop status:"));
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
        println!(
            "{}",
            t("cli-estop-domains-none", "  domain_blocks:  (none)")
        );
    } else {
        println!(
            "{}",
            ta(
                "cli-estop-domains",
                &[("v", &state.blocked_domains.join(", "))],
                "domain_blocks"
            )
        );
    }
    if state.frozen_tools.is_empty() {
        println!("{}", t("cli-estop-tools-none", "  tool_freeze:    (none)"));
    } else {
        println!(
            "{}",
            ta(
                "cli-estop-tools",
                &[("v", &state.frozen_tools.join(", "))],
                "tool_freeze"
            )
        );
    }
    if let Some(updated_at) = &state.updated_at {
        println!(
            "{}",
            ta(
                "cli-estop-updated-at",
                &[("v", &updated_at.to_string())],
                "updated_at"
            )
        );
    }
}

fn write_shell_completion<W: Write>(shell: CompletionShell, writer: &mut W) -> Result<()> {
    use clap_complete::generate;
    use clap_complete::shells;

    let mut cmd = Cli::command();
    let bin_name = cmd.get_name().to_string();

    match shell {
        CompletionShell::Bash => {
            generate(shells::Bash, &mut cmd, bin_name.clone(), writer);
            // Wrap clap's _zeroclaw to inject dynamic config path completion
            writeln!(
                writer,
                r#"
# Dynamic completion for zeroclaw config get/set paths
if type _zeroclaw &>/dev/null; then
    # Capture the original clap-generated function body so the wrapper
    # can fall back to it without entering an infinite recursion loop.
    eval "$(declare -f _zeroclaw | sed '1s/_zeroclaw/_zeroclaw_clap_orig/')"
    _zeroclaw() {{
        local cur="${{COMP_WORDS[COMP_CWORD]}}"
        if [[ "${{COMP_WORDS[*]}}" =~ "config "(get|set)" " ]]; then
            COMPREPLY=($(compgen -W "$(zeroclaw config complete "$cur" 2>/dev/null)" -- "$cur"))
            return
        fi
        _zeroclaw_clap_orig "$@"
    }}
fi"#
            )?;
        }
        CompletionShell::Fish => {
            generate(shells::Fish, &mut cmd, bin_name.clone(), writer);
            writeln!(
                writer,
                r#"
# Dynamic completion for zeroclaw config get/set paths
complete -c zeroclaw -n '__fish_seen_subcommand_from config; and __fish_seen_subcommand_from get set' \
    -a '(zeroclaw config complete (commandline -ct) 2>/dev/null)' -f"#
            )?;
        }
        CompletionShell::Zsh => {
            generate(shells::Zsh, &mut cmd, bin_name.clone(), writer);
            // Wrap clap's _zeroclaw to inject dynamic config path completion
            writeln!(
                writer,
                r#"
# Dynamic completion for zeroclaw config get/set paths
if (( $+functions[_zeroclaw] )); then
    functions[_zeroclaw_clap_orig]=$functions[_zeroclaw]
    _zeroclaw() {{
        if [[ "${{words[*]}}" == *"config "(get|set)* ]] && (( CURRENT > 3 )); then
            local -a props
            props=(${{(f)"$(zeroclaw config complete "$words[CURRENT]" 2>/dev/null)"}})
            compadd -a props
            return
        fi
        _zeroclaw_clap_orig "$@"
    }}
fi"#
            )?;
        }
        CompletionShell::PowerShell => {
            generate(shells::PowerShell, &mut cmd, bin_name.clone(), writer);
        }
        CompletionShell::Elvish => generate(shells::Elvish, &mut cmd, bin_name, writer),
    }

    writer.flush()?;
    Ok(())
}

/// Tell the operator that `vi_verify` is withheld from the model-visible
/// registry while no credential chain verifier exists.
///
/// Called once per config application: at process config load, and again when
/// the daemon reload arm re-reads config from disk. Registry assembly is the
/// wrong home for it, because that runs on ordinary gateway requests and on
/// nested SOP and delegation rebuilds. Each call site must sit after its
/// `runtime_trace::init_from_config`, or the record has no sink.
#[cfg(feature = "agent-runtime")]
fn warn_verifiable_intent_withheld(config: &Config) {
    if !config.verifiable_intent.enabled {
        return;
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
        "verifiable_intent: vi_verify is not registered as a model-callable tool because no credential chain verifier exists yet (see #9328)"
    );
}

/// Operator/admin tools retired from the model-visible registry: sections
/// that used to enable them as model tools stay parseable, so an operator
/// whose config still enables one learns where the capability went. Same
/// lifecycle as `warn_verifiable_intent_withheld` — process startup and
/// daemon reload, deliberately not registry assembly (which runs per
/// gateway request and per nested rebuild).
#[cfg(feature = "agent-runtime")]
fn warn_withheld_operator_tools(config: &Config) {
    if config.backup.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "backup: the backup model tool is retired; backup create/list/verify/restore are operator-only via the gateway operator API (POST/GET /api/agents/<alias>/backup). The [backup] section still configures that surface"
        );
    }
    if config.data_retention.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "data_retention: the data_management model tool is retired; retention status/stats/purge are operator-only via the gateway operator API (/api/agents/<alias>/data-retention). The [data_retention] section still configures that surface"
        );
    }
    if config.security_ops.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "security_ops: the security_ops model tool is retired and has no replacement surface; the diagnostics module is unreachable while enabled stays true. Unset security_ops.enabled to silence this notice"
        );
    }
}

fn gate_security_posture(
    config: &zeroclaw::config::Config,
    allow_degraded: bool,
) -> anyhow::Result<Option<tokio::task::JoinHandle<()>>> {
    if config.degraded_security.is_empty() {
        return Ok(None);
    }
    let sections = config.degraded_security.join(", ");
    if !allow_degraded {
        anyhow::bail!(
            "Config contains malformed security-critical sections ({sections}); \
             they were reset to defaults, so the running posture may be weaker \
             than intended. Refusing to serve with a degraded security posture. \
             Repair these sections in {} and restart — run `zeroclaw config \
             migrate` to see the precise error. To boot anyway (e.g. to reach \
             the gateway config editor and repair from there), re-run with \
             `--allow-degraded-security`.",
            config.config_path.display()
        );
    }
    let config_path = config.config_path.display().to_string();
    let handle = ::zeroclaw_spawn::spawn!(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            ticker.tick().await;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({ "degraded_security": sections })),
                &format!(
                    "Running with DEGRADED security: sections ({sections}) were reset to \
                     defaults and `--allow-degraded-security` was set. The posture may be \
                     weaker than intended — repair {config_path} and reload \
                     (SIGUSR1 / `zeroclaw admin reload`) as soon as possible."
                )
            );
        }
    });
    Ok(Some(handle))
}

fn companion_outbox_health_for_cli(
    config: &zeroclaw_config::schema::Config,
) -> zeroclaw_api::companion::CompanionOutboxHealth {
    zeroclaw_memory::probe_companion_outbox_health(config)
}

fn print_companion_outbox_line(config: &zeroclaw_config::schema::Config) {
    use zeroclaw_api::companion::CompanionOutboxStatus;

    let health = companion_outbox_health_for_cli(config);
    match health.status {
        CompanionOutboxStatus::NotConfigured => {
            println!(
                "{}",
                t(
                    "cli-status-companion-outbox-not-configured",
                    "Companion outbox: not configured"
                )
            );
        }
        CompanionOutboxStatus::Pending => {
            let pending = health.pending_count.to_string();
            if let Some(age) = health.oldest_pending_age_secs {
                let age = age.to_string();
                let fallback =
                    format!("Companion outbox: pending ({pending} events, oldest {age}s)");
                println!(
                    "{}",
                    ta(
                        "cli-status-companion-outbox-pending-oldest",
                        &[("pending", &pending), ("age", &age)],
                        &fallback
                    )
                );
            } else {
                let fallback = format!("Companion outbox: pending ({pending} events)");
                println!(
                    "{}",
                    ta(
                        "cli-status-companion-outbox-pending",
                        &[("pending", &pending)],
                        &fallback
                    )
                );
            }
        }
    }
}

fn spawn_companion_outbox_observer(
    store: Option<std::sync::Arc<zeroclaw_memory::CompanionStore>>,
) -> Option<tokio::task::JoinHandle<()>> {
    let store = store?;
    Some(::zeroclaw_spawn::spawn!(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            zeroclaw_memory::OUTBOX_OBSERVE_INTERVAL_SECS,
        ));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let _health = store.observe_local_outbox();
        }
    }))
}

#[cfg(feature = "gateway")]
async fn run_gateway_if_enabled(
    host: &str,
    port: u16,
    config: zeroclaw::config::Config,
    tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
) -> anyhow::Result<()> {
    let default_host = config.gateway.host.clone();
    let default_port = config.gateway.port;
    // Capture the launch command before the gateway starts so in-app upgrade
    // can self-respawn after the listener is released. Must mirror the same
    // call in the Daemon branch.
    zeroclaw_runtime::restart::record_launch();
    // Standalone gateway (no daemon supervisor): pass None for reload_tx so
    // /admin/reload returns 503 with a clear "no supervisor; restart
    // manually" message.
    // Companion store is constructed once here — run_gateway never opens it.
    let companion_store = zeroclaw_memory::create_companion_store(&config)?;
    let result = Box::pin(gateway::run_gateway(
        host,
        port,
        config,
        tx,
        None,
        companion_store,
    ))
    .await;
    // Self-respawn after the listener is released, if an in-app upgrade
    // requested it. No-op when no respawn was requested or on supervised
    // restart modes.
    zeroclaw_runtime::restart::respawn_if_requested();
    match result {
        Err(err) if is_addr_in_use_error(&err) => {
            let restart_port = available_gateway_restart_hint_port(host, port);
            anyhow::bail!(
                "{}",
                gateway_addr_in_use_message(host, port, &default_host, default_port, restart_port)
            );
        }
        other => other,
    }
}

#[cfg(not(feature = "gateway"))]
#[allow(clippy::unused_async)]
async fn run_gateway_if_enabled(
    _host: &str,
    _port: u16,
    _config: zeroclaw::config::Config,
    _tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
) -> anyhow::Result<()> {
    anyhow::bail!("Gateway feature is not enabled. Rebuild with --features gateway")
}

fn is_addr_in_use_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == ErrorKind::AddrInUse)
    })
}

fn is_default_gateway_addr(host: &str, port: u16, default_host: &str, default_port: u16) -> bool {
    host == default_host && port == default_port
}

fn gateway_addr_in_use_message(
    host: &str,
    port: u16,
    default_host: &str,
    default_port: u16,
    restart_port: Option<u16>,
) -> String {
    let mut lines = vec![
        format!("Port {port} is already in use, so the gateway could not start."),
        String::new(),
        "A ZeroClaw daemon or another service may already be running on this port.".to_string(),
        "Try one of:".to_string(),
        String::new(),
    ];

    if is_default_gateway_addr(host, port, default_host, default_port) {
        lines.push(format!("    open http://{host}:{port}"));
    }

    lines.push(gateway_paircode_recovery_command(
        host,
        port,
        default_host,
        default_port,
    ));
    if let Some(restart_port) = restart_port {
        lines.push(gateway_restart_recovery_command(
            host,
            restart_port,
            default_host,
        ));
    }
    lines.extend([
        String::new(),
        "To inspect the listener:".to_string(),
        format!("    lsof -nP -iTCP:{port} -sTCP:LISTEN"),
    ]);
    lines.join("\n")
}

fn gateway_restart_recovery_command(host: &str, port: u16, default_host: &str) -> String {
    let mut command = format!("    zeroclaw gateway start --port {port}");
    if host != default_host {
        write!(command, " --host {host}").expect("writing to String cannot fail");
    }
    command
}

fn gateway_paircode_recovery_command(
    host: &str,
    port: u16,
    default_host: &str,
    default_port: u16,
) -> String {
    if host == default_host && port == default_port {
        return "    zeroclaw gateway get-paircode".to_string();
    }

    let mut command = format!("    zeroclaw gateway get-paircode --port {port}");
    if host != default_host {
        write!(command, " --host {host}").expect("writing to String cannot fail");
    }
    command
}

fn available_gateway_restart_hint_port(host: &str, port: u16) -> Option<u16> {
    const SCAN_LIMIT: u16 = 20;

    for offset in 1..=SCAN_LIMIT {
        let Some(candidate) = port.checked_add(offset) else {
            break;
        };
        if std::net::TcpListener::bind(zeroclaw_infra::effective_gateway_bind_socket_addr(
            host, candidate,
        ))
        .is_ok()
        {
            return Some(candidate);
        }
    }

    None
}

/// Persist `model` as the default for the first configured provider.
#[cfg(feature = "agent-runtime")]
async fn handle_models_set(config: &mut Config, model: &str) -> Result<()> {
    crate::config::migration::ensure_disk_at_current_version(&config.config_path)?;
    let (type_key, alias) = {
        let entry = config
            .providers
            .models
            .iter_entries()
            .find(|(_, _, entry)| entry.model.as_ref().map_or(false, |m| !m.trim().is_empty()))
            .ok_or_else(|| {
                anyhow::Error::msg(
                    "No model provider configured. Run `zeroclaw config init` first.",
                )
            })?;
        (entry.0, entry.1.to_string())
    };
    let prop_path = format!("providers.models.{type_key}.{alias}.model");
    config.set_prop_persistent(&prop_path, model)?;
    Box::pin(config.save_dirty()).await?;
    println!(
        "{}",
        crate::i18n::get_required_cli_string_with_args(
            "cli-models-set-ok",
            &[
                ("model", model),
                ("provider", &format!("{type_key}.{alias}")),
            ]
        )
    );
    Ok(())
}

#[cfg(feature = "agent-runtime")]
async fn dispatch_models_command(model_command: ModelCommands, config: &mut Config) -> Result<()> {
    match model_command {
        ModelCommands::List {
            model_provider,
            check,
        } => doctor::run_configured_models(config, model_provider.as_deref(), check).await,
        ModelCommands::Refresh { model_provider, .. } => {
            doctor::run_models(config, model_provider.as_deref(), false, false).await
        }
        ModelCommands::Set { model } => handle_models_set(config, &model).await,
        ModelCommands::Status => {
            match config
                .providers
                .models
                .iter_entries()
                .find(|(_, _, entry)| entry.model.as_ref().map_or(false, |m| !m.trim().is_empty()))
            {
                Some((ty, alias, entry)) => {
                    let model = entry.model.as_deref().unwrap_or("unknown");
                    println!(
                        "{}",
                        crate::i18n::get_required_cli_string_with_args(
                            "cli-models-status-current",
                            &[("model", model), ("provider", &format!("{ty}.{alias}")),]
                        )
                    );
                }
                None => {
                    println!(
                        "{}",
                        crate::i18n::get_required_cli_string("cli-models-status-none")
                    );
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
