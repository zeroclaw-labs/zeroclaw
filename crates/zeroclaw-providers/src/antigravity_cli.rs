//! Antigravity CLI (`agy acp`) subprocess model provider.
//!
//! Every inference request spawns `agy acp`, the Agent Client Protocol stdio
//! server shipped with Google's Antigravity CLI, and runs one bounded
//! one-shot ACP exchange: initialize → authenticate → session/new →
//! session/set_model → session/prompt. The prompt travels as JSON-RPC on
//! child stdin; it is never placed in argv or a temporary file.
//!
//! # Usage
//!
//! ```toml
//! [providers.models.antigravity_cli.default]
//! model = "gemini-3.8-flash-high"
//! working_directory = "/path/to/agents/default/workspace"
//! api_key = "..."                      # set with `zeroclaw config set`
//! # auth_method = "gemini_api_key"     # default
//! # binary_path = "/home/you/.local/bin/agy"
//!
//! [agents.default]
//! model_provider = "antigravity_cli.default"
//! ```
//!
//! # Authentication policy
//!
//! ZeroClaw is an outer harness: it injects its own system prompt and owns
//! the conversation. Google's current Antigravity terms (FAQ, Terms §6, and
//! staff guidance on the Google AI forum) permit only API-key, Agent
//! Platform, and Gemini Enterprise sign-in for such harnesses. Accordingly:
//!
//! - `gemini_api_key` (default) sends this alias's typed `api_key` secret in
//!   the ACP `authenticate` `_meta.apiKey` field. The key never appears in
//!   argv or the child environment, and `GEMINI_*` passthrough is rejected.
//! - `agent_platform` authenticates with Application Default Credentials.
//! - `oauth_business` is an explicit opt-in that reuses a Gemini Enterprise
//!   sign-in cached in this alias's isolated state directory. The session is
//!   refused unless Antigravity reports a business tier.
//! - `oauth_personal` is refused when the provider is built.
//!
//! Every alias runs with its own `--gemini-dir`, so a personal sign-in cached
//! in `~/.gemini` can never be picked up, including by Antigravity's
//! fallback to a saved login when an explicit method has no credential.
//!
//! # Host-side trust boundary
//!
//! The child is a tool-capable agent outside ZeroClaw's native tool approval
//! path. The shared `acp_cli` plumbing therefore requires an explicit
//! absolute working directory, clears the inherited environment, bounds every
//! ACP frame and stream, rejects permission requests unless the alias passes
//! `--dangerously-skip-permissions`, and terminates the child process group
//! (Unix) or Job Object (Windows) on every exit path. Public errors never
//! include child stderr or raw ACP frames.
//!
//! # Limitations
//!
//! - Only the system prompt (if present) and final user message are
//!   forwarded. The system prompt uses Antigravity's
//!   `_meta.antigravity.systemPrompt.append` session extension.
//! - The provider is one-shot; ACP sessions are not resumed.
//! - Native ZeroClaw tool calls are not emitted; `ChatRequest.tools` is ignored.
//! - Token usage is not reported (`ChatResponse.usage` is `None`).
//! - Only temperatures `0.7` and `1.0` are accepted because `agy acp` has no
//!   sampling control.
//! - `timeout_secs` is clamped to one hour maximum.

use crate::acp_cli;
use crate::acp_cli::env::{
    base_env_allowed, env_name_starts_with, env_names_equal, is_valid_env_var_name,
};
use crate::acp_cli::process::{AcpSubprocessError, log_acp_failure, run_oneshot_subprocess};
use crate::acp_cli::protocol::{self as acp, AcpCliFlavor, AcpError, LabeledAcpError};
use crate::traits::{ChatRequest, ChatResponse, ModelProvider, ProviderCapabilities};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
pub use zeroclaw_config::schema::AntigravityCliAuthMethod;

/// Public error label and stable log-key prefix for this provider.
const FLAVOR: AcpCliFlavor = AcpCliFlavor {
    label: "Antigravity",
    key_prefix: "antigravity_cli",
};

/// Provider family name used in configuration errors and state paths.
const FAMILY: &str = "antigravity_cli";

/// Default Antigravity CLI binary name (resolved via `PATH`).
const DEFAULT_ANTIGRAVITY_CLI_BINARY: &str = "agy";

/// Model name used to signal "use the agent's default model".
const DEFAULT_MODEL_MARKER: &str = "default";

/// Default wall-clock budget when `timeout_secs` is unset on the alias.
const DEFAULT_ANTIGRAVITY_CLI_TIMEOUT: Duration = Duration::from_secs(600);

/// Upper bound for alias `timeout_secs`. Values above this are clamped.
const MAX_ANTIGRAVITY_CLI_TIMEOUT_SECS: u64 = 3_600;

const ANTIGRAVITY_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// `agy acp` flags that take no value.
const VALUELESS_EXTRA_ARG_FLAGS: &[&str] = &[
    "--sandbox",
    "--dangerously-skip-permissions",
    "--process-per-session",
];

/// Boolean `agy acp` flags accepted only in `--flag=value` form.
const INLINE_VALUE_EXTRA_ARG_FLAGS: &[&str] = &["--include-default-customizations"];

/// `agy acp` flags that consume exactly one following value.
const VALUE_TAKING_EXTRA_ARG_FLAGS: &[&str] = &["--add-dir", "--agent"];

/// Antigravity tier ids reported for Gemini Enterprise / Google Cloud
/// business sign-in (observed in Antigravity CLI 1.5.14). Consumer sign-in
/// reports tiers such as `free-tier` and is refused for `oauth_business`.
const BUSINESS_TIER_PREFIXES: &[&str] = &["gcp-", "agy-business-"];

/// Environment variable prefixes owned by the provider. API keys must use the
/// typed `api_key`; Antigravity state and auth selection are provider-owned.
const PROVIDER_OWNED_ENV_PREFIXES: &[&str] = &["GEMINI_", "AGY_", "ANTIGRAVITY_", "GOOGLE_GENAI_"];

/// Provider-owned exact environment variable names.
const PROVIDER_OWNED_ENV_NAMES: &[&str] = &["GOOGLE_API_KEY"];

/// The ACP auth method id `agy acp` advertises for each configured method.
fn acp_method_id(method: AntigravityCliAuthMethod) -> &'static str {
    match method {
        AntigravityCliAuthMethod::GeminiApiKey => "gemini-api-key",
        AntigravityCliAuthMethod::AgentPlatform => "agent-platform",
        AntigravityCliAuthMethod::OauthBusiness => "oauth-business",
        AntigravityCliAuthMethod::OauthPersonal => "oauth-personal",
    }
}

/// Model provider that invokes Antigravity CLI through ACP.
///
/// Deliberately not `Debug`: the handle carries the alias API key.
pub struct AntigravityCliModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `agy` binary.
    binary_path: PathBuf,
    /// Canonical subprocess cwd and ACP session boundary.
    working_directory: PathBuf,
    /// Absolute per-alias Antigravity state directory (`--gemini-dir`).
    state_directory: PathBuf,
    /// Authentication method sent to the child.
    auth_method: AntigravityCliAuthMethod,
    /// Gemini API key for [`AntigravityCliAuthMethod::GeminiApiKey`].
    api_key: Option<String>,
    /// Operator-selected environment variable names inherited at spawn time.
    env_passthrough: Vec<String>,
    /// Operator-supplied `agy acp` flags, after validation.
    extra_args: Vec<String>,
    /// Aggregate stdout budget for one ACP request.
    max_acp_stdout_bytes: usize,
    /// Wall-clock timeout for the ACP request.
    timeout: Duration,
    /// When true, serialize image markers as ACP `image` blocks and report
    /// vision capability. `agy acp` advertises `promptCapabilities.image`.
    vision_enabled: bool,
}

