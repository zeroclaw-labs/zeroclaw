//! Grok Build CLI subprocess model provider.
//!
//! Every inference request uses the documented `grok agent stdio` ACP
//! transport. The assembled prompt is sent as JSON-RPC on child stdin; it is
//! never placed in argv or a temporary file.
//! Authentication uses the Grok CLI login cache by default. API-key auth is an
//! explicit per-alias opt-in: export `XAI_API_KEY` into the ZeroClaw process and
//! list that name in `env_passthrough`. Typed alias `api_key` is rejected; this
//! provider does not store the key in config. At each spawn the child allowlist
//! resolves process env by name (including `XAI_API_KEY` when opted in) so the
//! CLI login cache remains the default owner and the key is never snapshotted
//! into the long-lived provider handle. A future typed Config bridge may map
//! the same name at load time; until then the documented process-env +
//! `env_passthrough` surface is the single operator credential path.
//!
//! # Usage
//!
//! ```toml
//! [providers.models.grok_cli.default]
//! model = "grok-4.5"
//! working_directory = "/path/to/agents/default/workspace"
//! # binary_path = "/home/you/.grok/bin/grok"
//!
//! [agents.default]
//! model_provider = "grok_cli.default"
//! classifier_provider = "xai.default"
//! ```
//!
//! # Host-side trust boundary
//!
//! The child is a tool-capable agent outside ZeroClaw's native tool approval
//! path. The provider therefore:
//!
//! - requires an explicit absolute working directory;
//! - clears the inherited environment and forwards only process-runtime entries
//!   plus explicit per-alias `env_passthrough` names;
//! - defaults to `--no-plan`, `--sandbox strict`, `--permission-mode dontAsk`,
//!   and an empty built-in tool set;
//! - rejects ACP permission requests unless the alias explicitly enables a
//!   bypass mode, in which case it selects the request's allow-once option;
//! - bounds ACP frames, configurable aggregate stdout, assistant output, and stderr;
//! - on success, failure, timeout, or cancellation, terminates the child
//!   process group (Unix) or Job Object (Windows) and reaps the direct child.
//!   Descendants that create a new session/process group can escape group
//!   kill on Unix; the public guarantee is process-group / Job Object scope,
//!   not every possible detached process on the host;
//! - never includes child stderr or raw ACP frames (or child-controlled
//!   protocol free-text such as stopReason / protocolVersion / string error
//!   codes) in public errors or logs.
//!
//! Operators can deliberately relax the CLI policy through `extra_args` (for
//! example, `--tools=Read,Grep`, `--permission-mode=acceptEdits`, or
//! `--sandbox=workspace`). Such overrides are explicit per-provider-alias
//! opt-ins. Bypass flags also authorize the headless ACP client to approve
//! interactive permission requests for that alias.
//!
//! # Limitations
//!
//! - Only the system prompt (if present) and final user message are forwarded.
//! - The provider is one-shot; ACP sessions are not resumed.
//! - Native ZeroClaw tool calls are not emitted; `ChatRequest.tools` is ignored.
//! - Token usage is not reported (`ChatResponse.usage` is `None`).
//! - Only temperatures `0.7` and `1.0` are accepted because Grok Build has no
//!   corresponding sampling flag.
//! - `timeout_secs` is clamped to one hour maximum.

mod acp;

use crate::traits::{ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, JobObject, KillOnDrop};
#[cfg(not(windows))]
use tokio::process::Child;

/// Default `grok` binary name (resolved via `PATH`).
const DEFAULT_GROK_CLI_BINARY: &str = "grok";

/// The only provider-owned environment variable an alias may explicitly pass.
const XAI_API_KEY_ENV: &str = "XAI_API_KEY";

/// Model name used to signal "use the CLI / config default model".
const DEFAULT_MODEL_MARKER: &str = "default";

/// Default wall-clock budget when `timeout_secs` is unset on the alias.
const DEFAULT_GROK_CLI_TIMEOUT: Duration = Duration::from_secs(600);

/// Upper bound for alias `timeout_secs` so a misconfigured value cannot hold a
/// provider slot indefinitely. Values above this are clamped.
const MAX_GROK_CLI_TIMEOUT_SECS: u64 = 3_600;

/// Additional bounded time allowed for reaping a terminated child.
const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded grace period for a cooperative ACP stdin shutdown.
const STDIN_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

/// Additional bounded time allowed for the stderr drain to observe EOF.
const STDERR_DRAIN_FINISH_TIMEOUT: Duration = Duration::from_millis(250);

/// Stderr is drained continuously, but only bounded metadata is retained.
const MAX_GROK_CLI_STDERR_BYTES: usize = 65_536;

const GROK_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// Default OS sandbox profile when the operator does not provide one.
const DEFAULT_SANDBOX_PROFILE: &str = "strict";

/// Provider-owned argv tokens. `extra_args` cannot replace the ACP transport,
/// prompt/session plumbing, model, cwd, or update policy.
const RESERVED_EXTRA_ARG_FLAGS: &[&str] = &[
    "-p",
    "--single",
    "--prompt-file",
    "--prompt-json",
    "--output-format",
    "--no-auto-update",
    "-m",
    "--model",
    "-s",
    "--session-id",
    "-r",
    "--resume",
    "-c",
    "--continue",
    "--cwd",
    "--debug-file",
    "--fork-session",
    "--json-schema",
    "--leader-socket",
    "--restore-code",
    "--system-prompt-override",
    "--system-prompt",
    "-w",
    "--worktree",
    "--worktree-ref",
    "--ref",
    "agent",
    "stdio",
];

/// Grok global flags that are safe to pass without an inline value. Requiring
/// every other override to use `--flag=value` prevents it from consuming the
/// provider-owned trailing `agent stdio` tokens as an option value.
const VALUELESS_EXTRA_ARG_FLAGS: &[&str] = &[
    "--always-approve",
    "--dangerously-skip-permissions",
    "--debug",
    "--disable-web-search",
    "--experimental-memory",
    "--no-alt-screen",
    "--no-memory",
    "--no-plan",
    "--no-subagents",
    "--oauth",
    "--verbatim",
    "--yolo",
];

/// Known Grok global options that consume exactly one following value. This
/// list preserves the existing `extra_args = ["--flag", "value"]` form while
/// still proving that no option can consume the trailing `agent stdio` tokens.
const VALUE_TAKING_EXTRA_ARG_FLAGS: &[&str] = &[
    "--agent",
    "--agents",
    "--allow",
    "--allowedTools",
    "--best-of-n",
    "--deny",
    "--disallowed-tools",
    "--disallowedTools",
    "--effort",
    "--max-turns",
    "--permission-mode",
    "--reasoning-effort",
    "--rules",
    "--sandbox",
    "--tools",
];

/// Environment variable names required for process startup, locale, and
/// explicitly configured network proxies.
const ENV_ALLOWLIST_EXACT: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USER",
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGNAME",
    "SHELL",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Model provider that invokes Grok Build through ACP.
pub struct GrokCliModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `grok` binary.
    binary_path: PathBuf,
    /// Canonical subprocess cwd and ACP session boundary.
    working_directory: PathBuf,
    /// Operator-selected environment variable names inherited at spawn time.
    env_passthrough: Vec<String>,
    /// Operator-supplied argv tokens, after provider-owned flag validation.
    extra_args: Vec<String>,
    /// Aggregate stdout budget for one ACP request.
    max_acp_stdout_bytes: usize,
    /// Wall-clock timeout for the ACP request.
    timeout: Duration,
    /// When true, serialize image markers as ACP `image` blocks and report
    /// vision capability. Does **not** rewrite Grok's `promptCapabilities.image`
    /// advertise (still false through 0.2.118); only ZeroClaw's send path.
    /// Live probes: protocol accepts the blocks, model does not reliably use
    /// the image content.
    vision_enabled: bool,
}

/// Typed builder for [`GrokCliModelProvider`].
///
/// `alias` and an existing absolute `working_directory` are required. Other
/// values use the provider's fail-closed defaults when unset.
#[must_use]
pub struct GrokCliBuilder {
    alias: String,
    binary_path: Option<String>,
    working_directory: Option<String>,
    env_passthrough: Vec<String>,
    extra_args: Vec<String>,
    max_acp_stdout_bytes: Option<usize>,
    timeout_secs: Option<u64>,
    vision_enabled: bool,
}

