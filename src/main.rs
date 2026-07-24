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
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

/// Resolve a `cli-*` Fluent key for CLI output. Routes through the runtime
/// i18n catalogue under `agent-runtime` (default + CI/release); without that
/// feature the runtime crate is absent, so the English `fallback` is used.
#[allow(unused_variables)]
fn t(key: &str, fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string(key)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

/// `t` with `{$name}` arguments.
#[allow(unused_variables)]
fn ta(key: &str, args: &[(&str, &str)], fallback: impl Into<String>) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(key, args)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.into() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

#[cfg(feature = "agent-runtime")]
fn qta(key: &str, args: &[(&str, &str)]) -> String {
    zeroclaw_runtime::i18n::get_required_cli_string_with_args(key, args)
}

#[cfg(feature = "agent-runtime")]
fn quickstart_row(key: &str, glyph: &str, summary: &str) -> String {
    qta(key, &[("glyph", glyph), ("summary", summary)])
}

#[cfg(feature = "agent-runtime")]
fn quickstart_step_label(step: zeroclaw_runtime::quickstart::QuickstartStep) -> String {
    t(step.label_key(), step.label())
}

/// Decorate the value at `path` in `config.toml` with a leading `# {comment}`
/// line, preserving any non-comment whitespace. Mirrors the gateway's
/// `apply_comments`. Best-effort — silently bails on parse errors so a
/// successful set isn't downgraded to a failure for a metadata problem.
async fn apply_comment_inline(
    config_path: &std::path::Path,
    path: &str,
    comment: &str,
) -> Result<()> {
    zeroclaw_config::comment_writer::apply_comments(
        config_path,
        &[(path.to_string(), comment.to_string())],
    )
    .await
    .context("failed to write comment annotation")
}

fn config_patch_prop_kind(config: &Config, path: &str) -> Option<crate::config::PropKind> {
    config
        .prop_fields()
        .into_iter()
        .find(|f| f.name == path)
        .map(|f| f.kind)
}

fn json_value_to_setprop_string(
    value: &serde_json::Value,
    config: &Config,
    path: &str,
) -> Result<String> {
    let kind = config_patch_prop_kind(config, path);
    zeroclaw_config::typed_value::coerce_for_set_prop(value, kind).map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"path": path, "error": e.message.clone()})),
            "config patch coercion rejected JSON value"
        );
        anyhow::Error::msg(e.message)
    })
}

fn config_patch_map_prop_error(err: anyhow::Error, path: &str, op_index: usize) -> ConfigApiError {
    let msg = err.to_string();
    if msg.starts_with("Unknown property") {
        ConfigApiError::path_not_found(path).with_op_index(op_index)
    } else {
        ConfigApiError::from_validation(err)
            .with_path(path)
            .with_op_index(op_index)
    }
}

fn config_patch_json_error(err: &ConfigApiError) -> Result<()> {
    eprintln!("{}", serde_json::to_string_pretty(err)?);
    std::process::exit(1);
}

fn config_patch_json_value_type_error(
    message: impl Into<String>,
    path: Option<String>,
    op_index: Option<usize>,
) -> ConfigApiError {
    let mut err = ConfigApiError::new(ConfigApiCode::ValueTypeMismatch, message.into());
    if let Some(path) = path {
        err = err.with_path(path);
    }
    if let Some(op_index) = op_index {
        err = err.with_op_index(op_index);
    }
    err
}

fn config_patch_fail_json_or_human<T>(
    json: bool,
    err: ConfigApiError,
    human: impl Into<String>,
) -> Result<T>
where
    T: Sized,
{
    if json {
        config_patch_json_error(&err)?;
    }
    anyhow::bail!("{}", human.into())
}

