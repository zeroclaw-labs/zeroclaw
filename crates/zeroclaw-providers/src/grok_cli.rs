//! Grok Build CLI subprocess model_provider.
//!
//! Spawns the `grok` binary in **documented headless mode** for each
//! inference request. Authentication uses the CLI's own session /
//! `XAI_API_KEY`.
//!
//! # Headless transport (version contract)
//!
//! Supported surface (Grok Build CLI that exposes these flags in
//! `grok --help`; verified against CLI ≥ 0.2.x / current stable):
//!
//! | Flag | Role |
//! | ---- | ---- |
//! | `-p` / `--single <PROMPT>` | Documented one-shot when the prompt fits under an argv budget |
//! | `--prompt-file /dev/stdin` | Large prompts on Unix: body on stdin (no disk file; like gemini_cli) |
//! | `--prompt-file <temp>` | Non-Unix fallback only: exclusive `0600` temp + RAII cleanup |
//! | `--output-format plain` | Stable plain-text capture for ZeroClaw |
//! | `--no-auto-update` | Scripts/CI headless hygiene (documented) |
//! | `-m` / `--model` | Optional model override |
//! | `--sandbox <profile>` | Default `strict` (fail-closed); override via `extra_args` |
//!
//! Official docs emphasize `-p` / `--single` and ACP (`grok agent stdio`).
//! This provider stays on **headless one-shot** (same contract as `gemini_cli`)
//! rather than a full ACP client. Large prompts avoid argv via stdin
//! (`--prompt-file /dev/stdin`), not a world-readable temp path. ACP remains
//! a future option for multi-turn embedding; see module tests for the help
//! surface contract.
//!
//! # Usage
//!
//! ```toml
//! [providers.models.grok_cli.default]
//! model = "grok-4.5"
//! # binary_path = "/home/you/.grok/bin/grok"
//! working_directory = "/path/to/agents/default/workspace"
//! # Default argv injects `--sandbox strict`. Ops agents that need broader
//! # access can override: extra_args = ["--sandbox", "off"]
//!
//! [agents.default]
//! model_provider = "grok_cli.default"
//! classifier_provider = "xai.default"  # recommended for channel bots
//! ```
//!
//! # Host-side trust boundary
//!
//! The child process is a tool-capable agent outside ZeroClaw's tool
//! approval path. This provider:
//!
//! - clears the inherited environment and allowlists runtime/auth vars only
//! - defaults to `--sandbox strict` unless `extra_args` already sets `--sandbox`
//! - rejects `extra_args` that collide with provider-owned transport flags
//! - never puts large prompts on argv or a predictable world-readable path
//! - bounds captured stdout and keeps stderr out of public error strings
//!
//! # Limitations
//!
//! - **Conversation history**: Only the system prompt (if present) and the last
//!   user message are forwarded (same contract as `gemini_cli`).
//! - **Native tool calls**: This provider does not emit ZeroClaw tool_calls.
//! - **Temperature**: Only baseline values `0.7` and `1.0` are accepted
//!   (CLI has no sampling flag; values are not forwarded).
//! - **ACP**: Not used here; prefer a future shared runner if multi-turn ACP
//!   embedding is required.
//!
use crate::traits::{ChatRequest, ChatResponse, ModelProvider, TokenUsage};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

/// Default `grok` binary name (resolved via `PATH`).
const DEFAULT_GROK_CLI_BINARY: &str = "grok";

/// Model name used to signal "use the CLI / config default model".
const DEFAULT_MODEL_MARKER: &str = "default";

/// Default wall-clock budget when `timeout_secs` is unset on the alias.
const DEFAULT_GROK_CLI_TIMEOUT: Duration = Duration::from_secs(600);

/// Prefer documented `--single` when the prompt fits comfortably under ARG_MAX.
/// Larger prompts use stdin (`--prompt-file /dev/stdin`) on Unix.
const PROMPT_ARGV_BYTE_BUDGET: usize = 32_768;