impl GrokCliBuilder {
    /// Override the `grok` binary path. Whitespace-only inputs use PATH lookup.
    pub fn binary_path(mut self, path: Option<&str>) -> Self {
        self.binary_path = path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        self
    }

    /// Set the subprocess cwd and ACP session boundary.
    pub fn working_directory(mut self, path: &str) -> Self {
        self.working_directory = Some(path.to_string());
        self
    }

    /// Add operator-selected environment variables to the child allowlist.
    pub fn env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// Set operator-selected Grok global flags.
    pub fn extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Set the aggregate stdout budget for one ACP request.
    pub fn max_acp_stdout_bytes(mut self, max_acp_stdout_bytes: Option<usize>) -> Self {
        self.max_acp_stdout_bytes = max_acp_stdout_bytes;
        self
    }

    /// Set the complete ACP request deadline. Zero uses the provider default.
    pub fn timeout_secs(mut self, timeout_secs: Option<u64>) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Enable sending ACP image blocks from the alias `vision = true` config.
    /// See the field docs: this is a local send-path switch, not a Grok
    /// capability rewrite.
    pub fn vision_enabled(mut self, vision_enabled: bool) -> Self {
        self.vision_enabled = vision_enabled;
        self
    }

    pub fn build(self) -> anyhow::Result<GrokCliModelProvider> {
        let binary_path = self
            .binary_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GROK_CLI_BINARY));
        let working_directory = GrokCliModelProvider::validate_working_directory(
            self.working_directory.as_deref().unwrap_or_default(),
        )?;
        let env_passthrough =
            GrokCliModelProvider::normalize_and_validate_env_passthrough(self.env_passthrough)?;
        let extra_args = GrokCliModelProvider::normalize_and_validate_extra_args(self.extra_args)?;
        let max_acp_stdout_bytes =
            GrokCliModelProvider::validate_acp_stdout_limit(self.max_acp_stdout_bytes)?;
        let timeout = self
            .timeout_secs
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Duration::from_secs(seconds.min(MAX_GROK_CLI_TIMEOUT_SECS)))
            .unwrap_or(DEFAULT_GROK_CLI_TIMEOUT);
        Ok(GrokCliModelProvider {
            alias: self.alias,
            binary_path,
            working_directory,
            env_passthrough,
            extra_args,
            max_acp_stdout_bytes,
            timeout,
            vision_enabled: self.vision_enabled,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StderrSummary {
    bytes_seen: usize,
    truncated: bool,
    read_failed: bool,
}

struct StderrDrain {
    task: Option<JoinHandle<StderrSummary>>,
}

impl StderrDrain {
    fn start(mut stderr: ChildStderr) -> Self {
        let task = ::zeroclaw_spawn::spawn!(async move {
            let mut summary = StderrSummary::default();
            let mut chunk = [0_u8; 8192];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let remaining =
                            MAX_GROK_CLI_STDERR_BYTES.saturating_sub(summary.bytes_seen);
                        let retained = remaining.min(n);
                        summary.bytes_seen = summary.bytes_seen.saturating_add(retained);
                        summary.truncated |= retained < n;
                    }
                    Err(_) => {
                        summary.read_failed = true;
                        break;
                    }
                }
            }
            summary
        });
        Self { task: Some(task) }
    }

    async fn finish(mut self) -> StderrSummary {
        let Some(mut task) = self.task.take() else {
            return StderrSummary::default();
        };
        match timeout(STDERR_DRAIN_FINISH_TIMEOUT, &mut task).await {
            Ok(Ok(summary)) => summary,
            Ok(Err(_)) => StderrSummary {
                read_failed: true,
                ..StderrSummary::default()
            },
            Err(_) => {
                task.abort();
                StderrSummary {
                    truncated: true,
                    ..StderrSummary::default()
                }
            }
        }
    }
}

impl Drop for StderrDrain {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Kill the process group on every exit path. The child is made its own group
/// before spawn, so this does not signal ZeroClaw itself.
#[cfg(unix)]
struct ProcessTreeGuard {
    pgid: Option<nix::unistd::Pid>,
}

#[cfg(unix)]
impl ProcessTreeGuard {
    fn new(child_pid: Option<u32>) -> Self {
        Self {
            pgid: child_pid
                .and_then(|pid| i32::try_from(pid).ok())
                .map(nix::unistd::Pid::from_raw),
        }
    }

    fn kill_now(&self) {
        if let Some(pgid) = self.pgid {
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
        }
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// On Windows, the child is assigned to a kill-on-close Job Object. Other
/// non-Unix targets fall back to Tokio's kill-on-drop child handling.
#[cfg(not(unix))]
struct ProcessTreeGuard;

#[cfg(not(unix))]
impl ProcessTreeGuard {
    fn new(_child_pid: Option<u32>) -> Self {
        Self
    }

    fn kill_now(&self) {}

    fn disarm(&mut self) {}
}

impl GrokCliModelProvider {
    /// Start a labelled construction chain for an ACP-backed provider.
    pub fn builder(alias: &str) -> GrokCliBuilder {
        GrokCliBuilder {
            alias: alias.to_string(),
            binary_path: None,
            working_directory: None,
            env_passthrough: Vec::new(),
            extra_args: Vec::new(),
            max_acp_stdout_bytes: None,
            timeout_secs: None,
            vision_enabled: false,
        }
    }

    fn validate_working_directory(value: &str) -> anyhow::Result<PathBuf> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            anyhow::bail!(
                "grok_cli requires an explicit working_directory for the ACP session boundary"
            );
        }
        let path = Path::new(trimmed);
        if !path.is_absolute() {
            anyhow::bail!("grok_cli working_directory must be an absolute path");
        }
        let canonical = std::fs::canonicalize(path).map_err(|_| {
            anyhow::Error::msg("grok_cli working_directory does not exist or is inaccessible")
        })?;
        if !canonical.is_dir() {
            anyhow::bail!("grok_cli working_directory must identify a directory");
        }
        Ok(canonical)
    }

    fn validate_acp_stdout_limit(value: Option<usize>) -> anyhow::Result<usize> {
        let limit = value.unwrap_or(acp::DEFAULT_ACP_STDOUT_LIMIT_BYTES);
        if !(acp::MIN_ACP_STDOUT_LIMIT_BYTES..=acp::MAX_ACP_STDOUT_LIMIT_BYTES).contains(&limit) {
            anyhow::bail!(
                "grok_cli max_acp_stdout_bytes must be between {} and {} bytes",
                acp::MIN_ACP_STDOUT_LIMIT_BYTES,
                acp::MAX_ACP_STDOUT_LIMIT_BYTES
            );
        }
        Ok(limit)
    }

    fn normalize_and_validate_env_passthrough(names: Vec<String>) -> anyhow::Result<Vec<String>> {
        let mut normalized: Vec<String> = Vec::with_capacity(names.len());
        for name in names {
            let name = name.trim();
            if !is_valid_env_var_name(name) {
                anyhow::bail!(
                    "grok_cli env_passthrough entry `{name}` is invalid; expected [A-Za-z_][A-Za-z0-9_]*"
                );
            }
            if is_disallowed_provider_env_var(name) {
                anyhow::bail!(
                    "grok_cli env_passthrough entry `{name}` is provider-owned \
                     (`XAI_*` other than `XAI_API_KEY`, and all `GROK_*`); \
                     other names (for example tool credentials) may be listed \
                     explicitly, and Grok CLI policy flags belong in `extra_args`"
                );
            }
            if !normalized
                .iter()
                .any(|existing| env_names_equal(existing.as_str(), name))
            {
                // Preserve the operator-supplied spelling; equality is
                // case-insensitive on Windows when checking membership later.
                normalized.push(name.to_string());
            }
        }
        Ok(normalized)
    }