fn parse_temperature(s: &str) -> std::result::Result<f64, String> {
    let t: f64 = s
        .parse()
        .map_err(|e| format!("invalid temperature '{s}': {e}"))?;
    config::schema::validate_temperature(t)
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
mod channels;
#[cfg(feature = "agent-runtime")]
mod cli_input;
mod commands;
#[cfg(feature = "agent-runtime")]
mod rag {
    pub use zeroclaw::rag::*;
}
#[cfg(feature = "agent-runtime")]
mod browse;
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
#[cfg(feature = "agent-runtime")]
mod hardware;
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
mod peripherals;
#[cfg(feature = "agent-runtime")]
mod platform;
#[cfg(feature = "plugins-wasm")]
mod plugin_registry;
#[cfg(feature = "plugins-wasm")]
mod plugins;
mod providers;
#[cfg(feature = "agent-runtime")]
mod security;
#[cfg(feature = "agent-runtime")]
mod security_status;
#[cfg(feature = "agent-runtime")]
mod service;
#[cfg(feature = "agent-runtime")]
mod skillforge;
#[cfg(feature = "agent-runtime")]
mod skills;
#[cfg(feature = "agent-runtime")]
mod sop;
#[cfg(feature = "agent-runtime")]
mod tools;
#[cfg(feature = "agent-runtime")]
mod trust;
#[cfg(feature = "agent-runtime")]
mod tunnel;
#[cfg(feature = "agent-runtime")]
mod util;
#[cfg(feature = "agent-runtime")]
mod verifiable_intent;

use config::Config;

// Re-export so binary modules can use crate::<CommandEnum> while keeping a single source of truth.
pub use zeroclaw::{
    AgentsCommands, ChannelCommands, ChannelsCommands, CronCommands, GatewayCommands,
    HardwareCommands, IntegrationCommands, MigrateCommands, PeripheralCommands, ProvidersCommands,
    ServiceCommands, SkillBundleCommands, SkillCommands, SopCommands, SopGraphFormat,
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

/// Subcommands for `zeroclaw eval`.
#[cfg(feature = "agent-runtime")]
#[derive(Subcommand, Debug)]
enum EvalCommands {
    /// Run a suite of evaluation cases.
    Run {
        /// Directory of `*.json` trace fixtures (defaults to `evals`).
        #[arg(long)]
        suite: Option<String>,

        /// Execution mode: `replay` (deterministic) or `live` (later phase).
        /// Defaults to config `[eval] mode`.
        #[arg(long)]
        mode: Option<String>,

        /// Output format.
        #[arg(long, value_enum, default_value = "table")]
        format: commands::eval::OutputFormat,
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

Launches an interactive chat session with the configured AI model_provider. \
Use --message for single-shot queries without entering interactive mode.

Examples:
  zeroclaw agent -a assistant                                          # interactive session
  zeroclaw agent -a assistant -m \"Summarize today's logs\"              # single message
  zeroclaw agent -a assistant -p anthropic --model claude-sonnet-4-20250514
  zeroclaw agent -a assistant --peripheral nucleo-f401re:/dev/ttyACM0")]
    Agent {
        /// Configured agent alias to run as (must match `[agents.<alias>]`).
        /// Required — there is no default agent.
        #[arg(short = 'a', long)]
        agent: String,

        /// Single message mode (don't enter interactive mode)
        #[arg(short, long)]
        message: Option<String>,

        /// Load and save interactive session state in this JSON file
        #[arg(long)]
        session_state_file: Option<PathBuf>,

        /// Model provider to use (openrouter, anthropic, openai, openai-codex)
        #[arg(short = 'p', long = "model-provider", alias = "provider")]
        model_provider: Option<String>,

        /// Model to use
        #[arg(long)]
        model: Option<String>,

        /// Temperature (0.0 - 2.0, defaults to `providers.models.<type>.<alias>.temperature`)
        #[arg(short, long, value_parser = parse_temperature)]
        temperature: Option<f64>,

        /// Attach a peripheral (board:path, e.g. nucleo-f401re:/dev/ttyACM0)
        #[arg(long)]
        peripheral: Vec<String>,
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

        /// Self-terminate after all socket clients disconnect (with grace period)
        #[arg(long)]
        ephemeral: bool,

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

    /// Manage channels (telegram, discord, slack)
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Manage communication channels.

Add, remove, list, send, and health-check channels that connect ZeroClaw \
to messaging platforms. Supported channel types: telegram, discord, \
slack, whatsapp, matrix, imessage, email.

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

    /// Manage standard operating procedures (SOPs)
    Sop {
        #[command(subcommand)]
        sop_command: SopCommands,
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

    /// Discover and introspect USB hardware
    // i18n-exempt: clap derive help — framework requires a compile-time literal
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
    // i18n-exempt: clap derive help — framework requires a compile-time literal
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
  zeroclaw config schema                                # print JSON Schema to stdout
  zeroclaw config schema > schema.json

Property path tab completion is included automatically in `zeroclaw completions <shell>`.")]
    Config {
        #[command(subcommand)]
        config_command: ConfigCommands,
    },

    /// Check for and apply updates
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Check for and apply ZeroClaw updates.

By default, downloads and installs the latest release with a \
6-phase pipeline: preflight, download, backup, validate, swap, \
and smoke test. Automatic rollback on failure.

Use --check to only check for updates without installing.
Use --force to skip the confirmation prompt.
Use --version to target a specific release instead of latest.

Examples:
  zeroclaw update                      # download and install latest
  zeroclaw update --check              # check only, don't install
  zeroclaw update --force              # install without confirmation
  zeroclaw update --version 0.6.0      # install specific version")]
    Update {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,
        /// Install even if the target is not newer (reinstall or downgrade/pin to --version)
        #[arg(long)]
        force: bool,
        /// Target version (default: latest)
        #[arg(long)]
        version: Option<String>,
        /// With --check, emit machine-readable JSON instead of human text
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
    /// Run the agent evaluation harness
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Run the agent evaluation harness.

Phase 0 supports deterministic replay: every `*.json` trace fixture in the suite \
directory is replayed through the real agent loop and graded against its declarative \
expectations. No network calls, fully deterministic. Exits non-zero if any case fails, \
so it can gate CI.

Examples:
  zeroclaw eval run                                  # replay ./evals
  zeroclaw eval run --suite evals --format json")]
    Eval {
        #[command(subcommand)]
        eval_command: EvalCommands,
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

    /// Launch or install the companion desktop app
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Launch the ZeroClaw companion desktop app.

The companion app is a lightweight menu bar / system tray application \
that connects to the same gateway as the CLI. It provides quick access \
to the dashboard, status monitoring, and device pairing.

Use --install to download the pre-built companion app for your platform.

Examples:
  zeroclaw desktop              # launch the companion app
  zeroclaw desktop --install    # download and install it")]
    Desktop {
        /// Download and install the companion app
        #[arg(long)]
        install: bool,
    },

    /// Deprecated: use `zeroclaw config` instead
    #[command(hide = true)]
    Props {
        #[command(subcommand)]
        props_command: DeprecatedPropsCommands,
    },

    /// Manage WASM plugins
    #[cfg(feature = "plugins-wasm")]
    Plugin {
        #[command(subcommand)]
        plugin_command: PluginCommands,
    },

    /// Fetch translated locale files (FTL) from upstream
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    #[command(long_about = "\
Fetch translated Fluent (.ftl) catalogues for a locale from the upstream \
repository and install them under `<config-dir>/data/ftl/<locale>/`, where the \
runtime and zerocode loaders read them.

Pass a single locale. By default every catalogue is fetched; restrict with \
--catalog (comma-separated): cli, tools, zerocode.

Examples:
  zeroclaw locales fetch ja
  zeroclaw locales fetch fr --catalog cli,tools
  zeroclaw locales fetch zh-CN --catalog zerocode")]
    Locales {
        #[command(subcommand)]
        locales_command: LocalesCommands,
    },
}

#[derive(Subcommand, Debug)]
enum LocalesCommands {
    // i18n-exempt: clap derive help — framework requires a compile-time literal
    /// Download translated FTL files for a locale from upstream
    Fetch {
        /// Locale code to fetch (e.g. `ja`, `fr`, `zh-CN`).
        locale: String,
        /// Comma-separated catalogues to fetch: cli, tools, zerocode.
        /// Omit to fetch all of them.
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

#[cfg(feature = "agent-runtime")]
fn quickstart_runtime_profile_for_provider(
    provider_type: &str,
    providers: &[zeroclaw_runtime::quickstart::QuickstartTypeOption],
    default_runtime_profile: &str,
) -> String {
    providers
        .iter()
        .find(|provider| provider.kind == provider_type)
        .and_then(|provider| provider.default_runtime_profile.as_deref())
        .unwrap_or(default_runtime_profile)
        .to_string()
}

/// `zeroclaw quickstart` CLI entry — checklist UX, not a wizard.
///
/// Mirrors the TUI Quickstart pane's structure: a single screen
/// listing all six selectors with `[ ]` / `[✓]` status and a one-line
/// summary, the user picks which selector to fill (any order), each
/// selector opens its own picker / field-form / channel-list sub-flow,
/// and `c` creates the agent once every selector is `[✓]`. There are
/// no pre-checked defaults anywhere — every selector starts `[ ]` and
/// is only satisfied by an explicit user choice (either a "Use
/// existing" pick of an already-configured alias, or a fully-filled
/// "Create new" entry).
///
/// All option lists, field shapes, presets, and the apply path come
/// directly from `zeroclaw_runtime::quickstart` — the same module the
/// gateway and TUI surfaces consume. No RPC, no daemon: the CLI is
/// compiled in-process with `zeroclaw-runtime` and calls
/// `snapshot_state` / `field_shape` / `apply_with_surface` as plain
/// functions.
///
/// Flag pre-fills (`--model-provider`, `--model`, `--api-key`,
/// `--agent`) silently seed the relevant selector's value and mark it
/// `[✓]` if the seed is enough to satisfy the selector; the user can
/// still open that selector and overwrite it.
#[cfg(feature = "agent-runtime")]
async fn run_quickstart_cli(
    model_provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Editor, FuzzySelect, Input, Password};
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
        RISK_PRESETS, SelectorChoice,
    };
    use zeroclaw_runtime::quickstart::{
        FieldSection, QuickstartTypeOption, Surface, apply_with_surface, field_shape,
        snapshot_state,
    };

    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stderr())
    {
        anyhow::bail!(
            "{}",
            t(
                "cli-quickstart-needs-tty",
                "Quickstart is interactive and needs a terminal on stdin and stderr. \
                 Run it from an interactive shell, or use \
                 `zeroclaw config set <path> <value>` for headless configuration."
            )
        );
    }

    #[derive(Default)]
    struct Form {
        provider: Option<ProviderChoice>,
        risk: Option<PresetChoice>,
        memory: Option<MemoryChoice>,
        channels: Vec<ChannelChoice>,
        // Tracks whether the user explicitly visited Channels and
        // confirmed "no channels". An empty `channels` Vec with
        // `channels_visited == false` is *not* satisfied — the
        // selector still shows `[ ]`.
        channels_visited: bool,
        peer_groups: Vec<zeroclaw_config::presets::QuickstartPeerGroup>,
        // Mirrors `channels_visited`: peer groups are optional, so an
        // empty `peer_groups` Vec only counts as satisfied once the
        // user has actually opened the selector and left it. Until
        // then the row stays `[ ]` rather than a pre-checked default.
        peer_groups_visited: bool,
        agent: Option<AgentChoice>,
    }
    enum ProviderChoice {
        Fresh {
            kind: String,
            display_name: String,
            alias: String,
            model: String,
            /// Round-trip of every non-`model` descriptor value the
            /// daemon's `field_shape()` emitted, keyed by descriptor
            /// key. The CLI doesn't know what these mean — the daemon
            /// authored them and consumes them on the way back.
            fields: std::collections::HashMap<String, String>,
        },
        Existing {
            alias_ref: String,
        },
    }
    enum PresetChoice {
        Fresh(&'static str),
        Existing(String),
    }
    enum ChannelChoice {
        Fresh {
            kind: String,
            display_name: String,
            alias: String,
            extras: std::collections::BTreeMap<String, String>,
        },
        Existing {
            alias_ref: String,
        },
    }
    struct AgentChoice {
        name: String,
        system_prompt: String,
        personality_files: Vec<zeroclaw_config::presets::QuickstartPersonalityFile>,
    }

    impl Form {
        fn provider_done(&self) -> bool {
            self.provider.is_some()
        }
        fn risk_done(&self) -> bool {
            self.risk.is_some()
        }
        fn memory_done(&self) -> bool {
            self.memory.is_some()
        }
        fn channels_done(&self) -> bool {
            self.channels_visited
        }
        fn peer_groups_done(&self) -> bool {
            self.peer_groups_visited
        }
        fn agent_done(&self) -> bool {
            self.agent
                .as_ref()
                .is_some_and(|a| !a.name.trim().is_empty())
        }
        fn all_done(&self) -> bool {
            self.provider_done()
                && self.risk_done()
                && self.memory_done()
                && self.channels_done()
                && self.agent_done()
        }
    }

    // ── Load config + canonical registries ──────────────────────
    let _dirs = crate::config::schema::resolve_runtime_dirs().await?;
    let mut cfg = Box::pin(crate::config::schema::Config::load_or_init()).await?;
    let state = snapshot_state(&cfg);
    let providers: &[QuickstartTypeOption] = &state.model_provider_types;
    let channel_types: &[QuickstartTypeOption] = &state.channel_types;
    if providers.is_empty() {
        anyhow::bail!(
            "Quickstart could not enumerate model providers — \
             zeroclaw_providers::list_model_providers() returned no entries."
        );
    }

    let mut form = Form::default();

    if let (Some(mp), Some(m)) = (model_provider.as_deref(), model.as_deref())
        && let Some((canonical_provider, codex_auth)) =
            zeroclaw_runtime::quickstart::resolve_model_provider_type(mp)
        && let Some(found) = providers
            .iter()
            .find(|p| p.kind.eq_ignore_ascii_case(canonical_provider))
    {
        let needs_key = !found.local && api_key.is_none() && !codex_auth;
        if !needs_key {
            let mut fields: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if codex_auth {
                fields.insert("auth_mode".to_string(), "codex".to_string());
            }
            if let Some(key) = api_key.as_deref().filter(|s| !s.is_empty()) {
                // Submission field keys are snake_case (`api_key`) — the apply
                // path round-trips them verbatim into `set_prop_persistent`,
                // which rejects kebab-case with "Unknown property".
                fields.insert("api_key".to_string(), key.to_string());
            }
            form.provider = Some(ProviderChoice::Fresh {
                kind: found.kind.clone(),
                display_name: found.display_name.clone(),
                alias: "default".to_string(),
                model: m.to_string(),
                fields,
            });
        }
    }
    if let Some(a) = agent.as_deref() {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            form.agent = Some(AgentChoice {
                name: trimmed.to_string(),
                system_prompt: String::new(),
                personality_files: Vec::new(),
            });
        }
    }

    // ── Main checklist loop ─────────────────────────────────────
    #[derive(Clone, Copy)]
    enum Action {
        Provider,
        Risk,
        Memory,
        Channels,
        PeerGroups,
        Agent,
        Create,
        Quit,
    }

    println!();
    println!(
        "{}",
        t(
            "cli-quickstart-title",
            "Quickstart — create one working agent end-to-end."
        )
    );
    println!();

    loop {
        // Render selector list with current status / summary.
        let glyph = |ok: bool| if ok { "[✓]" } else { "[ ]" };
        let provider_summary = match &form.provider {
            None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
            Some(ProviderChoice::Fresh {
                display_name,
                alias,
                model,
                ..
            }) => qta(
                "cli-quickstart-summary-provider-fresh",
                &[("name", display_name), ("alias", alias), ("model", model)],
            ),
            Some(ProviderChoice::Existing { alias_ref }) => qta(
                "cli-quickstart-summary-use-existing",
                &[("reference", alias_ref)],
            ),
        };
        let preset_summary = |p: &Option<PresetChoice>| -> String {
            match p {
                None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
                Some(PresetChoice::Fresh(name)) => {
                    qta("cli-quickstart-summary-preset-fresh", &[("name", name)])
                }
                Some(PresetChoice::Existing(a)) => {
                    qta("cli-quickstart-summary-use-existing", &[("reference", a)])
                }
            }
        };
        let memory_summary = match &form.memory {
            None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
            Some(kind) => serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase()),
        };
        let channels_summary = if !form.channels_visited {
            t("cli-quickstart-summary-not-yet-visited", "not yet visited")
        } else if form.channels.is_empty() {
            t(
                "cli-quickstart-summary-channels-none",
                "none (chat via `zeroclaw agent` only)",
            )
        } else {
            form.channels
                .iter()
                .map(|c| match c {
                    ChannelChoice::Fresh { kind, alias, .. } => format!("{kind}.{alias}"),
                    ChannelChoice::Existing { alias_ref } => alias_ref.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let agent_summary = match &form.agent {
            None => t("cli-quickstart-summary-not-yet-named", "not yet named"),
            Some(a) => qta(
                "cli-quickstart-summary-agent",
                &[
                    ("alias", &a.name),
                    ("chars", &a.system_prompt.len().to_string()),
                    ("files", &a.personality_files.len().to_string()),
                ],
            ),
        };
        let peer_groups_summary = if form.peer_groups.is_empty() {
            t(
                "cli-quickstart-summary-peer-groups-none",
                "none — channels accept no peers",
            )
        } else {
            form.peer_groups
                .iter()
                .map(|pg| format!("{} → {}", pg.channel, pg.name))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let risk_summary = preset_summary(&form.risk);
        let mut labels: Vec<String> = vec![
            quickstart_row(
                "cli-quickstart-row-model-provider",
                glyph(form.provider_done()),
                &provider_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-risk-profile",
                glyph(form.risk_done()),
                &risk_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-memory",
                glyph(form.memory_done()),
                &memory_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-channels",
                glyph(form.channels_done()),
                &channels_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-peer-groups",
                glyph(form.peer_groups_done()),
                &peer_groups_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-agent-identity",
                glyph(form.agent_done()),
                &agent_summary,
            ),
        ];
        let create_enabled = form.all_done();
        labels.push(if create_enabled {
            t("cli-quickstart-create-agent", "── Create agent")
        } else {
            t(
                "cli-quickstart-create-agent-locked",
                "── Create agent (locked — fill every selector first)",
            )
        });

        let actions = [
            Action::Provider,
            Action::Risk,
            Action::Memory,
            Action::Channels,
            Action::PeerGroups,
            Action::Agent,
            Action::Create,
        ];

        let pick = FuzzySelect::new()
            .with_prompt(t(
                "cli-quickstart-open-selector-prompt",
                "Open a selector (Enter), or pick Create. Esc to quit.",
            ))
            .items(&labels)
            .default(0)
            .max_length(labels.len())
            .interact_opt()?;
        let action = match pick {
            Some(i) => actions[i],
            None => Action::Quit, // Esc on the main checklist quits.
        };

        match action {
            Action::Quit => {
                println!(
                    "{}",
                    t(
                        "cli-quickstart-cancelled",
                        "Quickstart cancelled. No config written."
                    )
                );
                return Ok(());
            }
            Action::Create => {
                if !create_enabled {
                    println!(
                        "{}",
                        t(
                            "cli-quickstart-incomplete",
                            "  Not all selectors are filled yet."
                        )
                    );
                    continue;
                }
                break;
            }
            Action::Provider => {
                // Step 1: pick Existing or Fresh, when there are
                // existing providers to choose from.
                let mut mode_labels: Vec<String> = Vec::new();
                let mut mode_kinds: Vec<&str> = Vec::new();
                if !state.model_providers.is_empty() {
                    mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
                    mode_kinds.push("existing");
                }
                mode_labels.push(t("cli-quickstart-create-new", "Create new"));
                mode_kinds.push("fresh");
                let mode = if mode_labels.len() == 1 {
                    Some(0)
                } else {
                    FuzzySelect::new()
                        .with_prompt(t("cli-quickstart-model-provider-prompt", "Model provider"))
                        .items(&mode_labels)
                        .default(0)
                        .max_length(mode_labels.len())
                        .interact_opt()?
                };
                let Some(mi) = mode else { continue };
                if mode_kinds[mi] == "existing" {
                    let labels: Vec<String> = state.model_providers.clone();
                    let Some(i) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-pick-configured-provider",
                            "Pick a configured provider",
                        ))
                        .items(&labels)
                        .default(0)
                        .max_length(labels.len().max(1))
                        .interact_opt()?
                    else {
                        continue;
                    };
                    form.provider = Some(ProviderChoice::Existing {
                        alias_ref: labels[i].clone(),
                    });
                    continue;
                }
                // Fresh: type → alias → field form.
                let prov_labels: Vec<String> = providers
                    .iter()
                    .map(|p| {
                        if p.local {
                            qta(
                                "cli-quickstart-provider-local-label",
                                &[("name", &p.display_name)],
                            )
                        } else {
                            p.display_name.clone()
                        }
                    })
                    .collect();
                let Some(pi) = FuzzySelect::new()
                    .with_prompt(t("cli-quickstart-provider-type-prompt", "Provider type"))
                    .items(&prov_labels)
                    .default(0)
                    .max_length(prov_labels.len().max(1))
                    .interact_opt()?
                else {
                    continue;
                };
                let chosen = &providers[pi];
                let Ok(alias) = Input::<String>::new()
                    .with_prompt(qta(
                        "cli-quickstart-alias-for",
                        &[("name", &chosen.display_name)],
                    ))
                    .default("default".to_string())
                    .allow_empty(false)
                    .validate_with(|input: &String| {
                        zeroclaw_config::helpers::validate_alias_key(input)
                    })
                    .interact_text()
                else {
                    continue;
                };
                // Field shape from the canonical schema.
                let descriptors = field_shape(FieldSection::ModelProvider, &chosen.kind);
                let mut model = String::new();
                let mut field_buf: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                let mut aborted = false;
                for d in &descriptors {
                    if d.key == "api_key" {
                        let skips_api_key =
                            quickstart_field_value_eq(&field_buf, "auth_mode", "codex")
                                || (chosen.kind == "anthropic"
                                    && quickstart_field_value_eq(
                                        &field_buf,
                                        "auth_mode",
                                        "setup_token",
                                    ));
                        if skips_api_key {
                            continue;
                        }
                    }
                    // For the model field, upgrade the descriptor with a
                    // live catalog so `prompt_for_field` renders a picker
                    // instead of a free-text input. Empty catalog (live=false)
                    // leaves the descriptor unchanged → free-text fallback.
                    let upgraded;
                    let d_used = if d.key.eq_ignore_ascii_case("model") {
                        let (models, _pricing, live) =
                            zeroclaw_runtime::quickstart::model_catalog(&chosen.kind).await;
                        if live && !models.is_empty() {
                            upgraded = zeroclaw_runtime::quickstart::FieldDescriptor {
                                kind: zeroclaw_config::traits::PropKind::Enum,
                                enum_variants: Some(models),
                                ..d.clone()
                            };
                            &upgraded
                        } else {
                            d
                        }
                    } else {
                        d
                    };
                    let collected = prompt_for_field(d_used, None)?;
                    let Some(value) = collected else {
                        aborted = true;
                        break;
                    };
                    // `model` is hoisted to a top-level field on
                    // ProviderChoice for the summary line. Every other
                    // descriptor flows through `field_buf` keyed by
                    // its schema identifier — no cherry-picking.
                    if d.key.eq_ignore_ascii_case("model") {
                        model = value;
                    } else if !value.is_empty() && value != zeroclaw_config::traits::UNSET_DISPLAY {
                        field_buf.insert(d.key.clone(), value);
                    }
                }
                if aborted {
                    continue;
                }
                if model.is_empty() {
                    eprintln!(
                        "{}",
                        qta(
                            "cli-quickstart-model-field-missing-warning",
                            &[("provider", &chosen.kind)],
                        )
                    );
                    let Ok(m) = Input::<String>::new()
                        .with_prompt(qta(
                            "cli-quickstart-model-id-for",
                            &[("name", &chosen.display_name)],
                        ))
                        .allow_empty(false)
                        .interact_text()
                    else {
                        continue;
                    };
                    model = m;
                }
                form.provider = Some(ProviderChoice::Fresh {
                    kind: chosen.kind.clone(),
                    display_name: chosen.display_name.clone(),
                    alias,
                    model,
                    fields: field_buf,
                });
            }
            Action::Risk => {
                let chosen = pick_preset(
                    &t("cli-quickstart-risk-profile-prompt", "Risk profile"),
                    RISK_PRESETS
                        .iter()
                        .map(|p| (p.preset_name, p.label, p.help))
                        .collect(),
                    &state.risk_profiles,
                )?;
                if let Some(c) = chosen {
                    form.risk = Some(match c {
                        Ok(name) => PresetChoice::Fresh(name),
                        Err(alias) => PresetChoice::Existing(alias),
                    });
                }
            }
            Action::Memory => {
                let kinds: [MemoryChoice; 6] = [
                    MemoryChoice::Sqlite,
                    MemoryChoice::Markdown,
                    MemoryChoice::Postgres,
                    MemoryChoice::Qdrant,
                    MemoryChoice::Lucid,
                    MemoryChoice::None,
                ];
                #[allow(clippy::no_effect_underscore_binding)]
                let _exhaustive = |k: MemoryChoice| match k {
                    MemoryChoice::Sqlite
                    | MemoryChoice::Markdown
                    | MemoryChoice::Postgres
                    | MemoryChoice::Qdrant
                    | MemoryChoice::Lucid
                    | MemoryChoice::None => (),
                };
                let labels: Vec<String> = kinds
                    .iter()
                    .map(|k| {
                        serde_json::to_value(k)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| format!("{k:?}").to_lowercase())
                    })
                    .collect();
                let Some(i) = FuzzySelect::new()
                    .with_prompt(t("cli-quickstart-memory-backend-prompt", "Memory backend"))
                    .items(&labels)
                    .default(0)
                    .max_length(labels.len().max(1))
                    .interact_opt()?
                else {
                    continue;
                };
                form.memory = Some(kinds[i]);
            }
            Action::Channels => {
                // Channels sub-flow: list current drafts + Add / Done.
                loop {
                    let mut items: Vec<String> = form
                        .channels
                        .iter()
                        .map(|c| match c {
                            ChannelChoice::Fresh { kind, alias, .. } => qta(
                                "cli-quickstart-channel-remove-row",
                                &[("reference", &format!("{kind}.{alias}"))],
                            ),
                            ChannelChoice::Existing { alias_ref } => qta(
                                "cli-quickstart-channel-remove-row",
                                &[("reference", alias_ref)],
                            ),
                        })
                        .collect();
                    items.push(t("cli-quickstart-add-channel", "+ Add a channel"));
                    items.push(t(
                        "cli-quickstart-channels-done",
                        "Done (channels selector counts as visited)",
                    ));
                    let Some(i) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-channels-prompt",
                            "Channels (optional, 0..N)",
                        ))
                        .items(&items)
                        .default(items.len().saturating_sub(2))
                        .max_length(items.len())
                        .interact_opt()?
                    else {
                        break;
                    };
                    if i < form.channels.len() {
                        form.channels.remove(i);
                        continue;
                    }
                    if i == form.channels.len() {
                        // Add — pick Existing or Fresh.
                        let mut mode_labels: Vec<String> = Vec::new();
                        let mut mode_kinds: Vec<&str> = Vec::new();
                        if !state.unassigned_channels.is_empty() {
                            mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
                            mode_kinds.push("existing");
                        }
                        mode_labels.push(t("cli-quickstart-create-new", "Create new"));
                        mode_kinds.push("fresh");
                        let mode = if mode_labels.len() == 1 {
                            Some(0)
                        } else {
                            FuzzySelect::new()
                                .with_prompt(t(
                                    "cli-quickstart-channel-source-prompt",
                                    "Channel source",
                                ))
                                .items(&mode_labels)
                                .default(0)
                                .max_length(mode_labels.len())
                                .interact_opt()?
                        };
                        let Some(mi) = mode else { continue };
                        if mode_kinds[mi] == "existing" {
                            let labels: Vec<String> = state.unassigned_channels.clone();
                            if labels.is_empty() {
                                println!(
                                    "{}",
                                    t(
                                        "cli-quickstart-all-channels-bound",
                                        "  Every configured channel is already bound to an agent. Free one with `zeroclaw config set agents.<alias>.channels ...` before reusing it here.",
                                    )
                                );
                                continue;
                            }
                            let Some(ei) = FuzzySelect::new()
                                .with_prompt(t(
                                    "cli-quickstart-pick-configured-channel",
                                    "Pick a configured channel",
                                ))
                                .items(&labels)
                                .default(0)
                                .max_length(labels.len().max(1))
                                .interact_opt()?
                            else {
                                continue;
                            };
                            form.channels.push(ChannelChoice::Existing {
                                alias_ref: labels[ei].clone(),
                            });
                            continue;
                        }
                        if channel_types.is_empty() {
                            println!(
                                "{}",
                                t(
                                    "cli-no-channels-compiled",
                                    "  No channel types are compiled into this binary."
                                )
                            );
                            continue;
                        }
                        let labels: Vec<String> = channel_types
                            .iter()
                            .map(|c| c.display_name.clone())
                            .collect();
                        let Some(ci) = FuzzySelect::new()
                            .with_prompt(t("cli-quickstart-channel-type-prompt", "Channel type"))
                            .items(&labels)
                            .default(0)
                            .max_length(labels.len().max(1))
                            .interact_opt()?
                        else {
                            continue;
                        };
                        let chosen = &channel_types[ci];
                        let Ok(alias) = Input::<String>::new()
                            .with_prompt(qta(
                                "cli-quickstart-alias-for",
                                &[("name", &chosen.display_name)],
                            ))
                            .default(chosen.kind.clone())
                            .allow_empty(false)
                            .interact_text()
                        else {
                            continue;
                        };
                        let descriptors = field_shape(FieldSection::Channel, &chosen.kind);
                        let mut extras: std::collections::BTreeMap<String, String> =
                            std::collections::BTreeMap::new();
                        let mut aborted = false;
                        for d in &descriptors {
                            let Some(value) = prompt_for_field(d, None)? else {
                                aborted = true;
                                break;
                            };
                            if !value.is_empty() && value != zeroclaw_config::traits::UNSET_DISPLAY
                            {
                                extras.insert(d.key.clone(), value);
                            }
                        }
                        if aborted {
                            continue;
                        }
                        form.channels.push(ChannelChoice::Fresh {
                            kind: chosen.kind.clone(),
                            display_name: chosen.display_name.clone(),
                            alias,
                            extras,
                        });
                        continue;
                    }
                    // Done.
                    form.channels_visited = true;
                    break;
                }
            }
            Action::PeerGroups => {
                // Available channel refs: staged channels (this run) +
                // unassigned channels already in config. Refs already
                // covered by a staged peer-group are filtered out.
                let staged_refs: Vec<String> = form
                    .channels
                    .iter()
                    .map(|c| match c {
                        ChannelChoice::Fresh { kind, alias, .. } => format!("{kind}.{alias}"),
                        ChannelChoice::Existing { alias_ref } => alias_ref.clone(),
                    })
                    .collect();
                let claimed: std::collections::HashSet<String> = form
                    .peer_groups
                    .iter()
                    .map(|pg| pg.channel.clone())
                    .collect();
                let mut available: Vec<String> = staged_refs
                    .iter()
                    .chain(state.unassigned_channels.iter())
                    .filter(|r| !claimed.contains(r.as_str()))
                    .cloned()
                    .collect();
                available.dedup();
                loop {
                    let mut items: Vec<String> = form
                        .peer_groups
                        .iter()
                        .map(|pg| {
                            qta(
                                "cli-quickstart-peer-group-row",
                                &[
                                    ("channel", &pg.channel),
                                    ("name", &pg.name),
                                    ("count", &pg.external_peers.len().to_string()),
                                ],
                            )
                        })
                        .collect();
                    let drafts = items.len();
                    if !available.is_empty() {
                        items.push(t("cli-quickstart-add-peer-group", "+ Add peer group"));
                    }
                    items.push(t("cli-quickstart-done", "Done"));
                    let Some(pick) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-peer-groups-prompt",
                            "Peer groups (Enter on a row to remove, + Add to create)",
                        ))
                        .items(&items)
                        .default(items.len() - 1)
                        .max_length(items.len())
                        .interact_opt()?
                    else {
                        break;
                    };
                    if pick < drafts {
                        form.peer_groups.remove(pick);
                        continue;
                    }
                    if pick == drafts && !available.is_empty() {
                        let Some(ch_idx) = FuzzySelect::new()
                            .with_prompt(t(
                                "cli-quickstart-channel-to-authorize-prompt",
                                "Channel to authorize",
                            ))
                            .items(&available)
                            .default(0)
                            .max_length(available.len())
                            .interact_opt()?
                        else {
                            continue;
                        };
                        let channel = available[ch_idx].clone();
                        let (ch_type, ch_alias) = match channel.split_once('.') {
                            Some(parts) => parts,
                            None => continue,
                        };
                        let name = format!("{ch_type}_{ch_alias}_default");
                        let Ok(peers_raw) = Input::<String>::new()
                            .with_prompt(t(
                                "cli-quickstart-external-peers-prompt",
                                "External peers (comma- or newline-separated, blank for none)",
                            ))
                            .allow_empty(true)
                            .interact_text()
                        else {
                            continue;
                        };
                        let external_peers: Vec<String> = peers_raw
                            .split([',', '\n'])
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        form.peer_groups
                            .push(zeroclaw_config::presets::QuickstartPeerGroup {
                                name,
                                channel,
                                external_peers,
                                ignore: Vec::new(),
                            });
                        // The channel just got claimed; refresh the available list.
                        available = staged_refs
                            .iter()
                            .chain(state.unassigned_channels.iter())
                            .filter(|r| !form.peer_groups.iter().any(|pg| &pg.channel == *r))
                            .cloned()
                            .collect();
                        available.dedup();
                        continue;
                    }
                    // Done.
                    form.peer_groups_visited = true;
                    break;
                }
            }
            Action::Agent => {
                let default_name = form
                    .agent
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let mut input = Input::<String>::new()
                    .with_prompt(t("cli-quickstart-agent-alias-prompt", "Agent alias"))
                    .allow_empty(false)
                    .validate_with(|input: &String| {
                        zeroclaw_config::helpers::validate_alias_key(input)
                    });
                if !default_name.is_empty() {
                    input = input.default(default_name);
                }
                let Ok(name) = input.interact_text() else {
                    continue;
                };
                let mut system_prompt = form
                    .agent
                    .as_ref()
                    .map(|a| a.system_prompt.clone())
                    .unwrap_or_default();
                let edit = Confirm::new()
                    .with_prompt(t(
                        "cli-quickstart-edit-system-prompt",
                        "Edit system prompt in $EDITOR? (blank if you skip)",
                    ))
                    .default(false)
                    .interact_opt()?;
                if let Some(true) = edit
                    && let Some(edited) = Editor::new().edit(&system_prompt)?
                {
                    system_prompt = edited;
                }
                // Personality files. The canonical list comes from the
                // snapshot — no hardcoded filenames. Pre-seed buffers
                // from any previously-staged content so re-entering
                // Agent doesn't drop the user's edits.
                let prior_files: std::collections::HashMap<String, String> = form
                    .agent
                    .as_ref()
                    .map(|a| {
                        a.personality_files
                            .iter()
                            .map(|f| (f.filename.clone(), f.content.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                // Pre-render the default template set once; the per-file
                // [t] Use template option seeds the editor from this map.
                let template_ctx =
                    zeroclaw_runtime::agent::personality_templates::TemplateContext {
                        agent: trimmed_agent_name_for_templates(
                            form.agent.as_ref().map(|a| a.name.as_str()),
                        ),
                        ..Default::default()
                    };
                let templates: std::collections::HashMap<String, String> =
                    zeroclaw_runtime::agent::personality_templates::render_preset_default(
                        &template_ctx,
                    )
                    .into_iter()
                    .map(|(filename, content)| (filename.to_string(), content))
                    .collect();
                let mut personality_results: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();

                #[derive(Clone, Copy)]
                enum PersonalityAction {
                    StartWithTemplate,
                    StartFromScratch,
                    Skip,
                }
                impl PersonalityAction {
                    fn label(self, has_staged: bool) -> String {
                        match self {
                            Self::StartWithTemplate => t(
                                "cli-quickstart-personality-start-template",
                                "Start with template (open in $EDITOR)",
                            ),
                            Self::StartFromScratch => {
                                if has_staged {
                                    t(
                                        "cli-quickstart-personality-start-current",
                                        "Start from current content (open in $EDITOR)",
                                    )
                                } else {
                                    t(
                                        "cli-quickstart-personality-start-scratch",
                                        "Start from scratch (open in $EDITOR)",
                                    )
                                }
                            }
                            Self::Skip => t("cli-quickstart-personality-skip", "Skip"),
                        }
                    }
                }

                let files = state.personality_files;
                let mut idx: usize = 0;
                let mut back_to_checklist = false;
                while idx < files.len() {
                    let filename = files[idx];
                    // Prefer a decision made earlier in this loop (e.g. after
                    // stepping back), else fall back to any pre-staged content.
                    let staged = personality_results
                        .get(filename)
                        .or_else(|| prior_files.get(filename))
                        .cloned()
                        .unwrap_or_default();
                    let template_available = templates.contains_key(filename);

                    let mut actions: Vec<PersonalityAction> = Vec::with_capacity(3);
                    if template_available {
                        actions.push(PersonalityAction::StartWithTemplate);
                    }
                    actions.push(PersonalityAction::StartFromScratch);
                    actions.push(PersonalityAction::Skip);
                    let has_staged = !staged.is_empty();
                    let choices: Vec<String> =
                        actions.iter().map(|a| a.label(has_staged)).collect();
                    let position = if files.len() > 1 {
                        format!(" [{}/{}]", idx + 1, files.len())
                    } else {
                        String::new()
                    };
                    let back_hint = if idx > 0 {
                        t("cli-quickstart-esc-go-back", " (Esc to go back)")
                    } else {
                        t(
                            "cli-quickstart-esc-return-checklist",
                            " (Esc to return to checklist)",
                        )
                    };
                    let label = qta(
                        "cli-quickstart-personality-file-prompt",
                        &[
                            ("filename", filename),
                            ("position", &position),
                            ("back_hint", &back_hint),
                        ],
                    );
                    let Some(pick) = FuzzySelect::new()
                        .with_prompt(label)
                        .items(&choices)
                        .default(0)
                        .max_length(choices.len())
                        .interact_opt()?
                    else {
                        // Esc steps back one file in the stack. On the first
                        // file there's nowhere earlier to go, so it returns to
                        // the base checklist.
                        if idx == 0 {
                            back_to_checklist = true;
                            break;
                        }
                        idx -= 1;
                        continue;
                    };
                    match actions[pick] {
                        PersonalityAction::StartWithTemplate => {
                            let seed = templates
                                .get(filename)
                                .cloned()
                                .unwrap_or_else(|| staged.clone());
                            if let Some(edited) = Editor::new().edit(&seed)?
                                && !edited.trim().is_empty()
                            {
                                personality_results.insert(filename.to_string(), edited);
                            }
                        }
                        PersonalityAction::StartFromScratch => {
                            if let Some(edited) = Editor::new().edit(&staged)?
                                && !edited.trim().is_empty()
                            {
                                personality_results.insert(filename.to_string(), edited);
                            }
                        }
                        PersonalityAction::Skip => {
                            // Keep any previously-staged content rather than
                            // dropping it silently.
                            if has_staged {
                                personality_results.insert(filename.to_string(), staged);
                            }
                        }
                    }
                    idx += 1;
                }
                if back_to_checklist {
                    continue;
                }
                // Materialize in canonical file order; only files with content.
                let personality_files: Vec<zeroclaw_config::presets::QuickstartPersonalityFile> =
                    files
                        .iter()
                        .filter_map(|filename| {
                            personality_results.get(*filename).map(|content| {
                                zeroclaw_config::presets::QuickstartPersonalityFile {
                                    filename: (*filename).to_string(),
                                    content: content.clone(),
                                }
                            })
                        })
                        .collect();
                form.agent = Some(AgentChoice {
                    name,
                    system_prompt,
                    personality_files,
                });
            }
        }
    }

    // ── Assemble submission ─────────────────────────────────────
    let inline_auth = match form.provider.as_ref() {
        Some(ProviderChoice::Fresh {
            kind,
            alias,
            fields,
            ..
        }) => quickstart_inline_auth(kind, alias, fields),
        _ => None,
    };

    let provider = form.provider.expect("provider satisfied");
    let provider_type = match &provider {
        ProviderChoice::Fresh { kind, .. } => kind.as_str(),
        ProviderChoice::Existing { alias_ref } => alias_ref
            .split_once('.')
            .map(|(provider_type, _)| provider_type)
            .unwrap_or(alias_ref),
    };
    let runtime_profile = SelectorChoice::Fresh(quickstart_runtime_profile_for_provider(
        provider_type,
        providers,
        &state.default_runtime_profile,
    ));
    let model_provider = match provider {
        ProviderChoice::Fresh {
            kind,
            alias,
            model,
            fields,
            ..
        } => SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: kind,
            alias,
            model,
            fields,
        }),
        ProviderChoice::Existing { alias_ref } => SelectorChoice::Existing(alias_ref),
    };
    let risk_profile = match form.risk.expect("risk satisfied") {
        PresetChoice::Fresh(n) => SelectorChoice::Fresh(n.to_string()),
        PresetChoice::Existing(a) => SelectorChoice::Existing(a),
    };
    let memory = SelectorChoice::Fresh(form.memory.expect("memory satisfied"));
    let channels = form
        .channels
        .into_iter()
        .map(|c| match c {
            ChannelChoice::Fresh {
                kind,
                alias,
                extras,
                ..
            } => SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: kind,
                alias,
                fields: extras.into_iter().collect(),
            }),
            ChannelChoice::Existing { alias_ref } => SelectorChoice::Existing(alias_ref),
        })
        .collect();
    let agent_choice = form.agent.expect("agent satisfied");
    let submission = BuilderSubmission {
        model_provider,
        risk_profile,
        runtime_profile,
        memory,
        channels,
        peer_groups: form.peer_groups,
        agent: AgentIdentity {
            name: agent_choice.name.clone(),
            system_prompt: agent_choice.system_prompt,
            personality_file: None,
            personality_files: agent_choice.personality_files,
        },
    };

    match Box::pin(apply_with_surface(submission, &mut cfg, Surface::Cli)).await {
        Ok(applied) => {
            println!();
            println!(
                "{}",
                ta(
                    "cli-quickstart-complete",
                    &[("alias", &applied.alias)],
                    "Quickstart complete."
                )
            );
            if let Some(auth) = inline_auth {
                Box::pin(run_inline_provider_auth(auth, &mut cfg)).await;
            }
            println!();
            println!("{}", t("cli-next-steps", "Next steps:"));
            println!(
                "{}",
                qta(
                    "cli-quickstart-next-agent-command",
                    &[("alias", &applied.alias)]
                )
            );
            if which_zerocode_on_path() {
                println!("  zerocode                   # launch the TUI"); // i18n-exempt: literal command/identifier example
            }
            Ok(())
        }
        Err(errs) => {
            eprintln!();
            eprintln!(
                "{}",
                t(
                    "cli-agent-not-created",
                    "Your agent was not created — and nothing on disk was changed."
                )
            );
            eprintln!(
                "{}",
                t(
                    "cli-quickstart-fix-and-rerun",
                    "Your existing config is untouched. Fix the following and run quickstart again:",
                )
            );
            eprintln!();
            for e in &errs {
                eprintln!("  • {}: {}", quickstart_step_label(e.step), e.message);
            }
            eprintln!();
            anyhow::bail!(
                "{}",
                qta(
                    "cli-quickstart-could-not-finish",
                    &[("count", &errs.len().to_string())],
                )
            )
        }
    }
}

#[cfg(feature = "agent-runtime")]
fn model_path_provider_type(path: &str) -> Option<&'static str> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() != 5 || parts[0] != "providers" || parts[1] != "models" || parts[4] != "model" {
        return None;
    }
    let family = parts[2];
    zeroclaw_providers::list_model_providers()
        .iter()
        .find(|p| p.name == family)
        .map(|p| p.name)
}

fn map_key_for_prop_path<'a>(section_path: &str, prop_path: &'a str) -> Option<&'a str> {
    let tail = prop_path.strip_prefix(section_path)?.strip_prefix('.')?;
    let mut parts = tail.split('.');
    let key = parts.next().filter(|key| !key.is_empty())?;
    parts.next()?;
    Some(key)
}