/// Path used with `--prompt-file` so the child reads the piped prompt body.
/// Grok headless has no Gemini-style empty-`--prompt`+stdin mode; this is the
/// portable-within-Unix way to avoid writing the prompt to a disk temp file.
#[cfg(unix)]
const PROMPT_STDIN_PATH: &str = "/dev/stdin";

/// Bound captured child stdout (public response path).
const MAX_GROK_CLI_STDOUT_BYTES: usize = 1_048_576;

/// Bound stderr excerpts written to logs only (never raw public errors).
const MAX_GROK_CLI_STDERR_CHARS: usize = 512;

const GROK_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// Default OS sandbox profile injected when `extra_args` does not set one.
const DEFAULT_SANDBOX_PROFILE: &str = "strict";

/// Flags / tokens owned by this provider (rejected in `extra_args`).
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
    "--system-prompt-override",
    "--system-prompt",
];

/// Environment variable names always allowed in the child process.
const ENV_ALLOWLIST_EXACT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
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
    "XAI_API_KEY",
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

/// ModelProvider that invokes Grok Build CLI headless.
pub struct GrokCliModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `grok` binary.
    binary_path: PathBuf,
    /// Subprocess cwd so project `.grok/config.toml` resolves correctly.
    working_directory: Option<PathBuf>,
    /// Operator-supplied argv tokens (validated; no reserved transport flags).
    extra_args: Vec<String>,
    /// Wall-clock timeout for the child process.
    timeout: Duration,
}

/// How the prompt is handed to the CLI (mutually exclusive).
enum PromptHandoff {
    /// Documented `-p` / `--single` (argv).
    SingleArg(String),
    /// Large prompt: `--prompt-file /dev/stdin` + body written to child stdin.
    /// No disk file (gemini_cli-style ARG_MAX avoidance).
    StdinPipe(String),
    /// Non-Unix fallback: exclusive `0600` temp + `--prompt-file`.
    PromptFile(PromptFileGuard),
}

/// RAII cleanup for the exclusive prompt file (runs on cancel/drop too).
struct PromptFileGuard {
    path: PathBuf,
}

impl PromptFileGuard {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PromptFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Kill the process group on timeout/cancel paths (Unix).
#[cfg(unix)]
struct ChildGroupGuard {
    pgid: std::sync::atomic::AtomicI32,
}

#[cfg(unix)]
impl ChildGroupGuard {
    fn new(child_pid: Option<u32>) -> Self {
        let pgid = child_pid.and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        Self {
            pgid: std::sync::atomic::AtomicI32::new(pgid),
        }
    }

    fn disarm(&self) {
        self.pgid.store(0, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(unix)]
impl Drop for ChildGroupGuard {
    fn drop(&mut self) {
        let pgid = self.pgid.load(std::sync::atomic::Ordering::Acquire);
        if pgid <= 0 {
            return;
        }
        // SAFETY: pgid is the process group we created for this child only.
        unsafe {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            const SIGKILL: i32 = 9;
            let _ = kill(-pgid, SIGKILL);
        }
    }
}

impl GrokCliModelProvider {
    /// Create a new provider.
    ///
    /// `timeout_secs` comes from the alias `timeout_secs` field (shared
    /// `ModelProviderConfig`); falls back to [`DEFAULT_GROK_CLI_TIMEOUT`].
    pub fn new(
        alias: &str,
        binary_path: Option<&str>,
        working_directory: Option<&str>,
        extra_args: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> anyhow::Result<Self> {
        let binary_path = binary_path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_GROK_CLI_BINARY));
        let working_directory = working_directory
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        let extra_args = Self::normalize_and_validate_extra_args(extra_args)?;
        let timeout = timeout_secs
            .filter(|s| *s > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_GROK_CLI_TIMEOUT);
        Ok(Self {
            alias: alias.to_string(),
            binary_path,
            working_directory,
            extra_args,
            timeout,
        })
    }

    fn normalize_and_validate_extra_args(extra_args: Vec<String>) -> anyhow::Result<Vec<String>> {
        let extra_args: Vec<String> = extra_args
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for arg in &extra_args {
            let flag = arg.split('=').next().unwrap_or(arg);
            if RESERVED_EXTRA_ARG_FLAGS.contains(&flag) {
                anyhow::bail!(
                    "grok_cli extra_args must not include reserved flag `{flag}`. \
                     Prompt, model, output format, session, and cwd are owned by ZeroClaw. \
                     Use working_directory / model config fields instead."
                );
            }
        }
        Ok(extra_args)
    }

    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        GROK_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("Grok CLI model_provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by Grok CLI model_provider: {temperature}. \
                 Supported values: 0.7 or 1.0 (not forwarded; CLI has no sampling flag)"
            );
        }
        Ok(())
    }