    fn normalize_and_validate_extra_args(extra_args: Vec<String>) -> anyhow::Result<Vec<String>> {
        let extra_args: Vec<String> = extra_args
            .into_iter()
            .map(|arg| arg.trim().to_string())
            .filter(|arg| !arg.is_empty())
            .collect();
        let mut index = 0;
        while index < extra_args.len() {
            let arg = &extra_args[index];
            if arg == "--" || !arg.starts_with("--") {
                anyhow::bail!(
                    "grok_cli extra_args accepts long flags only and must not include positional arguments"
                );
            }
            let (flag, has_inline_value) = arg
                .split_once('=')
                .map_or((arg.as_str(), false), |(flag, _)| (flag, true));
            if RESERVED_EXTRA_ARG_FLAGS.contains(&flag) {
                anyhow::bail!(
                    "grok_cli extra_args must not include reserved flag `{flag}`. \
                     ACP transport, prompt, model, session, cwd, and update policy are owned by ZeroClaw."
                );
            }
            if has_inline_value || VALUELESS_EXTRA_ARG_FLAGS.contains(&flag) {
                index += 1;
                continue;
            }
            if VALUE_TAKING_EXTRA_ARG_FLAGS.contains(&flag) {
                let Some(value) = extra_args.get(index + 1) else {
                    anyhow::bail!(
                        "grok_cli extra_args option `{flag}` is missing its value and could consume the provider-owned ACP command"
                    );
                };
                if value.starts_with('-') {
                    anyhow::bail!(
                        "grok_cli extra_args option `{flag}` must use `--flag=value` when its value starts with `-`"
                    );
                }
                index += 2;
                continue;
            }
            anyhow::bail!(
                "grok_cli extra_args option `{flag}` must use `--flag=value` because its argument shape is not known"
            );
        }
        Ok(extra_args)
    }

    fn extra_args_set_any(extra_args: &[String], flags: &[&str]) -> bool {
        extra_args.iter().any(|arg| {
            let flag = arg.split('=').next().unwrap_or(arg);
            flags.contains(&flag)
        })
    }

    fn acp_permission_policy(extra_args: &[String]) -> acp::AcpPermissionPolicy {
        // Last matching permission-related flag wins, matching CLI last-wins
        // conventions so earlier bypass cannot stick after a later dontAsk.
        let mut policy = acp::AcpPermissionPolicy::RejectOnce;
        let mut index = 0;
        while index < extra_args.len() {
            let arg = &extra_args[index];
            if matches!(
                arg.as_str(),
                "--always-approve" | "--dangerously-skip-permissions" | "--yolo"
            ) {
                policy = acp::AcpPermissionPolicy::AllowOnce;
                index += 1;
                continue;
            }
            if let Some(mode) = arg.strip_prefix("--permission-mode=") {
                policy = if mode == "bypassPermissions" {
                    acp::AcpPermissionPolicy::AllowOnce
                } else {
                    acp::AcpPermissionPolicy::RejectOnce
                };
                index += 1;
                continue;
            }
            if arg == "--permission-mode" {
                let mode = extra_args.get(index + 1).map(String::as_str).unwrap_or("");
                policy = if mode == "bypassPermissions" {
                    acp::AcpPermissionPolicy::AllowOnce
                } else {
                    acp::AcpPermissionPolicy::RejectOnce
                };
                index += 2;
                continue;
            }
            index += 1;
        }
        policy
    }

    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        GROK_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|value| (temperature - value).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Grok CLI model provider received a non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Grok CLI model provider: {temperature}. \
                 Supported values: 0.7 or 1.0 (not forwarded; CLI has no sampling flag)"
            );
        }
        Ok(())
    }

    /// Build the documented ACP invocation. Permission and tool overrides are
    /// accepted only through explicit per-alias `extra_args`.
    fn build_cli_args(model: &str, extra_args: &[String]) -> Vec<String> {
        let mut args = Vec::with_capacity(16 + extra_args.len());
        args.push("--no-auto-update".to_string());
        args.push("--no-plan".to_string());

        if !Self::extra_args_set_any(extra_args, &["--sandbox"]) {
            args.push("--sandbox".to_string());
            args.push(DEFAULT_SANDBOX_PROFILE.to_string());
        }

        if !Self::extra_args_set_any(
            extra_args,
            &[
                "--permission-mode",
                "--always-approve",
                "--dangerously-skip-permissions",
                "--yolo",
            ],
        ) {
            args.push("--permission-mode".to_string());
            args.push("dontAsk".to_string());
        }

        if !Self::extra_args_set_any(extra_args, &["--tools"]) {
            args.push("--tools".to_string());
            args.push(String::new());
        }

        if Self::should_forward_model(model) {
            args.push(format!("--model={}", model.trim()));
        }
        args.extend(extra_args.iter().cloned());
        args.push("agent".to_string());
        args.push("stdio".to_string());
        args
    }

    fn apply_env_allowlist(cmd: &mut Command, env_passthrough: &[String]) -> bool {
        // Skip non-Unicode names/values rather than panicking on `vars()`.
        let variables = std::env::vars_os().filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            let value = value.into_string().ok()?;
            Some((key, value))
        });
        Self::apply_env_allowlist_from(cmd, env_passthrough, variables)
    }

    fn apply_env_allowlist_from(
        cmd: &mut Command,
        env_passthrough: &[String],
        variables: impl IntoIterator<Item = (String, String)>,
    ) -> bool {
        cmd.env_clear();
        let mut xai_api_key_available = false;
        for (key, value) in variables {
            if env_var_allowed(&key, env_passthrough) {
                if env_names_equal(&key, XAI_API_KEY_ENV) {
                    xai_api_key_available = !value.is_empty();
                }
                cmd.env(key, value);
            }
        }
        xai_api_key_available
    }

    async fn invoke_acp(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let prompt = self.acp_prompt_content(message);
        let args = Self::build_cli_args(model, &self.extra_args);
        let permission_policy = Self::acp_permission_policy(&self.extra_args);
        let mut cmd = Command::new(&self.binary_path);
        cmd.args(&args);
        cmd.current_dir(&self.working_directory);
        let xai_api_key_available = Self::apply_env_allowlist(&mut cmd, &self.env_passthrough);
        #[cfg(not(windows))]
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        #[cfg(windows)]
        let spawn_result = {
            let mut command = CommandWrap::from(cmd);
            command.wrap(KillOnDrop).wrap(JobObject);
            command.spawn()
        };
        #[cfg(not(windows))]
        let spawn_result = cmd.spawn();

        let mut child = match spawn_result {
            Ok(child) => child,
            Err(err) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "phase": "spawn",
                            "error_key": "grok_cli_spawn_failed",
                            "error": format!("{err}"),
                            "os_error": err.raw_os_error(),
                        })),
                    "grok_cli: failed to spawn ACP binary"
                );
                return Err(anyhow::Error::msg(format!(
                    "Failed to spawn Grok Build CLI ACP binary at {}: {err}. \
                     Ensure `grok` is installed and on PATH, or set binary_path.",
                    self.binary_path.display()
                )));
            }
        };

        let mut process_tree = ProcessTreeGuard::new(child.id());
        let Some(mut stdin) = take_child_stdin(&mut child) else {
            return Err(anyhow::Error::msg("Failed to open Grok ACP stdin"));
        };
        let Some(stdout) = take_child_stdout(&mut child) else {
            return Err(anyhow::Error::msg("Failed to open Grok ACP stdout"));
        };
        let Some(stderr) = take_child_stderr(&mut child) else {
            return Err(anyhow::Error::msg("Failed to open Grok ACP stderr"));
        };
        let stderr_drain = StderrDrain::start(stderr);

        let request_result = timeout(
            self.timeout,
            acp::run_oneshot_prompt(
                &mut stdin,
                stdout,
                &prompt,
                &self.working_directory,
                xai_api_key_available,
                permission_policy,
                self.max_acp_stdout_bytes,
            ),
        )
        .await;

        // Closing stdin tells a cooperative ACP server it can stop. We still
        // terminate the complete tree because a completed turn may have left
        // tool descendants running.
        let _ = timeout(STDIN_SHUTDOWN_TIMEOUT, stdin.shutdown()).await;
        drop(stdin);
        let reaped = terminate_child_tree(&mut child, &mut process_tree).await;
        let stderr_summary = stderr_drain.finish().await;

        match request_result {
            Ok(Ok(text)) if reaped => Ok(text),
            Ok(Ok(_)) => {
                log_acp_failure("reap", "grok_cli_reap_failed", stderr_summary);
                Err(anyhow::Error::msg(
                    "Grok Build CLI ACP process could not be reaped after completion",
                ))
            }
            Ok(Err(error)) => {
                log_acp_failure("protocol", error.error_key(), stderr_summary);
                Err(anyhow::Error::new(error))
            }
            Err(_) => {
                log_acp_failure("timeout", "grok_cli_timeout", stderr_summary);
                Err(anyhow::Error::msg(format!(
                    "Grok Build CLI ACP request timed out after {}s",
                    self.timeout.as_secs()
                )))
            }
        }
    }

    fn acp_prompt_content(&self, message: &str) -> Vec<acp::AcpPromptContent> {
        if !self.vision_enabled {
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        }

        // TODO(grok-cli-vision): Grok Build through 0.2.118 still advertises
        // `promptCapabilities.image = false`. With `vision = true`, ZeroClaw
        // sends ACP image blocks anyway. Live smoke on 0.2.118: session/prompt
        // accepts the blocks (no protocol error) but the model does not use
        // the image content. Keep this path experimental until advertise is
        // true *and* a live recognition smoke passes; then prefer following
        // the advertise bit instead of a local override.
        let (text, image_refs) = crate::multimodal::parse_image_markers(message);
        if image_refs.is_empty() {
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        }

        let Some(images) = image_refs
            .iter()
            .map(|image_ref| acp::AcpPromptContent::image_from_data_uri(image_ref))
            .collect::<Option<Vec<_>>>()
        else {
            // The runtime normally normalizes all provider-bound images into
            // data URIs. Preserve the original text on atypical direct calls
            // rather than silently dropping an image reference we cannot send.
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        };

        let mut prompt = Vec::with_capacity(images.len() + usize::from(!text.is_empty()));
        if !text.is_empty() {
            prompt.push(acp::AcpPromptContent::Text(text));
        }
        prompt.extend(images);
        prompt
    }
}