/// Typed builder for [`AntigravityCliModelProvider`].
///
/// `alias` and an existing absolute `working_directory` are required, plus
/// either an explicit `state_directory` or a `state_root` under which the
/// per-alias default is derived.
#[must_use]
pub struct AntigravityCliBuilder {
    alias: String,
    binary_path: Option<String>,
    working_directory: Option<String>,
    state_directory: Option<String>,
    state_root: Option<PathBuf>,
    auth_method: AntigravityCliAuthMethod,
    api_key: Option<String>,
    env_passthrough: Vec<String>,
    extra_args: Vec<String>,
    max_acp_stdout_bytes: Option<usize>,
    timeout_secs: Option<u64>,
    vision_enabled: bool,
}

impl AntigravityCliBuilder {
    /// Override the `agy` binary path. Whitespace-only inputs use PATH lookup.
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

    /// Explicit absolute Antigravity state directory for this alias.
    pub fn state_directory(mut self, path: Option<&str>) -> Self {
        self.state_directory = path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string);
        self
    }

    /// Root under which the default `antigravity_cli/<alias>` state
    /// directory is derived when no explicit state directory is set.
    pub fn state_root(mut self, root: PathBuf) -> Self {
        self.state_root = Some(root);
        self
    }

    /// Select the child authentication method.
    pub fn auth_method(mut self, method: AntigravityCliAuthMethod) -> Self {
        self.auth_method = method;
        self
    }

    /// Alias `api_key` secret. Whitespace-only inputs count as unset.
    pub fn api_key(mut self, key: Option<&str>) -> Self {
        self.api_key = key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        self
    }

    /// Add operator-selected environment variables to the child allowlist.
    pub fn env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// Set operator-selected `agy acp` flags.
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
    pub fn vision_enabled(mut self, vision_enabled: bool) -> Self {
        self.vision_enabled = vision_enabled;
        self
    }

    pub fn build(self) -> anyhow::Result<AntigravityCliModelProvider> {
        let auth_method = self.auth_method;
        let api_key = AntigravityCliModelProvider::validate_auth(auth_method, self.api_key)?;
        let working_directory = acp_cli::validate_working_directory(
            FAMILY,
            self.working_directory.as_deref().unwrap_or_default(),
        )?;
        let state_directory = AntigravityCliModelProvider::resolve_state_directory(
            &self.alias,
            self.state_directory.as_deref(),
            self.state_root.as_deref(),
        )?;
        let env_passthrough = AntigravityCliModelProvider::normalize_and_validate_env_passthrough(
            self.env_passthrough,
        )?;
        let extra_args =
            AntigravityCliModelProvider::normalize_and_validate_extra_args(self.extra_args)?;
        let max_acp_stdout_bytes =
            acp_cli::validate_acp_stdout_limit(FAMILY, self.max_acp_stdout_bytes)?;
        let timeout = self
            .timeout_secs
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Duration::from_secs(seconds.min(MAX_ANTIGRAVITY_CLI_TIMEOUT_SECS)))
            .unwrap_or(DEFAULT_ANTIGRAVITY_CLI_TIMEOUT);
        let path = std::env::var_os("PATH");
        let path_ext = std::env::var_os("PATHEXT");
        let binary_path = acp_cli::resolve_binary_path(
            FAMILY,
            DEFAULT_ANTIGRAVITY_CLI_BINARY,
            self.binary_path.as_deref(),
            path.as_deref(),
            path_ext.as_deref(),
        )?;
        Ok(AntigravityCliModelProvider {
            alias: self.alias,
            binary_path,
            working_directory,
            state_directory,
            auth_method,
            api_key,
            env_passthrough,
            extra_args,
            max_acp_stdout_bytes,
            timeout,
            vision_enabled: self.vision_enabled,
        })
    }
}

impl AntigravityCliModelProvider {
    /// Start a labelled construction chain for an ACP-backed provider.
    pub fn builder(alias: &str) -> AntigravityCliBuilder {
        AntigravityCliBuilder {
            alias: alias.to_string(),
            binary_path: None,
            working_directory: None,
            state_directory: None,
            state_root: None,
            auth_method: AntigravityCliAuthMethod::default(),
            api_key: None,
            env_passthrough: Vec::new(),
            extra_args: Vec::new(),
            max_acp_stdout_bytes: None,
            timeout_secs: None,
            vision_enabled: false,
        }
    }

    fn validate_auth(
        method: AntigravityCliAuthMethod,
        api_key: Option<String>,
    ) -> anyhow::Result<Option<String>> {
        match method {
            AntigravityCliAuthMethod::OauthPersonal => anyhow::bail!(
                "antigravity_cli auth_method `oauth_personal` is refused: Google's Antigravity \
                 terms do not permit personal Google sign-in inside third-party harnesses such \
                 as ZeroClaw, and doing so risks account suspension. Use `gemini_api_key` with \
                 this alias's api_key, `agent_platform` (Application Default Credentials), or \
                 `oauth_business` (Gemini Enterprise)."
            ),
            AntigravityCliAuthMethod::GeminiApiKey => {
                if api_key.is_none() {
                    anyhow::bail!(
                        "antigravity_cli auth_method `gemini_api_key` requires this alias's \
                         api_key; set it with `zeroclaw config set \
                         providers.models.antigravity_cli.<alias>.api_key`"
                    );
                }
                Ok(api_key)
            }
            AntigravityCliAuthMethod::AgentPlatform | AntigravityCliAuthMethod::OauthBusiness => {
                if api_key.is_some() {
                    anyhow::bail!(
                        "antigravity_cli api_key is used only with auth_method `gemini_api_key`; \
                         remove it or change auth_method"
                    );
                }
                Ok(None)
            }
        }
    }

    fn resolve_state_directory(
        alias: &str,
        configured: Option<&str>,
        state_root: Option<&Path>,
    ) -> anyhow::Result<PathBuf> {
        if let Some(configured) = configured {
            let path = Path::new(configured);
            if !path.is_absolute() {
                anyhow::bail!("antigravity_cli state_directory must be an absolute path");
            }
            return Ok(path.to_path_buf());
        }
        let valid_alias = !alias.is_empty()
            && alias != "."
            && alias != ".."
            && alias
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if !valid_alias {
            anyhow::bail!(
                "antigravity_cli alias cannot name a default state directory; set state_directory"
            );
        }
        // The child runs in `working_directory`, so a relative root must be
        // anchored to this process's directory, where ZeroClaw resolved its
        // own config, before the path is handed to the child.
        let Some(root) = state_root.and_then(|root| std::path::absolute(root).ok()) else {
            anyhow::bail!(
                "antigravity_cli needs an absolute ZeroClaw state directory or an explicit \
                 state_directory"
            );
        };
        Ok(root.join(FAMILY).join(alias))
    }