    fn redact_stderr(stderr: &[u8]) -> String {
        let text = String::from_utf8_lossy(stderr);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.chars().count() <= MAX_GROK_CLI_STDERR_CHARS {
            return trimmed.to_string();
        }
        let clipped: String = trimmed.chars().take(MAX_GROK_CLI_STDERR_CHARS).collect();
        format!("{clipped}...")
    }

    fn truncate_stdout(stdout: Vec<u8>) -> anyhow::Result<String> {
        let bytes = if stdout.len() > MAX_GROK_CLI_STDOUT_BYTES {
            &stdout[..MAX_GROK_CLI_STDOUT_BYTES]
        } else {
            stdout.as_slice()
        };
        let text = String::from_utf8(bytes.to_vec()).map_err(|err| {
            anyhow::Error::msg(format!("Grok Build CLI produced non-UTF-8 output: {err}"))
        })?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            anyhow::bail!(
                "Grok Build CLI returned empty output. \
                 Check authentication and that the CLI produced a plain-text reply."
            );
        }
        if stdout.len() > MAX_GROK_CLI_STDOUT_BYTES {
            return Ok(format!(
                "{trimmed}\n... [output truncated at {MAX_GROK_CLI_STDOUT_BYTES} bytes]"
            ));
        }
        Ok(trimmed.to_string())
    }

    fn extra_args_set_sandbox(extra_args: &[String]) -> bool {
        extra_args.iter().any(|a| {
            a == "--sandbox" || a.starts_with("--sandbox=") || a == "GROK_SANDBOX" // defensive; not a real flag
        })
    }

    /// Build argv after the binary (excluding the binary itself).
    fn build_cli_args(model: &str, handoff: &PromptHandoff, extra_args: &[String]) -> Vec<String> {
        let mut args = Vec::with_capacity(16 + extra_args.len());
        match handoff {
            PromptHandoff::SingleArg(prompt) => {
                args.push("--single".to_string());
                args.push(prompt.clone());
            }
            PromptHandoff::StdinPipe(_) => {
                args.push("--prompt-file".to_string());
                #[cfg(unix)]
                args.push(PROMPT_STDIN_PATH.to_string());
                #[cfg(not(unix))]
                args.push("/dev/stdin".to_string());
            }
            PromptHandoff::PromptFile(guard) => {
                args.push("--prompt-file".to_string());
                args.push(guard.path().display().to_string());
            }
        }
        args.push("--output-format".to_string());
        args.push("plain".to_string());
        args.push("--no-auto-update".to_string());
        if !Self::extra_args_set_sandbox(extra_args) {
            args.push("--sandbox".to_string());
            args.push(DEFAULT_SANDBOX_PROFILE.to_string());
        }
        if Self::should_forward_model(model) {
            args.push("-m".to_string());
            args.push(model.to_string());
        }
        args.extend(extra_args.iter().cloned());
        args
    }

    fn prepare_prompt_handoff(message: &str) -> anyhow::Result<PromptHandoff> {
        if message.len() <= PROMPT_ARGV_BYTE_BUDGET {
            return Ok(PromptHandoff::SingleArg(message.to_string()));
        }
        // Prefer stdin so large system prompts never hit ARG_MAX and never
        // land on a shared temp path (gemini_cli pattern, Grok-compatible).
        #[cfg(unix)]
        {
            if Path::new(PROMPT_STDIN_PATH).exists() {
                return Ok(PromptHandoff::StdinPipe(message.to_string()));
            }
        }
        Ok(PromptHandoff::PromptFile(create_secure_prompt_file(
            message,
        )?))
    }

    fn apply_env_allowlist(cmd: &mut Command) {
        cmd.env_clear();
        for (key, value) in std::env::vars() {
            if env_var_allowed(&key) {
                cmd.env(key, value);
            }
        }
    }

    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let handoff = Self::prepare_prompt_handoff(message)?;
        let args = Self::build_cli_args(model, &handoff, &self.extra_args);
        let pipe_stdin = matches!(handoff, PromptHandoff::StdinPipe(_));

        let mut cmd = Command::new(&self.binary_path);
        cmd.args(&args);
        if let Some(cwd) = self.working_directory.as_ref() {
            cmd.current_dir(cwd);
        }
        Self::apply_env_allowlist(&mut cmd);
        cmd.kill_on_drop(true);
        if pipe_stdin {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            // Own process group so timeout can reap descendants (tools, shells).
            cmd.process_group(0);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                drop(handoff);
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "phase": "spawn",
                            "error": format!("{err}"),
                        })),
                    "grok_cli: failed to spawn binary"
                );
                return Err(anyhow::Error::msg(format!(
                    "Failed to spawn Grok Build CLI binary at {}. \
                     Ensure `grok` is installed and on PATH \
                     (e.g. ~/.grok/bin), or set binary_path on the provider alias.",
                    self.binary_path.display()
                )));
            }
        };

        if let PromptHandoff::StdinPipe(body) = &handoff {
            let Some(mut stdin) = child.stdin.take() else {
                drop(handoff);
                return Err(anyhow::Error::msg(
                    "Failed to open Grok Build CLI stdin for large prompt handoff",
                ));
            };
            if let Err(err) = stdin.write_all(body.as_bytes()).await {
                drop(handoff);
                return Err(anyhow::Error::msg(format!(
                    "Failed to write prompt to Grok Build CLI stdin: {err}"
                )));
            }
            if let Err(err) = stdin.shutdown().await {
                drop(handoff);
                return Err(anyhow::Error::msg(format!(
                    "Failed to finalize Grok Build CLI stdin: {err}"
                )));
            }
        }

        #[cfg(unix)]
        let group_guard = ChildGroupGuard::new(child.id());

        // On timeout the future is dropped: kill_on_drop kills the leader and
        // ChildGroupGuard Drop SIGKILLs the process group (descendants).
        let wait_result = timeout(self.timeout, child.wait_with_output()).await;

        // Keep handoff alive until the child exits (temp-file path, if any).
        drop(handoff);

        let output = match wait_result {
            Ok(Ok(output)) => {
                #[cfg(unix)]
                group_guard.disarm();
                output
            }
            Ok(Err(err)) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "phase": "process_wait",
                            "error": format!("{err}"),
                        })),
                    "grok_cli: process wait failed"
                );
                return Err(anyhow::Error::msg(
                    "Grok Build CLI process failed while waiting for completion",
                ));
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Timeout)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "binary": self.binary_path.display().to_string(),
                            "timeout": format!("{:?}", self.timeout),
                        })),
                    "grok_cli: request timed out"
                );
                return Err(anyhow::Error::msg(format!(
                    "Grok Build CLI request timed out after {}s",
                    self.timeout.as_secs()
                )));
            }
        };

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr_excerpt = Self::redact_stderr(&output.stderr);
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "exit_status",
                        "status_code": code,
                        "stderr_excerpt": stderr_excerpt,
                    })),
                "grok_cli: non-zero exit"
            );
            anyhow::bail!(
                "Grok Build CLI exited with non-zero status {code}. \
                 Check authentication (`grok login` or XAI_API_KEY), \
                 sandbox profile, and workspace `.grok` permissions."
            );
        }

        Self::truncate_stdout(output.stdout)
    }
}