#[cfg(windows)]
type ManagedChild = Box<dyn ChildWrapper>;

#[cfg(not(windows))]
type ManagedChild = Child;

#[cfg(windows)]
fn take_child_stdin(child: &mut ManagedChild) -> Option<ChildStdin> {
    child.stdin().take()
}

#[cfg(not(windows))]
fn take_child_stdin(child: &mut ManagedChild) -> Option<ChildStdin> {
    child.stdin.take()
}

#[cfg(windows)]
fn take_child_stdout(child: &mut ManagedChild) -> Option<ChildStdout> {
    child.stdout().take()
}

#[cfg(not(windows))]
fn take_child_stdout(child: &mut ManagedChild) -> Option<ChildStdout> {
    child.stdout.take()
}

#[cfg(windows)]
fn take_child_stderr(child: &mut ManagedChild) -> Option<ChildStderr> {
    child.stderr().take()
}

#[cfg(not(windows))]
fn take_child_stderr(child: &mut ManagedChild) -> Option<ChildStderr> {
    child.stderr.take()
}

async fn terminate_child_tree(child: &mut ManagedChild, guard: &mut ProcessTreeGuard) -> bool {
    guard.kill_now();
    let _ = child.start_kill();
    match timeout(PROCESS_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => {
            guard.disarm();
            true
        }
        Ok(Err(_)) | Err(_) => false,
    }
}

fn log_acp_failure(phase: &'static str, error_key: &'static str, stderr: StderrSummary) {
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "phase": phase,
                "error_key": error_key,
                "stderr_bytes_seen": stderr.bytes_seen,
                "stderr_truncated": stderr.truncated,
                "stderr_read_failed": stderr.read_failed,
            })),
        "grok_cli: ACP subprocess failed"
    );
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