fn ensure_map_key_for_prop_path(config: &mut Config, prop_path: &str) -> Result<bool> {
    let Some((section_path, key)) = Config::map_key_sections()
        .into_iter()
        .filter(|section| section.kind == zeroclaw_config::traits::MapKeyKind::Map)
        .filter(|section| !section.resource_key)
        .filter_map(|section| {
            let key = map_key_for_prop_path(section.path, prop_path)?;
            Some((section.path, key))
        })
        .max_by_key(|(section_path, _)| section_path.len())
    else {
        return Ok(false);
    };

    // Route through the shared `create_map_key_checked` (not raw
    // `create_map_key`) so this CLI path inherits the reserved `default`
    // agent guard from the one place it's defined, rather than re-deriving
    // `section == "agents" && is_reserved_agent_alias(key)` here too. Without
    // this, widening past `providers.*` would let `config set
    // agents.default.enabled ...` auto-create the reserved runtime-fallback
    // agent alias, which the rename guard then refuses to ever rename.
    let created =
        match zeroclaw_config::alias_refs::create_map_key_checked(config, section_path, key) {
            Ok(created) => created,
            Err(zeroclaw_config::alias_refs::CreateError::Reserved(_)) => return Ok(false),
            Err(e) => return Err(anyhow::Error::msg(e.to_string())),
        };
    if created {
        // The section matched and the alias was newly materialized, but the
        // requested prop path might still not resolve (typo'd trailing field
        // name, or belt-and-suspenders against a resource-key path that
        // slipped past the `!resource_key` filter above). Roll back rather
        // than leave a phantom alias, falling through to the normal
        // "Unknown property" error exactly as before this alias existed.
        //
        // IMPORTANT: this probe/rollback must stay strictly inside the
        // `if created` branch. `create_map_key` returns `Ok(false)` when the
        // alias already existed (idempotent case) — never run this rollback
        // when `created == false`, or a bogus tail-field on an
        // ALREADY-EXISTING alias would delete a legitimate, pre-existing
        // config entry that has nothing to do with this call.
        if config.get_prop(prop_path).is_err() {
            let _ = config.delete_map_key(section_path, key);
            return Ok(false);
        }
        config.mark_dirty(&format!("{section_path}.{key}"));
    }
    Ok(created)
}

#[cfg(feature = "agent-runtime")]
fn trimmed_agent_name_for_templates(prior_name: Option<&str>) -> String {
    prior_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            zeroclaw_runtime::agent::personality_templates::TemplateContext::default().agent
        })
}

#[cfg(feature = "agent-runtime")]
fn prompt_for_field(
    desc: &zeroclaw_runtime::quickstart::FieldDescriptor,
    seed: Option<&str>,
) -> anyhow::Result<Option<String>> {
    use dialoguer::{FuzzySelect, Input, Password};
    use zeroclaw_config::traits::PropKind;
    if !desc.help.is_empty() {
        println!("  {}", desc.help);
    }
    let prompt = desc.label.clone();
    if desc.is_secret {
        // dialoguer 0.12 has no Esc-cancellable Password — only Ctrl+C
        // (returns `ErrorKind::Interrupted` wrapped in `dialoguer::Error::IO`).
        // Map that to `Ok(None)` so the caller treats it as "user backed
        // out" instead of bubbling a confusing IO-error message.
        match Password::new()
            .with_prompt(prompt.clone())
            .allow_empty_password(true)
            .interact()
        {
            Ok(pw) => return Ok(Some(pw)),
            Err(e) => {
                let io: std::io::Error = e.into();
                if io.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(io.into());
            }
        }
    }
    if let (PropKind::Enum, Some(variants)) = (&desc.kind, &desc.enum_variants) {
        let Some(i) = FuzzySelect::new()
            .with_prompt(prompt)
            .items(variants)
            .default(0)
            .max_length(variants.len().max(1))
            .interact_opt()?
        else {
            return Ok(None);
        };
        return Ok(Some(variants[i].clone()));
    }
    let mut input = Input::<String>::new()
        .with_prompt(prompt)
        .allow_empty(!desc.required);
    if let Some(s) = seed {
        input = input.default(s.to_string());
    } else if let Some(d) = desc.default.as_deref()
        && !d.is_empty()
        && d != zeroclaw_config::traits::UNSET_DISPLAY
    {
        // `<unset>` is a display placeholder for an unset Option, not a
        // real default. Seeding it pre-fills the prompt so a bare Enter
        // submits `<unset>`, which the daemon then validates against the
        // field's true type (e.g. a bool) and rejects.
        input = input.default(d.to_string());
    }
    // Same Ctrl+C-as-cancel mapping as the Password branch above.
    match input.interact_text() {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            let io: std::io::Error = e.into();
            if io.kind() == std::io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(io.into())
            }
        }
    }
}

#[cfg(feature = "agent-runtime")]
fn pick_preset(
    prompt: &str,
    presets: Vec<(&'static str, &'static str, &'static str)>,
    existing: &[String],
) -> anyhow::Result<Option<Result<&'static str, String>>> {
    use dialoguer::FuzzySelect;
    let mut mode_labels: Vec<String> = Vec::new();
    let mut mode_kinds: Vec<&str> = Vec::new();
    if !existing.is_empty() {
        mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
        mode_kinds.push("existing");
    }
    mode_labels.push(t("cli-quickstart-pick-preset", "Pick a preset"));
    mode_kinds.push("preset");
    let mode = if mode_labels.len() == 1 {
        Some(0)
    } else {
        FuzzySelect::new()
            .with_prompt(prompt)
            .items(&mode_labels)
            .default(0)
            .max_length(mode_labels.len())
            .interact_opt()?
    };
    let Some(mi) = mode else { return Ok(None) };
    if mode_kinds[mi] == "existing" {
        let Some(i) = FuzzySelect::new()
            .with_prompt(qta(
                "cli-quickstart-pick-existing-prompt",
                &[("prompt", prompt)],
            ))
            .items(existing)
            .default(0)
            .max_length(existing.len().max(1))
            .interact_opt()?
        else {
            return Ok(None);
        };
        return Ok(Some(Err(existing[i].clone())));
    }
    let labels: Vec<String> = presets
        .iter()
        .map(|(_, label, help)| format!("{label}  —  {help}"))
        .collect();
    let Some(i) = FuzzySelect::new()
        .with_prompt(qta(
            "cli-quickstart-pick-preset-prompt",
            &[("prompt", prompt)],
        ))
        .items(&labels)
        .default(0)
        .max_length(labels.len().max(1))
        .interact_opt()?
    else {
        return Ok(None);
    };
    Ok(Some(Ok(presets[i].0)))
}

#[cfg(feature = "agent-runtime")]
fn which_zerocode_on_path() -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join("zerocode").is_file()))
        .unwrap_or(false)
}