fn env_var_allowed(key: &str) -> bool {
    if ENV_ALLOWLIST_EXACT.contains(&key) {
        return true;
    }
    // Locale + Grok / xAI runtime knobs (GROK_SANDBOX, GROK_*, XAI_*).
    if key.starts_with("LC_") || key.starts_with("GROK_") || key.starts_with("XAI_") {
        return true;
    }
    false
}

fn create_secure_prompt_file(contents: &str) -> anyhow::Result<PromptFileGuard> {
    use std::io::Write;

    let dir = std::env::temp_dir();
    for _ in 0..32 {
        let name = format!("zeroclaw-grok-prompt-{}.md", uuid::Uuid::new_v4());
        let path = dir.join(name);

        #[cfg(unix)]
        let open_result = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
        };

        #[cfg(not(unix))]
        let open_result = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);

        match open_result {
            Ok(mut file) => {
                file.write_all(contents.as_bytes()).map_err(|err| {
                    let _ = std::fs::remove_file(&path);
                    anyhow::Error::msg(format!("Failed to write Grok CLI prompt file: {err}"))
                })?;
                file.sync_all().map_err(|err| {
                    let _ = std::fs::remove_file(&path);
                    anyhow::Error::msg(format!("Failed to sync Grok CLI prompt file: {err}"))
                })?;
                return Ok(PromptFileGuard { path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(anyhow::Error::msg(format!(
                    "Failed to create exclusive Grok CLI prompt file: {err}"
                )));
            }
        }
    }
    anyhow::bail!("Failed to create exclusive Grok CLI prompt file after retries")
}