    fn normalize_and_validate_env_passthrough(names: Vec<String>) -> anyhow::Result<Vec<String>> {
        let mut normalized: Vec<String> = Vec::with_capacity(names.len());
        for name in names {
            let name = name.trim();
            if !is_valid_env_var_name(name) {
                anyhow::bail!(
                    "antigravity_cli env_passthrough entry `{name}` is invalid; expected [A-Za-z_][A-Za-z0-9_]*"
                );
            }
            if is_provider_owned_env_var(name) {
                anyhow::bail!(
                    "antigravity_cli env_passthrough entry `{name}` is provider-owned \
                     (`GEMINI_*`, `GOOGLE_API_KEY`, `GOOGLE_GENAI_*`, `AGY_*`, `ANTIGRAVITY_*`); \
                     use the alias api_key for Gemini API keys"
                );
            }
            if !normalized
                .iter()
                .any(|existing| env_names_equal(existing.as_str(), name))
            {
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
                    "antigravity_cli extra_args accepts long flags only and must not include positional arguments"
                );
            }
            let (flag, inline_value) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)));
            if VALUELESS_EXTRA_ARG_FLAGS.contains(&flag) {
                if inline_value.is_some() {
                    anyhow::bail!("antigravity_cli extra_args option `{flag}` takes no value");
                }
                index += 1;
                continue;
            }
            if INLINE_VALUE_EXTRA_ARG_FLAGS.contains(&flag) {
                if !matches!(inline_value, Some("true" | "false")) {
                    anyhow::bail!(
                        "antigravity_cli extra_args option `{flag}` must be written `{flag}=true` or `{flag}=false`"
                    );
                }
                index += 1;
                continue;
            }
            if VALUE_TAKING_EXTRA_ARG_FLAGS.contains(&flag) {
                if inline_value.is_some_and(|value| !value.is_empty()) {
                    index += 1;
                    continue;
                }
                let Some(value) = extra_args.get(index + 1) else {
                    anyhow::bail!(
                        "antigravity_cli extra_args option `{flag}` is missing its value"
                    );
                };
                if value.starts_with('-') {
                    anyhow::bail!(
                        "antigravity_cli extra_args option `{flag}` must use `--flag=value` when its value starts with `-`"
                    );
                }
                index += 2;
                continue;
            }
            anyhow::bail!(
                "antigravity_cli extra_args option `{flag}` is not supported. Transport, state \
                 directory, login, telemetry, logging, and endpoint flags are owned by ZeroClaw; \
                 supported flags are --sandbox, --dangerously-skip-permissions, \
                 --process-per-session, --include-default-customizations=<bool>, --add-dir, \
                 and --agent."
            );
        }
        Ok(extra_args)
    }

    fn acp_permission_policy(extra_args: &[String]) -> acp::AcpPermissionPolicy {
        if extra_args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions")
        {
            acp::AcpPermissionPolicy::AllowOnce
        } else {
            acp::AcpPermissionPolicy::RejectOnce
        }
    }

    fn forwarded_model(model: &str) -> Option<&str> {
        let trimmed = model.trim();
        (!trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER).then_some(trimmed)
    }

    fn supports_temperature(temperature: f64) -> bool {
        ANTIGRAVITY_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|value| (temperature - value).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Antigravity CLI model provider received a non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Antigravity CLI model provider: {temperature}. \
                 Supported values: 0.7 or 1.0 (not forwarded; agy acp has no sampling control)"
            );
        }
        Ok(())
    }

    /// `agy acp` argv: telemetry off and the per-alias state directory, then
    /// validated operator flags.
    fn build_cli_args(state_directory: &Path, extra_args: &[String]) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = Vec::with_capacity(4 + extra_args.len());
        args.push("acp".into());
        args.push("--disable-telemetry".into());
        args.push("--gemini-dir".into());
        args.push(state_directory.as_os_str().to_owned());
        args.extend(extra_args.iter().map(Into::into));
        args
    }

    fn apply_env_allowlist(cmd: &mut Command, env_passthrough: &[String]) {
        Self::apply_env_allowlist_from(cmd, env_passthrough, acp_cli::env::unicode_process_env());
    }

    fn apply_env_allowlist_from(
        cmd: &mut Command,
        env_passthrough: &[String],
        variables: impl IntoIterator<Item = (String, String)>,
    ) {
        cmd.env_clear();
        for (key, value) in variables {
            if env_var_allowed(&key, env_passthrough) {
                cmd.env(key, value);
            }
        }
    }

    /// `_meta` for `session/new`: the system prompt rides Antigravity's
    /// documented append extension.
    fn session_meta(system: Option<&str>) -> Option<Value> {
        system
            .filter(|system| !system.trim().is_empty())
            .map(|system| json!({ "antigravity": { "systemPrompt": { "append": system } } }))
    }

    async fn ensure_state_directory(&self) -> anyhow::Result<()> {
        #[cfg(unix)]
        let owner = Some(nix::unistd::geteuid().as_raw());
        #[cfg(not(unix))]
        let owner: Option<u32> = None;
        prepare_state_directory(&self.state_directory, owner)
            .await
            .map_err(|(refusal, detail)| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "state_directory",
                            "error_key": refusal.error_key(),
                            "error": detail,
                        })),
                    "antigravity_cli: state directory refused"
                );
                anyhow::Error::msg(refusal.message())
            })
    }

    async fn invoke_acp(
        &self,
        system: Option<&str>,
        message: &str,
        model: &str,
    ) -> anyhow::Result<String> {
        self.ensure_state_directory().await?;
        let prompt = self.acp_prompt_content(message);
        let session_meta = Self::session_meta(system);
        let wants_system_prompt = session_meta.is_some();
        let args = Self::build_cli_args(&self.state_directory, &self.extra_args);
        let mut cmd = Command::new(&self.binary_path);
        cmd.args(&args);
        cmd.current_dir(&self.working_directory);
        Self::apply_env_allowlist(&mut cmd, &self.env_passthrough);

        let auth_method = self.auth_method;
        let session_check = move |session: &Value| check_session_tier(auth_method, session);
        let request = acp::OneshotPrompt {
            prompt: &prompt,
            cwd: &self.working_directory,
            permission_policy: Self::acp_permission_policy(&self.extra_args),
            max_stdout_bytes: self.max_acp_stdout_bytes,
            session_meta: session_meta.as_ref(),
            model: Self::forwarded_model(model),
            session_check: Some(&session_check),
            failure_marker: Some(AGENT_EXECUTOR_ERROR_PREFIX),
        };
        let api_key = self.api_key.as_deref();
        let outcome = run_oneshot_subprocess(cmd, self.timeout, &request, |initialize| {
            if wants_system_prompt && !supports_system_prompt_append(initialize) {
                return Err(AcpError::SessionPolicy {
                    reason: "agy acp does not advertise system prompt append; upgrade Antigravity CLI",
                });
            }
            authenticate_params(auth_method, api_key, initialize)
        })
        .await;

        match outcome.result {
            Ok(text) => match classify_agent_error(&text) {
                None => Ok(text),
                Some(failure) => {
                    log_acp_failure(FLAVOR, "agent", failure.error_key, outcome.stderr);
                    Err(failure.into_error())
                }
            },
            Err(AcpSubprocessError::Spawn(err)) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "phase": "spawn",
                            "error_key": "antigravity_cli_spawn_failed",
                            "error": format!("{err}"),
                            "os_error": err.raw_os_error(),
                        })),
                    "antigravity_cli: failed to spawn ACP binary"
                );
                Err(anyhow::Error::msg(format!(
                    "Failed to spawn Antigravity CLI ACP binary at {}: {err}. \
                     Ensure `agy` is installed and on PATH, or set binary_path.",
                    self.binary_path.display()
                )))
            }
            Err(AcpSubprocessError::Pipe(stream)) => Err(anyhow::Error::msg(format!(
                "Failed to open Antigravity ACP {stream}"
            ))),
            Err(AcpSubprocessError::Reap) => {
                log_acp_failure(
                    FLAVOR,
                    "reap",
                    "antigravity_cli_reap_failed",
                    outcome.stderr,
                );
                Err(anyhow::Error::msg(
                    "Antigravity CLI ACP process could not be reaped after completion",
                ))
            }
            Err(AcpSubprocessError::Protocol(error)) => {
                let error = LabeledAcpError {
                    flavor: FLAVOR,
                    error,
                };
                log_acp_failure(FLAVOR, "protocol", &error.error_key(), outcome.stderr);
                Err(anyhow::Error::new(error))
            }
            Err(AcpSubprocessError::Timeout) => {
                log_acp_failure(FLAVOR, "timeout", "antigravity_cli_timeout", outcome.stderr);
                Err(anyhow::Error::msg(format!(
                    "Antigravity CLI ACP request timed out after {}s",
                    self.timeout.as_secs()
                )))
            }
        }
    }

    fn acp_prompt_content(&self, message: &str) -> Vec<acp::AcpPromptContent> {
        if !self.vision_enabled {
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        }
        let (text, image_refs) = crate::multimodal::parse_user_message_image_refs(message);
        if image_refs.is_empty() {
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        }
        let Some(images) = image_refs
            .iter()
            .map(|image_ref| acp::AcpPromptContent::image_from_data_uri(image_ref))
            .collect::<Option<Vec<_>>>()
        else {
            // Preserve the original text rather than silently dropping an
            // image reference that cannot be sent as an ACP block.
            return vec![acp::AcpPromptContent::Text(message.to_string())];
        };
        let mut prompt = Vec::with_capacity(images.len() + 1);
        if !text.is_empty() {
            prompt.push(acp::AcpPromptContent::Text(text));
        }
        prompt.extend(images);
        prompt
    }
}