/// Environment name equality for allow/deny/dedup. Windows process env is
/// case-insensitive, so mixed-case spellings of the same variable must not
/// bypass denylists or miss allowlists there.
fn env_names_equal(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn env_name_starts_with(name: &str, prefix: &str) -> bool {
    if cfg!(windows) {
        name.len() >= prefix.len()
            && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    } else {
        name.starts_with(prefix)
    }
}

fn is_disallowed_provider_env_var(name: &str) -> bool {
    !env_names_equal(name, XAI_API_KEY_ENV)
        && (env_name_starts_with(name, "XAI_") || env_name_starts_with(name, "GROK_"))
}

fn env_var_allowed(key: &str, env_passthrough: &[String]) -> bool {
    ENV_ALLOWLIST_EXACT
        .iter()
        .any(|allowed| env_names_equal(key, allowed))
        || env_name_starts_with(key, "LC_")
        || env_passthrough
            .iter()
            .any(|name| env_names_equal(name, key))
}

#[async_trait]
impl ModelProvider for GrokCliModelProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            vision: self.vision_enabled,
            ..ProviderCapabilities::default()
        }
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if let Some(temperature) = temperature {
            Self::validate_temperature(temperature)?;
        }

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => format!("{system}\n\n{message}"),
            _ => message.to_string(),
        };
        self.invoke_acp(&full_message, model).await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;
        Ok(ChatResponse {
            text: Some(text),
            tool_calls: Vec::new(),
            // ACP one-shot path does not surface reliable token accounting.
            usage: None,
            reasoning_content: None,
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for GrokCliModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::GrokCli,
            ),
        )
    }

    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn provider(
        binary: Option<&str>,
        cwd: &Path,
        extra: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> GrokCliModelProvider {
        GrokCliModelProvider::builder("test")
            .binary_path(binary)
            .working_directory(cwd.to_str().expect("UTF-8 test path"))
            .extra_args(extra)
            .timeout_secs(timeout_secs)
            .build()
            .expect("provider")
    }

    #[test]
    fn builder_requires_explicit_absolute_working_directory() {
        let missing = match GrokCliModelProvider::builder("test").build() {
            Ok(_) => panic!("blank cwd must fail"),
            Err(error) => error,
        };
        assert!(missing.to_string().contains("requires an explicit"));

        let relative = match GrokCliModelProvider::builder("test")
            .working_directory("relative")
            .build()
        {
            Ok(_) => panic!("relative cwd must fail"),
            Err(error) => error,
        };
        assert!(relative.to_string().contains("absolute path"));
    }

    #[test]
    fn builder_canonicalizes_working_directory() {
        let temp = TempDir::new().expect("tempdir");
        let model_provider = provider(None, temp.path(), vec![], None);
        assert_eq!(
            model_provider.working_directory,
            std::fs::canonicalize(temp.path()).expect("canonical path")
        );
        assert_eq!(model_provider.binary_path, PathBuf::from("grok"));
        assert!(model_provider.env_passthrough.is_empty());
        assert_eq!(
            model_provider.max_acp_stdout_bytes,
            acp::DEFAULT_ACP_STDOUT_LIMIT_BYTES
        );
        assert_eq!(model_provider.timeout, DEFAULT_GROK_CLI_TIMEOUT);
        assert!(!model_provider.vision_enabled);
        assert!(!model_provider.capabilities().vision);
    }

    #[test]
    fn vision_override_serializes_normalized_images_as_acp_blocks() {
        let temp = TempDir::new().expect("tempdir");
        let model_provider = GrokCliModelProvider::builder("test")
            .working_directory(temp.path().to_str().expect("UTF-8 test path"))
            .vision_enabled(true)
            .build()
            .expect("vision-enabled provider");

        assert!(model_provider.capabilities().vision);
        assert_eq!(
            model_provider.acp_prompt_content("Describe [IMAGE:data:image/png;base64,cG5n]"),
            vec![
                acp::AcpPromptContent::Text("Describe".to_string()),
                acp::AcpPromptContent::Image {
                    data: "cG5n".to_string(),
                    mime_type: "image/png".to_string(),
                },
            ]
        );
    }

    #[test]
    fn disabled_vision_override_keeps_image_marker_as_text() {
        let temp = TempDir::new().expect("tempdir");
        let model_provider = provider(None, temp.path(), vec![], None);
        let message = "Describe [IMAGE:data:image/png;base64,cG5n]";

        assert_eq!(
            model_provider.acp_prompt_content(message),
            vec![acp::AcpPromptContent::Text(message.to_string())]
        );
    }

    #[test]
    fn builder_uses_and_validates_acp_stdout_limit() {
        let temp = TempDir::new().expect("tempdir");
        let cwd = temp.path().to_str().expect("UTF-8 test path");
        let configured = 8 * 1024 * 1024;
        let model_provider = GrokCliModelProvider::builder("test")
            .working_directory(cwd)
            .max_acp_stdout_bytes(Some(configured))
            .build()
            .expect("configured limit");
        assert_eq!(model_provider.max_acp_stdout_bytes, configured);

        for invalid in [
            0,
            acp::MIN_ACP_STDOUT_LIMIT_BYTES - 1,
            acp::MAX_ACP_STDOUT_LIMIT_BYTES + 1,
        ] {
            let result = GrokCliModelProvider::builder("test")
                .working_directory(cwd)
                .max_acp_stdout_bytes(Some(invalid))
                .build();
            let error = match result {
                Ok(_) => panic!("invalid stdout limit must fail"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("max_acp_stdout_bytes"));
        }
    }

    #[test]
    fn builder_validates_and_deduplicates_env_passthrough() {
        let temp = TempDir::new().expect("tempdir");
        let model_provider = GrokCliModelProvider::builder("test")
            .working_directory(temp.path().to_str().expect("UTF-8 test path"))
            .env_passthrough(vec![
                " AWS_ACCESS_KEY_ID ".to_string(),
                "AWS_ACCESS_KEY_ID".to_string(),
                "AWS_SECRET_ACCESS_KEY".to_string(),
                XAI_API_KEY_ENV.to_string(),
            ])
            .build()
            .expect("valid env passthrough");
        assert_eq!(
            model_provider.env_passthrough,
            [
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                XAI_API_KEY_ENV
            ]
        );

        let invalid = GrokCliModelProvider::builder("test")
            .working_directory(temp.path().to_str().expect("UTF-8 test path"))
            .env_passthrough(vec!["AWS-SECRET".to_string()])
            .build();
        assert!(invalid.is_err(), "invalid environment name must fail");

        for reserved in ["XAI_AUTH_TOKEN", "GROK_SANDBOX"] {
            let result = GrokCliModelProvider::builder("test")
                .working_directory(temp.path().to_str().expect("UTF-8 test path"))
                .env_passthrough(vec![reserved.to_string()])
                .build();
            assert!(
                result.is_err(),
                "provider-owned environment variable must fail: {reserved}"
            );
        }
    }

    #[test]
    fn builder_rejects_reserved_transport_flags() {
        let temp = TempDir::new().expect("tempdir");
        for flag in [
            "--prompt-file",
            "--single",
            "--cwd",
            "--debug-file",
            "--leader-socket",
            "--worktree",
            "agent",
            "stdio",
        ] {
            let result = GrokCliModelProvider::builder("test")
                .working_directory(temp.path().to_str().expect("UTF-8 test path"))
                .extra_args(vec![flag.to_string()])
                .build();
            assert!(result.is_err(), "reserved flag must fail: {flag}");
        }
    }

    #[test]
    fn builder_rejects_positional_extra_args() {
        let temp = TempDir::new().expect("tempdir");
        for extra in [
            vec!["PRIVATE_PROMPT".to_string()],
            vec!["-pPRIVATE_PROMPT".to_string()],
            vec!["--".to_string()],
        ] {
            let result = GrokCliModelProvider::builder("test")
                .working_directory(temp.path().to_str().expect("UTF-8 test path"))
                .extra_args(extra)
                .build();
            assert!(result.is_err(), "positional form must fail");
        }
    }

    #[test]
    fn builder_validates_values_that_could_consume_acp_command() {
        let temp = TempDir::new().expect("tempdir");
        let cwd = temp.path().to_str().expect("UTF-8 test path");
        let missing_value = GrokCliModelProvider::builder("test")
            .working_directory(cwd)
            .extra_args(vec!["--tools".to_string()])
            .build();
        let error = match missing_value {
            Ok(_) => panic!("missing inline value must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("missing its value"));

        assert!(
            GrokCliModelProvider::builder("test")
                .working_directory(cwd)
                .extra_args(vec![
                    "--tools".to_string(),
                    "Read,Grep".to_string(),
                    "--always-approve".to_string(),
                ])
                .build()
                .is_ok()
        );
    }

    #[test]
    fn build_cli_args_is_acp_only_and_fail_closed() {
        let args = GrokCliModelProvider::build_cli_args("grok-4.5", &[]);
        assert_eq!(&args[args.len() - 2..], ["agent", "stdio"]);
        assert!(args.iter().any(|arg| arg == "--no-auto-update"));
        assert!(args.iter().any(|arg| arg == "--no-plan"));

        let sandbox = args
            .iter()
            .position(|arg| arg == "--sandbox")
            .expect("sandbox flag");
        assert_eq!(args[sandbox + 1], "strict");

        let permission = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("permission flag");
        assert_eq!(args[permission + 1], "dontAsk");

        let tools = args
            .iter()
            .position(|arg| arg == "--tools")
            .expect("tools flag");
        assert!(args[tools + 1].is_empty());

        assert!(!args.iter().any(|arg| arg == "--single"));
        assert!(!args.iter().any(|arg| arg == "--prompt-file"));
        assert!(!args.iter().any(|arg| arg == "--output-format"));
        assert!(args.iter().any(|arg| arg == "--model=grok-4.5"));
    }

    #[test]
    fn build_cli_args_requires_explicit_policy_overrides() {
        let extra = vec![
            "--sandbox=off".to_string(),
            "--permission-mode=acceptEdits".to_string(),
            "--tools=Read,Grep".to_string(),
        ];
        let args = GrokCliModelProvider::build_cli_args(DEFAULT_MODEL_MARKER, &extra);
        assert!(args.windows(extra.len()).any(|window| window == extra));
        assert!(!args.iter().any(|arg| arg == "strict"));
        assert!(!args.iter().any(|arg| arg == "dontAsk"));
        assert!(!args.iter().any(String::is_empty));
        assert!(!args.iter().any(|arg| arg.starts_with("--model=")));
    }

    #[test]
    fn acp_permission_policy_requires_explicit_bypass() {
        for extra in [
            vec![],
            vec!["--permission-mode=dontAsk".to_string()],
            vec!["--permission-mode".to_string(), "acceptEdits".to_string()],
            vec!["--always-approve=false".to_string()],
        ] {
            assert_eq!(
                GrokCliModelProvider::acp_permission_policy(&extra),
                acp::AcpPermissionPolicy::RejectOnce
            );
        }

        for extra in [
            vec!["--always-approve".to_string()],
            vec!["--dangerously-skip-permissions".to_string()],
            vec!["--yolo".to_string()],
            vec!["--permission-mode=bypassPermissions".to_string()],
            vec![
                "--permission-mode".to_string(),
                "bypassPermissions".to_string(),
            ],
            vec![
                "--permission-mode=acceptEdits".to_string(),
                "--always-approve".to_string(),
            ],
            // last-wins: earlier dontAsk does not stick after bypass
            vec![
                "--permission-mode=dontAsk".to_string(),
                "--permission-mode=bypassPermissions".to_string(),
            ],
        ] {
            assert_eq!(
                GrokCliModelProvider::acp_permission_policy(&extra),
                acp::AcpPermissionPolicy::AllowOnce
            );
        }

        // last-wins: earlier bypass is cancelled by a later non-bypass mode
        assert_eq!(
            GrokCliModelProvider::acp_permission_policy(&[
                "--permission-mode=bypassPermissions".to_string(),
                "--permission-mode=dontAsk".to_string(),
            ]),
            acp::AcpPermissionPolicy::RejectOnce
        );
    }

    #[test]
    fn validate_temperature_allows_only_cli_baselines() {
        assert!(GrokCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(GrokCliModelProvider::validate_temperature(1.0).is_ok());
        assert!(GrokCliModelProvider::validate_temperature(0.2).is_err());
        assert!(GrokCliModelProvider::validate_temperature(f64::NAN).is_err());
    }

    #[test]
    fn env_allowlist_blocks_unrelated_secrets() {
        let none = &[];
        assert!(env_var_allowed("PATH", none));
        assert!(env_var_allowed("LC_ALL", none));
        assert!(!env_var_allowed("XAI_API_KEY", none));
        assert!(!env_var_allowed("XAI_AUTH_TOKEN", none));
        assert!(!env_var_allowed("GROK_SANDBOX", none));
        assert!(!env_var_allowed("ZEROCLAW_SLACK_BOT_TOKEN", none));
        assert!(!env_var_allowed("AWS_SECRET_ACCESS_KEY", none));
        assert!(!env_var_allowed("OPENAI_API_KEY", none));
    }

    #[test]
    fn env_allowlist_accepts_only_explicit_passthrough_names() {
        let passthrough = vec!["AWS_ACCESS_KEY_ID".to_string()];
        assert!(env_var_allowed("AWS_ACCESS_KEY_ID", &passthrough));
        assert!(!env_var_allowed("AWS_SECRET_ACCESS_KEY", &passthrough));
    }

    #[cfg(windows)]
    #[test]
    fn windows_env_allow_and_deny_are_case_insensitive() {
        assert!(env_var_allowed("Path", &[]), "Windows PATH via mixed case");
        assert!(
            env_var_allowed("SystemRoot", &[]),
            "Windows SYSTEMROOT via mixed case"
        );
        assert!(
            is_disallowed_provider_env_var("grok_sandbox"),
            "GROK_* denylist must match case-insensitively on Windows"
        );
        assert!(
            is_disallowed_provider_env_var("Xai_Auth_Token"),
            "XAI_* denylist must match case-insensitively on Windows"
        );
        assert!(
            !is_disallowed_provider_env_var("xai_api_key"),
            "XAI_API_KEY exception must match case-insensitively on Windows"
        );
        let deduped = GrokCliModelProvider::normalize_and_validate_env_passthrough(vec![
            "Aws_Access_Key_Id".to_string(),
            "AWS_ACCESS_KEY_ID".to_string(),
        ])
        .expect("mixed-case passthrough");
        assert_eq!(
            deduped.len(),
            1,
            "duplicate names that differ only by case must collapse on Windows"
        );
        let mut command = Command::new("grok");
        let available = GrokCliModelProvider::apply_env_allowlist_from(
            &mut command,
            &["XAI_API_KEY".to_string()],
            [("xai_api_key".to_string(), "mixed-case-key".to_string())],
        );
        assert!(available);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_env_allow_and_deny_remain_case_sensitive() {
        assert!(
            !env_var_allowed("Path", &[]),
            "Unix PATH allowlist stays exact-case"
        );
        assert!(
            !is_disallowed_provider_env_var("grok_sandbox"),
            "Unix GROK_* denylist stays exact-case"
        );
        assert!(is_disallowed_provider_env_var("GROK_SANDBOX"));
    }

    #[test]
    fn apply_env_allowlist_copies_only_builtin_and_explicit_entries() {
        let mut command = Command::new("grok");
        let xai_api_key_available = GrokCliModelProvider::apply_env_allowlist_from(
            &mut command,
            &["AWS_ACCESS_KEY_ID".to_string()],
            [
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("AWS_ACCESS_KEY_ID".to_string(), "test-access".to_string()),
                (
                    "AWS_SECRET_ACCESS_KEY".to_string(),
                    "test-secret".to_string(),
                ),
                (
                    "ZEROCLAW_SLACK_BOT_TOKEN".to_string(),
                    "test-token".to_string(),
                ),
                ("XAI_API_KEY".to_string(), "test-xai-key".to_string()),
                ("GROK_SANDBOX".to_string(), "off".to_string()),
            ],
        );
        assert!(!xai_api_key_available);
        let copied: Vec<_> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(copied.contains(&("PATH".to_string(), Some("/usr/bin".to_string()))));
        assert!(copied.contains(&(
            "AWS_ACCESS_KEY_ID".to_string(),
            Some("test-access".to_string())
        )));
        assert!(
            !copied
                .iter()
                .any(|(name, _)| name == "AWS_SECRET_ACCESS_KEY")
        );
        assert!(
            !copied
                .iter()
                .any(|(name, _)| name == "ZEROCLAW_SLACK_BOT_TOKEN")
        );
        assert!(!copied.iter().any(|(name, _)| name == "XAI_API_KEY"));
        assert!(!copied.iter().any(|(name, _)| name == "GROK_SANDBOX"));
    }

    #[test]
    fn explicit_xai_api_key_passthrough_controls_acp_auth_availability() {
        let mut command = Command::new("grok");
        let available = GrokCliModelProvider::apply_env_allowlist_from(
            &mut command,
            &[XAI_API_KEY_ENV.to_string()],
            [(XAI_API_KEY_ENV.to_string(), "test-api-key".to_string())],
        );
        assert!(available);
        assert!(command.as_std().get_envs().any(|(name, value)| {
            name == XAI_API_KEY_ENV && value.is_some_and(|value| value == "test-api-key")
        }));

        let mut empty_command = Command::new("grok");
        let available = GrokCliModelProvider::apply_env_allowlist_from(
            &mut empty_command,
            &[XAI_API_KEY_ENV.to_string()],
            [(XAI_API_KEY_ENV.to_string(), String::new())],
        );
        assert!(!available, "an empty API key must not select API-key auth");
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_stable_error() {
        let temp = TempDir::new().expect("tempdir");
        let model_provider = provider(Some("/nonexistent/path/to/grok"), temp.path(), vec![], None);
        let error = model_provider
            .invoke_acp("hello", "default")
            .await
            .expect_err("missing binary must fail");
        let message = error.to_string();
        assert!(message.contains("Failed to spawn Grok Build CLI ACP binary"));
        assert!(!message.contains("prompt-file"));
    }

    /// Compatibility check that executes the installed CLI's ACP command
    /// parser when Grok Build is available locally.
    #[test]
    fn local_grok_accepts_documented_acp_security_flags_when_available() {
        let version = std::process::Command::new("grok").arg("--version").output();
        let Ok(version) = version else {
            return;
        };
        if !version.status.success() {
            return;
        }

        let output = std::process::Command::new("grok")
            .args([
                "--no-auto-update",
                "--no-plan",
                "--sandbox",
                "strict",
                "--permission-mode",
                "dontAsk",
                "--tools",
                "",
                "agent",
                "stdio",
                "--help",
            ])
            .output()
            .expect("installed grok must execute ACP help");
        assert!(
            output.status.success(),
            "installed Grok Build rejected the ACP/security argv; version={} stderr={}",
            String::from_utf8_lossy(&version.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let help = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(help.contains("grok agent stdio"));
    }

    /// Explicit live contract check for reviewer/maintainer evidence. Unlike
    /// the parser compatibility test above, this never silently skips: invoke
    /// it deliberately on a host with an installed and authenticated CLI.
    #[tokio::test]
    #[ignore = "requires an installed and authenticated Grok Build CLI"]
    async fn live_grok_acp_small_and_large_prompts() {
        let version = std::process::Command::new("grok")
            .arg("--version")
            .output()
            .expect("installed Grok Build CLI");
        assert!(version.status.success(), "grok --version must succeed");
        eprintln!("{}", String::from_utf8_lossy(&version.stdout).trim());

        for (label, prompt, marker) in [
            (
                "small",
                "Output only this token: ACP_LIVE_SMALL".to_string(),
                "ACP_LIVE_SMALL",
            ),
            (
                "large",
                format!(
                    "{}Output only this token: ACP_LIVE_LARGE",
                    " ".repeat(33_000)
                ),
                "ACP_LIVE_LARGE",
            ),
        ] {
            let temp = TempDir::new().expect("live Grok cwd");
            let model_provider = provider(None, temp.path(), vec![], Some(120));
            let reply = model_provider
                .invoke_acp(&prompt, DEFAULT_MODEL_MARKER)
                .await
                .expect("live Grok ACP response");
            eprintln!("{label}: {reply}");
            // The live agent may include additional prose in its final
            // answer; the marker proves that the complete prompt reached
            // Grok without treating model wording as deterministic test
            // output.
            assert!(
                reply.contains(marker),
                "live Grok ACP response must contain {marker}: {reply}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires an installed and authenticated Grok Build CLI"]
    async fn live_grok_acp_waits_for_native_tool_completion() {
        // Tool-enabled aliases are an explicit `extra_args` opt-in. Grok must
        // pre-authorize the shell form via `--allow` so the ACP client never
        // sees `session/request_permission` (the client defaults to
        // reject-once). Under the default `--sandbox strict`, Grok Build
        // 0.2.118 still escalates a matching `Bash(printf *)` allow to the
        // ACP host; `--sandbox=workspace` lets the allow rule pre-authorize
        // without host approval. Bypass flags are covered by a separate live
        // test; this one proves the allow-preauth path.
        let temp = TempDir::new().expect("live Grok cwd");
        let model_provider = provider(
            None,
            temp.path(),
            vec![
                "--tools=run_terminal_cmd".to_string(),
                "--allow=Bash(printf *)".to_string(),
                "--sandbox=workspace".to_string(),
            ],
            Some(120),
        );
        let reply = model_provider
            .invoke_acp(
                "Use the shell tool to run `printf ACP_TOOL_EXEC_OK`. After the tool succeeds, output the token ACP_TOOL_FINAL_OK.",
                DEFAULT_MODEL_MARKER,
            )
            .await
            .expect("live Grok ACP tool response");
        eprintln!("tool: {reply}");
        assert!(reply.contains("ACP_TOOL_FINAL_OK"), "reply: {reply}");
    }

    #[tokio::test]
    #[ignore = "requires an installed and authenticated Grok Build CLI"]
    async fn live_grok_acp_allows_prompted_complex_shell_in_bypass_mode() {
        let temp = TempDir::new().expect("live Grok cwd");
        let marker = temp.path().join("permission-marker");
        let model_provider = provider(
            None,
            temp.path(),
            vec![
                "--sandbox=workspace".to_string(),
                "--tools=run_terminal_cmd".to_string(),
                "--permission-mode=bypassPermissions".to_string(),
                "--deny=Bash(false *)".to_string(),
            ],
            Some(120),
        );
        let reply = model_provider
            .invoke_acp(
                "Use the shell tool to run exactly this multiline command:\n```\n# OpenClaw style memory in agent home\nprintf ACP_PERMISSION_EXEC_OK > permission-marker\n```\nAfter the marker is written, output the token ACP_PERMISSION_FINAL_OK.",
                DEFAULT_MODEL_MARKER,
            )
            .await
            .expect("live Grok ACP permission response");
        eprintln!("permission: {reply}");
        assert!(reply.contains("ACP_PERMISSION_FINAL_OK"), "reply: {reply}");
        assert_eq!(
            std::fs::read_to_string(marker).expect("tool marker"),
            "ACP_PERMISSION_EXEC_OK"
        );
    }

    #[tokio::test]
    #[ignore = "requires an installed and authenticated Grok Build CLI"]
    async fn live_grok_acp_rejects_unapproved_tool() {
        let temp = TempDir::new().expect("live Grok cwd");
        let marker = temp.path().join("denied-marker");
        let model_provider = provider(
            None,
            temp.path(),
            vec![
                "--tools=run_terminal_cmd".to_string(),
                "--permission-mode=dontAsk".to_string(),
            ],
            Some(120),
        );
        let error = model_provider
            .invoke_acp(
                "Attempt to use the shell tool to run exactly `printf ACP_DENY_EXEC_BAD > denied-marker`. Whether the tool is denied or succeeds, output the token ACP_DENY_FINAL_OK.",
                DEFAULT_MODEL_MARKER,
            )
            .await
            .expect_err("unapproved shell must fail closed");
        assert!(
            error
                .to_string()
                .contains("completed without agent message text"),
            "error: {error}"
        );
        assert!(!marker.exists(), "deny rule must prevent the tool write");
    }

    #[cfg(unix)]
    mod unix_fake_child {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use tokio::time::{Instant, sleep};

        const SUCCESS_BODY: &str = r#"
printf '%s\n' "$@" > args.txt
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}]}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"fake-session"}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"PRIVATE_PROMPT_ECHO_MUST_NOT_ESCAPE"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"PRIVATE_THOUGHT_MUST_NOT_ESCAPE"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"FAKE_ACP_OK"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
while IFS= read -r line; do :; done
"#;

        const PROGRESS_THEN_TOOL_BODY: &str = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}]}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"fake-session"}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"PROGRESS_MUST_NOT_ESCAPE"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"tool_call","title":"lookup"}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"FAKE_PROGRESS_BOUNDARY_OK"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
while IFS= read -r line; do :; done
"#;

        const PERMISSION_REQUEST_BODY: &str = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}]}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"fake-session"}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":91,"method":"session/request_permission","params":{"sessionId":"fake-session","options":[{"optionId":"allow","kind":"allow_once"},{"optionId":"deny","kind":"reject_once"}]}}'