#[async_trait]
impl ModelProvider for GrokCliModelProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if let Some(t) = temperature {
            Self::validate_temperature(t)?;
        }

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => {
                format!("{system}\n\n{message}")
            }
            _ => message.to_string(),
        };

        self.invoke_cli(&full_message, model).await
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
            usage: Some(TokenUsage::default()),
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

    fn provider(
        binary: Option<&str>,
        cwd: Option<&str>,
        extra: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> GrokCliModelProvider {
        GrokCliModelProvider::new("test", binary, cwd, extra, timeout_secs).expect("provider")
    }

    #[test]
    fn new_uses_explicit_binary_path() {
        let p = provider(Some("/home/user/.grok/bin/grok"), None, vec![], None);
        assert_eq!(p.binary_path, PathBuf::from("/home/user/.grok/bin/grok"));
        assert!(p.working_directory.is_none());
        assert!(p.extra_args.is_empty());
        assert_eq!(p.timeout, DEFAULT_GROK_CLI_TIMEOUT);
    }

    #[test]
    fn new_defaults_to_grok() {
        let p = provider(None, None, vec![], None);
        assert_eq!(p.binary_path, PathBuf::from("grok"));
    }

    #[test]
    fn new_ignores_blank_binary_path() {
        let p = provider(Some("   "), None, vec![], None);
        assert_eq!(p.binary_path, PathBuf::from("grok"));
    }

    #[test]
    fn new_sets_working_directory() {
        let p = provider(None, Some("/tmp/agent-ws"), vec![], None);
        assert_eq!(p.working_directory, Some(PathBuf::from("/tmp/agent-ws")));
    }

    #[test]
    fn new_honors_timeout_secs() {
        let p = provider(None, None, vec![], Some(90));
        assert_eq!(p.timeout, Duration::from_secs(90));
    }

    #[test]
    fn new_trims_and_drops_empty_extra_args() {
        let p = provider(
            None,
            None,
            vec!["--max-turns".into(), "  ".into(), "20".into()],
            None,
        );
        assert_eq!(
            p.extra_args,
            vec!["--max-turns".to_string(), "20".to_string()]
        );
    }

    #[test]
    fn new_rejects_reserved_extra_args() {
        let result = GrokCliModelProvider::new(
            "test",
            None,
            None,
            vec!["--prompt-file".into(), "/tmp/x".into()],
            None,
        );
        let err = match result {
            Ok(_) => panic!("expected reserved flag rejection"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("reserved flag"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn new_rejects_reserved_model_flag() {
        let result =
            GrokCliModelProvider::new("test", None, None, vec!["-m".into(), "x".into()], None);
        let err = match result {
            Ok(_) => panic!("expected reserved flag rejection"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("reserved flag"));
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(GrokCliModelProvider::should_forward_model("grok-4.5"));
        assert!(GrokCliModelProvider::should_forward_model("grok-build"));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!GrokCliModelProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!GrokCliModelProvider::should_forward_model(""));
        assert!(!GrokCliModelProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(GrokCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(GrokCliModelProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = GrokCliModelProvider::validate_temperature(0.2).unwrap_err();
        assert!(
            err.to_string()
                .contains("temperature unsupported by Grok CLI")
        );
    }

    #[test]
    fn build_cli_args_small_prompt_uses_documented_single() {
        let handoff = PromptHandoff::SingleArg("hello".into());
        let args = GrokCliModelProvider::build_cli_args(DEFAULT_MODEL_MARKER, &handoff, &[]);
        assert_eq!(args[0], "--single");
        assert_eq!(args[1], "hello");
        assert!(args.iter().any(|a| a == "--output-format"));
        assert!(args.iter().any(|a| a == "plain"));
        assert!(args.iter().any(|a| a == "--no-auto-update"));
        // Fail-closed default sandbox.
        let sandbox_idx = args.iter().position(|a| a == "--sandbox").expect("sandbox");
        assert_eq!(args[sandbox_idx + 1], "strict");
        assert!(!args.iter().any(|a| a == "-m"));
        assert!(!args.iter().any(|a| a == "--prompt-file"));
    }

    #[test]
    fn build_cli_args_large_prompt_uses_stdin_path() {
        let handoff = PromptHandoff::StdinPipe("big".into());
        let args = GrokCliModelProvider::build_cli_args("grok-4.5", &handoff, &[]);
        assert_eq!(args[0], "--prompt-file");
        assert_eq!(args[1], "/dev/stdin");
        let m_idx = args.iter().position(|a| a == "-m").expect("-m flag");
        assert_eq!(args[m_idx + 1], "grok-4.5");
        assert!(!args.iter().any(|a| a == "--single"));
    }

    #[test]
    fn build_cli_args_prompt_file_fallback_path() {
        let guard = PromptFileGuard {
            path: PathBuf::from("/tmp/prompt.md"),
        };
        let handoff = PromptHandoff::PromptFile(guard);
        let args = GrokCliModelProvider::build_cli_args("grok-4.5", &handoff, &[]);
        assert_eq!(args[0], "--prompt-file");
        assert_eq!(args[1], "/tmp/prompt.md");
    }

    #[test]
    fn build_cli_args_respects_operator_sandbox_override() {
        let handoff = PromptHandoff::SingleArg("hi".into());
        let extra = vec!["--sandbox".to_string(), "off".to_string()];
        let args = GrokCliModelProvider::build_cli_args("default", &handoff, &extra);
        let sandbox_flags: Vec<_> = args.iter().filter(|a| *a == "--sandbox").collect();
        assert_eq!(sandbox_flags.len(), 1, "must not double-inject --sandbox");
        let idx = args.iter().position(|a| a == "--sandbox").unwrap();
        assert_eq!(args[idx + 1], "off");
    }

    #[test]
    fn build_cli_args_appends_extra_args() {
        let handoff = PromptHandoff::SingleArg("hi".into());
        let extra = vec!["--max-turns".to_string(), "20".to_string()];
        let args = GrokCliModelProvider::build_cli_args("grok-4.5", &handoff, &extra);
        assert!(args.ends_with(extra.as_slice()));
    }

    #[test]
    fn prepare_prompt_handoff_small_uses_single() {
        let h = GrokCliModelProvider::prepare_prompt_handoff("short").unwrap();
        match h {
            PromptHandoff::SingleArg(s) => assert_eq!(s, "short"),
            PromptHandoff::StdinPipe(_) | PromptHandoff::PromptFile(_) => {
                panic!("expected single-arg handoff")
            }
        }
    }

    #[test]
    fn prepare_prompt_handoff_large_uses_stdin_on_unix() {
        let big = "x".repeat(PROMPT_ARGV_BYTE_BUDGET + 1);
        let h = GrokCliModelProvider::prepare_prompt_handoff(&big).unwrap();
        match h {
            PromptHandoff::StdinPipe(body) => {
                assert_eq!(body.len(), PROMPT_ARGV_BYTE_BUDGET + 1);
            }
            PromptHandoff::PromptFile(guard) => {
                // Non-Unix or missing /dev/stdin: secure temp fallback.
                assert!(guard.path().exists());
                let body = std::fs::read_to_string(guard.path()).unwrap();
                assert_eq!(body.len(), PROMPT_ARGV_BYTE_BUDGET + 1);
            }
            PromptHandoff::SingleArg(_) => panic!("expected large-prompt handoff"),
        }
    }

    #[test]
    fn env_allowlist_blocks_unrelated_secrets() {
        assert!(env_var_allowed("PATH"));
        assert!(env_var_allowed("XAI_API_KEY"));
        assert!(env_var_allowed("GROK_SANDBOX"));
        assert!(env_var_allowed("LC_ALL"));
        assert!(!env_var_allowed("ZEROCLAW_SLACK_BOT_TOKEN"));
        assert!(!env_var_allowed("AWS_SECRET_ACCESS_KEY"));
        assert!(!env_var_allowed("OPENAI_API_KEY"));
    }

    #[test]
    fn truncate_stdout_rejects_empty() {
        let err = GrokCliModelProvider::truncate_stdout(b"   \n".to_vec()).unwrap_err();
        assert!(err.to_string().contains("empty output"));
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_stable_error() {
        let model_provider = provider(Some("/nonexistent/path/to/grok"), None, vec![], None);
        let result = model_provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn Grok Build CLI binary"),
            "unexpected error message: {msg}"
        );
        // Public error must not echo OS err details beyond the template.
        assert!(!msg.contains("prompt-file"));
    }

    /// Live compatibility check: when `grok` is on PATH, its `--help` must
    /// document the headless surface this provider relies on.
    #[test]
    fn local_grok_help_lists_supported_headless_surface_when_available() {
        let output = std::process::Command::new("grok").arg("--help").output();
        let Ok(output) = output else {
            return; // binary not installed in this environment
        };
        if !output.status.success() {
            return;
        }
        let help = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            help.contains("--single") || help.contains("-p"),
            "installed grok --help must document -p/--single"
        );
        assert!(
            help.contains("--prompt-file"),
            "installed grok --help must document --prompt-file (large-prompt path)"
        );
        assert!(
            help.contains("--output-format"),
            "installed grok --help must document --output-format"
        );
        // `--no-auto-update` is documented in the headless scripting guide and
        // accepted by current builds; it may be omitted from short `--help`.
        assert!(
            help.contains("--sandbox"),
            "installed grok --help must document --sandbox"
        );
    }
}