/// Complete `authenticate` params for the configured method, or `None` to
/// use the cached sign-in in the isolated state directory (business only).
/// Fails closed when `agy acp` does not advertise the configured method.
fn authenticate_params(
    method: AntigravityCliAuthMethod,
    api_key: Option<&str>,
    initialize: &Value,
) -> Result<Option<Value>, AcpError> {
    let method_id = acp_method_id(method);
    let advertised = initialize
        .get("authMethods")
        .or_else(|| initialize.get("auth_methods"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            entry
                .get("id")
                .or_else(|| entry.get("methodId"))
                .and_then(Value::as_str)
        })
        .any(|id| id == method_id);
    if !advertised {
        return Err(AcpError::NoAuthenticationMethod);
    }
    match method {
        AntigravityCliAuthMethod::GeminiApiKey => {
            let key = api_key.ok_or(AcpError::NoAuthenticationMethod)?;
            Ok(Some(
                json!({ "methodId": method_id, "_meta": { "apiKey": key } }),
            ))
        }
        AntigravityCliAuthMethod::AgentPlatform => Ok(Some(json!({ "methodId": method_id }))),
        // An explicit `authenticate` for oauth-business starts an interactive
        // browser sign-in. Reuse the sign-in cached in the isolated state
        // directory instead; `check_session_tier` verifies it is a business
        // tier before any prompt is sent.
        AntigravityCliAuthMethod::OauthBusiness => Ok(None),
        AntigravityCliAuthMethod::OauthPersonal => Err(AcpError::SessionPolicy {
            reason: "personal Google sign-in is not permitted for antigravity_cli",
        }),
    }
}

/// Why the alias state directory was not used. `agy` keeps its cached
/// sign-in and session files there, so another local account must not be
/// able to redirect the directory through a link or write into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateDirectoryRefusal {
    Unavailable,
    NotADirectory,
    NotOwned,
    NotPrivate,
}

impl StateDirectoryRefusal {
    fn error_key(self) -> &'static str {
        match self {
            Self::Unavailable => "antigravity_cli_state_directory_failed",
            Self::NotADirectory => "antigravity_cli_state_directory_not_directory",
            Self::NotOwned => "antigravity_cli_state_directory_not_owned",
            Self::NotPrivate => "antigravity_cli_state_directory_not_private",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Unavailable => "antigravity_cli could not create its state directory",
            Self::NotADirectory => {
                "antigravity_cli state directory must be a directory, not a link or file"
            }
            Self::NotOwned => {
                "antigravity_cli state directory must be owned by the user running ZeroClaw"
            }
            Self::NotPrivate => {
                "antigravity_cli state directory is accessible to other users and could not be \
                 made owner-only (chmod 700)"
            }
        }
    }
}