IFS= read -r line
printf '%s\n' "$line" > permission-response.json
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"FAKE_PERMISSION_OK"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
while IFS= read -r line; do :; done
"#;

        fn fake_grok(temp: &TempDir, body: &str) -> PathBuf {
            let path = temp.path().join("fake-grok");
            std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}")).expect("write fake Grok");
            let mut permissions = std::fs::metadata(&path)
                .expect("fake Grok metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).expect("chmod fake Grok");
            path
        }

        fn fake_provider(temp: &TempDir, body: &str, timeout_secs: u64) -> GrokCliModelProvider {
            fake_provider_with_extra_args(temp, body, timeout_secs, vec![])
        }

        fn fake_provider_with_extra_args(
            temp: &TempDir,
            body: &str,
            timeout_secs: u64,
            extra_args: Vec<String>,
        ) -> GrokCliModelProvider {
            let binary = fake_grok(temp, body);
            provider(
                Some(binary.to_str().expect("UTF-8 fake path")),
                temp.path(),
                extra_args,
                Some(timeout_secs),
            )
        }

        fn fake_provider_with_stdout_limit(
            temp: &TempDir,
            body: &str,
            timeout_secs: u64,
            max_acp_stdout_bytes: usize,
        ) -> GrokCliModelProvider {
            let binary = fake_grok(temp, body);
            GrokCliModelProvider::builder("test")
                .binary_path(Some(binary.to_str().expect("UTF-8 fake path")))
                .working_directory(temp.path().to_str().expect("UTF-8 test path"))
                .max_acp_stdout_bytes(Some(max_acp_stdout_bytes))
                .timeout_secs(Some(timeout_secs))
                .build()
                .expect("provider")
        }

        fn fake_provider_with_vision(
            temp: &TempDir,
            body: &str,
            timeout_secs: u64,
        ) -> GrokCliModelProvider {
            let binary = fake_grok(temp, body);
            GrokCliModelProvider::builder("test")
                .binary_path(Some(binary.to_str().expect("UTF-8 fake path")))
                .working_directory(temp.path().to_str().expect("UTF-8 test path"))
                .timeout_secs(Some(timeout_secs))
                .vision_enabled(true)
                .build()
                .expect("vision-enabled provider")
        }

        async fn wait_for_pid(path: &Path) -> i32 {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(value) = tokio::fs::read_to_string(path).await
                    && let Ok(pid) = value.trim().parse::<i32>()
                {
                    return pid;
                }
                assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
                sleep(Duration::from_millis(20)).await;
            }
        }

        fn process_exists(pid: i32) -> bool {
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
        }

        async fn assert_process_exits(pid: i32) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while process_exists(pid) && Instant::now() < deadline {
                sleep(Duration::from_millis(20)).await;
            }
            assert!(!process_exists(pid), "process {pid} survived cleanup");
        }

        #[tokio::test]
        async fn fake_child_uses_acp_for_small_and_large_prompts_without_argv_exposure() {
            for prompt in [
                "PRIVATE_SMALL_PROMPT".to_string(),
                format!("PRIVATE_LARGE_PROMPT{}", "x".repeat(131_072)),
            ] {
                let temp = TempDir::new().expect("tempdir");
                let model_provider = fake_provider(&temp, SUCCESS_BODY, 5);
                let reply = model_provider
                    .invoke_acp(&prompt, "default")
                    .await
                    .expect("fake ACP reply");
                assert_eq!(reply, "FAKE_ACP_OK");

                let args =
                    std::fs::read_to_string(temp.path().join("args.txt")).expect("captured argv");
                assert!(!args.contains(&prompt));
                assert!(!args.contains("--single"));
                assert!(!args.contains("--prompt-file"));
                assert!(args.contains("agent\nstdio"));
                assert!(args.contains("--no-plan\n"));
                assert!(args.contains("--permission-mode\ndontAsk"));
                assert!(args.contains("--tools\n\n"));

                let requests = std::fs::read_to_string(temp.path().join("requests.ndjson"))
                    .expect("captured requests");
                assert!(requests.contains(&prompt));
            }
        }

        #[tokio::test]
        async fn fake_child_sends_images_as_acp_image_blocks_when_vision_is_enabled() {
            let temp = TempDir::new().expect("tempdir");
            let model_provider = fake_provider_with_vision(&temp, SUCCESS_BODY, 5);
            let reply = model_provider
                .invoke_acp("Describe [IMAGE:data:image/png;base64,cG5n]", "default")
                .await
                .expect("fake ACP reply");
            assert_eq!(reply, "FAKE_ACP_OK");

            let requests = std::fs::read_to_string(temp.path().join("requests.ndjson"))
                .expect("captured requests");
            let prompt_request: serde_json::Value = serde_json::from_str(
                requests
                    .lines()
                    .nth(3)
                    .expect("session/prompt request must be captured"),
            )
            .expect("session/prompt request must be JSON");
            assert_eq!(prompt_request["method"], "session/prompt");

            let blocks = prompt_request["params"]["prompt"]
                .as_array()
                .expect("session prompt must contain blocks");
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0]["type"], "text");
            assert_eq!(blocks[0]["text"], "Describe");
            assert_eq!(blocks[1]["type"], "image");
            assert_eq!(blocks[1]["data"], "cG5n");
            assert_eq!(blocks[1]["mimeType"], "image/png");
        }

        #[tokio::test]
        async fn fake_child_discards_progress_before_tool_result() {
            let temp = TempDir::new().expect("tempdir");
            let model_provider = fake_provider(&temp, PROGRESS_THEN_TOOL_BODY, 5);
            let reply = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect("fake ACP reply");
            assert_eq!(reply, "FAKE_PROGRESS_BOUNDARY_OK");
            assert!(!reply.contains("PROGRESS_MUST_NOT_ESCAPE"));
        }

        #[tokio::test]
        async fn fake_child_permission_response_follows_explicit_bypass_policy() {
            for (extra_args, expected_option_id) in [
                (vec![], "deny"),
                (
                    vec!["--permission-mode=bypassPermissions".to_string()],
                    "allow",
                ),
            ] {
                let temp = TempDir::new().expect("tempdir");
                let model_provider =
                    fake_provider_with_extra_args(&temp, PERMISSION_REQUEST_BODY, 5, extra_args);
                let reply = model_provider
                    .invoke_acp("hello", "default")
                    .await
                    .expect("fake ACP reply");
                assert_eq!(reply, "FAKE_PERMISSION_OK");

                let encoded = std::fs::read_to_string(temp.path().join("permission-response.json"))
                    .expect("captured permission response");
                let response: serde_json::Value =
                    serde_json::from_str(encoded.trim()).expect("valid permission response");
                assert_eq!(response["id"], 91);
                assert_eq!(
                    response.pointer("/result/outcome/outcome"),
                    Some(&serde_json::Value::String("selected".to_string()))
                );
                assert_eq!(
                    response.pointer("/result/outcome/optionId"),
                    Some(&serde_json::Value::String(expected_option_id.to_string()))
                );
            }
        }

        #[tokio::test]
        async fn fake_child_drains_large_stderr_without_exposing_it() {
            let temp = TempDir::new().expect("tempdir");
            let secret = "RAW_STDERR_SECRET_MUST_NOT_ESCAPE";
            let body = format!(
                "i=0\nwhile [ $i -lt 10000 ]; do printf '%s' '{secret}' >&2; i=$((i + 1)); done\n{SUCCESS_BODY}"
            );
            let model_provider = fake_provider(&temp, &body, 5);
            let reply = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect("stderr must be drained concurrently");
            assert_eq!(reply, "FAKE_ACP_OK");
            assert!(!reply.contains(secret));
        }

        #[tokio::test]
        async fn fake_child_failure_never_returns_raw_stderr() {
            let temp = TempDir::new().expect("tempdir");
            let secret = "RAW_STDERR_SECRET_MUST_NOT_ESCAPE";
            let body =
                format!("printf '%s\\n' '{secret}' >&2\nprintf '%s\\n' 'not-json'\nsleep 30");
            let model_provider = fake_provider(&temp, &body, 5);
            let error = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect_err("invalid ACP response must fail");
            assert!(!error.to_string().contains(secret));
        }

        #[tokio::test]
        async fn fake_child_stdout_frame_overflow_fails_closed() {
            let temp = TempDir::new().expect("tempdir");
            let body = r#"
IFS= read -r line
printf '%s' '{"jsonrpc":"2.0","id":1,"result":{"padding":"'
head -c 1100000 /dev/zero | tr '\000' x
printf '%s\n' '"}}'
sleep 30
"#;
            let model_provider = fake_provider(&temp, body, 5);
            let error = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect_err("oversized frame must fail");
            assert!(error.to_string().contains("stdout frame exceeded"));
        }

        #[tokio::test]
        async fn fake_child_honors_configured_stdout_limit() {
            let temp = TempDir::new().expect("tempdir");
            let body = r#"
IFS= read -r line
printf '%s' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}],"padding":"'
head -c 600000 /dev/zero | tr '\000' x
printf '%s\n' '"}}'
IFS= read -r line
printf '%s' '{"jsonrpc":"2.0","id":2,"result":{"padding":"'
head -c 600000 /dev/zero | tr '\000' x
printf '%s\n' '"}}'
sleep 30
"#;
            let model_provider =
                fake_provider_with_stdout_limit(&temp, body, 5, acp::MIN_ACP_STDOUT_LIMIT_BYTES);
            let error = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect_err("configured aggregate stdout limit must fail");
            assert!(error.to_string().contains(&format!(
                "stdout exceeded {} bytes",
                acp::MIN_ACP_STDOUT_LIMIT_BYTES
            )));
        }

        #[tokio::test]
        async fn fake_child_prompt_write_stall_obeys_timeout() {
            let temp = TempDir::new().expect("tempdir");
            let body = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"cached_token"}]}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"fake-session"}}'