#[cfg(feature = "plugins-wasm")]
#[derive(Subcommand, Debug)]
enum PluginCommands {
    /// List installed plugins
    List,
    /// Search an installable plugin registry
    Search {
        /// Query to match against plugin names and descriptions
        query: String,
        /// Registry JSON URL to search
        #[arg(long)]
        registry: Option<String>,
    },
    /// Install a plugin from a local directory/manifest or registry name
    Install {
        /// Path to plugin directory/manifest, or registry name/version
        source: String,
        /// Registry JSON URL used for install-by-name
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove an installed plugin
    Remove {
        /// Plugin name
        name: String,
    },
    /// Show information about a plugin
    Info {
        /// Plugin name
        name: String,
    },
    /// Move plugins from legacy install directories into the configured one
    Migrate,
}

#[cfg(feature = "plugins-wasm")]
fn plugin_host_with_configured_security(
    config: &crate::config::schema::Config,
) -> Result<zeroclaw::plugins::host::PluginHost> {
    let mode = zeroclaw::plugins::host::PluginHost::resolve_signature_mode(
        &config.plugins.security.signature_mode,
    );
    let trusted = config.plugins.security.trusted_publisher_keys.clone();
    Ok(
        zeroclaw::plugins::host::PluginHost::from_plugins_dir_with_security(
            &config.plugins.resolved_plugins_dir(),
            mode,
            trusted,
        )?,
    )
}

#[cfg(feature = "plugins-wasm")]
async fn seed_plugin_config_entry(
    config: &mut crate::config::schema::Config,
    plugin_name: &str,
) -> Result<()> {
    let whole_config_degraded = config
        .degraded_security
        .iter()
        .any(|s| s == crate::config::migration::WHOLE_CONFIG_SENTINEL);
    if whole_config_degraded || config.degraded_sections.iter().any(|s| s == "plugins") {
        eprintln!(
            "{}",
            ta(
                "cli-plugin-config-entry-seed-skipped",
                &[("name", plugin_name)],
                "warning: skipped seeding the plugin config entry: the \
                 [plugins] section on disk is malformed. Repair it, add \
                 `[[plugins.entries]]` with the plugin name, then set values \
                 with `zeroclaw config set plugins.entries.<name>.config.<key>`."
            )
        );
        return Ok(());
    }
    if plugin_name.is_empty() || plugin_name.contains('.') {
        eprintln!(
            "{}",
            ta(
                "cli-plugin-config-entry-seed-unaddressable",
                &[("name", plugin_name)],
                "warning: skipped seeding the plugin config entry: the plugin \
                 name cannot be addressed by a dotted config path. Add a \
                 `[[plugins.entries]]` block to the config file by hand."
            )
        );
        return Ok(());
    }
    let created = config
        .create_map_key("plugins.entries", plugin_name)
        .map_err(anyhow::Error::msg)?;
    if !created {
        return Ok(());
    }
    config.mark_dirty(&format!("plugins.entries.{plugin_name}"));
    Box::pin(config.save_dirty()).await?;
    println!(
        "{}",
        ta(
            "cli-plugin-config-entry-seeded",
            &[("name", plugin_name)],
            "Seeded config entry. Set plugin config values with \
             `zeroclaw config set plugins.entries.<name>.config.<key>`."
        )
    );
    Ok(())
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
        /// Section prefix (e.g. channels.matrix). Omit to init all.
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
    /// Print the API explorer URL (plus a hint if the daemon isn't running).
    Docs,
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
async fn fetch_locales(locale: &str, catalog: Option<&str>) -> Result<()> {
    let locale = validated_locale(locale)?;

    let selected: Vec<&(&str, &str, &str)> = match catalog {
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
                    .find(|(n, _, _)| n == name)
                {
                    Some(entry) => out.push(entry),
                    None => {
                        let valid = zeroclaw_config::schema::FTL_CATALOGS
                            .iter()
                            .map(|(n, _, _)| *n)
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

    // Prefer the tag matching this binary; fall back to master.
    let version = env!("CARGO_PKG_VERSION");
    let refs = [format!("v{version}"), "master".to_string()];
    let client = reqwest::Client::new();
    let mut fetched = 0u32;

    for (name, path_tmpl, out_name) in selected {
        let repo_path = path_tmpl.replace("{locale}", &locale);
        let mut body: Option<String> = None;
        for git_ref in &refs {
            let url = format!(
                "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/{git_ref}/{repo_path}"
            );
            let resp = client.get(&url).send().await?;
            if resp.status().is_success() {
                body = Some(resp.text().await?);
                break;
            }
        }
        match body {
            Some(content) => {
                let out_path = dest.join(out_name);
                std::fs::write(&out_path, content)
                    .with_context(|| format!("writing {}", out_path.display()))?;
                println!(
                    "{}",
                    ta(
                        "cli-locales-fetched",
                        &[("name", name), ("path", &out_path.display().to_string())],
                        "fetched catalogue",
                    )
                );
                fetched += 1;
            }
            None => {
                eprintln!(
                    "{}",
                    ta(
                        "cli-locales-skipped",
                        &[
                            ("name", name),
                            ("path", &repo_path),
                            ("refs", &refs.join(", "))
                        ],
                        "skipped: not on upstream",
                    )
                );
            }
        }
    }

    if fetched == 0 {
        bail!("no catalogues fetched for locale '{locale}'");
    }
    println!(
        "{}",
        ta(
            "cli-locales-installed",
            &[
                ("count", &fetched.to_string()),
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

    async_main(command)
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
                print!(
                    "{}",
                    zeroclaw_config::schema_markdown::generate(&schema.to_value())
                );
                return Ok(());
            }
            #[cfg(not(feature = "schema-export"))]
            anyhow::bail!("zeroclaw was built without the 'schema-export' feature");
        }
        _ => {}
    }

    let default_floor = match &cli.command {
        Commands::Daemon {
            ephemeral: true, ..
        } => "debug",
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
                    zeroclaw_channels::orchestrator::deliver_announcement(
                        &config, &channel, &target, thread_id, &output,
                    )
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
            Box::pin(run_quickstart_cli(model_provider, model, api_key, agent)).await?;
            Ok(())
        }

        Commands::Agent {
            agent: agent_alias,
            message,
            session_state_file,
            model_provider,
            model,
            temperature,
            peripheral,
        } => {
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

            // Wire CLI channel for interactive mode
            zeroclaw_runtime::agent::loop_::register_cli_channel_fn(Box::new(|| {
                Box::new(zeroclaw_channels::cli::CliChannel::new("cli"))
            }));

            // Wire peripheral tools (gpio_read/gpio_write etc.) for `zeroclaw agent`.
            // Mirrors the registration done for the daemon command.
            #[cfg(feature = "hardware")]
            zeroclaw_runtime::agent::loop_::register_peripheral_tools_fn(Box::new(|config| {
                Box::pin(async move {
                    zeroclaw_hardware::peripherals::create_peripheral_tools(&config).await
                })
            }));

            // Register channel map factory for late-bound tool handle population.
            zeroclaw_runtime::agent::loop_::register_channel_map_fn(Box::new({
                let config_clone = config.clone();
                move || zeroclaw_channels::orchestrator::build_channel_map(&config_clone)
            }));

            Box::pin(agent::run(
                config,
                &agent_alias,
                message,
                model_provider,
                model,
                final_temperature,
                peripheral,
                true,
                session_state_file,
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
            #[cfg(feature = "channel-acp-server")]
            {
                let mut acp_config = channels::acp_server::AcpServerConfig {
                    max_sessions: config.acp.max_sessions,
                    session_timeout_secs: config.acp.session_timeout_secs,
                };
                if let Some(max) = max_sessions {
                    acp_config.max_sessions = max;
                }
                if let Some(timeout) = session_timeout {
                    acp_config.session_timeout_secs = timeout;
                }
                let store =
                    zeroclaw_infra::acp_session_store::AcpSessionStore::new(&config.data_dir)
                        .map(std::sync::Arc::new)
                        .inspect_err(|e| {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({"error": e.to_string()})),
                                "Failed to open ACP session store"
                            );
                        })
                        .ok();
                let server = if let Some(store) = store {
                    std::sync::Arc::new(channels::acp_server::AcpServer::new_with_store(
                        config, acp_config, store,
                    ))
                } else {
                    std::sync::Arc::new(channels::acp_server::AcpServer::new(config, acp_config))
                };
                server.run().await
            }
            #[cfg(not(feature = "channel-acp-server"))]
            {
                let _ = (max_sessions, session_timeout);
                anyhow::bail!("ACP server requires the `channel-acp-server` feature")
            }
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
            ephemeral,
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

            // Wire CLI channel for interactive mode
            #[cfg(feature = "agent-runtime")]
            zeroclaw_runtime::agent::loop_::register_cli_channel_fn(Box::new(|| {
                Box::new(zeroclaw_channels::cli::CliChannel::new("cli"))
            }));

            // Wire peripheral tools from zeroclaw-hardware
            #[cfg(feature = "hardware")]
            zeroclaw_runtime::agent::loop_::register_peripheral_tools_fn(Box::new(|config| {
                Box::pin(async move {
                    zeroclaw_hardware::peripherals::create_peripheral_tools(&config).await
                })
            }));

            // Cron delivery is registered earlier (before the command match)
            // so it works for both `daemon` and `gateway start`.

            let canvas_store = zeroclaw_runtime::tools::CanvasStore::new();
            let canvas_store_for_gateway = canvas_store.clone();
            let canvas_store_for_channels = canvas_store.clone();

            // Capture the launch command now, before any in-app upgrade can
            // swap the binary on disk (after which `current_exe()` resolves to a
            // "(deleted)" path on Linux). Used by the post-loop self-respawn.
            zeroclaw_runtime::restart::record_launch();

            // Reload loop. `daemon::run` returns DaemonExit::Shutdown on
            // SIGINT/SIGTERM (loop ends) or DaemonExit::Reload on SIGUSR1
            // (loop re-reads config from disk and re-runs). The PID stays
            // the same across reloads — only the in-process subsystems
            // tear down + re-instantiate.
            let mut current_config = config;
            // Nag task for the degraded-security warning, scoped to the
            // current config. Re-evaluated each reload iteration so a repaired
            // config stops the warning and a freshly-degraded one starts it.
            let mut degraded_nag: Option<tokio::task::JoinHandle<()>> =
                gate_security_posture(&current_config, allow_degraded_security)?;
            loop {
                // Per-iteration clones so the subsystem closures (which
                // `move`-capture) don't consume the outer bindings on the
                // first iteration; reload would otherwise see a moved value.
                let canvas_store_for_gateway = canvas_store_for_gateway.clone();
                let canvas_store_for_channels = canvas_store_for_channels.clone();
                let mut registry = daemon::DaemonRegistry::new();

                // SOP loading is gated on `[sop] sops_dir`: unset disables all
                // SOP runtime behavior, matching the documented rollback path.
                let (sop_engine, sop_audit) = if current_config.sop.sops_dir.is_some() {
                    let mem: Arc<dyn zeroclaw_memory::Memory> = Arc::from(
                        zeroclaw_memory::create_memory_from_config(&current_config, None)?,
                    );
                    let sop_adapters = build_sop_adapters(&current_config);
                    let (engine, audit) = zeroclaw_runtime::sop::build_sop_engine(
                        current_config.sop.clone(),
                        &current_config.data_dir,
                        mem,
                        sop_adapters,
                    );
                    (Some(engine), Some(audit))
                } else {
                    (None, None)
                };

                // EPIC A1 + SOP cron: drive periodic maintenance and cron
                // triggers against the shared engine for this daemon iteration.
                let sop_maintenance = spawn_sop_maintenance(
                    sop_engine.as_ref(),
                    sop_audit.as_ref(),
                    current_config.sop.maintenance_interval_secs,
                );

                #[cfg(feature = "gateway")]
                registry.register_gateway(Box::new({
                    let sop_e = sop_engine.clone();
                    let sop_a = sop_audit.clone();
                    move |host, port, config, tx, reload_controls, tui_registry| {
                        let canvas_store = canvas_store_for_gateway.clone();
                        let sop_engine = sop_e.clone();
                        let sop_audit = sop_a.clone();
                        Box::pin(async move {
                            Box::pin(zeroclaw_gateway::run_gateway(
                                &host,
                                port,
                                config,
                                tx,
                                reload_controls,
                                tui_registry,
                                Some(canvas_store),
                                sop_engine,
                                sop_audit,
                            ))
                            .await
                        })
                    }
                }));

                registry.register_channels(Box::new({
                    let sop_e = sop_engine.clone();
                    let sop_a = sop_audit.clone();
                    move |config, cancel| {
                        let canvas_store = canvas_store_for_channels.clone();
                        let sop_engine = sop_e.clone();
                        let sop_audit = sop_a.clone();
                        Box::pin(async move {
                            Box::pin(zeroclaw_channels::orchestrator::start_channels(
                                config,
                                Some(canvas_store),
                                cancel,
                                sop_engine,
                                sop_audit,
                            ))
                            .await
                        })
                    }
                }));

                #[cfg(feature = "channel-mqtt")]
                registry.register_mqtt(Box::new({
                    let engine = sop_engine.clone();
                    let audit = sop_audit.clone();
                    move |mqtt_config| {
                        let engine = engine.clone();
                        let audit = audit.clone();
                        Box::pin(async move {
                            if let (Some(engine), Some(audit)) = (engine, audit) {
                                zeroclaw_channels::orchestrator::mqtt::run_mqtt_sop_listener(
                                    &mqtt_config,
                                    engine,
                                    audit,
                                )
                                .await
                            } else {
                                // No SOPs directory configured — this is a valid
                                // user state, not a misconfiguration. Skip the
                                // listener gracefully.
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Skip
                                    ),
                                    "MQTT SOP listener skipped — no SOPs directory configured"
                                );
                                Ok(())
                            }
                        })
                    }
                }));

                registry.register_socket(Box::new(|ctx, cancel, client_count| {
                    Box::pin(async move {
                        zeroclaw_runtime::rpc::local::run_local_listener(ctx, cancel, client_count)
                            .await
                    })
                }));

                registry.register_wss(Box::new(|ctx, cancel, client_count| {
                    Box::pin(async move {
                        let wss_cfg = ctx.config.read().wss.clone();
                        if !wss_cfg.enabled {
                            // WSS disabled — park until cancelled.
                            cancel.cancelled().await;
                            return Ok(());
                        }
                        let tls_acceptor = zeroclaw_runtime::rpc::wss::build_tls_acceptor(
                            &wss_cfg.cert_path,
                            &wss_cfg.key_path,
                        )?;
                        let bind_addr: std::net::SocketAddr =
                            format!("{}:{}", wss_cfg.bind, wss_cfg.port).parse()?;
                        zeroclaw_runtime::rpc::wss::run_wss_listener(
                            ctx,
                            cancel,
                            client_count,
                            tls_acceptor,
                            bind_addr,
                        )
                        .await
                    })
                }));

                // Pass the shared SOP engine through the registry so
                // RpcContext (RPC/TUI agent sessions) can share it.
                registry.set_sop_engine(sop_engine, sop_audit);

                let exit = Box::pin(daemon::run(
                    current_config.clone(),
                    host.clone(),
                    port,
                    registry,
                    ephemeral,
                ))
                .await;
                if let Some(handle) = sop_maintenance {
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
                        #[cfg(feature = "agent-runtime")]
                        observability::runtime_trace::init_from_config(
                            &current_config.observability,
                            &current_config.data_dir,
                        );
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

        Commands::Status { format } => {
            if format.as_deref() == Some("exit-code") {
                // Lightweight health probe for Docker HEALTHCHECK
                let port = config.gateway.port;
                let host = if config.gateway.host == "[::]" || config.gateway.host == "0.0.0.0" {
                    "127.0.0.1"
                } else {
                    &config.gateway.host
                };
                let url = format!("http://{}:{}/health", host, port);
                match reqwest::Client::new()
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        std::process::exit(0);
                    }
                    _ => {
                        std::process::exit(1);
                    }
                }
            }
            println!("{}", t("cli-status-title", "🦀 ZeroClaw Status"));
            println!();
            println!(
                "{}",
                ta(
                    "cli-status-version",
                    &[("v", env!("CARGO_PKG_VERSION"))],
                    "Version"
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-workspace",
                    &[("v", &config.data_dir.display().to_string())],
                    "Workspace"
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-config",
                    &[("v", &config.config_path.display().to_string())],
                    "Config"
                )
            );
            println!();
            let mut shown_provider = false;
            for (family, alias, entry) in config.providers.models.iter_entries() {
                let model = entry.model.as_deref().unwrap_or("(none)");
                if shown_provider {
                    println!(
                        "{}",
                        ta(
                            "cli-status-provider-indent",
                            &[("family", family), ("alias", alias)],
                            "ModelProvider"
                        )
                    );
                    println!("{}", ta("cli-status-model", &[("model", model)], "Model"));
                } else {
                    println!(
                        "{}",
                        ta(
                            "cli-status-provider",
                            &[("family", family), ("alias", alias)],
                            "ModelProvider"
                        )
                    );
                    println!("{}", ta("cli-status-model", &[("model", model)], "Model"));
                    shown_provider = true;
                }
            }
            if !shown_provider {
                println!(
                    "{}",
                    t(
                        "cli-status-provider-none",
                        "🤖 ModelProvider:      (none configured)"
                    )
                );
            }
            println!(
                "{}",
                ta(
                    "cli-status-observability",
                    &[("v", config.observability.backend.as_wire())],
                    "Observability"
                )
            );
            let trace_storage_mode = config.observability.log_persistence.as_wire().to_string();
            let trace_storage_path = config.observability.log_persistence_path.to_string();
            let trace_storage_fallback = format!(
                "🧾 Trace storage:  {} ({})",
                trace_storage_mode, trace_storage_path
            );
            println!(
                "{}",
                ta(
                    "cli-status-trace-storage",
                    &[("mode", &trace_storage_mode), ("path", &trace_storage_path),],
                    &trace_storage_fallback
                )
            );
            // Per-agent autonomy: each enabled agent picks its own
            // risk_profile, so list them rather than collapsing to one.
            let mut agent_aliases: Vec<&String> = config
                .agents
                .iter()
                .filter(|(_, a)| a.enabled)
                .map(|(alias, _)| alias)
                .collect();
            agent_aliases.sort();
            if agent_aliases.is_empty() {
                println!(
                    "{}",
                    t(
                        "cli-status-agents-none",
                        "🛡️  Agents:        (none configured)"
                    )
                );
            } else {
                let summary: Vec<String> = agent_aliases
                    .iter()
                    .map(|alias| match config.risk_profile_for_agent(alias) {
                        Some(p) => {
                            let level = format!("{:?}", p.level);
                            let fallback = format!("{alias}={level}");
                            ta(
                                "cli-status-agent-risk-profile",
                                &[("alias", alias), ("level", &level)],
                                &fallback,
                            )
                        }
                        None => {
                            let fallback = format!("{alias}=<no risk_profile>");
                            ta(
                                "cli-status-agent-no-risk-profile-summary",
                                &[("alias", alias)],
                                &fallback,
                            )
                        }
                    })
                    .collect();
                println!(
                    "{}",
                    ta("cli-status-agents", &[("v", &summary.join(", "))], "Agents")
                );
            }
            println!(
                "{}",
                ta(
                    "cli-status-runtime",
                    &[("v", config.runtime.kind.as_wire())],
                    "Runtime"
                )
            );
            if service::is_running(&config) {
                println!(
                    "{}",
                    t("cli-status-service-running", "🟢 Service:       running")
                );
            } else {
                println!(
                    "{}",
                    t("cli-status-service-stopped", "🔴 Service:       stopped")
                );
            }
            #[cfg(feature = "gateway")]
            {
                match zeroclaw_gateway::resolve_web_dashboard_availability(&config) {
                    Some(zeroclaw_gateway::WebDashboardAvailability::Embedded) => {
                        let path = "embedded";
                        let fallback = format!("🌐 Web UI:        FOUND ({path})");
                        println!(
                            "{}",
                            ta("cli-status-web-ui-found", &[("path", path)], &fallback)
                        );
                    }
                    Some(zeroclaw_gateway::WebDashboardAvailability::Filesystem(web_dist_dir)) => {
                        let path = web_dist_dir.display().to_string();
                        let fallback = format!("🌐 Web UI:        FOUND ({path})");
                        println!(
                            "{}",
                            ta("cli-status-web-ui-found", &[("path", &path)], &fallback)
                        );
                    }
                    None => {
                        println!(
                            "{}",
                            t("cli-status-web-ui-missing", "🌐 Web UI:        MISSING")
                        );
                    }
                }
            }
            let effective_memory_backend = config.resolve_active_storage().kind();
            let heartbeat_value = if config.heartbeat.enabled {
                let interval_minutes = config.heartbeat.interval_minutes.to_string();
                let heartbeat_every_fallback = format!("every {}min", interval_minutes);
                ta(
                    "cli-status-heartbeat-every-minutes",
                    &[("minutes", &interval_minutes)],
                    &heartbeat_every_fallback,
                )
            } else {
                t("cli-status-word-disabled", "disabled")
            };
            let heartbeat_fallback = format!("💓 Heartbeat:      {}", heartbeat_value);
            println!(
                "{}",
                ta(
                    "cli-status-heartbeat",
                    &[("v", &heartbeat_value)],
                    &heartbeat_fallback
                )
            );
            let memory_backend = effective_memory_backend.to_string();
            let memory_auto_save = if config.memory.auto_save {
                t("cli-status-word-on", "on")
            } else {
                t("cli-status-word-off", "off")
            };
            let memory_fallback = format!(
                "🧠 Memory:         {} (auto-save: {})",
                memory_backend, memory_auto_save
            );
            println!(
                "{}",
                ta(
                    "cli-status-memory",
                    &[
                        ("backend", &memory_backend),
                        ("auto_save", &memory_auto_save),
                    ],
                    &memory_fallback
                )
            );

            println!();
            // Per-agent security: each enabled agent's risk profile.
            for alias in &agent_aliases {
                let Some(profile) = config.risk_profile_for_agent(alias) else {
                    println!(
                        "{}",
                        ta(
                            "cli-status-security-noprofile",
                            &[("alias", alias)],
                            "Security: no risk_profile"
                        )
                    );
                    continue;
                };
                println!(
                    "{}",
                    ta("cli-status-security", &[("alias", alias)], "Security")
                );
                println!(
                    "{}",
                    ta(
                        "cli-status-workspace-only",
                        &[("v", &profile.workspace_only.to_string())],
                        "Workspace only"
                    )
                );
                let allowed_roots = if profile.allowed_roots.is_empty() {
                    t("cli-status-word-none", "(none)")
                } else {
                    profile.allowed_roots.join(", ")
                };
                let allowed_roots_fallback = format!("  Allowed roots:     {}", allowed_roots);
                println!(
                    "{}",
                    ta(
                        "cli-status-allowed-roots",
                        &[("v", &allowed_roots)],
                        &allowed_roots_fallback
                    )
                );
                let allowed_commands = profile.allowed_commands.join(", ");
                let allowed_commands_fallback =
                    format!("  Allowed commands:  {}", allowed_commands);
                println!(
                    "{}",
                    ta(
                        "cli-status-allowed-commands",
                        &[("v", &allowed_commands)],
                        &allowed_commands_fallback
                    )
                );
                let actions_cap = config
                    .runtime_profile_for_agent(alias)
                    .map_or(0, |r| r.max_actions_per_hour);
                println!(
                    "{}",
                    ta(
                        "cli-status-max-actions",
                        &[("v", &actions_cap.to_string())],
                        "Max actions/hour"
                    )
                );
            }
            let cost_tracking = if config.cost.enabled {
                t("cli-status-word-enabled", "enabled")
            } else {
                t("cli-status-word-disabled", "disabled")
            };
            let cost_tracking_fallback = format!("  Cost tracking:     {}", cost_tracking);
            println!(
                "{}",
                ta(
                    "cli-status-cost-tracking",
                    &[("v", &cost_tracking)],
                    &cost_tracking_fallback
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-max-cost-day",
                    &[("v", &format!("{:.2}", config.cost.daily_limit_usd))],
                    "Max cost/day"
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-max-cost-month",
                    &[("v", &format!("{:.2}", config.cost.monthly_limit_usd))],
                    "Max cost/month"
                )
            );
            if config.cost.enabled {
                match cost::CostTracker::new(config.cost.clone(), &config.data_dir) {
                    Ok(tracker) => match tracker.get_summary() {
                        Ok(summary) => {
                            let spent_today = format!("{:.4}", summary.daily_cost_usd);
                            let daily_limit = format!("{:.2}", config.cost.daily_limit_usd);
                            let spent_today_fallback =
                                format!("  Spent today:       ${spent_today} / ${daily_limit}");
                            println!(
                                "{}",
                                ta(
                                    "cli-status-spent-today",
                                    &[("spent", &spent_today), ("limit", &daily_limit)],
                                    &spent_today_fallback
                                )
                            );
                            let spent_month = format!("{:.4}", summary.monthly_cost_usd);
                            let monthly_limit = format!("{:.2}", config.cost.monthly_limit_usd);
                            let spent_month_fallback =
                                format!("  Spent this month:  ${spent_month} / ${monthly_limit}");
                            println!(
                                "{}",
                                ta(
                                    "cli-status-spent-month",
                                    &[("spent", &spent_month), ("limit", &monthly_limit)],
                                    &spent_month_fallback
                                )
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "{}",
                                ta(
                                    "cli-warn-cost-usage",
                                    &[("err", &e.to_string())],
                                    "Could not load cost usage"
                                )
                            );
                        }
                    },
                    Err(e) => {
                        eprintln!(
                            "{}",
                            ta(
                                "cli-warn-cost-tracker",
                                &[("err", &e.to_string())],
                                "Could not init cost tracker"
                            )
                        );
                    }
                }
            }
            println!(
                "{}",
                ta(
                    "cli-status-otp",
                    &[("v", &config.security.otp.enabled.to_string())],
                    "OTP enabled"
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-estop",
                    &[("v", &config.security.estop.enabled.to_string())],
                    "E-stop enabled"
                )
            );
            println!();
            println!("{}", t("cli-status-channels", "Channels:"));
            println!("{}", t("cli-status-cli-always", "  CLI:      ✅ always"));
            for entry in zeroclaw_channels::listing::compiled_channels(&config.channels) {
                let channel_status = if entry.configured {
                    t("cli-status-word-configured", "configured")
                } else {
                    t("cli-status-word-not-configured", "not configured")
                };
                let status = if entry.configured {
                    ta(
                        "cli-status-channel-configured",
                        &[("status", &channel_status)],
                        format!("✅ {channel_status}"),
                    )
                } else {
                    ta(
                        "cli-status-channel-not-configured",
                        &[("status", &channel_status)],
                        format!("❌ {channel_status}"),
                    )
                };
                println!("  {:9} {}", entry.name, status);
            }
            let uncompiled =
                zeroclaw_channels::listing::configured_uncompiled_channels(&config.channels);
            if !uncompiled.is_empty() {
                println!(
                    "{}",
                    t(
                        "cli-channels-not-compiled-header",
                        "  Configured but not compiled in this binary:"
                    )
                );
                for entry in &uncompiled {
                    let status = t(
                        "cli-status-channel-not-compiled",
                        "🚫 configured, not compiled",
                    );
                    println!("  {:9} {}", entry.name, status);
                }
                println!(
                    "{}",
                    t(
                        "cli-channels-build-hint",
                        "  Build from source with `./install.sh --source --preset full`, `--features channels-full`, or the specific `channel-*` feature."
                    )
                );
            }
            println!();
            println!("{}", t("cli-status-peripherals", "Peripherals:"));
            let peripherals_enabled = if config.peripherals.enabled {
                t("cli-status-word-yes", "yes")
            } else {
                t("cli-status-word-no", "no")
            };
            let peripherals_enabled_fallback = format!("  Enabled:   {}", peripherals_enabled);
            println!(
                "{}",
                ta(
                    "cli-status-peripherals-enabled",
                    &[("v", &peripherals_enabled)],
                    &peripherals_enabled_fallback
                )
            );
            println!(
                "{}",
                ta(
                    "cli-status-boards",
                    &[("v", &config.peripherals.boards.len().to_string())],
                    "Boards"
                )
            );

            Ok(())
        }

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
                #[cfg(feature = "hardware")]
                zeroclaw_runtime::agent::loop_::register_peripheral_tools_fn(Box::new(|config| {
                    Box::pin(async move {
                        zeroclaw_hardware::peripherals::create_peripheral_tools(&config).await
                    })
                }));

                let cancel = tokio_util::sync::CancellationToken::new();
                let (sop_engine, sop_audit) = if config.sop.sops_dir.is_some() {
                    let mem: Arc<dyn zeroclaw_memory::Memory> =
                        Arc::from(zeroclaw_memory::create_memory_from_config(&config, None)?);
                    let sop_adapters = build_sop_adapters(&config);
                    let (engine, audit) = zeroclaw_runtime::sop::build_sop_engine(
                        config.sop.clone(),
                        &config.data_dir,
                        mem,
                        sop_adapters,
                    );
                    (Some(engine), Some(audit))
                } else {
                    (None, None)
                };
                // EPIC A1 + SOP cron: same tick as the full daemon path.
                let sop_maintenance = spawn_sop_maintenance(
                    sop_engine.as_ref(),
                    sop_audit.as_ref(),
                    config.sop.maintenance_interval_secs,
                );
                let result = Box::pin(channels::start_channels(
                    config, None, cancel, sop_engine, sop_audit,
                ))
                .await;
                if let Some(handle) = sop_maintenance {
                    handle.abort();
                }
                result
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

        Commands::Sop { sop_command } => match sop_command {
            // Out-of-band approval verbs talk to the running daemon over the
            // gateway (they must see the daemon's runs, not a throwaway local
            // engine). List/Validate/Show stay local + synchronous.
            cmd @ (SopCommands::Approve { .. }
            | SopCommands::Deny { .. }
            | SopCommands::Pending) => sop_admin_dispatch(cmd, &config).await,
            other => sop::handle_command(other, &config),
        },

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
            Box::pin(peripherals::handle_command(
                peripheral_command.clone(),
                &config,
            ))
            .await
        }

        Commands::Desktop {
            install: do_install,
        } => {
            let download_url = "https://www.zeroclawlabs.ai/download";

            if do_install {
                println!(
                    "{}",
                    t(
                        "cli-desktop-download",
                        "Download the ZeroClaw companion app:"
                    )
                );
                println!();
                #[cfg(target_os = "macos")]
                {
                    println!("  macOS:  {download_url}"); // i18n-exempt: literal command/identifier example
                    println!();
                    println!(
                        "{}",
                        t(
                            "cli-desktop-homebrew",
                            "Or install via Homebrew (coming soon):"
                        )
                    );
                    println!("  brew install --cask zeroclaw"); // i18n-exempt: literal command/identifier example
                }
                #[cfg(target_os = "linux")]
                {
                    println!("  Linux:  {download_url}"); // i18n-exempt: literal command/identifier example
                    println!();
                    println!(
                        "{}",
                        t(
                            "cli-desktop-linux-pkg",
                            "  Download the .deb or .AppImage for your architecture."
                        )
                    );
                }
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                {
                    println!("  {download_url}");
                }
                println!();

                // On macOS, open the download page in the browser
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("open").arg(download_url).spawn();
                }
                #[cfg(target_os = "linux")]
                {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(download_url)
                        .spawn();
                }
                return Ok(());
            }

            // Locate the companion app
            let desktop_bin = {
                let mut found = None;

                // 1. macOS: check /Applications/ZeroClaw.app
                #[cfg(target_os = "macos")]
                {
                    let app_paths = [
                        PathBuf::from("/Applications/ZeroClaw.app/Contents/MacOS/ZeroClaw"),
                        PathBuf::from(std::env::var("HOME").unwrap_or_default())
                            .join("Applications/ZeroClaw.app/Contents/MacOS/ZeroClaw"),
                    ];
                    for app in &app_paths {
                        if app.is_file() {
                            found = Some(app.clone());
                            break;
                        }
                    }
                }

                // 2. Same directory as the current executable
                if found.is_none()
                    && let Ok(exe) = std::env::current_exe()
                {
                    let sibling = exe.with_file_name("zeroclaw-desktop");
                    if sibling.is_file() {
                        found = Some(sibling);
                    }
                }

                // 3. Common cargo/local install locations under the user's home directory.
                //    Uses directories::UserDirs so HOME (Unix) and USERPROFILE (Windows)
                //    are both resolved correctly. On Windows the binary is .exe — try
                //    both names since which::which (step 4) only catches PATH entries.
                if found.is_none()
                    && let Some(home) =
                        directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
                {
                    let bin_names: &[&str] = if cfg!(windows) {
                        &["zeroclaw-desktop.exe", "zeroclaw-desktop"]
                    } else {
                        &["zeroclaw-desktop"]
                    };
                    // .cargo/bin works the same on Windows; .local/bin is XDG (Unix only).
                    let dirs: &[&str] = if cfg!(windows) {
                        &[".cargo/bin"]
                    } else {
                        &[".cargo/bin", ".local/bin"]
                    };
                    'outer: for dir in dirs {
                        for name in bin_names {
                            let candidate = home.join(dir).join(name);
                            if candidate.is_file() {
                                found = Some(candidate);
                                break 'outer;
                            }
                        }
                    }
                }

                // 4. Fallback to PATH lookup
                if found.is_none()
                    && let Ok(path) = which::which("zeroclaw-desktop")
                {
                    found = Some(path);
                }

                found
            };

            match desktop_bin {
                Some(bin) => {
                    println!(
                        "{}",
                        t(
                            "cli-desktop-launching",
                            "Launching ZeroClaw companion app..."
                        )
                    );
                    let _child = std::process::Command::new(&bin)
                        .spawn()
                        .with_context(|| format!("Failed to launch {}", bin.display()))?;
                    Ok(())
                }
                None => {
                    println!(
                        "{}",
                        t(
                            "cli-desktop-not-installed",
                            "ZeroClaw companion app is not installed."
                        )
                    );
                    println!();
                    println!(
                        "{}",
                        ta(
                            "cli-desktop-download-at",
                            &[("url", download_url)],
                            "Download it at"
                        )
                    );
                    println!("  Or run: zeroclaw desktop --install"); // i18n-exempt: literal command
                    println!();
                    println!(
                        "{}",
                        t(
                            "cli-desktop-blurb1",
                            "The companion app is a lightweight menu bar app that"
                        )
                    );
                    println!(
                        "{}",
                        t(
                            "cli-desktop-blurb2",
                            "connects to the same gateway as the CLI."
                        )
                    );
                    std::process::exit(1);
                }
            }
        }

        Commands::Locales { locales_command } => {
            let LocalesCommands::Fetch { locale, catalog } = locales_command;
            fetch_locales(&locale, catalog.as_deref()).await?;
            Ok(())
        }

        Commands::Update {
            check,
            force,
            version,
            json,
        } => {
            if check {
                let info = commands::update::check(version.as_deref()).await?;
                if json {
                    // Machine-readable shape consumed by the gateway's
                    // `GET /api/version/check`. Keep field names stable.
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "current_version": info.current_version,
                            "latest_version": info.latest_version,
                            "is_newer": info.is_newer,
                            "release_url": info.release_url,
                            "release_notes": info.release_notes,
                            "published_at": info.published_at,
                        }))?
                    );
                } else if info.is_newer {
                    println!(
                        "{}",
                        ta(
                            "cli-update-available",
                            &[
                                ("current", &info.current_version),
                                ("latest", &info.latest_version)
                            ],
                            "Update available"
                        )
                    );
                } else {
                    println!(
                        "{}",
                        ta(
                            "cli-update-already-current",
                            &[("version", &info.current_version)],
                            "Already up to date"
                        )
                    );
                }
                Ok(())
            } else {
                commands::update::run(version.as_deref(), force).await
            }
        }

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

        Commands::Eval { eval_command } => match eval_command {
            EvalCommands::Run {
                suite,
                mode,
                format,
            } => {
                let suite_dir = suite.unwrap_or_else(|| config.eval.suite_dir.clone());
                let mode: zeroclaw_eval::Mode =
                    mode.unwrap_or_else(|| config.eval.mode.clone()).parse()?;
                let report = commands::eval::run(std::path::PathBuf::from(suite_dir), mode).await?;
                commands::eval::print_report(&report, format);
                if !report.all_passed() {
                    std::process::exit(1);
                }
                Ok(())
            }
        },

        Commands::Config { config_command } => match config_command {
            ConfigCommands::Schema { path } => {
                #[cfg(feature = "schema-export")]
                {
                    let schema = schemars::schema_for!(config::Config);
                    let value = match path.as_deref() {
                        None => serde_json::to_value(&schema)
                            .context("failed to serialize JSON Schema")?,
                        Some(prop_path) => {
                            let full = serde_json::to_value(&schema)
                                .context("failed to serialize JSON Schema")?;
                            let mut out = full;
                            if let serde_json::Value::Object(ref mut map) = out {
                                map.insert(
                                    "x-zeroclaw-requested-path".into(),
                                    serde_json::Value::String(prop_path.into()),
                                );
                            }
                            out
                        }
                    };
                    println!("{}", serde_json::to_string_pretty(&value)?);
                    Ok(())
                }
                #[cfg(not(feature = "schema-export"))]
                {
                    let _ = path;
                    anyhow::bail!("zeroclaw was built without the 'schema-export' feature")
                }
            }
            ConfigCommands::List { filter, secrets } => {
                let entries = config.prop_fields();
                println!(
                    "{}",
                    t(
                        "cli-config-legend",
                        "Legend: \u{1f489} env-overridden  \u{1f512} secret"
                    )
                );
                println!();
                let mut current_category = "";
                for entry in &entries {
                    if secrets && !entry.is_secret {
                        continue;
                    }
                    if let Some(ref f) = filter
                        && !entry.name.starts_with(f.as_str())
                    {
                        continue;
                    }
                    if entry.category != current_category {
                        if !current_category.is_empty() {
                            println!();
                        }
                        println!("{}:", entry.category);
                        current_category = entry.category;
                    }
                    let env = if config.prop_is_env_overridden(&entry.name) {
                        "\u{1f489} "
                    } else {
                        "  "
                    };
                    let lock = if entry.is_secret { " \u{1f512}" } else { "" };
                    println!(
                        "{env}{:<45} = {:<20} ({}){lock}",
                        entry.name, entry.display_value, entry.type_hint
                    );
                }
                Ok(())
            }
            ConfigCommands::Get { path, json } => {
                let known_paths: Vec<String> =
                    config.prop_fields().into_iter().map(|f| f.name).collect();
                let path = zeroclaw_config::helpers::resolve_field_path(&known_paths, &path);
                if Config::prop_is_secret(&path) {
                    let entries = config.prop_fields();
                    let populated = entries
                        .iter()
                        .find(|e| e.name == path)
                        .map(|e| e.display_value != "<unset>")
                        .unwrap_or(false);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "path": path,
                                "populated": populated,
                            }))?
                        );
                    } else if populated {
                        println!(
                            "{}",
                            ta(
                                "cli-config-secret-set",
                                &[("path", &path)],
                                "is set (encrypted secret, value not displayed)"
                            )
                        );
                    } else {
                        println!(
                            "{}",
                            ta(
                                "cli-config-secret-unset",
                                &[("path", &path)],
                                "is not set (encrypted secret)"
                            )
                        );
                    }
                } else {
                    match config.get_prop(&path) {
                        Ok(value) => {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(&serde_json::json!({
                                        "path": path,
                                        "value": value,
                                    }))?
                                );
                            } else {
                                println!("{value}");
                            }
                        }
                        Err(e) => {
                            // Classify the anyhow string into a stable code so
                            // the CLI's --json envelope matches the HTTP shape.
                            // Same single-source-of-truth helper the gateway
                            // uses; never hardcode a code at the call site.
                            let api_err =
                                zeroclaw_config::api_error::ConfigApiError::from_validation(
                                    anyhow::Error::msg(e.to_string()),
                                )
                                .with_path(&path);
                            if json {
                                eprintln!("{}", serde_json::to_string_pretty(&api_err)?);
                                std::process::exit(1);
                            }
                            anyhow::bail!("{e}");
                        }
                    }
                }
                Ok(())
            }
            ConfigCommands::Set {
                path,
                value,
                no_interactive,
                comment,
                json,
            } => {
                crate::config::migration::ensure_disk_at_current_version(&config.config_path)?;
                let known_paths: Vec<String> =
                    config.prop_fields().into_iter().map(|f| f.name).collect();
                let mut path = zeroclaw_config::helpers::resolve_field_path(&known_paths, &path);
                if ensure_map_key_for_prop_path(&mut config, &path)? {
                    let known_paths: Vec<String> =
                        config.prop_fields().into_iter().map(|f| f.name).collect();
                    path = zeroclaw_config::helpers::resolve_field_path(&known_paths, &path);
                }
                if no_interactive {
                    let val = value.ok_or_else(|| {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"path": path})),
                            "config set --no-interactive refused: positional value missing"
                        );
                        anyhow::Error::msg(format!(
                            "Value required in --no-interactive mode. Usage: zeroclaw config set --no-interactive {path} <value>"
                        ))
                    })?;
                    config.set_prop_persistent(&path, &val)?;
                } else if Config::prop_is_secret(&path) {
                    if value.is_some() {
                        eprintln!(
                            "  \u{26a0} {path} is an encrypted secret \u{2014} using masked input."
                        );
                    }
                    let secret_value = dialoguer::Password::new()
                        .with_prompt(format!("Enter value for {path}"))
                        .interact()?;
                    let secret_value = secret_value.trim().to_string();
                    if secret_value.is_empty() {
                        anyhow::bail!("Value cannot be empty.");
                    }
                    config.set_prop_persistent(&path, &secret_value)?;
                } else if let Some(val) = value {
                    config.set_prop_persistent(&path, &val)?;
                } else if let Some(provider_type) = model_path_provider_type(&path) {
                    use dialoguer::{FuzzySelect, Input};
                    let provider_ref = path
                        .split('.')
                        .nth(3)
                        .map(|alias| format!("{provider_type}.{alias}"));
                    let catalog_selector = provider_ref.as_deref().unwrap_or(provider_type);
                    let (models, _pricing, live) =
                        zeroclaw_runtime::quickstart::model_catalog_with_config(
                            Some(&config),
                            catalog_selector,
                        )
                        .await;
                    if live && !models.is_empty() {
                        let current = config.get_prop(&path).unwrap_or_default();
                        let default = models.iter().position(|m| m == &current).unwrap_or(0);
                        let Some(idx) = FuzzySelect::new()
                            .with_prompt(format!("Model id for {provider_type}"))
                            .items(&models)
                            .default(default)
                            .max_length(models.len().max(1))
                            .interact_opt()?
                        else {
                            anyhow::bail!("cancelled");
                        };
                        config.set_prop_persistent(&path, &models[idx])?;
                    } else {
                        eprintln!(
                            "  no live catalog for `{provider_type}` — \
                             enter the model id manually."
                        );
                        let m = Input::<String>::new()
                            .with_prompt(format!("Model id for {provider_type}"))
                            .allow_empty(false)
                            .interact_text()?;
                        config.set_prop_persistent(&path, &m)?;
                    }
                } else {
                    let field_info = config.prop_fields().into_iter().find(|f| f.name == path);
                    let variants = field_info.as_ref().and_then(|info| {
                        let get_variants = info.enum_variants?;
                        let variants = get_variants();
                        let current_index = variants
                            .iter()
                            .position(|v| v == &info.display_value)
                            .unwrap_or(0);
                        Some((variants, current_index))
                    });
                    if let Some((variants, current_index)) = variants {
                        let selected = Select::new()
                            .with_prompt(format!("Select value for {path}"))
                            .items(&variants)
                            .default(current_index)
                            .interact()?;
                        config.set_prop_persistent(&path, &variants[selected])?;
                    } else if field_info
                        .as_ref()
                        .is_some_and(|f| f.kind == crate::config::PropKind::StringArray)
                    {
                        let current_items: Vec<String> = field_info
                            .as_ref()
                            .and_then(|f| {
                                let raw = toml::from_str::<toml::Value>(&format!(
                                    "v = {}",
                                    if f.display_value == "<unset>" {
                                        "[]".to_string()
                                    } else {
                                        f.display_value.clone()
                                    }
                                ))
                                .ok();
                                raw.and_then(|v| v.get("v").cloned())
                                    .and_then(|v| v.as_array().cloned())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                            .collect()
                                    })
                            })
                            .unwrap_or_default();
                        let editor_content = current_items.join("\n");
                        let edited = dialoguer::Editor::new()
                            .edit(&editor_content)?
                            .unwrap_or(editor_content);
                        let val = edited
                            .lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(", ");
                        config.set_prop_persistent(&path, &val)?;
                    } else {
                        anyhow::bail!("Value required. Usage: zeroclaw config set {path} <value>");
                    }
                }
                Box::pin(config.save_dirty()).await?;
                if let Some(c) = comment.as_ref()
                    && !c.is_empty()
                {
                    apply_comment_inline(&config.config_path, &path, c).await?;
                }
                if json {
                    let envelope = if Config::prop_is_secret(&path) {
                        serde_json::json!({"path": path, "populated": true})
                    } else {
                        let value_str = config.get_prop(&path).unwrap_or_default();
                        serde_json::json!({"path": path, "value": value_str})
                    };
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                } else {
                    println!(
                        "{}",
                        ta("cli-config-updated", &[("path", &path)], "updated")
                    );
                }
                Ok(())
            }
            ConfigCommands::Init { section, json } => {
                crate::config::migration::ensure_disk_at_current_version(&config.config_path)?;
                let initialized: Vec<String> = config
                    .init_defaults(section.as_deref())
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                if !initialized.is_empty() {
                    for section in &initialized {
                        config.mark_dirty(section);
                    }
                    Box::pin(config.save_dirty()).await?;
                }
                if json {
                    let envelope = serde_json::json!({"initialized": initialized});
                    println!("{}", serde_json::to_string_pretty(&envelope)?);
                } else if initialized.is_empty() {
                    println!(
                        "{}",
                        t(
                            "cli-config-all-configured",
                            "All sections already configured."
                        )
                    );
                } else {
                    println!(
                        "Initialized {} section(s) with defaults:",
                        initialized.len()
                    );
                    for name in &initialized {
                        println!("  {name}");
                    }
                    println!(
                        "\n{}",
                        t(
                            "cli-config-review-hint",
                            "Run `zeroclaw config list` to review, then set required fields."
                        )
                    );
                }
                Ok(())
            }
            ConfigCommands::Migrate { json } => {
                match crate::config::migration::migrate_file_in_place(&config.config_path)? {
                    Some(report) => {
                        let to = report.to_version;
                        if json {
                            let envelope = serde_json::json!({
                                "migrated": true,
                                "backup_path": report.backup_path.display().to_string(),
                                "schema_version": to,
                            });
                            println!("{}", serde_json::to_string_pretty(&envelope)?);
                        } else {
                            println!(
                                "{}",
                                ta(
                                    "cli-config-backed-up",
                                    &[("path", &report.backup_path.display().to_string())],
                                    "Backed up to"
                                )
                            );
                            println!(
                                "Migrated {} to schema version {to}.",
                                config.config_path.display()
                            );
                        }
                    }
                    None => {
                        let strict_error = std::fs::read_to_string(&config.config_path)
                            .ok()
                            .and_then(|raw| {
                                crate::config::migration::migrate_to_current(&raw)
                                    .err()
                                    .map(|e| format!("{e:#}"))
                            });
                        if json {
                            let envelope = serde_json::json!({
                                "migrated": false,
                                "schema_version": crate::config::migration::CURRENT_SCHEMA_VERSION,
                                "valid": strict_error.is_none(),
                                "error": strict_error,
                            });
                            println!("{}", serde_json::to_string_pretty(&envelope)?);
                            if strict_error.is_some() {
                                std::process::exit(1);
                            }
                        } else {
                            println!(
                                "{}",
                                t(
                                    "cli-config-schema-current",
                                    "Config already at current schema version."
                                )
                            );
                            if let Some(error) = strict_error {
                                anyhow::bail!(
                                    "config at {} does not deserialize strictly; the resilient \
                                     loader is substituting defaults for the failing section. \
                                     Parse error: {error}",
                                    config.config_path.display()
                                );
                            }
                        }
                    }
                }
                Ok(())
            }
            ConfigCommands::Patch { input, json } => {
                crate::config::migration::ensure_disk_at_current_version(&config.config_path)?;
                let body = match input.as_deref() {
                    None | Some("-") => {
                        use std::io::Read;
                        let mut buf = String::new();
                        if let Err(err) = std::io::stdin().read_to_string(&mut buf) {
                            let api_err = ConfigApiError::new(
                                ConfigApiCode::InternalError,
                                format!("failed to read JSON Patch from stdin: {err}"),
                            );
                            config_patch_fail_json_or_human(
                                json,
                                api_err,
                                format!("Failed to read JSON Patch from stdin: {err}"),
                            )?;
                        }
                        buf
                    }
                    Some(path) => match tokio::fs::read_to_string(path).await {
                        Ok(body) => body,
                        Err(err) => {
                            let api_err = ConfigApiError::new(
                                ConfigApiCode::InternalError,
                                format!("failed to read JSON Patch from {path}: {err}"),
                            );
                            config_patch_fail_json_or_human(
                                json,
                                api_err,
                                format!("Failed to read JSON Patch from {path}: {err}"),
                            )?
                        }
                    },
                };

                let parsed: serde_json::Value = match serde_json::from_str(body.trim()) {
                    Ok(parsed) => parsed,
                    Err(err) => {
                        let api_err = config_patch_json_value_type_error(
                            format!("JSON Patch body must be valid JSON: {err}"),
                            None,
                            None,
                        );
                        config_patch_fail_json_or_human(
                            json,
                            api_err,
                            format!("JSON Patch body must be valid JSON: {err}"),
                        )?
                    }
                };
                let ops = match parsed.as_array() {
                    Some(ops) => ops,
                    None => {
                        let api_err = config_patch_json_value_type_error(
                            "JSON Patch body must be a JSON array of operations",
                            None,
                            None,
                        );
                        config_patch_fail_json_or_human(
                            json,
                            api_err,
                            "JSON Patch body must be a JSON array of operations",
                        )?
                    }
                };

                let mut results: Vec<serde_json::Value> = Vec::with_capacity(ops.len());

                for (idx, op) in ops.iter().enumerate() {
                    let object = match op.as_object() {
                        Some(object) => object,
                        None => {
                            let message = format!("JSON Patch op[{idx}] must be an object");
                            let api_err = config_patch_json_value_type_error(
                                message.clone(),
                                None,
                                Some(idx),
                            );
                            config_patch_fail_json_or_human(json, api_err, message)?
                        }
                    };
                    let op_name = match object.get("op").and_then(|v| v.as_str()) {
                        Some(op_name) => op_name,
                        None => {
                            let message =
                                format!("JSON Patch op[{idx}] requires string `op` field");
                            let api_err = config_patch_json_value_type_error(
                                message.clone(),
                                None,
                                Some(idx),
                            );
                            config_patch_fail_json_or_human(json, api_err, message)?
                        }
                    };
                    let raw_path = match object.get("path").and_then(|v| v.as_str()) {
                        Some(raw_path) => raw_path,
                        None => {
                            let message =
                                format!("JSON Patch op[{idx}] requires string `path` field");
                            let api_err = config_patch_json_value_type_error(
                                message.clone(),
                                None,
                                Some(idx),
                            );
                            config_patch_fail_json_or_human(json, api_err, message)?
                        }
                    };
                    let path = if let Some(stripped) = raw_path.strip_prefix('/') {
                        stripped.replace('/', ".")
                    } else {
                        raw_path.to_string()
                    };
                    if matches!(op_name, "add" | "replace") && config.ensure_map_key_for_path(&path)
                    {
                        let err = ConfigApiError::new(
                            ConfigApiCode::ValidationFailed,
                            "alias `default` is reserved and cannot be created",
                        )
                        .with_path(&path)
                        .with_op_index(idx);
                        let human = format!(
                            "op[{idx}] `{op_name}` on `{path}`: alias `default` is reserved and cannot be created"
                        );
                        config_patch_fail_json_or_human(json, err, human)?;
                    }
                    let comment = match object.get("comment") {
                        Some(value) => match value.as_str() {
                            Some(comment) => Some(comment),
                            None => {
                                let message = format!(
                                    "JSON Patch op[{idx}] `comment` field must be a string"
                                );
                                let api_err = config_patch_json_value_type_error(
                                    message.clone(),
                                    Some(path.clone()),
                                    Some(idx),
                                );
                                config_patch_fail_json_or_human(json, api_err, message)?
                            }
                        },
                        None => None,
                    };
                    let is_secret = Config::prop_is_secret(&path);

                    let result_entry: serde_json::Value = match op_name {
                        "add" | "replace" => {
                            let value = op.get("value").ok_or_else(|| {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Reject
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "op": op_name,
                                            "op_index": idx,
                                            "path": path,
                                        })
                                    ),
                                    "config patch op rejected: missing `value` field"
                                );
                                anyhow::Error::msg(format!(
                                    "op[{idx}] `{op_name}` on `{path}`: missing `value` field"
                                ))
                            })?;
                            let value_str = json_value_to_setprop_string(value, &config, &path)?;
                            match config.set_prop_persistent(&path, &value_str) {
                                Ok(()) => {}
                                Err(err) => {
                                    let api_err = config_patch_map_prop_error(err, &path, idx);
                                    let human = format!(
                                        "op[{idx}] `{op_name}` on `{path}` failed: {}",
                                        api_err.message
                                    );
                                    config_patch_fail_json_or_human(json, api_err, human)?;
                                }
                            }
                            if is_secret {
                                serde_json::json!({
                                    "op": op_name,
                                    "path": path,
                                    "populated": !value_str.is_empty(),
                                })
                            } else {
                                serde_json::json!({
                                    "op": op_name,
                                    "path": path,
                                    "value": value_str,
                                })
                            }
                        }
                        "remove" => {
                            match config.set_prop_persistent(&path, "") {
                                Ok(()) => {}
                                Err(err) => {
                                    let api_err = config_patch_map_prop_error(err, &path, idx);
                                    let human = format!(
                                        "op[{idx}] `remove` on `{path}` failed: {}",
                                        api_err.message
                                    );
                                    config_patch_fail_json_or_human(json, api_err, human)?;
                                }
                            }
                            if is_secret {
                                serde_json::json!({
                                    "op": "remove",
                                    "path": path,
                                    "populated": false,
                                })
                            } else {
                                serde_json::json!({
                                    "op": "remove",
                                    "path": path,
                                    "value": serde_json::Value::Null,
                                })
                            }
                        }
                        "test" => {
                            if is_secret {
                                let err =
                                    ConfigApiError::secret_test_forbidden(&path).with_op_index(idx);
                                let human = format!(
                                    "op[{idx}] `test` on `{path}`: secret_test_forbidden \
                                     \u{2014} test ops are not allowed against secret paths"
                                );
                                config_patch_fail_json_or_human(json, err, human)?;
                            }
                            let want = match op.get("value") {
                                Some(value) => value,
                                None => {
                                    let err = ConfigApiError::new(
                                        ConfigApiCode::ValueTypeMismatch,
                                        "JSON Patch `test` op requires `value` field",
                                    )
                                    .with_path(&path)
                                    .with_op_index(idx);
                                    let human = format!(
                                        "op[{idx}] `test` on `{path}`: missing `value` field"
                                    );
                                    config_patch_fail_json_or_human(json, err, human)?
                                }
                            };
                            let actual = match config.get_prop(&path) {
                                Ok(actual) => actual,
                                Err(err) => {
                                    let human = format!(
                                        "op[{idx}] `test` on `{path}` failed to read current value: {err}"
                                    );
                                    let api_err = config_patch_map_prop_error(err, &path, idx);
                                    config_patch_fail_json_or_human(json, api_err, human)?
                                }
                            };
                            let want_str = match zeroclaw_config::typed_value::coerce_for_set_prop(
                                want,
                                config_patch_prop_kind(&config, &path),
                            ) {
                                Ok(want_str) => want_str,
                                Err(err) => {
                                    let err = err.with_path(&path).with_op_index(idx);
                                    config_patch_fail_json_or_human(
                                        json,
                                        err.clone(),
                                        err.message.clone(),
                                    )?
                                }
                            };
                            if actual != want_str {
                                let err = ConfigApiError::new(
                                    ConfigApiCode::ValidationFailed,
                                    format!(
                                        "`test` op failed: expected {want_str:?}, got {actual:?}"
                                    ),
                                )
                                .with_path(&path)
                                .with_op_index(idx);
                                let human = format!(
                                    "op[{idx}] `test` on `{path}` failed: expected {want_str}, got {actual}"
                                );
                                config_patch_fail_json_or_human(json, err, human)?;
                            }
                            serde_json::json!({
                                "op": "test",
                                "path": path,
                                "value": actual,
                            })
                        }
                        "move" | "copy" => {
                            let err = ConfigApiError::op_not_supported(op_name)
                                .with_path(&path)
                                .with_op_index(idx);
                            let human = format!(
                                "op[{idx}] `{op_name}` on `{path}`: op_not_supported \
                                 \u{2014} move/copy require a reference graph that is not built yet"
                            );
                            config_patch_fail_json_or_human(json, err, human)?
                        }
                        other => {
                            let err = ConfigApiError::new(
                                ConfigApiCode::OpNotSupported,
                                format!("unknown JSON Patch operation `{other}`"),
                            )
                            .with_path(&path)
                            .with_op_index(idx);
                            let human = format!("op[{idx}] unknown JSON Patch operation `{other}`");
                            config_patch_fail_json_or_human(json, err, human)?
                        }
                    };
                    results.push(result_entry);
                }

                if let Err(err) = config.validate() {
                    let api_err = ConfigApiError::from_validation(err);
                    let human = format!(
                        "validation failed after applying patch \u{2014} no changes saved: {}",
                        api_err.message
                    );
                    config_patch_fail_json_or_human(json, api_err, human)?;
                }
                Box::pin(config.save_dirty()).await?;

                if json {
                    let body = serde_json::json!({"saved": true, "results": results});
                    println!("{}", serde_json::to_string_pretty(&body)?);
                } else {
                    println!(
                        "{}",
                        ta(
                            "cli-config-applied-ops",
                            &[("count", &results.len().to_string())],
                            "Applied operations"
                        )
                    );
                    for entry in &results {
                        let op = entry.get("op").and_then(|v| v.as_str()).unwrap_or("?");
                        let path = entry.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        if let Some(populated) = entry.get("populated").and_then(|v| v.as_bool()) {
                            let lock = "\u{1f512}";
                            let label = if populated { "set" } else { "unset" };
                            println!("  {op:<8} {path}  {lock} ({label})");
                        } else {
                            let value = entry
                                .get("value")
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "null".to_string());
                            println!("  {op:<8} {path} = {value}");
                        }
                    }
                }
                Ok(())
            }
            ConfigCommands::Docs => {
                let port = config.gateway.port;
                let host = if config.gateway.host == "[::]" || config.gateway.host == "0.0.0.0" {
                    "127.0.0.1".to_string()
                } else {
                    config.gateway.host.clone()
                };
                let url = format!("http://{host}:{port}/api/docs");

                let health = format!("http://{host}:{port}/health");
                let daemon_running = reqwest::Client::new()
                    .get(&health)
                    .timeout(std::time::Duration::from_secs(2))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);

                println!("{url}");
                if !daemon_running {
                    eprintln!(
                        "Note: gateway does not appear to be running at {host}:{port}. \
                         Start it with `zeroclaw service start` (background) or `zeroclaw daemon` (foreground) to load the explorer."
                    );
                }
                Ok(())
            }
            ConfigCommands::Complete { partial } => {
                let prefix = partial.as_deref().unwrap_or("");
                for entry in config.prop_fields() {
                    if entry.name.starts_with(prefix) {
                        println!("{}", entry.name);
                    }
                }
                Ok(())
            }
            ConfigCommands::Generate { version, encrypt } => {
                let target = version.unwrap_or(crate::config::migration::CURRENT_SCHEMA_VERSION);
                let zeroclaw_dir = config
                    .config_path
                    .parent()
                    .map(std::path::Path::to_path_buf);
                let opts = crate::config::migration::GenerateOptions {
                    encrypt_secrets: encrypt,
                    secret_store_dir: zeroclaw_dir.as_deref(),
                };
                let toml_out = crate::config::migration::generate(target, &opts)?;
                print!("{toml_out}");
                Ok(())
            }
        },

        Commands::Props { .. } => {
            anyhow::bail!(
                "`zeroclaw props` has been renamed to `zeroclaw config`. \
                 Replace `props` with `config` in your command and try again."
            );
        }

        #[cfg(feature = "plugins-wasm")]
        Commands::Plugin { plugin_command } => match plugin_command {
            PluginCommands::List => {
                let host = plugin_host_with_configured_security(&config)?;
                let plugins = host.list_plugins();
                if plugins.is_empty() {
                    println!("{}", t("cli-plugins-none", "No plugins installed."));
                } else {
                    println!("{}", t("cli-plugins-installed", "Installed plugins:"));
                    for p in &plugins {
                        println!(
                            "  {} v{} — {}",
                            p.name,
                            p.version,
                            p.description.as_deref().unwrap_or("(no description)")
                        );
                    }
                }
                let target = config.plugins.resolved_plugins_dir().display().to_string();
                for legacy in crate::config::schema::legacy_plugin_dirs_with_entries(&config) {
                    eprintln!(
                        "{}",
                        ta(
                            "cli-plugin-legacy-detected",
                            &[("path", &legacy.display().to_string()), ("target", &target)],
                            "Note: plugins in a legacy location are not loaded by the agent — \
                             run `zeroclaw plugin migrate` to move them.",
                        )
                    );
                }
                Ok(())
            }
            PluginCommands::Search { query, registry } => {
                let registry_url = plugin_registry::registry_url(registry.as_deref());
                let index = plugin_registry::fetch_registry_index(&registry_url).await?;
                zeroclaw::plugins::registry::write_cached_registry_index(
                    &config.data_dir,
                    &registry_url,
                    &index,
                )?;
                let matches = plugin_registry::search_entries(&index, &query);
                if matches.is_empty() {
                    println!(
                        "{}",
                        ta(
                            "cli-plugin-search-none",
                            &[("query", &query)],
                            "No matching plugins."
                        )
                    );
                } else {
                    println!(
                        "{}",
                        ta(
                            "cli-plugin-search-results",
                            &[("query", &query), ("count", &matches.len().to_string())],
                            "Plugins matching query:"
                        )
                    );
                    for plugin in &matches {
                        let missing_description;
                        let description = if let Some(description) = plugin.description.as_deref() {
                            description
                        } else {
                            missing_description =
                                t("cli-plugin-no-description", "(no description)");
                            &missing_description
                        };
                        println!(
                            "{}",
                            ta(
                                "cli-plugin-search-result",
                                &[
                                    ("name", &plugin.name),
                                    ("version", &plugin.version),
                                    ("description", description),
                                ],
                                "Plugin search result"
                            )
                        );
                    }
                }
                Ok(())
            }
            PluginCommands::Install { source, registry } => {
                if plugin_registry::looks_like_url(&source) {
                    bail!(
                        "`zeroclaw plugin install <url>` is not supported; use `--registry <url>` with a plugin name, or install a local plugin path"
                    );
                }
                let mut host = plugin_host_with_configured_security(&config)?;
                if plugin_registry::is_local_plugin_source(&source) {
                    let name = host.install(&source)?;
                    println!(
                        "{}",
                        ta(
                            "cli-plugin-installed-from",
                            &[("source", &source)],
                            "Plugin installed"
                        )
                    );
                    Box::pin(seed_plugin_config_entry(&mut config, &name)).await?;
                } else {
                    let registry_url = plugin_registry::registry_url(registry.as_deref());
                    println!(
                        "{}",
                        ta(
                            "cli-plugin-install-resolving",
                            &[("source", &source)],
                            "Resolving plugin from registry..."
                        )
                    );
                    let downloaded = plugin_registry::download_registry_plugin(
                        &registry_url,
                        &source,
                        Some(&config.data_dir),
                    )
                    .await?;
                    let plugin_dir = downloaded.plugin_dir().display().to_string();
                    let name = host.install(&plugin_dir)?;
                    println!(
                        "{}",
                        ta(
                            "cli-plugin-installed-name-version",
                            &[
                                ("name", &downloaded.manifest().name),
                                ("version", &downloaded.manifest().version),
                            ],
                            "Plugin installed"
                        )
                    );
                    Box::pin(seed_plugin_config_entry(&mut config, &name)).await?;
                }
                Ok(())
            }
            PluginCommands::Remove { name } => {
                let mut host = plugin_host_with_configured_security(&config)?;
                host.remove(&name)?;
                println!(
                    "{}",
                    ta("cli-plugin-removed", &[("name", &name)], "Plugin removed")
                );
                Ok(())
            }
            PluginCommands::Info { name } => {
                let host = plugin_host_with_configured_security(&config)?;
                match host.get_plugin(&name) {
                    Some(info) => {
                        println!(
                            "{}",
                            ta(
                                "cli-plugin-name-version",
                                &[("name", &info.name), ("version", &info.version)],
                                "Plugin"
                            )
                        );
                        if let Some(desc) = &info.description {
                            println!(
                                "{}",
                                ta("cli-plugin-description", &[("desc", desc)], "Description")
                            );
                        }
                        println!(
                            "{}",
                            ta(
                                "cli-plugin-capabilities",
                                &[("v", &format!("{:?}", info.capabilities))],
                                "Capabilities"
                            )
                        );
                        println!(
                            "{}",
                            ta(
                                "cli-plugin-permissions",
                                &[("v", &format!("{:?}", info.permissions))],
                                "Permissions"
                            )
                        );
                        match &info.wasm_path {
                            Some(path) => println!(
                                "{}",
                                ta(
                                    "cli-plugin-wasm",
                                    &[("path", &path.display().to_string())],
                                    "WASM"
                                )
                            ),
                            None => println!(
                                "{}",
                                t("cli-plugin-wasm-none", "WASM: (skill-only plugin)")
                            ),
                        }
                    }
                    None => println!(
                        "{}",
                        ta(
                            "cli-plugin-not-found",
                            &[("name", &name)],
                            "Plugin not found"
                        )
                    ),
                }
                Ok(())
            }
            PluginCommands::Migrate => {
                let target = config.plugins.resolved_plugins_dir();
                let target_str = target.display().to_string();
                let legacy_dirs = crate::config::schema::legacy_plugin_dirs_with_entries(&config);
                let mut total = 0usize;
                for legacy in &legacy_dirs {
                    let moved = zeroclaw::plugins::host::migrate_plugins_dir(legacy, &target)?;
                    if moved > 0 {
                        println!(
                            "{}",
                            ta(
                                "cli-plugin-migrated",
                                &[
                                    ("count", &moved.to_string()),
                                    ("path", &legacy.display().to_string()),
                                    ("target", &target_str),
                                ],
                                "Migrated plugins from a legacy location.",
                            )
                        );
                    }
                    total += moved;
                }
                if total == 0 {
                    println!("{}", t("cli-plugin-migrate-none", "Nothing to migrate."));
                }
                Ok(())
            }
        },
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