/// Create the state directory owner-only, or admit an existing one only when
/// it is a real directory (not a link) owned by `owner`, tightening it to
/// owner-only if needed. `owner` is `None` where ownership is not checked.
async fn prepare_state_directory(
    path: &Path,
    owner: Option<u32>,
) -> Result<(), (StateDirectoryRefusal, String)> {
    let unavailable =
        |error: std::io::Error| (StateDirectoryRefusal::Unavailable, error.to_string());
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = tokio::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(path).await.map_err(unavailable)?;
            tokio::fs::symlink_metadata(path)
                .await
                .map_err(unavailable)?
        }
        Err(error) => return Err(unavailable(error)),
    };
    // `symlink_metadata` does not follow a link, so a link reports as one here.
    if !metadata.file_type().is_dir() {
        return Err((
            StateDirectoryRefusal::NotADirectory,
            "state directory entry is a link or non-directory".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if let Some(owner) = owner
            && metadata.uid() != owner
        {
            return Err((
                StateDirectoryRefusal::NotOwned,
                format!("state directory uid {} != {owner}", metadata.uid()),
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(|error| (StateDirectoryRefusal::NotPrivate, error.to_string()))?;
        }
    }
    #[cfg(not(unix))]
    let _ = owner;
    Ok(())
}

/// Prefix `agy acp` (1.5.x) puts on a failed turn. The failure arrives as an
/// ordinary `agent_message_chunk` followed by `stopReason: end_turn`, so there
/// is no protocol-level signal to classify by. It can follow chunks of a
/// partial answer, so it is passed to the shared reader as the
/// `failure_marker`, which starts the reply at the chunk that carries it.
const AGENT_EXECUTOR_ERROR_PREFIX: &str = "\u{26a0} agent executor error:";

/// A failed Antigravity turn mapped to provider-owned public text.
#[derive(Debug, PartialEq, Eq)]
struct AgentFailure {
    error_key: &'static str,
    message: &'static str,
    /// Replaying the turn with the same alias cannot succeed (a rejected key
    /// or denied model), so retry policy should move to the next provider.
    non_retryable: bool,
}

impl AgentFailure {
    fn into_error(self) -> anyhow::Error {
        if self.non_retryable {
            anyhow::Error::new(crate::traits::NonRetryableProviderError::new(self.message))
        } else {
            anyhow::Error::msg(self.message)
        }
    }
}

/// Recognize a failed turn reported as assistant text and map it to a fixed
/// public message. The child's text is never echoed: only well-known status
/// tokens select the message, so callers and retry policy see a stable error
/// instead of an error report masquerading as a model answer.
fn classify_agent_error(reply: &str) -> Option<AgentFailure> {
    let report = reply.trim().strip_prefix(AGENT_EXECUTOR_ERROR_PREFIX)?;
    let failure = if report.contains("API_KEY_INVALID") {
        AgentFailure {
            error_key: "antigravity_cli_api_key_invalid",
            message: "Antigravity CLI agent rejected the Gemini API key (API_KEY_INVALID)",
            non_retryable: true,
        }
    } else if report.contains("RESOURCE_EXHAUSTED") || report.contains("Error 429") {
        AgentFailure {
            error_key: "antigravity_cli_rate_limited",
            message: "Antigravity CLI agent hit a quota or rate limit (429 RESOURCE_EXHAUSTED)",
            non_retryable: false,
        }
    } else if report.contains("PERMISSION_DENIED") || report.contains("Error 403") {
        AgentFailure {
            error_key: "antigravity_cli_permission_denied",
            message: "Antigravity CLI agent was denied access to the model (403 PERMISSION_DENIED)",
            non_retryable: true,
        }
    } else if report.contains("UNAVAILABLE") || report.contains("Error 503") {
        AgentFailure {
            error_key: "antigravity_cli_unavailable",
            message: "Antigravity CLI agent reported the model service unavailable (503 UNAVAILABLE)",
            non_retryable: false,
        }
    } else {
        AgentFailure {
            error_key: "antigravity_cli_agent_error",
            message: "Antigravity CLI agent reported a model execution error; see the agy ACP log in the alias state directory",
            non_retryable: false,
        }
    };
    Some(failure)
}

fn supports_system_prompt_append(initialize: &Value) -> bool {
    initialize
        .pointer("/agentCapabilities/_meta/antigravity/systemPrompt/append")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Refuse a cached `oauth_business` session unless Antigravity reports a
/// business tier. Other methods authenticate explicitly in an isolated state
/// directory and are not tier-checked.
fn check_session_tier(method: AntigravityCliAuthMethod, session: &Value) -> Result<(), AcpError> {
    if method != AntigravityCliAuthMethod::OauthBusiness {
        return Ok(());
    }
    let tier = session
        .pointer("/_meta/antigravity/tier/id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if BUSINESS_TIER_PREFIXES
        .iter()
        .any(|prefix| tier.starts_with(prefix))
    {
        Ok(())
    } else {
        Err(AcpError::SessionPolicy {
            reason: "the cached Antigravity sign-in is not a Gemini Enterprise business tier; \
                     sign in with a Gemini Enterprise account in this alias's state directory, \
                     or use gemini_api_key or agent_platform",
        })
    }
}

fn is_provider_owned_env_var(name: &str) -> bool {
    PROVIDER_OWNED_ENV_NAMES
        .iter()
        .any(|owned| env_names_equal(name, owned))
        || PROVIDER_OWNED_ENV_PREFIXES
            .iter()
            .any(|prefix| env_name_starts_with(name, prefix))
}

fn env_var_allowed(key: &str, env_passthrough: &[String]) -> bool {
    base_env_allowed(key)
        || env_passthrough
            .iter()
            .any(|name| env_names_equal(name, key))
}

#[async_trait]
impl ModelProvider for AntigravityCliModelProvider {
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
        self.invoke_acp(system_prompt, message, model).await
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

impl ::zeroclaw_api::attribution::Attributable for AntigravityCliModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::AntigravityCli,
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

    const TEST_KEY: &str = "TEST_GEMINI_KEY_MUST_NOT_LEAK";

    fn test_executable_path() -> PathBuf {
        std::fs::canonicalize(std::env::current_exe().expect("current test executable"))
            .expect("canonical test executable")
    }

    fn builder(temp: &TempDir) -> AntigravityCliBuilder {
        let binary = test_executable_path();
        AntigravityCliModelProvider::builder("test")
            .binary_path(Some(binary.to_str().expect("UTF-8 test executable")))
            .working_directory(temp.path().to_str().expect("UTF-8 test path"))
            .state_root(temp.path().join("zeroclaw"))
            .api_key(Some(TEST_KEY))
    }

    fn build_error(builder: AntigravityCliBuilder) -> String {
        match builder.build() {
            Ok(_) => panic!("build must fail"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn builder_defaults_to_api_key_auth_and_isolated_alias_state() {
        let temp = TempDir::new().expect("tempdir");
        let provider = builder(&temp).build().expect("provider");
        assert_eq!(provider.auth_method, AntigravityCliAuthMethod::GeminiApiKey);
        assert_eq!(provider.api_key.as_deref(), Some(TEST_KEY));
        assert_eq!(
            provider.state_directory,
            temp.path()
                .join("zeroclaw")
                .join("antigravity_cli")
                .join("test")
        );
        assert_eq!(
            provider.working_directory,
            std::fs::canonicalize(temp.path()).expect("canonical path")
        );
        assert_eq!(
            provider.max_acp_stdout_bytes,
            acp::DEFAULT_ACP_STDOUT_LIMIT_BYTES
        );
        assert_eq!(provider.timeout, DEFAULT_ANTIGRAVITY_CLI_TIMEOUT);
        assert!(!provider.capabilities().vision);
    }

    #[test]
    fn personal_sign_in_is_refused_at_build_time() {
        let temp = TempDir::new().expect("tempdir");
        let error = build_error(
            builder(&temp)
                .api_key(None)
                .auth_method(AntigravityCliAuthMethod::OauthPersonal),
        );
        assert!(error.contains("oauth_personal"), "{error}");
        assert!(error.contains("refused"), "{error}");
    }

    #[test]
    fn api_key_auth_requires_key_and_other_methods_reject_it() {
        let temp = TempDir::new().expect("tempdir");
        let missing = build_error(builder(&temp).api_key(Some("   ")));
        assert!(
            missing.contains("requires this alias's api_key"),
            "{missing}"
        );

        for method in [
            AntigravityCliAuthMethod::AgentPlatform,
            AntigravityCliAuthMethod::OauthBusiness,
        ] {
            let error = build_error(builder(&temp).auth_method(method));
            assert!(error.contains("only with auth_method"), "{error}");
            assert!(!error.contains(TEST_KEY));
            builder(&temp)
                .api_key(None)
                .auth_method(method)
                .build()
                .expect("keyless method builds");
        }
    }

    #[test]
    fn state_directory_must_be_absolute_and_alias_safe() {
        let temp = TempDir::new().expect("tempdir");
        let relative = build_error(builder(&temp).state_directory(Some("relative/state")));
        assert!(relative.contains("absolute path"), "{relative}");

        let explicit = temp.path().join("explicit");
        let provider = builder(&temp)
            .state_directory(Some(explicit.to_str().expect("UTF-8 path")))
            .build()
            .expect("explicit state directory");
        assert_eq!(provider.state_directory, explicit);

        let unsafe_alias = AntigravityCliModelProvider::resolve_state_directory(
            "../escape",
            None,
            Some(temp.path()),
        )
        .expect_err("path-like alias must not name a state directory");
        assert!(unsafe_alias.to_string().contains("set state_directory"));

        let no_root = AntigravityCliModelProvider::resolve_state_directory("test", None, None)
            .expect_err("missing root must fail");
        assert!(
            no_root
                .to_string()
                .contains("absolute ZeroClaw state directory")
        );
    }

    #[test]
    fn relative_state_root_resolves_against_the_current_directory() {
        let resolved = AntigravityCliModelProvider::resolve_state_directory(
            "work",
            None,
            Some(Path::new("config")),
        )
        .expect("a relative ZEROCLAW_CONFIG_DIR must still yield a state directory");
        let cwd = std::env::current_dir().expect("current directory");
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert_eq!(resolved, cwd.join("config").join(FAMILY).join("work"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn state_directory_must_be_a_private_owned_real_directory() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let temp = TempDir::new().expect("tempdir");
        let me = std::fs::metadata(temp.path())
            .expect("tempdir metadata")
            .uid();
        let mode = |path: &Path| {
            std::fs::symlink_metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };

        let fresh = temp.path().join("fresh").join("state");
        prepare_state_directory(&fresh, Some(me))
            .await
            .expect("missing directory is created");
        assert_eq!(mode(&fresh), 0o700);

        let open = temp.path().join("open");
        std::fs::create_dir(&open).expect("open dir");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).expect("chmod 777");
        prepare_state_directory(&open, Some(me))
            .await
            .expect("an owned, writable directory is tightened");
        assert_eq!(mode(&open), 0o700);

        let target = temp.path().join("target");
        std::fs::create_dir(&target).expect("link target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o777))
            .expect("chmod 777");
        let link = temp.path().join("link");
        symlink(&target, &link).expect("symlink");
        let (refusal, _) = prepare_state_directory(&link, Some(me))
            .await
            .expect_err("a link must be refused");
        assert_eq!(refusal, StateDirectoryRefusal::NotADirectory);
        assert_eq!(mode(&target), 0o777, "a refused link target is untouched");

        let dangling = temp.path().join("dangling");
        symlink(temp.path().join("absent"), &dangling).expect("dangling symlink");
        let (refusal, _) = prepare_state_directory(&dangling, Some(me))
            .await
            .expect_err("a dangling link must be refused");
        assert_eq!(refusal, StateDirectoryRefusal::NotADirectory);
        assert!(!temp.path().join("absent").exists());

        let file = temp.path().join("file");
        std::fs::write(&file, b"").expect("file");
        let (refusal, _) = prepare_state_directory(&file, Some(me))
            .await
            .expect_err("a file must be refused");
        assert_eq!(refusal, StateDirectoryRefusal::NotADirectory);

        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).expect("foreign dir");
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o777))
            .expect("chmod 777");
        let (refusal, _) = prepare_state_directory(&foreign, Some(me.wrapping_add(1)))
            .await
            .expect_err("a directory owned by another account must be refused");
        assert_eq!(refusal, StateDirectoryRefusal::NotOwned);
        assert_eq!(mode(&foreign), 0o777, "a refused directory is untouched");
    }

    #[test]
    fn env_passthrough_rejects_key_and_provider_owned_names() {
        for owned in [
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "GOOGLE_GENAI_API_KEY",
            "AGY_ADC_AUTH",
            "ANTIGRAVITY_HOME",
        ] {
            let error = AntigravityCliModelProvider::normalize_and_validate_env_passthrough(vec![
                owned.to_string(),
            ])
            .expect_err("provider-owned name must fail");
            assert!(error.to_string().contains("provider-owned"), "{owned}");
        }
        let normalized = AntigravityCliModelProvider::normalize_and_validate_env_passthrough(vec![
            " GOOGLE_CLOUD_PROJECT ".to_string(),
            "GOOGLE_CLOUD_PROJECT".to_string(),
            "GOOGLE_CLOUD_LOCATION".to_string(),
        ])
        .expect("cloud project names are allowed");
        assert_eq!(
            normalized,
            ["GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_LOCATION"]
        );
        assert!(
            AntigravityCliModelProvider::normalize_and_validate_env_passthrough(vec![
                "1BAD".to_string()
            ])
            .is_err()
        );
    }

    #[test]
    fn env_allowlist_copies_only_runtime_and_explicit_entries() {
        let mut cmd = Command::new("agy");
        AntigravityCliModelProvider::apply_env_allowlist_from(
            &mut cmd,
            &["GOOGLE_CLOUD_PROJECT".to_string()],
            [
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("GOOGLE_CLOUD_PROJECT".to_string(), "proj".to_string()),
                ("GEMINI_API_KEY".to_string(), TEST_KEY.to_string()),
                ("AWS_SECRET_ACCESS_KEY".to_string(), "aws".to_string()),
            ],
        );
        let envs: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(envs.contains(&("PATH".to_string(), Some("/usr/bin".to_string()))));
        assert!(envs.contains(&("GOOGLE_CLOUD_PROJECT".to_string(), Some("proj".to_string()))));
        assert!(!envs.iter().any(|(key, _)| key == "GEMINI_API_KEY"));
        assert!(!envs.iter().any(|(key, _)| key == "AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn extra_args_accept_only_known_flags() {
        let accepted = AntigravityCliModelProvider::normalize_and_validate_extra_args(vec![
            "--sandbox".to_string(),
            "--add-dir".to_string(),
            "/srv/shared".to_string(),
            "--agent=reviewer".to_string(),
            "--include-default-customizations=false".to_string(),
        ])
        .expect("known flags");
        assert_eq!(accepted.len(), 5);

        for rejected in [
            vec!["--gemini-dir=/tmp/x".to_string()],
            vec!["--gemini_dir".to_string(), "/tmp/x".to_string()],
            vec!["--app-data-dir=x".to_string()],
            vec!["--login".to_string()],
            vec!["--stdio".to_string()],
            vec!["--worker".to_string()],
            vec!["--cloud_code_endpoint=https://example.test".to_string()],
            vec!["--log-file=/tmp/log".to_string()],
            vec!["--disable_telemetry=false".to_string()],
            vec!["--use_stubby_auth".to_string()],
            vec!["--unknown".to_string()],
            vec!["positional".to_string()],
            vec!["--".to_string()],
            vec!["--sandbox=false".to_string()],
            vec!["--include-default-customizations".to_string()],
            vec!["--add-dir".to_string()],
            vec!["--agent".to_string(), "--sandbox".to_string()],
        ] {
            assert!(
                AntigravityCliModelProvider::normalize_and_validate_extra_args(rejected.clone())
                    .is_err(),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn cli_args_isolate_state_and_disable_telemetry() {
        let args = AntigravityCliModelProvider::build_cli_args(
            Path::new("/state/antigravity_cli/test"),
            &["--sandbox".to_string()],
        );
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "acp",
                "--disable-telemetry",
                "--gemini-dir",
                "/state/antigravity_cli/test",
                "--sandbox"
            ]
        );
    }

    #[test]
    fn permission_policy_requires_explicit_bypass() {
        assert_eq!(
            AntigravityCliModelProvider::acp_permission_policy(&[]),
            acp::AcpPermissionPolicy::RejectOnce
        );
        assert_eq!(
            AntigravityCliModelProvider::acp_permission_policy(&["--sandbox".to_string()]),
            acp::AcpPermissionPolicy::RejectOnce
        );
        assert_eq!(
            AntigravityCliModelProvider::acp_permission_policy(&[
                "--dangerously-skip-permissions".to_string()
            ]),
            acp::AcpPermissionPolicy::AllowOnce
        );
    }

    fn initialize_with_all_methods() -> Value {
        json!({
            "authMethods": [
                { "id": "oauth-personal" },
                { "id": "oauth-business" },
                { "id": "gemini-api-key" },
                { "id": "agent-platform" }
            ],
            "agentCapabilities": {
                "_meta": { "antigravity": { "systemPrompt": { "append": true } } }
            }
        })
    }

    #[test]
    fn authenticate_params_follow_the_configured_method() {
        let initialize = initialize_with_all_methods();
        assert_eq!(
            authenticate_params(
                AntigravityCliAuthMethod::GeminiApiKey,
                Some(TEST_KEY),
                &initialize
            )
            .expect("api key auth"),
            Some(json!({ "methodId": "gemini-api-key", "_meta": { "apiKey": TEST_KEY } }))
        );
        assert_eq!(
            authenticate_params(AntigravityCliAuthMethod::AgentPlatform, None, &initialize)
                .expect("agent platform auth"),
            Some(json!({ "methodId": "agent-platform" }))
        );
        assert_eq!(
            authenticate_params(AntigravityCliAuthMethod::OauthBusiness, None, &initialize)
                .expect("business uses cached sign-in"),
            None
        );
        assert!(matches!(
            authenticate_params(AntigravityCliAuthMethod::OauthPersonal, None, &initialize),
            Err(AcpError::SessionPolicy { .. })
        ));

        let api_key_missing = json!({ "authMethods": [{ "id": "agent-platform" }] });
        assert!(matches!(
            authenticate_params(
                AntigravityCliAuthMethod::GeminiApiKey,
                Some(TEST_KEY),
                &api_key_missing
            ),
            Err(AcpError::NoAuthenticationMethod)
        ));
    }

    #[test]
    fn business_sessions_require_a_business_tier() {
        let session =
            |tier: &str| json!({ "_meta": { "antigravity": { "tier": { "id": tier } } } });
        for tier in [
            "gcp-ge-standard-tier",
            "gcp-enterprise-tier",
            "agy-business-paygo-tier",
        ] {
            check_session_tier(AntigravityCliAuthMethod::OauthBusiness, &session(tier))
                .expect("business tier");
        }
        for tier in ["free-tier", "g1-pro-tier", ""] {
            let error = check_session_tier(AntigravityCliAuthMethod::OauthBusiness, &session(tier))
                .expect_err("consumer tier must be refused");
            assert!(matches!(error, AcpError::SessionPolicy { .. }));
        }
        assert!(check_session_tier(AntigravityCliAuthMethod::OauthBusiness, &json!({})).is_err());
        check_session_tier(
            AntigravityCliAuthMethod::GeminiApiKey,
            &session("free-tier"),
        )
        .expect("explicitly authenticated methods are not tier-checked");
    }

    #[test]
    fn agent_executor_errors_map_to_fixed_public_failures() {
        let invalid_key = "\n\u{26a0} agent executor error: generating and executing: Error 400, Message: API key not valid. Please pass a valid API key., Status: INVALID_ARGUMENT, Details: [map[reason:API_KEY_INVALID]]\n";
        let failure = classify_agent_error(invalid_key).expect("invalid key failure");
        assert_eq!(failure.error_key, "antigravity_cli_api_key_invalid");
        assert!(!failure.message.contains("Please pass"));

        let quota = "\u{26a0} agent executor error: Error 429, Status: RESOURCE_EXHAUSTED";
        assert_eq!(
            classify_agent_error(quota)
                .expect("quota failure")
                .error_key,
            "antigravity_cli_rate_limited"
        );
        let unknown = "\u{26a0} agent executor error: CHILD_TEXT_MUST_NOT_ESCAPE";
        let failure = classify_agent_error(unknown).expect("generic failure");
        assert_eq!(failure.error_key, "antigravity_cli_agent_error");
        assert!(!failure.message.contains("CHILD_TEXT_MUST_NOT_ESCAPE"));

        assert_eq!(classify_agent_error("A normal answer."), None);
        assert_eq!(
            classify_agent_error("Quoting it: \u{26a0} agent executor error: x"),
            None,
            "only a reply that starts with the executor prefix is a failure"
        );
    }

    #[test]
    fn agent_failures_drive_the_shared_retry_policy() {
        let cases = [
            ("Error 400, Details: [map[reason:API_KEY_INVALID]]", true),
            ("Error 403, Status: PERMISSION_DENIED", true),
            ("Error 429, Status: RESOURCE_EXHAUSTED", false),
            ("Error 503, Status: UNAVAILABLE", false),
            ("something unrecognized", false),
        ];
        for (report, non_retryable) in cases {
            let reply = format!("{AGENT_EXECUTOR_ERROR_PREFIX} {report}");
            let failure = classify_agent_error(&reply).expect("executor failure");
            let error = failure.into_error();
            assert_eq!(
                crate::reliable::is_non_retryable(&error),
                non_retryable,
                "retry policy for {report:?} (message: {error})"
            );
        }
    }

    #[test]
    fn system_prompt_uses_session_meta_and_requires_capability() {
        assert_eq!(AntigravityCliModelProvider::session_meta(None), None);
        assert_eq!(AntigravityCliModelProvider::session_meta(Some("  ")), None);
        assert_eq!(
            AntigravityCliModelProvider::session_meta(Some("SYSTEM")),
            Some(json!({ "antigravity": { "systemPrompt": { "append": "SYSTEM" } } }))
        );
        assert!(supports_system_prompt_append(&initialize_with_all_methods()));
        assert!(!supports_system_prompt_append(&json!({})));
    }

    #[test]
    fn model_and_temperature_follow_cli_contract() {
        assert_eq!(
            AntigravityCliModelProvider::forwarded_model("default"),
            None
        );
        assert_eq!(AntigravityCliModelProvider::forwarded_model("  "), None);
        assert_eq!(
            AntigravityCliModelProvider::forwarded_model(" gemini-3.8-flash-high "),
            Some("gemini-3.8-flash-high")
        );
        AntigravityCliModelProvider::validate_temperature(0.7).expect("0.7");
        AntigravityCliModelProvider::validate_temperature(1.0).expect("1.0");
        assert!(AntigravityCliModelProvider::validate_temperature(0.2).is_err());
        assert!(AntigravityCliModelProvider::validate_temperature(f64::NAN).is_err());
    }

    #[test]
    fn vision_override_serializes_images_without_the_system_prompt() {
        let temp = TempDir::new().expect("tempdir");
        let provider = builder(&temp)
            .vision_enabled(true)
            .build()
            .expect("vision provider");
        assert!(provider.capabilities().vision);
        assert_eq!(
            provider.acp_prompt_content("Describe [IMAGE:data:image/png;base64,cG5n]"),
            vec![
                acp::AcpPromptContent::Text("Describe".to_string()),
                acp::AcpPromptContent::Image {
                    data: "cG5n".to_string(),
                    mime_type: "image/png".to_string(),
                },
            ]
        );
    }

    /// Live smoke against an installed `agy`. Uses an isolated state
    /// directory, so no cached sign-in is reused. Set
    /// `ZEROCLAW_TEST_ANTIGRAVITY_API_KEY` to a Gemini API key, and optionally
    /// `ZEROCLAW_TEST_ANTIGRAVITY_MODEL` to a model id from the account.
    #[tokio::test]
    #[ignore = "requires an installed Antigravity CLI and a Gemini API key"]
    async fn live_agy_acp_api_key_prompt() {
        let key = std::env::var("ZEROCLAW_TEST_ANTIGRAVITY_API_KEY")
            .expect("ZEROCLAW_TEST_ANTIGRAVITY_API_KEY");
        let model = std::env::var("ZEROCLAW_TEST_ANTIGRAVITY_MODEL")
            .unwrap_or_else(|_| DEFAULT_MODEL_MARKER.to_string());
        let temp = TempDir::new().expect("tempdir");
        let provider = AntigravityCliModelProvider::builder("live")
            .working_directory(temp.path().to_str().expect("UTF-8 test path"))
            .state_root(temp.path().join("zeroclaw"))
            .api_key(Some(&key))
            .timeout_secs(Some(300))
            .build()
            .expect("provider");
        let reply = provider
            .invoke_acp(
                Some("Answer with exactly the single word the user asks for."),
                "Reply with the word ZEROCLAW_AGY_OK and nothing else.",
                &model,
            )
            .await
            .expect("live reply");
        assert!(reply.contains("ZEROCLAW_AGY_OK"), "reply: {reply}");
    }

    #[cfg(unix)]
    mod unix_fake_child {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// Fake `agy acp` for the API-key flow with a forwarded model:
        /// initialize(1) → authenticate(2) → session/new(3) →
        /// session/set_model(4) → session/prompt(5).
        const API_KEY_BODY: &str = r#"
printf '%s\n' "$@" > args.txt
env > env.txt
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true}}}},"authMethods":[{"id":"oauth-personal"},{"id":"oauth-business"},{"id":"gemini-api-key"},{"id":"agent-platform"}]}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"fake-session","_meta":{"antigravity":{"tier":{"id":"free-tier"}}}}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"PRIVATE_THOUGHT_MUST_NOT_ESCAPE"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"FAKE_AGY_OK"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{"stopReason":"end_turn"}}'
while IFS= read -r line; do :; done
"#;

        /// Fake `agy acp` reporting a consumer tier for a cached sign-in:
        /// initialize(1) → session/new(2); anything further is recorded.
        const CONSUMER_TIER_BODY: &str = r#"
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"_meta":{"antigravity":{"systemPrompt":{"append":true}}}},"authMethods":[{"id":"oauth-personal"},{"id":"oauth-business"},{"id":"gemini-api-key"},{"id":"agent-platform"}]}}'
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fake-session","_meta":{"antigravity":{"tier":{"name":"Antigravity Starter Quota","id":"free-tier"}}}}}'
while IFS= read -r line; do printf '%s\n' "$line" >> requests.ndjson; done
"#;

        /// Fake `agy acp` without the system prompt extension.
        const NO_SYSTEM_PROMPT_BODY: &str = r#"
IFS= read -r line
printf '%s\n' "$line" >> requests.ndjson
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"authMethods":[{"id":"gemini-api-key"}]}}'
while IFS= read -r line; do printf '%s\n' "$line" >> requests.ndjson; done
"#;

        const FAKE_AGY_PROLOGUE: &str = r#"#!/bin/sh