sleep 30
"#;
            let model_provider = fake_provider(&temp, body, 1);
            let prompt = "x".repeat(4 * 1024 * 1024);
            let started = Instant::now();
            let error = model_provider
                .invoke_acp(&prompt, "default")
                .await
                .expect_err("blocked prompt write must time out");
            assert!(error.to_string().contains("timed out after 1s"));
            assert!(started.elapsed() < Duration::from_secs(4));
        }

        #[tokio::test]
        async fn cancellation_kills_leader_and_descendant() {
            let temp = TempDir::new().expect("tempdir");
            let body = r#"
printf '%s\n' "$$" > leader.pid
sleep 30 &
printf '%s\n' "$!" > descendant.pid
sleep 30
"#;
            let model_provider = fake_provider(&temp, body, 60);
            let task = ::zeroclaw_spawn::spawn!(async move {
                model_provider.invoke_acp("hello", "default").await
            });
            let leader = wait_for_pid(&temp.path().join("leader.pid")).await;
            let descendant = wait_for_pid(&temp.path().join("descendant.pid")).await;

            task.abort();
            let _ = task.await;
            assert_process_exits(leader).await;
            assert_process_exits(descendant).await;
        }

        #[tokio::test]
        async fn successful_request_kills_descendant() {
            let temp = TempDir::new().expect("tempdir");
            let body =
                format!("sleep 30 &\nprintf '%s\\n' \"$!\" > descendant.pid\n{SUCCESS_BODY}");
            let model_provider = fake_provider(&temp, &body, 5);
            let reply = model_provider
                .invoke_acp("hello", "default")
                .await
                .expect("fake ACP reply");
            assert_eq!(reply, "FAKE_ACP_OK");

            let descendant = wait_for_pid(&temp.path().join("descendant.pid")).await;
            assert_process_exits(descendant).await;
        }

        #[tokio::test]
        async fn timeout_kills_leader_and_descendant() {
            let temp = TempDir::new().expect("tempdir");
            let body = r#"
printf '%s\n' "$$" > leader.pid
sleep 30 &
printf '%s\n' "$!" > descendant.pid
sleep 30
"#;
            let model_provider = fake_provider(&temp, body, 1);
            let leader_path = temp.path().join("leader.pid");
            let descendant_path = temp.path().join("descendant.pid");
            let task = ::zeroclaw_spawn::spawn!(async move {
                model_provider.invoke_acp("hello", "default").await
            });
            let leader = wait_for_pid(&leader_path).await;
            let descendant = wait_for_pid(&descendant_path).await;
            let result = task.await.expect("provider task");
            assert!(
                result
                    .expect_err("timeout expected")
                    .to_string()
                    .contains("timed out")
            );
            assert_process_exits(leader).await;
            assert_process_exits(descendant).await;
        }
    }
}