// ─── Gateway helper functions ───────────────────────────────────────────────

/// Resolve gateway host and port from CLI args or config.
fn resolve_gateway_addr(config: &Config, port: Option<u16>, host: Option<String>) -> (u16, String) {
    let port = port.unwrap_or(config.gateway.port);
    let host = host.unwrap_or_else(|| config.gateway.host.clone());
    (port, host)
}

/// Log gateway startup message.
fn log_gateway_start(host: &str, port: u16) {
    if port == 0 {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"host": host})),
            "🚀 Starting ZeroClaw Gateway on (random port)"
        );
    } else {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"host": host, "port": port})),
            "🚀 Starting ZeroClaw Gateway on"
        );
    }
}

/// Gracefully shutdown a running gateway via the admin endpoint.
#[cfg(feature = "agent-runtime")]
async fn shutdown_gateway(host: &str, port: u16, path_prefix: Option<&str>) -> Result<()> {
    let url = gateway_admin_url(host, port, path_prefix, "/admin/shutdown");
    let client = reqwest::Client::new();

    match client
        .post(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => {
            let status = response.status();
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"endpoint": url, "status": status.as_u16()})),
                "gateway admin shutdown returned non-success status"
            );
            Err(anyhow::Error::msg(format!(
                "Gateway responded with status: {status}"
            )))
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"endpoint": url, "error": format!("{}", e)})),
                "gateway admin shutdown: connect failed"
            );
            Err(anyhow::Error::msg(format!(
                "Failed to connect to gateway: {e}"
            )))
        }
    }
}