set -eu
if [ "${1-}" = --warm-up ]; then exit 0; fi
"#;

        fn fake_agy(temp: &TempDir, body: &str) -> PathBuf {
            let path = temp.path().join("fake-agy");
            // Write from a child process so no sibling test thread can fork
            // while this process holds the script open for writing (ETXTBSY).
            let written = std::process::Command::new("/bin/sh")
                .args(["-c", r#"printf '%s' "$1" > "$2""#, "sh"])
                .arg(format!("{FAKE_AGY_PROLOGUE}{body}"))
                .arg(&path)
                .status()
                .expect("write fake agy");
            assert!(written.success(), "writing fake agy failed: {written}");
            let mut permissions = std::fs::metadata(&path)
                .expect("fake agy metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&path, permissions).expect("chmod fake agy");
            let warm_up = std::process::Command::new(&path)
                .arg("--warm-up")
                .status()
                .expect("run fake agy warm-up");
            assert!(warm_up.success(), "fake agy warm-up failed: {warm_up}");
            path
        }

        fn fake_builder(temp: &TempDir, body: &str) -> AntigravityCliBuilder {
            let binary = fake_agy(temp, body);
            builder(temp)
                .binary_path(Some(binary.to_str().expect("UTF-8 fake path")))
                .timeout_secs(Some(10))
        }

        fn requests(temp: &TempDir) -> Vec<Value> {
            std::fs::read_to_string(temp.path().join("requests.ndjson"))
                .expect("captured requests")
                .lines()
                .map(|line| serde_json::from_str(line).expect("request JSON"))
                .collect()
        }

        #[tokio::test]
        async fn api_key_travels_only_in_authenticate_meta() {
            let temp = TempDir::new().expect("tempdir");
            let provider = fake_builder(&temp, API_KEY_BODY).build().expect("provider");
            let reply = provider
                .invoke_acp(
                    Some("SYSTEM_PROMPT"),
                    "PRIVATE_PROMPT",
                    "gemini-3.8-flash-high",
                )
                .await
                .expect("fake ACP reply");
            assert_eq!(reply, "FAKE_AGY_OK");

            let args = std::fs::read_to_string(temp.path().join("args.txt")).expect("argv");
            let env = std::fs::read_to_string(temp.path().join("env.txt")).expect("env");
            for captured in [&args, &env] {
                assert!(!captured.contains(TEST_KEY));
                assert!(!captured.contains("PRIVATE_PROMPT"));
                assert!(!captured.contains("SYSTEM_PROMPT"));
            }
            let state = temp
                .path()
                .join("zeroclaw")
                .join("antigravity_cli")
                .join("test");
            assert_eq!(
                args.lines().collect::<Vec<_>>(),
                [
                    "acp",
                    "--disable-telemetry",
                    "--gemini-dir",
                    state.to_str().expect("UTF-8 state path")
                ]
            );
            let mode = std::fs::metadata(&state)
                .expect("state directory created")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "state directory must be private");

            let requests = requests(&temp);
            let methods: Vec<&str> = requests
                .iter()
                .filter_map(|request| request["method"].as_str())
                .collect();
            assert_eq!(
                methods,
                [
                    "initialize",
                    "authenticate",
                    "session/new",
                    "session/set_model",
                    "session/prompt"
                ]
            );
            assert_eq!(
                requests[1]["params"],
                json!({ "methodId": "gemini-api-key", "_meta": { "apiKey": TEST_KEY } })
            );
            assert_eq!(
                requests[2]["params"]["_meta"],
                json!({ "antigravity": { "systemPrompt": { "append": "SYSTEM_PROMPT" } } })
            );
            assert_eq!(requests[3]["params"]["modelId"], "gemini-3.8-flash-high");
            assert_eq!(
                requests[4]["params"]["prompt"],
                json!([{ "type": "text", "text": "PRIVATE_PROMPT" }])
            );
        }

        #[tokio::test]
        async fn business_sign_in_with_consumer_tier_never_sends_the_prompt() {
            let temp = TempDir::new().expect("tempdir");
            let provider = fake_builder(&temp, CONSUMER_TIER_BODY)
                .api_key(None)
                .auth_method(AntigravityCliAuthMethod::OauthBusiness)
                .build()
                .expect("provider");
            let error = provider
                .invoke_acp(None, "PROMPT_MUST_NOT_BE_SENT", "gemini-3.8-flash-high")
                .await
                .expect_err("consumer tier must be refused");
            let message = error.to_string();
            assert!(
                message.starts_with("Antigravity ACP session rejected"),
                "{message}"
            );
            assert!(
                message.contains("not a Gemini Enterprise business tier"),
                "{message}"
            );

            let requests = requests(&temp);
            let methods: Vec<&str> = requests
                .iter()
                .filter_map(|request| request["method"].as_str())
                .collect();
            assert_eq!(methods, ["initialize", "session/new"]);
            assert!(
                !std::fs::read_to_string(temp.path().join("requests.ndjson"))
                    .expect("requests")
                    .contains("PROMPT_MUST_NOT_BE_SENT")
            );
        }

        #[tokio::test]
        async fn system_prompt_without_agent_support_fails_before_authenticate() {
            let temp = TempDir::new().expect("tempdir");
            let provider = fake_builder(&temp, NO_SYSTEM_PROMPT_BODY)
                .build()
                .expect("provider");
            let error = provider
                .invoke_acp(Some("SYSTEM"), "hello", "default")
                .await
                .expect_err("missing capability must fail");
            assert!(
                error.to_string().contains("system prompt append"),
                "{error}"
            );
            let requests = requests(&temp);
            assert_eq!(
                requests.len(),
                1,
                "only initialize may be sent: {requests:?}"
            );
        }

        #[tokio::test]
        async fn missing_binary_returns_stable_build_error() {
            let temp = TempDir::new().expect("tempdir");
            let error = build_error(
                builder(&temp).binary_path(Some(
                    temp.path()
                        .join("no-such-agy")
                        .to_str()
                        .expect("UTF-8 path"),
                )),
            );
            assert!(error.contains("binary was not found or is not executable"));
        }
    }
}