/// Dispatch the gateway-backed SOP verbs. Requires the `agent-runtime` build (the
/// gateway HTTP client + `gateway_admin_url` live behind it, like `shutdown_gateway`);
/// without it these verbs cannot reach the daemon, so they error clearly.
async fn sop_admin_dispatch(cmd: SopCommands, config: &crate::config::Config) -> Result<()> {
    #[cfg(feature = "agent-runtime")]
    {
        sop_admin_request(cmd, config).await
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        let _ = (cmd, config);
        anyhow::bail!(
            "`zeroclaw sop approve/deny/pending` requires the agent-runtime build (the gateway client)"
        )
    }
}

/// CLI -> daemon dispatch for the out-of-band SOP approval verbs (EPIC C, C8).
/// Posts to `/admin/sop/*` on the running gateway (mirrors `shutdown_gateway`);
/// never builds a throwaway local engine, which cannot see the daemon's runs.
#[cfg(feature = "agent-runtime")]
async fn sop_admin_request(cmd: SopCommands, config: &crate::config::Config) -> Result<()> {
    let host = config.gateway.host.clone();
    let port = config.gateway.port;
    let prefix = config.gateway.path_prefix.as_deref();
    let client = reqwest::Client::new();
    match cmd {
        SopCommands::Pending => {
            let url = gateway_admin_url(&host, port, prefix, "/admin/sop/pending");
            let resp = client
                .get(&url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
                .map_err(|e| anyhow::Error::msg(format!("Failed to connect to gateway: {e}")))?;
            let status = resp.status();
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if !status.is_success() {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("request failed");
                anyhow::bail!("Gateway responded {status}: {err}");
            }
            let pending = body
                .get("pending")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default();
            if pending.is_empty() {
                println!(
                    "{}",
                    t("cli-sop-pending-none", "No SOP runs waiting for approval.")
                );
            } else {
                println!(
                    "{}",
                    t("cli-sop-pending-header", "SOP runs waiting for approval:")
                );
                for r in pending {
                    let run_id = r.get("run_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let sop_name = r.get("sop_name").and_then(|v| v.as_str()).unwrap_or("?");
                    let step = r
                        .get("step")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                        .to_string();
                    let total = r
                        .get("total_steps")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0)
                        .to_string();
                    println!(
                        "{}",
                        ta(
                            "cli-sop-pending-row",
                            &[
                                ("run_id", run_id),
                                ("sop_name", sop_name),
                                ("step", &step),
                                ("total", &total),
                            ],
                            "  (sop run)",
                        )
                    );
                }
            }
            Ok(())
        }
        SopCommands::Approve { run_id } => {
            let url = gateway_admin_url(&host, port, prefix, "/admin/sop/approve");
            sop_admin_post(&client, &url, serde_json::json!({ "run_id": run_id })).await
        }
        SopCommands::Deny { run_id, reason } => {
            let url = gateway_admin_url(&host, port, prefix, "/admin/sop/deny");
            sop_admin_post(
                &client,
                &url,
                serde_json::json!({ "run_id": run_id, "reason": reason }),
            )
            .await
        }
        // List/Validate/Show are dispatched on the local synchronous path.
        _ => unreachable!("local SOP verbs are handled by sop::handle_command"),
    }
}

/// POST a JSON body to a gateway SOP admin endpoint and report the outcome.
#[cfg(feature = "agent-runtime")]
async fn sop_admin_post(
    client: &reqwest::Client,
    url: &str,
    body: serde_json::Value,
) -> Result<()> {
    let resp = client
        .post(url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| anyhow::Error::msg(format!("Failed to connect to gateway: {e}")))?;
    let status = resp.status();
    let out: serde_json::Value = resp.json().await.unwrap_or_default();
    if status.is_success() {
        println!(
            "{}",
            out.get("outcome").and_then(|v| v.as_str()).unwrap_or("ok")
        );
        Ok(())
    } else {
        // Non-2xx bodies from the SOP routes carry the typed `outcome` label
        // (e.g. not_waiting -> 404, rejected_self_approval -> 403), not `error`;
        // prefer it so the operator sees why, falling back to `error`.
        let detail = out
            .get("outcome")
            .and_then(|v| v.as_str())
            .or_else(|| out.get("error").and_then(|v| v.as_str()))
            .unwrap_or("request failed");
        anyhow::bail!("Gateway responded {status}: {detail}");
    }
}

#[cfg(feature = "agent-runtime")]
enum PaircodeAction {
    /// GET the current code; do not mint or revoke anything.
    Show,
    /// Issue a fresh code for an additional client; revoke nothing.
    AddClient,
    /// Revoke every paired token + clear the registry, then issue a code.
    RotateAll,
    /// Revoke a single device's token, then issue a code.
    RotateDevice(String),
}

#[cfg(feature = "agent-runtime")]
impl PaircodeAction {
    /// True when the action mints a new code (POST), false for `Show` (GET).
    fn mints_code(&self) -> bool {
        !matches!(self, PaircodeAction::Show)
    }

    /// True when the action revokes existing tokens.
    fn is_rotation(&self) -> bool {
        matches!(
            self,
            PaircodeAction::RotateAll | PaircodeAction::RotateDevice(_)
        )
    }

    /// The `rotate` query value to send, if any.
    fn rotate_query(&self) -> Option<String> {
        match self {
            PaircodeAction::RotateAll => Some("all".to_string()),
            PaircodeAction::RotateDevice(id) => Some(id.clone()),
            PaircodeAction::Show | PaircodeAction::AddClient => None,
        }
    }
}

/// Outcome of a `get-paircode` request.
#[cfg(feature = "agent-runtime")]
enum PaircodeResult {
    /// A code was returned (with an optional human-readable message).
    Code {
        code: String,
        message: Option<String>,
    },
    /// No code is available (with an optional explanatory message from the
    /// gateway, e.g. a revoke that succeeded but could not issue a code).
    NoCode { message: Option<String> },
}

#[cfg(feature = "agent-runtime")]
async fn fetch_paircode(
    host: &str,
    port: u16,
    path_prefix: Option<&str>,
    action: &PaircodeAction,
) -> Result<PaircodeResult> {
    let client = reqwest::Client::new();

    let response = if action.mints_code() {
        let mut url = gateway_admin_url(host, port, path_prefix, "/admin/paircode/new");
        if let Some(rotate) = action.rotate_query() {
            url.push_str("?rotate=");
            url.push_str(&urlencoding::encode(&rotate));
        }
        client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
    } else {
        let url = gateway_admin_url(host, port, path_prefix, "/admin/paircode");
        client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
    };

    let response = response.map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
            "gateway paircode fetch: connect failed"
        );
        anyhow::Error::msg(format!("Failed to connect to gateway: {e}"))
    })?;

    let status = response.status();
    let json: serde_json::Value = response.json().await.map_err(|e| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(
                    ::serde_json::json!({"error": format!("{}", e), "status": status.as_u16()})
                ),
            "gateway paircode response: JSON parse failed"
        );
        anyhow::Error::msg(format!("Gateway responded with status {status}: {e}"))
    })?;

    let message = json
        .get("message")
        .and_then(|v| v.as_str())
        .map(String::from);

    if json.get("success").and_then(|v| v.as_bool()) != Some(true) {
        if !status.is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"status": status.as_u16()})),
                "gateway paircode fetch returned non-success status"
            );
        }
        return Ok(PaircodeResult::NoCode { message });
    }

    match json.get("pairing_code").and_then(|v| v.as_str()) {
        Some(code) => Ok(PaircodeResult::Code {
            code: code.to_string(),
            message,
        }),
        None => Ok(PaircodeResult::NoCode { message }),
    }
}

#[cfg(feature = "agent-runtime")]
fn gateway_admin_url(host: &str, port: u16, path_prefix: Option<&str>, admin_path: &str) -> String {
    let prefix = path_prefix.unwrap_or("");
    format!("http://{host}:{port}{prefix}{admin_path}")
}

#[cfg(feature = "agent-runtime")]
fn paircode_no_code_message(
    host: &str,
    port: u16,
    default_host: &str,
    default_port: u16,
    action: &PaircodeAction,
    require_pairing: bool,
    gateway_message: Option<&str>,
) -> String {
    let mut lines = Vec::new();

    if let Some(message) = gateway_message.filter(|m| !m.trim().is_empty()) {
        lines.push(format!("⚠️  {message}"));
    } else if require_pairing {
        lines
            .push("🔐 Gateway pairing is enabled, but no active pairing code is available.".into());
    } else {
        lines.push(t(
            "cli-pairing-disabled",
            "⚠️  Gateway pairing is disabled in config.",
        ));
        lines.push("All requests will be accepted without authentication.".into());
        lines.push("To enable pairing, set [gateway] require_pairing = true.".into());
        return indent_paircode_lines(lines);
    }

    lines.push(String::new());
    match action {
        PaircodeAction::Show => {
            lines.push(
                "`zeroclaw gateway get-paircode` only displays an existing active code; it does not mint a new one."
                    .into(),
            );
            lines.push("To pair another device, run:".into());
            lines.push(paircode_command(
                host,
                port,
                default_host,
                default_port,
                Some("--new"),
            ));
            lines.push(String::new());
            lines.push("To revoke existing pairings and mint a replacement code, run:".into());
            lines.push(paircode_command(
                host,
                port,
                default_host,
                default_port,
                Some("--rotate"),
            ));
        }
        PaircodeAction::AddClient => {
            lines.push(
                "The gateway did not mint a new pairing code. A code may already be pending, or pairing may need a reset."
                    .into(),
            );
            lines.push(
                "Try again shortly, or revoke existing pairings and mint a replacement code:"
                    .into(),
            );
            lines.push(paircode_command(
                host,
                port,
                default_host,
                default_port,
                Some("--rotate"),
            ));
        }
        PaircodeAction::RotateAll | PaircodeAction::RotateDevice(_) => {
            lines.push("The rotate request completed without returning a replacement code.".into());
            lines.push("Check whether pairing is enabled, then request a new device code:".into());
            lines.push(paircode_command(
                host,
                port,
                default_host,
                default_port,
                Some("--new"),
            ));
        }
    }

    lines.push(String::new());
    lines.push("To inspect the running gateway:".into());
    lines.push(format!("    open http://{host}:{port}"));
    indent_paircode_lines(lines)
}

#[cfg(feature = "agent-runtime")]
fn paircode_command(
    host: &str,
    port: u16,
    default_host: &str,
    default_port: u16,
    flag: Option<&str>,
) -> String {
    let mut command = "    zeroclaw gateway get-paircode".to_string();
    if let Some(flag) = flag {
        command.push(' ');
        command.push_str(flag);
    }
    if port != default_port {
        write!(command, " --port {port}").expect("writing to String cannot fail");
    }
    if host != default_host {
        write!(command, " --host {host}").expect("writing to String cannot fail");
    }
    command
}

#[cfg(feature = "agent-runtime")]
fn indent_paircode_lines(lines: Vec<String>) -> String {
    lines
        .into_iter()
        .map(|line| {
            if line.starts_with("    ") || line.is_empty() {
                line
            } else {
                format!("  {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Interactive CLI input helpers used by `auth paste-token` /
// `auth setup-token` / `auth paste-redirect`. The dialoguer dep belongs
// to the binary; auth/mod.rs in zeroclaw-providers shouldn't pull it in,
// so reads live here and trait flows accept the resulting string.

#[cfg(feature = "agent-runtime")]
fn read_auth_input(prompt: &str) -> Result<String> {
    let input = Password::new()
        .with_prompt(prompt)
        .allow_empty_password(false)
        .interact()?;
    Ok(input.trim().to_string())
}

#[cfg(feature = "agent-runtime")]
fn read_plain_input(prompt: &str) -> Result<String> {
    let input: String = cli_input::Input::new()
        .with_prompt(prompt)
        .interact_text()?;
    Ok(input.trim().to_string())
}

#[cfg(feature = "agent-runtime")]
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

#[cfg(feature = "agent-runtime")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum InlineProviderAuth {
    Codex,
    AnthropicSetupToken { alias: String },
}

#[cfg(feature = "agent-runtime")]
fn quickstart_field_value_eq(
    fields: &std::collections::HashMap<String, String>,
    key: &str,
    expected: &str,
) -> bool {
    fields
        .get(key)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

#[cfg(feature = "agent-runtime")]
fn quickstart_inline_auth(
    kind: &str,
    alias: &str,
    fields: &std::collections::HashMap<String, String>,
) -> Option<InlineProviderAuth> {
    if kind == "openai" && quickstart_field_value_eq(fields, "auth_mode", "codex") {
        return Some(InlineProviderAuth::Codex);
    }
    if kind == "anthropic" && quickstart_field_value_eq(fields, "auth_mode", "setup_token") {
        return Some(InlineProviderAuth::AnthropicSetupToken {
            alias: alias.to_string(),
        });
    }
    None
}

/// `~/.codex/auth.json` — the credential file the upstream Codex CLI writes.
/// When present, offer a direct import instead of starting a fresh browser flow.
#[cfg(feature = "agent-runtime")]
fn codex_auth_json_path() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join(".codex").join("auth.json"))
}

#[cfg(feature = "agent-runtime")]
async fn run_inline_provider_auth(auth: InlineProviderAuth, config: &mut Config) {
    use dialoguer::Confirm;

    let codex_import = match &auth {
        InlineProviderAuth::Codex => codex_auth_json_path().filter(|path| path.exists()),
        InlineProviderAuth::AnthropicSetupToken { .. } => None,
    };
    let (prompt, skip_hint) = match &auth {
        InlineProviderAuth::Codex => (
            if codex_import.is_some() {
                t(
                    "cli-quickstart-auth-codex-import-prompt",
                    "Found an existing Codex login (~/.codex/auth.json) — import it now?",
                )
            } else {
                t(
                    "cli-quickstart-auth-codex-prompt",
                    "Sign in to OpenAI Codex with your ChatGPT account now?",
                )
            },
            t(
                "cli-quickstart-auth-codex-skip-hint",
                "  Finish later with: zeroclaw auth login --model-provider openai-codex",
            ),
        ),
        InlineProviderAuth::AnthropicSetupToken { alias } => (
            ta(
                "cli-quickstart-auth-anthropic-prompt",
                &[("alias", alias)],
                "Run `claude setup-token` for this Anthropic provider now?",
            ),
            ta(
                "cli-quickstart-auth-anthropic-skip-hint",
                &[("alias", alias)],
                "  Finish later with: claude setup-token",
            ),
        ),
    };
    if !Confirm::new()
        .with_prompt(prompt)
        .default(true)
        .interact()
        .unwrap_or(false)
    {
        println!("{skip_hint}");
        return;
    }

    let result = match auth {
        InlineProviderAuth::Codex => {
            let cmd = AuthCommands::Login {
                model_provider: "openai-codex".to_string(),
                profile: "default".to_string(),
                device_code: false,
                import: codex_import,
            };
            handle_auth_command(cmd, config).await
        }
        InlineProviderAuth::AnthropicSetupToken { alias } => {
            Box::pin(run_anthropic_setup_token_inline(&alias, config)).await
        }
    };
    if let Err(error) = result {
        let error = error.to_string();
        eprintln!(
            "{}",
            ta(
                "cli-quickstart-auth-failed",
                &[("error", &error)],
                "  Auth setup didn't complete.",
            )
        );
        println!("{skip_hint}");
    }
}

#[cfg(feature = "agent-runtime")]
async fn run_anthropic_setup_token_inline(alias: &str, config: &mut Config) -> Result<()> {
    let status = tokio::process::Command::new("claude")
        .arg("setup-token")
        .status()
        .await
        .context("failed to run `claude setup-token`; is the Claude CLI installed and on PATH?")?;
    if !status.success() {
        bail!("`claude setup-token` exited with status {status}");
    }

    let token = read_auth_input(&t(
        "cli-quickstart-auth-anthropic-token-prompt",
        "Paste the token from `claude setup-token`",
    ))?;
    if token.trim().is_empty() {
        bail!("Token cannot be empty");
    }

    let path = format!("providers.models.anthropic.{alias}.api_key");
    config.set_prop_persistent(&path, token.trim())?;
    Box::pin(config.save_dirty()).await?;
    println!(
        "{}",
        ta(
            "cli-quickstart-auth-anthropic-saved",
            &[("alias", alias)],
            "  Saved Claude setup token.",
        )
    );
    Ok(())
}

#[allow(clippy::too_many_lines)]
#[cfg(feature = "agent-runtime")]
async fn handle_auth_command(auth_command: AuthCommands, config: &Config) -> Result<()> {
    let auth_service = auth::AuthService::from_config(config);
    let auth_cli_formatter =
        |key: &str, args: &[(&str, &str)], fallback: &str| ta(key, args, fallback);

    match auth_command {
        AuthCommands::Login {
            model_provider,
            profile,
            device_code,
            import,
        } => {
            let provider: auth::AuthProvider = model_provider.parse()?;
            let client = reqwest::Client::new();
            let ctx = auth::AuthFlowContext {
                config,
                auth_service: &auth_service,
                client: &client,
                format_cli: &auth_cli_formatter,
            };
            provider
                .flow()
                .login(&ctx, &profile, device_code, import.as_deref())
                .await
        }

        AuthCommands::PasteRedirect {
            model_provider,
            profile,
            input,
        } => {
            let provider: auth::AuthProvider = model_provider.parse()?;
            let client = reqwest::Client::new();
            let ctx = auth::AuthFlowContext {
                config,
                auth_service: &auth_service,
                client: &client,
                format_cli: &auth_cli_formatter,
            };
            let input_str: Option<String> = match input {
                Some(value) => Some(value),
                None => Some(read_plain_input("Paste redirect URL or OAuth code")?),
            };
            provider
                .flow()
                .paste_redirect(&ctx, &profile, input_str.as_deref())
                .await
        }

        AuthCommands::PasteToken {
            model_provider,
            profile,
            token,
            auth_kind,
        } => {
            let model_provider = auth::normalize_model_provider(&model_provider)?;
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
                .store_model_provider_token(&model_provider, &profile, &token, metadata, true)
                .await?;
            println!(
                "{}",
                ta("cli-auth-saved", &[("profile", &profile)], "Saved profile")
            );
            println!(
                "{}",
                ta(
                    "cli-auth-active-for",
                    &[("provider", &model_provider), ("profile", &profile)],
                    "Active profile"
                )
            );
            Ok(())
        }

        AuthCommands::SetupToken {
            model_provider,
            profile,
        } => {
            let model_provider = auth::normalize_model_provider(&model_provider)?;
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
                .store_model_provider_token(&model_provider, &profile, &token, metadata, true)
                .await?;
            println!(
                "{}",
                ta("cli-auth-saved", &[("profile", &profile)], "Saved profile")
            );
            println!(
                "{}",
                ta(
                    "cli-auth-active-for",
                    &[("provider", &model_provider), ("profile", &profile)],
                    "Active profile"
                )
            );
            Ok(())
        }

        AuthCommands::Refresh {
            model_provider,
            profile,
        } => {
            let provider: auth::AuthProvider = model_provider.parse()?;
            let client = reqwest::Client::new();
            let ctx = auth::AuthFlowContext {
                config,
                auth_service: &auth_service,
                client: &client,
                format_cli: &auth_cli_formatter,
            };
            let status = provider
                .flow()
                .refresh_status(&ctx, profile.as_deref())
                .await?;
            match status {
                auth::RefreshStatus::Refreshed { profile } => {
                    println!(
                        "{}",
                        ta(
                            "cli-auth-refresh-ok",
                            &[("profile", &profile)],
                            "Token refresh OK"
                        )
                    );
                    Ok(())
                }
                auth::RefreshStatus::NoProfile => {
                    bail!(
                        "No auth profile found. Run `zeroclaw auth login --model-provider <provider>` first.",
                    )
                }
            }
        }

        AuthCommands::Logout {
            model_provider,
            profile,
        } => {
            let model_provider = auth::normalize_model_provider(&model_provider)?;
            let removed = auth_service
                .remove_profile(&model_provider, &profile)
                .await?;
            if removed {
                println!(
                    "{}",
                    ta(
                        "cli-auth-removed",
                        &[("provider", &model_provider), ("profile", &profile)],
                        "Removed auth profile"
                    )
                );
            } else {
                println!(
                    "{}",
                    ta(
                        "cli-auth-not-found",
                        &[("provider", &model_provider), ("profile", &profile)],
                        "Auth profile not found"
                    )
                );
            }
            Ok(())
        }

        AuthCommands::Use {
            model_provider,
            profile,
        } => {
            let model_provider = auth::normalize_model_provider(&model_provider)?;
            auth_service
                .set_active_profile(&model_provider, &profile)
                .await?;
            println!(
                "{}",
                ta(
                    "cli-auth-active-for",
                    &[("provider", &model_provider), ("profile", &profile)],
                    "Active profile"
                )
            );
            Ok(())
        }

        AuthCommands::List => {
            let data = auth_service.load_profiles().await?;
            if data.profiles.is_empty() {
                println!("{}", t("cli-auth-none", "No auth profiles configured."));
                return Ok(());
            }

            for (id, profile) in &data.profiles {
                let active = data
                    .active_profiles
                    .get(&profile.model_provider)
                    .is_some_and(|active_id| active_id == id);
                let marker = if active { "*" } else { " " };
                println!("{marker} {id}");
            }

            Ok(())
        }

        AuthCommands::Status => {
            let data = auth_service.load_profiles().await?;
            if data.profiles.is_empty() {
                println!("{}", t("cli-auth-none", "No auth profiles configured."));
                return Ok(());
            }

            for (id, profile) in &data.profiles {
                let active = data
                    .active_profiles
                    .get(&profile.model_provider)
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
            println!("{}", t("cli-auth-active", "Active profiles:"));
            for (model_provider, profile_id) in &data.active_profiles {
                println!("  {model_provider}: {profile_id}");
            }

            Ok(())
        }

        AuthCommands::EmailLogin { channel, profile } => {
            let email_cfg = config.channels.email.get(&channel).ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "No [channels.email.{channel}] block found in config. \
                     Add the block with an [channels.email.{channel}.oauth2] section first."
                ))
            })?;

            let oauth2 = email_cfg.oauth2.as_ref().ok_or_else(|| anyhow::Error::msg(format!(
                "[channels.email.{channel}] exists but has no [channels.email.{channel}.oauth2] block."
            )))?;

            let client = reqwest::Client::new();
            let device = auth::email_oauth2::start_device_code_flow(
                &client,
                &oauth2.device_code_url,
                &oauth2.client_id,
                &oauth2.scopes,
            )
            .await?;

            println!("Email OAuth2 device-code login started."); // i18n-exempt: interactive device-code CLI prompt
            println!("Visit:  {}", device.verification_uri); // i18n-exempt: interactive device-code CLI prompt
            println!("Code:   {}", device.user_code); // i18n-exempt: interactive device-code CLI prompt
            if let Some(ref uri) = device.verification_uri_complete {
                println!("Or open directly: {uri}"); // i18n-exempt: interactive device-code CLI prompt
            }
            println!("Waiting for authorization…"); // i18n-exempt: interactive device-code CLI prompt

            let token_set = auth::email_oauth2::poll_device_code_tokens(
                &client,
                &oauth2.token_url,
                &oauth2.client_id,
                &device,
            )
            .await?;

            let channel_alias = format!("email.{channel}");
            auth_service
                .store_email_oauth2_tokens(&channel_alias, &profile, token_set)
                .await?;
            println!("Saved profile {profile} for {channel_alias}"); // i18n-exempt: interactive device-code CLI prompt
            Ok(())
        }
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

/// Build the SOP channel-backed adapters from one shared channel map:
/// - the approval ROUTE adapter, so a SOP that parks at a policied gate (or later
///   times out) can deliver its approval request / escalation notice to a real
///   channel (Discord, Slack, ...);
/// - the FORGE-WRITE adapter, so an approved `forge.comment` capability step can
///   post its comment back to the forge by driving the git channel's normal
///   outbound path.
///
/// - the LLM adapter, so an `llm.generate` capability step can run one bounded
///   model call on the default agent's resolved provider.
///
/// Each field is `None` when not applicable (no channels at all; no git channel
/// for the forge half; no resolvable default model provider for the llm half), in
/// which case `build_sop_engine` falls back to the log-only no-op route adapter
/// and the fail-closed `forge.comment` / `llm.generate` placeholders (unchanged
/// behavior). MUST be called from within the tokio runtime: it captures
/// `Handle::current()` so the sync, under-the-engine-lock adapter calls can bridge
/// to the async channel/provider calls.
#[cfg(feature = "agent-runtime")]
fn build_sop_adapters(config: &Config) -> zeroclaw_runtime::sop::SopEngineAdapters {
    // `llm.generate` runs on the DEFAULT agent's resolved model provider — the
    // daemon-level model of record. No resolvable provider = fail-closed.
    let llm: Option<std::sync::Arc<dyn zeroclaw_runtime::sop::capability::LlmGenerateAdapter>> =
        config
            .resolved_model_provider_for_agent("default")
            .and_then(|(provider_type, alias, entry)| {
                // Alias-aware factory WITH the alias's runtime options: the options
                // carry zeroclaw_dir (auth-profile store) and per-alias runtime
                // knobs — without them, OAuth/subscription providers (codex,
                // opencode) sit unauthenticated and never answer. This mirrors the
                // delegate tool's provider construction.
                let options = zeroclaw::providers::provider_runtime_options_for_alias(
                    config,
                    provider_type,
                    alias,
                );
                let provider = match zeroclaw::providers::create_model_provider_for_alias(
                    config,
                    provider_type,
                    alias,
                    entry.api_key.as_deref(),
                    &options,
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": e.to_string()})),
                            "SOP llm.generate adapter unavailable: default model provider failed to build"
                        );
                        return None;
                    }
                };
                let model = entry.model.clone().unwrap_or_else(|| "default".to_string());
                Some(std::sync::Arc::new(
                    zeroclaw_runtime::sop::capability::ProviderLlmAdapter::new(
                        std::sync::Arc::from(provider),
                        model,
                    ),
                ) as _)
            });

    let channels = zeroclaw_channels::orchestrator::build_channel_map(config);
    // Startup validation: this send-only adapter's channel map omits channels that
    // need runtime SOP handles (e.g. AMQP SOP-dispatch channels). Surface at BOOT any
    // configured approval route whose channel is absent here, so a `request_route` /
    // `escalation_route` that would silently fail to deliver at gate time is caught up
    // front rather than on the first parked gate. This runs BEFORE the empty-map return:
    // when there are no deliverable channels at all, EVERY configured route is
    // undeliverable and must still be surfaced.
    // A route target must be a channel that can actually deliver OUTBOUND; an
    // inbound-only channel (e.g. AMQP, whose `send` is a no-op) in the map cannot send
    // an approval notice, so it is not a resolvable route target.
    let deliverable_keys: std::collections::HashSet<String> = channels
        .iter()
        .filter(|(_, ch)| ch.supports_outbound_send())
        .map(|(key, _)| key.clone())
        .collect();
    for issue in zeroclaw_runtime::sop::approval::unresolvable_approval_routes(
        &config.sop.approval,
        &deliverable_keys,
    ) {
        match issue {
            zeroclaw_runtime::sop::approval::ApprovalRouteIssue::Malformed {
                policy,
                route_kind,
                route,
            } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "policy": policy,
                            "route_kind": route_kind,
                            "route": route,
                        })),
                    "SOP approval route is malformed; use the required channel:recipient format"
                );
            }
            zeroclaw_runtime::sop::approval::ApprovalRouteIssue::UndeliverableChannel {
                policy,
                route_kind,
                route,
                channel_key,
            } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "policy": policy,
                            "route_kind": route_kind,
                            "route": route,
                            "channel": channel_key,
                        })),
                    "SOP approval route names a channel the route adapter cannot deliver to; \
                     its approval notices will not be sent (the channel may require runtime SOP \
                     handles this send-only adapter lacks)"
                );
            }
        }
    }
    if channels.is_empty() {
        return zeroclaw_runtime::sop::SopEngineAdapters {
            llm,
            ..Default::default()
        };
    }
    let handle = tokio::runtime::Handle::current();
    let route: std::sync::Arc<dyn zeroclaw_runtime::sop::approval::ApprovalRouteAdapter> =
        std::sync::Arc::new(zeroclaw_runtime::sop::approval::ChannelRouteAdapter::new(
            channels.clone(),
            handle.clone(),
        ));
    // Only offer the forge adapter when a git channel actually exists, so
    // `forge.comment` stays fail-closed on daemons without a forge.
    let has_git = channels.keys().any(|k| k == "git" || k.starts_with("git."));
    let forge: Option<std::sync::Arc<dyn zeroclaw_runtime::sop::capability::ForgeCommentAdapter>> =
        has_git.then(|| {
            std::sync::Arc::new(zeroclaw_runtime::sop::capability::ChannelForgeAdapter::new(
                channels,
            )) as _
        });
    zeroclaw_runtime::sop::SopEngineAdapters {
        route: Some(route),
        forge,
        llm,
    }
}

/// Spawn the periodic SOP maintenance tick (EPIC A1 + SOP cron): on each interval it
/// fires fail-closed approval timeouts, reaps expired concurrency-claim leases,
/// prunes terminal runs past the retention policy, and dispatches cached cron
/// SOP triggers. Returns `None` (no task) when the tick is disabled
/// (`interval_secs == 0`) or no SOP engine is configured. The caller owns the
/// returned handle and aborts it when the foreground daemon/channel run exits.
/// The tick itself self-approves nothing - timeout handling follows
/// `approval_timeout_action` (default `escalate`, fail-closed).
#[cfg(feature = "agent-runtime")]
fn spawn_sop_maintenance(
    sop_engine: Option<&std::sync::Arc<std::sync::Mutex<zeroclaw_runtime::sop::SopEngine>>>,
    sop_audit: Option<&std::sync::Arc<zeroclaw_runtime::sop::SopAuditLogger>>,
    interval_secs: u64,
) -> Option<tokio::task::JoinHandle<()>> {
    if interval_secs == 0 {
        return None;
    }
    let engine = sop_engine.cloned()?;
    let audit = sop_audit.cloned();
    let cron_cache = audit
        .as_ref()
        .map(|_| zeroclaw_runtime::sop::dispatch::SopCronCache::from_engine(&engine));
    Some(::zeroclaw_spawn::spawn!(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last_cron_check = chrono::Utc::now();
        loop {
            ticker.tick().await;
            let Some(report) = run_sop_maintenance_tick(
                &engine,
                audit.as_ref(),
                cron_cache.as_ref(),
                &mut last_cron_check,
            )
            .await
            else {
                continue;
            };
            if !report.is_empty() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "timed_out": report.maintenance.timed_out,
                            "reaped_claims": report.maintenance.reaped_claims,
                            "pruned_runs": report.maintenance.pruned_runs,
                            "cron_started": report.cron_started,
                            "cron_skipped": report.cron_skipped,
                            "cron_no_match": report.cron_no_match,
                        })),
                    "SOP maintenance tick"
                );
            }
        }
    }))
}

#[cfg(feature = "agent-runtime")]
#[derive(Default)]
struct SopMaintenanceTickReport {
    maintenance: zeroclaw_runtime::sop::MaintenanceSummary,
    cron_started: usize,
    cron_skipped: usize,
    cron_blocked_unsafe: usize,
    cron_no_match: usize,
}

#[cfg(feature = "agent-runtime")]
impl SopMaintenanceTickReport {
    fn is_empty(&self) -> bool {
        self.maintenance.is_empty()
            && self.cron_started == 0
            && self.cron_skipped == 0
            && self.cron_blocked_unsafe == 0
            && self.cron_no_match == 0
    }
}

#[cfg(feature = "agent-runtime")]
async fn run_sop_maintenance_tick(
    engine: &std::sync::Arc<std::sync::Mutex<zeroclaw_runtime::sop::SopEngine>>,
    audit: Option<&std::sync::Arc<zeroclaw_runtime::sop::SopAuditLogger>>,
    cron_cache: Option<&zeroclaw_runtime::sop::dispatch::SopCronCache>,
    last_cron_check: &mut chrono::DateTime<chrono::Utc>,
) -> Option<SopMaintenanceTickReport> {
    let maintenance = match engine.lock() {
        Ok(mut e) => e.run_maintenance_tick(),
        Err(_) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "SOP maintenance tick: engine lock poisoned; skipping this pass"
            );
            return None;
        }
    };

    let mut report = SopMaintenanceTickReport {
        maintenance,
        ..SopMaintenanceTickReport::default()
    };

    if let (Some(audit), Some(cache)) = (audit, cron_cache) {
        let results = zeroclaw_runtime::sop::dispatch::check_sop_cron_triggers(
            engine,
            audit,
            cache,
            last_cron_check,
        )
        .await;
        for result in &results {
            match result {
                zeroclaw_runtime::sop::dispatch::DispatchResult::Started { .. } => {
                    report.cron_started += 1;
                }
                zeroclaw_runtime::sop::dispatch::DispatchResult::Skipped { .. }
                | zeroclaw_runtime::sop::dispatch::DispatchResult::Deferred { .. }
                | zeroclaw_runtime::sop::dispatch::DispatchResult::Coalesced { .. } => {
                    // A2: deferred (backpressure) / coalesced triggers did not start a
                    // run this tick; the cron schedule re-fires them next pass. The
                    // precise outcome is logged by process_headless_results below.
                    report.cron_skipped += 1;
                }
                zeroclaw_runtime::sop::dispatch::DispatchResult::BlockedUnsafe { .. } => {
                    report.cron_blocked_unsafe += 1;
                }
                zeroclaw_runtime::sop::dispatch::DispatchResult::NoMatch => {
                    report.cron_no_match += 1;
                }
            }
        }
        zeroclaw_runtime::sop::dispatch::process_headless_results(&results);
    }

    Some(report)
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
    // manually" message, None for tui_registry (no TUI socket), and None
    // for canvas_store so the gateway falls back to its own default.
    let result = Box::pin(gateway::run_gateway(
        host, port, config, tx, None, None, None, None, None,
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
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::net::TcpListener;

    #[test]
    fn probe_config_dir_extracts_global_flag_in_all_forms() {
        fn argv(parts: &[&str]) -> std::vec::IntoIter<std::ffi::OsString> {
            parts
                .iter()
                .map(|s| std::ffi::OsString::from(*s))
                .collect::<Vec<_>>()
                .into_iter()
        }

        let command = Cli::command();

        // argv[0] is consumed by clap as the binary name.
        // Space form.
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "--config-dir", "/x"])),
            Some("/x".to_string())
        );
        // Equals form.
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "--config-dir=/y"])),
            Some("/y".to_string())
        );
        // Global arg: may appear *after* a subcommand.
        assert_eq!(
            probe_config_dir(
                &command,
                argv(&["zeroclaw", "status", "--config-dir", "/z"])
            ),
            Some("/z".to_string())
        );
        // Absent.
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "status"])),
            None
        );
        // `--` ends option parsing; later values must never redirect config.
        assert_eq!(
            probe_config_dir(
                &command,
                argv(&[
                    "zeroclaw",
                    "config",
                    "set",
                    "locale",
                    "--",
                    "--config-dir=/ignored",
                ])
            ),
            None
        );
        // Present but empty — returned verbatim for clap's validation path.
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "--config-dir", ""])),
            Some(String::new())
        );
    }

    #[test]
    fn probe_config_dir_follows_clap_token_ownership() {
        fn argv(parts: &[&str]) -> std::vec::IntoIter<std::ffi::OsString> {
            parts
                .iter()
                .map(|s| std::ffi::OsString::from(*s))
                .collect::<Vec<_>>()
                .into_iter()
        }

        let command = Cli::command();
        let external_payload = [
            "zeroclaw",
            "props",
            "legacy-command",
            "--config-dir=/unintended",
        ];

        // The external subcommand owns every remaining token, including one
        // that looks like a global option.
        let cli = Cli::try_parse_from(external_payload)
            .expect("the deprecated external-subcommand path is valid clap input");
        assert!(cli.config_dir.is_none());
        assert_eq!(probe_config_dir(&command, argv(&external_payload)), None);

        // Option-looking and terminating tokens cannot satisfy the spaced
        // form's required value.
        assert!(Cli::try_parse_from(["zeroclaw", "--config-dir", "--help"]).is_err());
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "--config-dir", "--help"])),
            None
        );
        assert_eq!(
            probe_config_dir(&command, argv(&["zeroclaw", "--config-dir", "--"])),
            None
        );
    }

    #[test]
    fn cli_quickstart_uses_advertised_local_provider_runtime_default() {
        let providers = vec![zeroclaw_runtime::quickstart::QuickstartTypeOption {
            kind: "lmstudio".into(),
            display_name: "LM Studio".into(),
            local: true,
            default_runtime_profile: Some("local_small".into()),
        }];

        assert_eq!(
            quickstart_runtime_profile_for_provider("lmstudio", &providers, "unbounded"),
            "local_small"
        );
    }

    #[test]
    fn cli_quickstart_uses_advertised_remote_provider_runtime_default() {
        let providers = vec![zeroclaw_runtime::quickstart::QuickstartTypeOption {
            kind: "anthropic".into(),
            display_name: "Anthropic".into(),
            local: false,
            default_runtime_profile: Some("unbounded".into()),
        }];

        assert_eq!(
            quickstart_runtime_profile_for_provider("anthropic", &providers, "unbounded"),
            "unbounded"
        );
    }

    #[test]
    fn cli_quickstart_uses_state_fallback_when_provider_has_no_override() {
        let providers = vec![zeroclaw_runtime::quickstart::QuickstartTypeOption {
            kind: "ollama".into(),
            display_name: "Ollama".into(),
            local: true,
            default_runtime_profile: None,
        }];

        assert_eq!(
            quickstart_runtime_profile_for_provider("ollama", &providers, "unbounded"),
            "unbounded"
        );
    }

    #[test]
    fn cap_line_utf8_safe_no_panic_on_multibyte_boundary() {
        // Neutral multi-byte placeholder text; each CJK char is 3 bytes, so a
        // byte cap can land inside a character. Pre-fix this panicked via the
        // raw `String::truncate(cap)`.
        let mut line = "语言".repeat(64); // 128 chars, 384 bytes, all 3-byte
        let cap = 10; // byte index 10 is mid-character (10 % 3 != 0)
        assert!(
            !line.is_char_boundary(cap),
            "precondition: cap splits a char"
        );
        cap_line_utf8_safe(&mut line, cap);
        assert!(line.len() <= cap, "must not exceed the byte cap");
        assert!(
            line.is_char_boundary(line.len()),
            "result must end on a valid UTF-8 char boundary"
        );
        // cap 10 floors to byte 9 = three whole 3-byte chars.
        assert_eq!(
            line, "语言语",
            "should keep whole chars up to the floored cap"
        );
    }

    #[test]
    fn cap_line_utf8_safe_is_noop_when_within_cap() {
        let mut line = String::from("héllo"); // 6 bytes
        cap_line_utf8_safe(&mut line, 1024);
        assert_eq!(line, "héllo");
    }

    #[test]
    fn cap_line_utf8_safe_ascii_exact_cap() {
        let mut line = String::from("abcdefgh");
        cap_line_utf8_safe(&mut line, 4);
        assert_eq!(line, "abcd");
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn cli_definition_has_no_flag_conflicts() {
        Cli::command().debug_assert();
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn quickstart_inline_auth_uses_auth_mode_field() {
        let fields =
            std::collections::HashMap::from([("auth_mode".to_string(), " codex ".to_string())]);
        assert_eq!(
            quickstart_inline_auth("openai", "codex", &fields),
            Some(InlineProviderAuth::Codex)
        );

        let fields =
            std::collections::HashMap::from([("auth_mode".to_string(), "setup_token".to_string())]);
        assert_eq!(
            quickstart_inline_auth("anthropic", "max", &fields),
            Some(InlineProviderAuth::AnthropicSetupToken {
                alias: "max".to_string()
            })
        );

        assert_eq!(quickstart_inline_auth("openai", "api", &fields), None);
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn ensure_map_key_materializes_typed_provider_entries() {
        use crate::config::schema::Config;
        for (path, value) in [
            ("providers.models.openai.default.model", "gpt-4o"),
            ("providers.tts.openai.default.voice", "alloy"),
            ("providers.transcription.openai.default.model", "whisper-1"),
            ("channels.telegram.default.bot_token", "tok"),
        ] {
            let mut config = Config::default();
            assert!(
                config.set_prop(path, value).is_err(),
                "precondition: {path} should be unknown on a fresh config"
            );
            config.ensure_map_key_for_path(path);
            assert!(
                config.set_prop(path, value).is_ok(),
                "{path} must be settable after map-key materialization"
            );
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn ensure_map_key_ignores_non_map_paths() {
        use crate::config::schema::Config;
        let mut config = Config::default();
        config.ensure_map_key_for_path("gateway.port");
        config.ensure_map_key_for_path("locale");
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
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
    #[cfg(feature = "agent-runtime")]
    fn gateway_admin_url_uses_unprefixed_admin_path_by_default() {
        assert_eq!(
            gateway_admin_url("127.0.0.1", 42617, None, "/admin/paircode"),
            "http://127.0.0.1:42617/admin/paircode"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn gateway_admin_url_prepends_configured_path_prefix() {
        assert_eq!(
            gateway_admin_url("localhost", 42617, Some("/zeroclaw"), "/admin/paircode/new"),
            "http://localhost:42617/zeroclaw/admin/paircode/new"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_accepts_model_provider_and_api_key_in_quick_mode() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "onboard",
            "--model-provider",
            "openrouter",
            "--model",
            "custom-model-946",
            "--api-key",
            "sk-issue946",
        ])
        .expect("quick onboard invocation should parse");

        match cli.command {
            Commands::Onboard {
                force,
                channels_only,
                api_key,
                model_provider,
                model,
                ..
            } => {
                assert!(!force);
                assert!(!channels_only);
                assert_eq!(model_provider.as_deref(), Some("openrouter"));
                assert_eq!(model.as_deref(), Some("custom-model-946"));
                assert_eq!(api_key.as_deref(), Some("sk-issue946"));
            }
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
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
    #[cfg(feature = "agent-runtime")]
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
    #[cfg(feature = "agent-runtime")]
    fn bash_completion_avoids_infinite_recursion() {
        let mut output = Vec::new();
        write_shell_completion(CompletionShell::Bash, &mut output)
            .expect("completion generation should succeed");
        let script = String::from_utf8(output).expect("completion output should be valid utf-8");
        // The wrapper must capture the original clap-generated function body
        // (via declare -f) rather than calling _zeroclaw by name, which would
        // create an infinite recursion loop after _zeroclaw is redefined.
        assert!(
            script.contains("declare -f _zeroclaw"),
            "bash completion should use declare -f to capture the original _zeroclaw function body"
        );
        assert!(
            !script.contains("_zeroclaw_clap_orig() { _zeroclaw \"$@\"; }"),
            "bash completion must not define _zeroclaw_clap_orig as a simple forwarder to _zeroclaw"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_accepts_force_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--force"])
            .expect("onboard --force should parse");

        match cli.command {
            Commands::Onboard { force, .. } => assert!(force),
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_rejects_removed_interactive_flag() {
        // --interactive was removed; onboard auto-detects TTY instead.
        assert!(Cli::try_parse_from(["zeroclaw", "onboard", "--interactive"]).is_err());
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_parses_quick_flag() {
        let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--quick"])
            .expect("onboard --quick should parse");

        match cli.command {
            Commands::Onboard { quick, .. } => assert!(quick),
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn gateway_get_paircode_cli_accepts_port_and_host_overrides() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "gateway",
            "get-paircode",
            "--new",
            "--port",
            "3001",
            "--host",
            "192.168.1.20",
        ])
        .expect("gateway get-paircode overrides should parse");

        match cli.command {
            Commands::Gateway {
                gateway_command:
                    Some(zeroclaw::GatewayCommands::GetPaircode {
                        new,
                        rotate,
                        rotate_device,
                        port,
                        host,
                    }),
            } => {
                assert!(new);
                assert!(!rotate);
                assert_eq!(rotate_device, None);
                assert_eq!(port, Some(3001));
                assert_eq!(host.as_deref(), Some("192.168.1.20"));
            }
            other => panic!("expected gateway get-paircode command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn security_status_cli_requires_agent_and_parses_json_form() {
        let err = Cli::try_parse_from(["zeroclaw", "security", "status"])
            .expect_err("security status requires --agent");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);

        let cli =
            Cli::try_parse_from(["zeroclaw", "security", "status", "--agent", "ops", "--json"])
                .expect("security status --agent --json should parse");
        match cli.command {
            Commands::Security {
                security_command: SecurityCommands::Status { agent, json },
            } => {
                assert_eq!(agent, "ops");
                assert!(json);
            }
            other => panic!("expected security status command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn gateway_get_paircode_rotate_flags_parse_and_conflict() {
        let cli = Cli::try_parse_from(["zeroclaw", "gateway", "get-paircode", "--rotate"])
            .expect("gateway get-paircode --rotate should parse");
        match cli.command {
            Commands::Gateway {
                gateway_command: Some(zeroclaw::GatewayCommands::GetPaircode { rotate, .. }),
            } => assert!(rotate),
            other => panic!("expected gateway get-paircode command, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "zeroclaw",
            "gateway",
            "get-paircode",
            "--rotate-device",
            "dash-1",
        ])
        .expect("gateway get-paircode --rotate-device should parse");
        match cli.command {
            Commands::Gateway {
                gateway_command: Some(zeroclaw::GatewayCommands::GetPaircode { rotate_device, .. }),
            } => assert_eq!(rotate_device.as_deref(), Some("dash-1")),
            other => panic!("expected gateway get-paircode command, got {other:?}"),
        }

        assert!(
            Cli::try_parse_from(["zeroclaw", "gateway", "get-paircode", "--new", "--rotate"])
                .is_err(),
            "--new and --rotate must conflict"
        );
        assert!(
            Cli::try_parse_from([
                "zeroclaw",
                "gateway",
                "get-paircode",
                "--rotate",
                "--rotate-device",
                "dash-1"
            ])
            .is_err(),
            "--rotate and --rotate-device must conflict"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn paircode_url_combines_host_port_override_with_configured_path_prefix() {
        assert_eq!(
            gateway_admin_url(
                "127.0.0.1",
                9001,
                Some("/agents/myagent"),
                "/admin/paircode/new"
            ),
            "http://127.0.0.1:9001/agents/myagent/admin/paircode/new",
        );
        assert_eq!(
            gateway_admin_url("192.168.1.20", 42617, Some("/gw"), "/admin/paircode"),
            "http://192.168.1.20:42617/gw/admin/paircode",
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn paircode_no_code_message_explains_bare_command_does_not_mint() {
        let default = config::GatewayConfig::default();
        let msg = paircode_no_code_message(
            "127.0.0.1",
            42617,
            &default.host,
            default.port,
            &PaircodeAction::Show,
            true,
            Some("Pairing is active but no new code available (already paired or code expired)"),
        );

        assert!(msg.contains("only displays an existing active code; it does not mint"));
        assert!(msg.contains("zeroclaw gateway get-paircode --new"));
        assert!(msg.contains("zeroclaw gateway get-paircode --rotate"));
        assert!(msg.contains("open http://127.0.0.1:42617"));
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn paircode_no_code_message_preserves_host_port_on_suggestions() {
        let default = config::GatewayConfig::default();
        let msg = paircode_no_code_message(
            "192.168.1.20",
            9001,
            &default.host,
            default.port,
            &PaircodeAction::Show,
            true,
            None,
        );

        assert!(
            msg.contains("zeroclaw gateway get-paircode --new --port 9001 --host 192.168.1.20")
        );
        assert!(
            msg.contains("zeroclaw gateway get-paircode --rotate --port 9001 --host 192.168.1.20")
        );
        assert!(msg.contains("open http://192.168.1.20:9001"));
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn paircode_no_code_message_omits_configured_default_host_port() {
        let msg = paircode_no_code_message(
            "192.168.1.20",
            9001,
            "192.168.1.20",
            9001,
            &PaircodeAction::Show,
            true,
            None,
        );

        assert!(msg.contains("zeroclaw gateway get-paircode --new\n"));
        assert!(msg.contains("zeroclaw gateway get-paircode --rotate\n"));
        assert!(!msg.contains("--port 9001"));
        assert!(!msg.contains("--host 192.168.1.20"));
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn paircode_no_code_message_for_new_suggests_rotate() {
        let default = config::GatewayConfig::default();
        let msg = paircode_no_code_message(
            "127.0.0.1",
            42617,
            &default.host,
            default.port,
            &PaircodeAction::AddClient,
            true,
            Some("Pairing is active but no new code available (already paired or code expired)"),
        );

        assert!(msg.contains("did not mint a new pairing code"));
        assert!(msg.contains("zeroclaw gateway get-paircode --rotate"));
    }

    #[test]
    fn gateway_addr_in_use_message_guides_default_gateway_recovery() {
        let default = config::GatewayConfig::default();
        let msg = gateway_addr_in_use_message(
            "127.0.0.1",
            42617,
            &default.host,
            default.port,
            Some(42618),
        );

        assert!(msg.contains("Port 42617 is already in use"));
        assert!(msg.contains("open http://127.0.0.1:42617"));
        assert!(msg.contains("zeroclaw gateway get-paircode\n"));
        assert!(msg.contains("zeroclaw gateway start --port 42618"));
        assert!(msg.contains("lsof -nP -iTCP:42617 -sTCP:LISTEN"));
    }

    #[test]
    fn gateway_addr_in_use_message_keeps_non_default_host_context() {
        let default = config::GatewayConfig::default();
        let msg =
            gateway_addr_in_use_message("0.0.0.0", 9001, &default.host, default.port, Some(9002));

        assert!(!msg.contains("open http://127.0.0.1:42617"));
        assert!(msg.contains("zeroclaw gateway get-paircode --port 9001 --host 0.0.0.0"));
        assert!(msg.contains("zeroclaw gateway start --port 9002 --host 0.0.0.0"));
        assert!(msg.contains("lsof -nP -iTCP:9001 -sTCP:LISTEN"));
    }

    #[test]
    fn gateway_addr_in_use_message_omits_restart_when_no_available_port() {
        let default = config::GatewayConfig::default();
        let msg =
            gateway_addr_in_use_message("127.0.0.1", 42617, &default.host, default.port, None);

        assert!(msg.contains("zeroclaw gateway get-paircode\n"));
        assert!(!msg.contains("zeroclaw gateway start --port"));
        assert!(msg.contains("lsof -nP -iTCP:42617 -sTCP:LISTEN"));
    }

    #[test]
    fn gateway_addr_in_use_message_skips_occupied_restart_hint_port() {
        let default = config::GatewayConfig::default();
        let (port, mut listeners) = reserve_consecutive_local_ports(3);
        let available_port = port + 2;
        drop(listeners.pop());

        let restart_port = available_gateway_restart_hint_port("127.0.0.1", port);
        let msg = gateway_addr_in_use_message(
            "127.0.0.1",
            port,
            &default.host,
            default.port,
            restart_port,
        );

        assert!(
            !msg.contains(&format!("zeroclaw gateway start --port {}", port + 1)),
            "{msg}"
        );
        assert!(
            msg.contains(&format!("zeroclaw gateway start --port {available_port}")),
            "{msg}"
        );
    }

    #[test]
    fn gateway_addr_in_use_message_uses_configured_default_gateway_recovery() {
        let msg = gateway_addr_in_use_message("192.168.1.20", 9001, "192.168.1.20", 9001, None);

        assert!(msg.contains("open http://192.168.1.20:9001"));
        assert!(msg.contains("zeroclaw gateway get-paircode\n"));
        assert!(!msg.contains("get-paircode --port 9001"));
    }

    #[test]
    fn gateway_restart_hint_uses_gateway_bind_fallback_for_hostnames() {
        let (port, mut listeners) = reserve_consecutive_local_ports(3);
        let available_port = port + 2;
        drop(listeners.pop());

        assert_eq!(
            available_gateway_restart_hint_port("localhost", port),
            Some(available_port)
        );
    }

    #[test]
    fn gateway_bind_addr_resolver_accepts_bracketed_ipv6_hosts() {
        let addr = zeroclaw_infra::effective_gateway_bind_socket_addr("[::1]", 9001);

        assert_eq!(addr.port(), 9001);
        assert!(addr.is_ipv6());
    }

    #[test]
    fn gateway_addr_in_use_detector_recognizes_nested_io_error() {
        let err = std::io::Error::from(ErrorKind::AddrInUse);
        let err = anyhow::Error::new(err).context("gateway bind failed");

        assert!(is_addr_in_use_error(&err));
    }

    fn reserve_consecutive_local_ports(count: u16) -> (u16, Vec<TcpListener>) {
        for _ in 0..100 {
            let Ok(first) = TcpListener::bind(("127.0.0.1", 0)) else {
                continue;
            };
            let port = first.local_addr().expect("listener has local addr").port();
            if port > u16::MAX - count {
                continue;
            }

            let mut listeners = vec![first];
            let mut reserved_all = true;
            for offset in 1..count {
                match TcpListener::bind(("127.0.0.1", port + offset)) {
                    Ok(listener) => listeners.push(listener),
                    Err(_) => {
                        reserved_all = false;
                        break;
                    }
                }
            }

            if reserved_all {
                return (port, listeners);
            }
        }

        panic!("could not reserve {count} consecutive local ports");
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_quick_and_channels_only_conflict() {
        // --quick and --channels-only should both parse at the CLI level
        // (the conflict is checked at runtime), but we verify both flags parse.
        let cli = Cli::try_parse_from(["zeroclaw", "onboard", "--quick", "--channels-only"]);
        assert!(
            cli.is_ok(),
            "--quick --channels-only should parse at CLI level"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_bare_parses() {
        let cli = Cli::try_parse_from(["zeroclaw", "onboard"]).expect("bare onboard should parse");

        match cli.command {
            Commands::Onboard { section, .. } => assert!(section.is_none()),
            other => panic!("expected onboard command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn onboard_cli_positional_sections_parse() {
        for w in zeroclaw_config::sections::QUICKSTART_SECTIONS {
            let cli = Cli::try_parse_from(["zeroclaw", "onboard", w.as_str()])
                .unwrap_or_else(|_| panic!("onboard {} should parse", w.as_str()));
            match cli.command {
                Commands::Onboard { section, .. } => assert_eq!(section, Some(*w)),
                other => panic!("expected onboard command, got {other:?}"),
            }
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn homebrew_onboard_config_dir_detects_cellar_paths() {
        assert_eq!(
            resolve_homebrew_onboard_config_dir(
                Path::new("/opt/homebrew/Cellar/zeroclaw/0.8.0/bin/zeroclaw"),
                |_| None,
            ),
            Some(PathBuf::from("/opt/homebrew/var/zeroclaw")),
        );
        assert_eq!(
            resolve_homebrew_onboard_config_dir(
                Path::new("/usr/local/Cellar/zeroclaw/0.8.0/bin/zeroclaw"),
                |_| None,
            ),
            Some(PathBuf::from("/usr/local/var/zeroclaw")),
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn homebrew_onboard_config_dir_detects_brew_bin_symlink_layout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let prefix = temp.path().join("homebrew");
        std::fs::create_dir_all(prefix.join("Cellar")).expect("create Cellar marker");
        let exe = prefix.join("bin/zeroclaw");

        assert_eq!(
            resolve_homebrew_onboard_config_dir(&exe, |_| None),
            Some(prefix.join("var/zeroclaw")),
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn homebrew_onboard_config_dir_preserves_explicit_runtime_paths() {
        let exe = Path::new("/opt/homebrew/Cellar/zeroclaw/0.8.0/bin/zeroclaw");

        for var in [
            "ZEROCLAW_CONFIG_DIR",
            "ZEROCLAW_DATA_DIR",
            "ZEROCLAW_WORKSPACE",
        ] {
            assert_eq!(
                resolve_homebrew_onboard_config_dir(exe, |name| {
                    (name == var).then(|| "/tmp/zeroclaw-explicit".to_string())
                }),
                None,
                "{var} should take precedence over Homebrew detection",
            );
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn homebrew_onboard_config_dir_treats_workspace_whitespace_as_explicit() {
        let exe = Path::new("/opt/homebrew/Cellar/zeroclaw/0.8.0/bin/zeroclaw");

        assert_eq!(
            resolve_homebrew_onboard_config_dir(exe, |name| {
                (name == "ZEROCLAW_WORKSPACE").then(|| "   ".to_string())
            }),
            None,
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn apply_homebrew_onboard_config_dir_sets_detected_config_dir() {
        let exe = Path::new("/opt/homebrew/Cellar/zeroclaw/0.8.0/bin/zeroclaw");
        let mut applied = None;

        let detected = apply_homebrew_onboard_config_dir_with(
            exe,
            |_| None,
            |name, value| applied = Some((name, value.to_path_buf())),
        );

        assert_eq!(detected, Some(PathBuf::from("/opt/homebrew/var/zeroclaw")));
        assert_eq!(
            applied,
            Some((
                "ZEROCLAW_CONFIG_DIR",
                PathBuf::from("/opt/homebrew/var/zeroclaw"),
            )),
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn apply_homebrew_onboard_config_dir_skips_explicit_config_dir() {
        let exe = Path::new("/opt/homebrew/Cellar/zeroclaw/0.8.0/bin/zeroclaw");
        let mut applied = None;

        let detected = apply_homebrew_onboard_config_dir_with(
            exe,
            |name| (name == "ZEROCLAW_CONFIG_DIR").then(|| "/tmp/zeroclaw".to_string()),
            |name, value| applied = Some((name, value.to_path_buf())),
        );

        assert_eq!(detected, None);
        assert_eq!(applied, None);
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
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
    #[cfg(feature = "agent-runtime")]
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
    #[cfg(feature = "agent-runtime")]
    fn agent_command_parses_with_temperature() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "agent",
            "--agent",
            "morning-shift",
            "--temperature",
            "0.5",
        ])
        .expect("agent command with temperature should parse");

        match cli.command {
            Commands::Agent { temperature, .. } => {
                assert_eq!(temperature, Some(0.5));
            }
            other => panic!("expected agent command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn agent_command_parses_without_temperature() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "agent",
            "--agent",
            "morning-shift",
            "--message",
            "hello",
        ])
        .expect("agent command without temperature should parse");

        match cli.command {
            Commands::Agent { temperature, .. } => {
                assert_eq!(temperature, None);
            }
            other => panic!("expected agent command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn agent_command_parses_session_state_file() {
        let cli = Cli::try_parse_from([
            "zeroclaw",
            "agent",
            "--agent",
            "morning-shift",
            "--session-state-file",
            "session.json",
        ])
        .expect("agent command with session state file should parse");

        match cli.command {
            Commands::Agent {
                session_state_file, ..
            } => {
                assert_eq!(session_state_file, Some(PathBuf::from("session.json")));
            }
            other => panic!("expected agent command, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn agent_uses_provider_temperature_when_unset() {
        // When the user doesn't pass --temperature, the agent CLI
        // resolves from the agent's model_provider entry's temperature,
        // bottoming out at 0.7.
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("openai", "default")
            .expect("known family")
            .temperature = Some(1.5);

        let user_temperature: Option<f64> = std::hint::black_box(None);
        let final_temperature = user_temperature.unwrap_or_else(|| {
            config
                .providers
                .models
                .find("openai", "default")
                .and_then(|e| e.temperature)
                .unwrap_or(0.7)
        });

        assert!((final_temperature - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn config_set_materializes_missing_typed_provider_alias() {
        let mut config = Config::default();
        let path = "providers.models.deepseek.default.model";

        assert!(
            config
                .providers
                .models
                .find("deepseek", "default")
                .is_none(),
            "fresh config should not already contain the requested provider alias"
        );

        let created = ensure_map_key_for_prop_path(&mut config, path)
            .expect("known typed provider path should be materialized");

        assert!(created, "missing provider alias should be created");
        config
            .set_prop_persistent(path, "deepseek-chat")
            .expect("materialized path should be writable");
        assert_eq!(
            config
                .providers
                .models
                .find("deepseek", "default")
                .and_then(|provider| provider.model.as_deref()),
            Some("deepseek-chat")
        );

        let known_paths: Vec<String> = config.prop_fields().into_iter().map(|f| f.name).collect();
        let api_key_path = zeroclaw_config::helpers::resolve_field_path(
            &known_paths,
            "providers.models.deepseek.default.api-key",
        );
        config
            .set_prop_persistent(&api_key_path, "sk-test-placeholder")
            .expect(
                "kebab-case secret path should resolve to the materialized typed provider field",
            );
        assert_eq!(
            config
                .providers
                .models
                .find("deepseek", "default")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("sk-test-placeholder")
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn config_set_materializes_missing_tts_provider_alias() {
        let mut config = Config::default();
        let path = "providers.tts.openai.alloy.voice";

        assert!(
            config
                .providers
                .tts
                .iter_entries()
                .all(|(family, alias, _)| !(family == "openai" && alias == "alloy")),
            "fresh config should not already contain the requested tts alias"
        );

        let created = ensure_map_key_for_prop_path(&mut config, path)
            .expect("known typed tts provider path should be materialized");

        assert!(created, "missing tts alias should be created");
        config
            .set_prop_persistent(path, "alloy")
            .expect("materialized tts path should be writable");
        assert!(
            config
                .providers
                .tts
                .iter_entries()
                .any(|(family, alias, _)| family == "openai" && alias == "alloy"),
            "tts alias should resolve after materialization"
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn config_set_materializes_missing_transcription_provider_alias() {
        let mut config = Config::default();
        let raw = "providers.transcription.groq.fast.model";

        assert!(
            config
                .providers
                .transcription
                .iter_aliases()
                .all(|(family, alias)| !(family == "groq" && alias == "fast")),
            "fresh config should not already contain the requested transcription alias"
        );

        // Mirror the CLI `config set` path exactly: resolve, materialize the
        // map key, then re-resolve so the now-present alias field is found.
        let known: Vec<String> = config.prop_fields().into_iter().map(|f| f.name).collect();
        let mut path = zeroclaw_config::helpers::resolve_field_path(&known, raw);
        let created = ensure_map_key_for_prop_path(&mut config, &path)
            .expect("known typed transcription provider path should be materialized");
        assert!(created, "missing transcription alias should be created");
        let known: Vec<String> = config.prop_fields().into_iter().map(|f| f.name).collect();
        path = zeroclaw_config::helpers::resolve_field_path(&known, &path);

        config
            .set_prop_persistent(&path, "whisper-large-v3")
            .expect("materialized transcription path should be writable");
        assert!(
            config
                .providers
                .transcription
                .iter_aliases()
                .any(|(family, alias)| family == "groq" && alias == "fast"),
            "transcription alias should resolve after materialization"
        );
    }

    #[test]
    fn config_set_does_not_materialize_non_provider_map_keys() {
        let mut config = Config::default();
        let created = ensure_map_key_for_prop_path(
            &mut config,
            "cost.rates.providers.models.openai.gpt-4.1.input_per_mtok",
        )
        .expect("resource-key map paths should be ignored, not rejected");

        assert!(
            !created,
            "auto-materialization must stay scoped to alias-keyed sections, excluding #[resource_key] sections"
        );
        assert!(
            config.cost.rates.providers.models.openai.is_empty(),
            "no bogus model-id key should have been materialized under cost.rates",
        );
    }

    #[test]
    fn ensure_map_key_materializes_non_provider_alias_sections() {
        for (path, value) in [
            ("risk_profiles.newprofile.level", "supervised"),
            ("channels.telegram.main.enabled", "true"),
            ("channels.telegram.main.bot_token", "tok"),
            ("peer_groups.pi400_owner.channel", "telegram.main"),
        ] {
            let mut config = Config::default();
            assert!(
                config.set_prop(path, value).is_err(),
                "precondition: {path} should be unknown on a fresh config"
            );
            let created = ensure_map_key_for_prop_path(&mut config, path)
                .expect("newly-widened alias sections should materialize");
            assert!(created, "{path}'s alias should be created");
            assert!(
                config.set_prop(path, value).is_ok(),
                "{path} must be settable after map-key materialization"
            );
        }
    }

    #[test]
    fn ensure_map_key_for_prop_path_refuses_reserved_default_agent() {
        let mut config = Config::default();

        let created = ensure_map_key_for_prop_path(&mut config, "agents.default.enabled")
            .expect("agents is a known map-keyed section; refusal is not an error");
        assert!(
            !created,
            "must refuse to auto-create the reserved `default` agent alias"
        );
        assert!(
            config.agents.is_empty(),
            "no `agents.default` entry should have been left behind by the refused create"
        );

        let created = ensure_map_key_for_prop_path(&mut config, "agents.researcher.enabled")
            .expect("non-reserved agent aliases should still materialize");
        assert!(
            created,
            "agents.<non-default> must still auto-materialize like every other widened section"
        );
        assert!(
            config.agents.contains_key("researcher"),
            "researcher alias should have been created"
        );
    }

    #[test]
    fn ensure_map_key_rolls_back_alias_on_unknown_tail_field() {
        let mut config = Config::default();
        let path = "risk_profiles.newprofile.not_a_real_field";

        let created = ensure_map_key_for_prop_path(&mut config, path)
            .expect("section resolves; only the tail field is bogus");
        assert!(
            !created,
            "must not report success when the tail field doesn't resolve"
        );
        assert!(
            config
                .get_map_keys("risk_profiles")
                .unwrap_or_default()
                .is_empty(),
            "the tentatively-created alias must be rolled back, not left dangling",
        );
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn config_set_materializes_missing_channel_alias() {
        let mut config = Config::default();
        let path = "channels.telegram.default.bot_token";

        assert!(
            !config.channels.telegram.contains_key("default"),
            "fresh config should not already contain the default channel alias"
        );

        let created = ensure_map_key_for_prop_path(&mut config, path)
            .expect("known channel path should be materialized");

        assert!(created, "missing channel alias should be created");
        config
            .set_prop_persistent(path, "test-token")
            .expect("materialized channel path should be writable");
        assert_eq!(
            config
                .channels
                .telegram
                .get("default")
                .unwrap()
                .bot_token
                .as_str(),
            "test-token"
        );
    }

    #[tokio::test]
    #[cfg(feature = "agent-runtime")]
    async fn sop_maintenance_tick_dispatches_cached_cron_triggers() {
        use std::sync::{Arc, Mutex};
        use zeroclaw_config::schema::{MemoryConfig, SopConfig};
        use zeroclaw_memory::traits::Memory;
        use zeroclaw_runtime::sop::{
            Sop, SopEngine, SopExecutionMode, SopPriority, SopStep, SopStepKind, SopTrigger,
        };

        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![Sop {
            name: "cron-sop".into(),
            description: "cron regression".into(),
            version: "0.1.0".into(),
            execution_mode: SopExecutionMode::Supervised,
            priority: SopPriority::Normal,
            triggers: vec![SopTrigger::Cron {
                expression: "* * * * *".into(),
            }],
            steps: vec![SopStep {
                number: 1,
                title: "Step one".into(),
                body: "Do step one".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
                kind: SopStepKind::default(),
                schema: None,
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 2,
            location: None,
            deterministic: false,
            admission_policy: zeroclaw_runtime::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }]);
        let engine = Arc::new(Mutex::new(engine));

        let tmp = tempfile::tempdir().expect("temp dir");
        let mem_cfg = MemoryConfig {
            backend: "sqlite".into(),
            ..MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(zeroclaw_runtime::sop::SopAuditLogger::new(memory));
        let cache = zeroclaw_runtime::sop::dispatch::SopCronCache::from_engine(&engine);

        let mut last_cron_check = chrono::Utc::now() - chrono::Duration::minutes(2);
        let report =
            run_sop_maintenance_tick(&engine, Some(&audit), Some(&cache), &mut last_cron_check)
                .await
                .expect("maintenance tick should complete");

        assert_eq!(report.cron_started, 1);
        assert_eq!(engine.lock().unwrap().active_runs().len(), 1);
    }

    #[test]
    #[cfg(feature = "agent-runtime")]
    fn agent_fallback_uses_hardcoded_when_config_uses_default() {
        // Test that when config uses default value (0.7), fallback still works
        let config = Config::default();

        // Simulate None temperature (user didn't provide --temperature)
        let user_temperature: Option<f64> = std::hint::black_box(None);
        let final_temperature = user_temperature.unwrap_or_else(|| {
            config
                .providers
                .models
                .iter_entries()
                .next()
                .and_then(|(_, _, e)| e.temperature)
                .unwrap_or(0.7)
        });

        assert!((final_temperature - 0.7).abs() < f64::EPSILON);
    }

    #[tokio::test]
    #[cfg(feature = "agent-runtime")]
    async fn gate_security_posture_fails_closed_unless_allowed() {
        use crate::config::schema::Config;

        // Clean posture: no gate, no nag.
        let clean = Config::default();
        assert!(clean.degraded_security.is_empty());
        let handle = gate_security_posture(&clean, false).expect("clean posture must pass");
        assert!(handle.is_none(), "clean posture must not spawn a nag");

        // Degraded posture, not allowed: must refuse to serve.
        let mut degraded = Config::default();
        degraded.degraded_security = vec!["security".to_string()];
        assert!(
            gate_security_posture(&degraded, false).is_err(),
            "degraded posture must fail closed when not explicitly allowed"
        );

        // Degraded posture, explicitly allowed: boots and returns a nag handle.
        let nag = gate_security_posture(&degraded, true)
            .expect("degraded posture must boot when allowed")
            .expect("allowed degraded posture must spawn a nag task");
        nag.abort();

        // Whole-config loss (sentinel marker) is degraded too: same fail-closed
        // behavior so a defaulted security posture cannot serve silently.
        let mut whole = Config::default();
        whole.degraded_security = vec![crate::config::migration::WHOLE_CONFIG_SENTINEL.to_string()];
        assert!(
            gate_security_posture(&whole, false).is_err(),
            "whole-config loss must fail closed when not explicitly allowed"
        );
    }

    #[tokio::test]
    #[cfg(feature = "agent-runtime")]
    async fn models_set_persists_model_and_preserves_slash_bearing_ids() {
        use crate::config::schema::{AnthropicModelProviderConfig, Config, ModelProviderConfig};

        let tmp = tempfile::TempDir::new().expect("temp dir");
        let config_path = tmp.path().join("config.toml");

        std::fs::write(
            &config_path,
            format!(
                "schema_version = {}\n\n[providers.models.anthropic.default]\nmodel = \"claude-opus-4-7\"\n",
                crate::config::migration::CURRENT_SCHEMA_VERSION,
            ),
        )
        .unwrap();

        let mut config = Config {
            config_path: config_path.clone(),
            data_dir: tmp.path().join("workspace"),
            schema_version: crate::config::migration::CURRENT_SCHEMA_VERSION,
            ..Config::default()
        };
        config.providers.models.anthropic.insert(
            "default".to_string(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("claude-opus-4-7".to_string()),
                    ..Default::default()
                },
            },
        );

        // ── Test 1: Normal model ID persists via the dispatch boundary ──
        dispatch_models_command(
            ModelCommands::Set {
                model: "claude-sonnet-4-6".to_string(),
            },
            &mut config,
        )
        .await
        .expect("normal model ID must persist");

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            contents.contains("claude-sonnet-4-6"),
            "normal model ID must be persisted to config.toml; got:\n{contents}"
        );

        // ── Test 2: Slash-bearing model ID preserved as-is ──
        dispatch_models_command(
            ModelCommands::Set {
                model: "anthropic/claude-sonnet-4-20250514".to_string(),
            },
            &mut config,
        )
        .await
        .expect("slash-bearing model ID must persist");

        let contents = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            contents.contains("anthropic/claude-sonnet-4-20250514"),
            "slash-bearing model ID must be stored as-is; got:\n{contents}"
        );

        // ── Test 3: No configured provider → error surfaced by dispatch ──
        let mut empty_config = Config {
            config_path: tmp.path().join("empty.toml"),
            data_dir: tmp.path().join("empty_workspace"),
            schema_version: crate::config::migration::CURRENT_SCHEMA_VERSION,
            ..Config::default()
        };
        let err = dispatch_models_command(
            ModelCommands::Set {
                model: "any-model".to_string(),
            },
            &mut empty_config,
        )
        .await
        .expect_err("empty config must fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("No model provider configured"),
            "error must mention missing provider; got: {msg}"
        );
    }
}
