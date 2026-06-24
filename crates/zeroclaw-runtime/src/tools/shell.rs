use crate::platform::RuntimeAdapter;
use crate::security::SecurityPolicy;
use crate::security::traits::Sandbox;
use async_trait::async_trait;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::platform::is_android;
use zeroclaw_api::tool::{Tool, ToolResult, with_ephemeral_workspace_warning};

/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
const POST_EXIT_DRAIN: Duration = Duration::from_millis(250);

/// Drop guard that SIGKILLs the child's process group on cancel/timeout paths.
/// Disarmed after `child.wait()` returns so it never signals a recycled PID.
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
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Kill)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({ "pgid": pgid, "signal": "SIGKILL" })),
            "shell tool reaping child process group"
        );
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
#[cfg(not(target_os = "windows"))]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Environment variables safe to pass to shell commands on Windows.
/// Includes Windows-specific variables needed for cmd.exe and program resolution.
#[cfg(target_os = "windows")]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TERM",
    "LANG",
    "USERNAME",
];

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn Sandbox>,
    timeout_secs: u64,
    /// Environment forwarded from the connected TUI client. When set, these
    /// vars are overlaid on top of the safe-env snapshot, letting the user's
    /// real shell environment (PATH, credentials, etc.) reach subprocesses
    /// even though the daemon itself may have a stripped-down env.
    tui_env: Option<HashMap<String, String>>,
    /// Whether workspace writes performed by the command persist on the host.
    /// `false` when the runtime uses an ephemeral sandbox (e.g. Docker without
    /// a workspace volume mount), in which case files written via shell succeed
    /// inside the container but are invisible on the host and discarded at
    /// session end. The shell tool can't tell a read from a write, so rather
    /// than refusing (like `file_write`) it attaches a loud warning to every
    /// executed command's result. See issue #4627.
    persistent_writes: bool,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        let timeout_secs = security.shell_timeout_secs;
        Self {
            security,
            runtime,
            sandbox: Arc::new(crate::security::NoopSandbox),
            timeout_secs,
            tui_env: None,
            persistent_writes: true,
        }
    }

    pub fn new_with_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        let timeout_secs = security.shell_timeout_secs;
        Self {
            security,
            runtime,
            sandbox,
            timeout_secs,
            tui_env: None,
            persistent_writes: true,
        }
    }

    /// Mark whether the active runtime persists workspace writes to the host.
    ///
    /// Pass `false` for an ephemeral runtime (Docker tmpfs / no volume mount)
    /// to attach a loud ephemeral-workspace warning to every executed command,
    /// so silent data loss is visible (issue #4627). Defaults to `true`,
    /// preserving existing behaviour on native runtimes and in tests.
    pub fn with_persistent_writes(mut self, persistent: bool) -> Self {
        self.persistent_writes = persistent;
        self
    }

    /// Override the command execution timeout (in seconds).
    pub fn with_timeout_secs(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Overlay the TUI client's environment on top of the safe-env snapshot.
    ///
    /// Pass `Some(env)` to enable forwarding; `None` is a no-op (same as not
    /// calling this method at all).
    pub fn with_tui_env(mut self, env: Option<HashMap<String, String>>) -> Self {
        self.tui_env = env;
        self
    }
}

/// Decode raw process output bytes to a UTF-8 String.
///
/// On Windows, cmd.exe emits bytes in the active console output code page
/// (e.g. CP936/GBK on Simplified Chinese systems). We query the code page at
/// runtime and transcode via `encoding_rs` so non-ASCII characters survive
/// intact instead of being replaced by U+FFFD.
///
/// On all other platforms the shell runs under the user's locale (usually
/// UTF-8 already), so `from_utf8_lossy` is sufficient.
#[cfg(target_os = "windows")]
fn decode_output(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::GetACP;
    use windows::Win32::System::Console::GetConsoleOutputCP;

    let cp = unsafe { GetConsoleOutputCP() };
    let cp = if cp == 0 { unsafe { GetACP() } } else { cp };

    decode_output_with_code_page(bytes, cp)
}

#[cfg(any(target_os = "windows", test))]
fn decode_output_with_code_page(bytes: &[u8], cp: u32) -> String {
    let encoding = windows_code_page_to_encoding(cp);
    if std::ptr::eq(encoding, encoding_rs::UTF_8) {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        let (cow, _enc_used, _had_errors) = encoding.decode(bytes);
        cow.into_owned()
    }
}

/// Map a Windows code page identifier to an `encoding_rs` `Encoding`.
/// Falls back to UTF-8 (lossy) for unknown code pages.
#[cfg(any(target_os = "windows", test))]
fn windows_code_page_to_encoding(cp: u32) -> &'static encoding_rs::Encoding {
    match cp {
        932 => encoding_rs::SHIFT_JIS,
        936 | 54936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        1250 => encoding_rs::WINDOWS_1250,
        1251 => encoding_rs::WINDOWS_1251,
        1252 => encoding_rs::WINDOWS_1252,
        1253 => encoding_rs::WINDOWS_1253,
        1254 => encoding_rs::WINDOWS_1254,
        1255 => encoding_rs::WINDOWS_1255,
        1256 => encoding_rs::WINDOWS_1256,
        1257 => encoding_rs::WINDOWS_1257,
        1258 => encoding_rs::WINDOWS_1258,
        20127 | 65001 => encoding_rs::UTF_8,
        _ => encoding_rs::UTF_8,
    }
}

#[cfg(not(target_os = "windows"))]
fn decode_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Name of the environment variable that carries the in-flight session key
/// into shell tools.
pub(crate) const SESSION_ID_ENV_VAR: &str = "ZEROCLAW_SESSION_ID";

fn get_session_id() -> Option<String> {
    zeroclaw_api::TOOL_LOOP_SESSION_KEY
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .filter(|key| !key.is_empty())
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk commands in supervised mode",
                    "default": false
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "command"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'command' parameter")
            })?;
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        match self.security.validate_command_execution(command, approved) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };

        // Apply sandbox wrapping before execution.
        // The Sandbox trait operates on std::process::Command, so use as_std_mut()
        // to get a mutable reference to the underlying command.
        self.sandbox.wrap_command(cmd.as_std_mut()).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "shell tool: sandbox wrap_command failed"
            );
            anyhow::Error::msg(format!("Sandbox error: {e}"))
        })?;

        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        // Injected after env_clear so it survives; absent when the turn is unscoped.
        if let Some(session_id) = get_session_id() {
            cmd.env(SESSION_ID_ENV_VAR, session_id);
        }

        // Overlay TUI env on top of the safe-env snapshot. TUI vars win on
        // conflict — the user's real PATH etc. should take precedence over
        // whatever the daemon process inherited.
        if let Some(ref tui_env) = self.tui_env {
            for (k, v) in tui_env {
                cmd.env(k, v);
            }
        }

        // Android: platform tools (sh, getprop, am, dumpsys, content, pm, ...)
        // live in /system/bin and /system/xbin. The cleared+rebuilt PATH above
        // may omit them, leaving the shell unable to resolve any platform tool.
        // Detect Android at runtime (works for bionic and musl builds).
        if is_android() {
            let ambient = std::env::var("PATH").unwrap_or_default();
            let tui_path = self
                .tui_env
                .as_ref()
                .and_then(|env| env.get("PATH"))
                .map(String::as_str);
            cmd.env("PATH", android_child_path(tui_path, &ambient));
        }

        let timeout_secs = self.timeout_secs;
        // Run in own process group so `ChildGroupGuard` can reap the
        // whole subtree (backgrounded jobs, subshells) on any exit path.
        #[cfg(unix)]
        cmd.process_group(0);
        cmd.kill_on_drop(true);
        // `output()` pipes stdio implicitly; `spawn()` does not.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to spawn command: {e}")),
                });
            }
        };

        #[cfg(unix)]
        let group_guard = ChildGroupGuard::new(child.id());

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let drain_stdout = drain_capped(stdout_handle, MAX_OUTPUT_BYTES);
        let drain_stderr = drain_capped(stderr_handle, MAX_OUTPUT_BYTES);
        let wait_fut = async {
            let status = child.wait().await?;
            #[cfg(unix)]
            group_guard.disarm();
            let (out, err) = tokio::join!(
                tokio::time::timeout(POST_EXIT_DRAIN, drain_stdout),
                tokio::time::timeout(POST_EXIT_DRAIN, drain_stderr),
            );
            Ok::<_, std::io::Error>((status, out.unwrap_or_default(), err.unwrap_or_default()))
        };

        let mut result =
            match tokio::time::timeout(Duration::from_secs(timeout_secs), wait_fut).await {
                Ok(Ok((status, stdout_bytes, stderr_bytes))) => {
                    let mut stdout = decode_output(&stdout_bytes);
                    let mut stderr = decode_output(&stderr_bytes);

                    if stdout.len() > MAX_OUTPUT_BYTES {
                        let mut b = MAX_OUTPUT_BYTES.min(stdout.len());
                        while b > 0 && !stdout.is_char_boundary(b) {
                            b -= 1;
                        }
                        stdout.truncate(b);
                        stdout.push_str("\n... [output truncated at 1MB]");
                    }
                    if stderr.len() > MAX_OUTPUT_BYTES {
                        let mut b = MAX_OUTPUT_BYTES.min(stderr.len());
                        while b > 0 && !stderr.is_char_boundary(b) {
                            b -= 1;
                        }
                        stderr.truncate(b);
                        stderr.push_str("\n... [stderr truncated at 1MB]");
                    }

                    ToolResult {
                        success: status.success(),
                        output: stdout,
                        error: if stderr.is_empty() {
                            None
                        } else {
                            Some(stderr)
                        },
                    }
                }
                Ok(Err(e)) => ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to execute command: {e}")),
                },
                Err(_) => ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Command timed out after {timeout_secs}s and was killed"
                    )),
                },
            };

        // The command ran inside an ephemeral workspace: any files it wrote are
        // invisible on the host and discarded at session end (issue #4627).
        // Inject the warning into whichever field the dispatcher surfaces to the
        // model — `output` on success, `error` on failure — so it is never lost.
        if !self.persistent_writes {
            result.output = with_ephemeral_workspace_warning(&result.output);
            if let Some(err) = result.error.take() {
                result.error = Some(with_ephemeral_workspace_warning(&err));
            }
        }

        Ok(result)
    }
}

async fn drain_capped<R>(reader: Option<R>, cap: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut reader) = reader else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(cap.saturating_sub(buf.len()).max(1));
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= cap {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    buf
}

/// Compose the child `PATH` for an Android shell: the platform tool dirs
/// (`/system/bin:/system/xbin`) are prefixed onto the curated PATH, with a
/// TUI-provided PATH winning over the daemon's ambient PATH. Yields the bare
/// platform dirs when the resolved base is empty.
fn android_child_path(tui_path: Option<&str>, ambient_path: &str) -> String {
    let base = tui_path.unwrap_or(ambient_path);
    if base.is_empty() {
        "/system/bin:/system/xbin".to_string()
    } else {
        format!("/system/bin:/system/xbin:{base}")
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn android_child_path_prefixes_platform_dirs_with_tui_path_winning() {
        assert_eq!(
            super::android_child_path(Some("/usr/local/bin"), "/daemon"),
            "/system/bin:/system/xbin:/usr/local/bin"
        );
        assert_eq!(
            super::android_child_path(None, "/daemon"),
            "/system/bin:/system/xbin:/daemon"
        );
        assert_eq!(
            super::android_child_path(None, ""),
            "/system/bin:/system/xbin"
        );
    }

    #[test]
    fn is_android_returns_bool_without_panicking() {
        let _ = zeroclaw_api::platform::is_android();
    }
    use super::*;
    use crate::platform::{NativeRuntime, RuntimeAdapter};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use zeroclaw_tools::wrappers::{PathGuardedTool, RateLimitedTool};

    #[tokio::test]
    async fn get_session_id_returns_scoped_session_key() {
        let got = crate::agent::loop_::scope_session_key(Some("gw_abc-123".to_string()), async {
            get_session_id()
        })
        .await;
        assert_eq!(got, Some("gw_abc-123".to_string()));
    }

    #[test]
    fn get_session_id_none_outside_a_scoped_turn() {
        assert_eq!(get_session_id(), None);
    }

    #[tokio::test]
    async fn get_session_id_none_for_empty_session_key() {
        let got =
            crate::agent::loop_::scope_session_key(Some(String::new()), async { get_session_id() })
                .await;
        assert_eq!(got, None);
    }

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    #[cfg(windows)]
    fn stdin_reader_command() -> &'static str {
        "more"
    }

    #[cfg(not(windows))]
    fn stdin_reader_command() -> &'static str {
        "cat"
    }

    #[cfg(windows)]
    fn success_with_stderr_command() -> &'static str {
        "echo out && echo warn 1>&2"
    }

    #[cfg(not(windows))]
    fn success_with_stderr_command() -> &'static str {
        "echo out; echo warn >&2"
    }

    #[cfg(windows)]
    fn medium_risk_write_command() -> &'static str {
        "copy /Y NUL zeroclaw_shell_approval_test"
    }

    #[cfg(not(windows))]
    fn medium_risk_write_command() -> &'static str {
        "touch zeroclaw_shell_approval_test"
    }

    fn medium_risk_write_base() -> &'static str {
        medium_risk_write_command()
            .split_whitespace()
            .next()
            .expect("medium-risk test command should have a base command")
    }

    /// Returns the fully-wrapped shell tool as it is composed in production:
    /// RateLimited(PathGuarded(ShellTool)).  Tests that verify path-blocking or
    /// rate-limiting behaviour must use this helper so they exercise the wrappers.
    fn wrapped_shell(security: Arc<SecurityPolicy>) -> RateLimitedTool<PathGuardedTool<ShellTool>> {
        RateLimitedTool::new(
            PathGuardedTool::new(
                ShellTool::new(security.clone(), test_runtime()),
                security.clone(),
            ),
            security,
        )
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required field should be an array")
                .contains(&json!("command"))
        );
        assert!(schema["properties"]["approved"].is_object());
    }

    #[tokio::test]
    async fn shell_stdin_is_eof_not_the_terminal() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![stdin_reader_command().into()],
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let fut = tool.execute(json!({"command": stdin_reader_command()}));
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), fut).await;
        assert!(
            res.is_ok(),
            "a stdin-reading command hung — stdin is not null and may reach the terminal"
        );
        assert!(
            res.unwrap()
                .expect("stdin reader should return a result")
                .success
        );
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("not allowed") || error.contains("high-risk"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .expect("error field should be present for blocked command")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    // ── Ephemeral-workspace warning (issue #4627) ────────────────

    /// On an ephemeral runtime the shell tool stays usable but every executed
    /// command's output carries a loud warning so writes that won't persist are
    /// visible. The original command output must be preserved below the banner.
    #[tokio::test]
    async fn shell_warns_on_ephemeral_workspace() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_persistent_writes(false);
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command should run");
        assert!(result.success);
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must be present in output, got: {}",
            result.output
        );
        assert!(
            result.output.contains("mount_workspace"),
            "warning must name the config key to fix it, got: {}",
            result.output
        );
        assert!(
            result.output.contains("hello"),
            "original command output must be preserved, got: {}",
            result.output
        );
    }

    /// A failed command surfaces `error`, not `output`, to the model. The
    /// ephemeral warning must be injected into the error field too so it is
    /// never lost on the failure path.
    #[tokio::test]
    async fn shell_warns_on_ephemeral_workspace_failure_path() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_persistent_writes(false);
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz_4627"}))
            .await
            .expect("command should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("EPHEMERAL WORKSPACE"),
            "ephemeral warning must reach the error field on failures, got: {:?}",
            result.error
        );
    }

    /// A command that exits 0 but also writes to stderr yields
    /// `{ success: true, output, error: Some }`. The dispatcher shows `output`
    /// on success, but the banner must land in BOTH fields so it survives
    /// regardless of which the model reads. Exercises the dual-field branch.
    #[tokio::test]
    async fn shell_warns_on_ephemeral_success_with_stderr() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime())
            .with_persistent_writes(false);
        let result = tool
            .execute(json!({"command": success_with_stderr_command()}))
            .await
            .expect("command should run");
        assert!(
            result.success,
            "command should exit 0, got error: {:?}",
            result.error
        );
        assert!(
            result.output.contains("EPHEMERAL WORKSPACE") && result.output.contains("out"),
            "output must carry banner and preserve stdout, got: {}",
            result.output
        );
        let err = result.error.as_deref().unwrap_or("");
        assert!(
            err.contains("EPHEMERAL WORKSPACE") && err.contains("warn"),
            "error must carry banner and preserve stderr, got: {err:?}"
        );
    }

    /// On a persistent runtime (the default) no warning is attached.
    #[tokio::test]
    async fn shell_no_warning_when_persistent() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command should run");
        assert!(result.success);
        assert!(
            !result.output.contains("EPHEMERAL WORKSPACE"),
            "no ephemeral warning expected on a persistent runtime, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": format!("cat {}", absolute_path_outside_workspace())}))
            .await
            .expect("absolute path argument should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": format!("grep --file={} root ./src", absolute_path_outside_workspace())}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": format!("grep -f{} root ./src", absolute_path_outside_workspace())}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = wrapped_shell(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![env_print_command().into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec![env_print_command().into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    #[cfg(target_os = "windows")]
    fn env_print_command() -> &'static str {
        "set"
    }

    #[cfg(not(target_os = "windows"))]
    fn env_print_command() -> &'static str {
        "env"
    }

    #[cfg(target_os = "windows")]
    fn home_env_key() -> &'static str {
        "USERPROFILE"
    }

    #[cfg(not(target_os = "windows"))]
    fn home_env_key() -> &'static str {
        "HOME"
    }

    #[cfg(target_os = "windows")]
    fn absolute_path_outside_workspace() -> &'static str {
        r"C:\Windows\win.ini"
    }

    #[cfg(not(target_os = "windows"))]
    fn absolute_path_outside_workspace() -> &'static str {
        "/etc/passwd"
    }

    fn env_output_contains_key(output: &str, key: &str) -> bool {
        output.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(name, _)| env_key_eq(name, key))
        })
    }

    fn env_output_contains_assignment(output: &str, key: &str, value: &str) -> bool {
        output.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(name, actual)| env_key_eq(name, key) && actual == value)
        })
    }

    #[cfg(target_os = "windows")]
    fn env_key_eq(actual: &str, expected: &str) -> bool {
        actual.eq_ignore_ascii_case(expected)
    }

    #[cfg(not(target_os = "windows"))]
    fn env_key_eq(actual: &str, expected: &str) -> bool {
        actual == expected
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: test-only, single-threaded test runner.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                // SAFETY: test-only, single-threaded test runner.
                Some(val) => unsafe { std::env::set_var(self.key, val) },
                // SAFETY: test-only, single-threaded test runner.
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("ZEROCLAW_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "ZEROCLAW_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(
            env_output_contains_key(&result.output, home_env_key()),
            "{} should be available in shell environment",
            home_env_key()
        );
        assert!(
            env_output_contains_key(&result.output, "PATH"),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("ZEROCLAW_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["ZEROCLAW_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");
        assert!(result.success);
        assert!(env_output_contains_assignment(
            &result.output,
            "ZEROCLAW_TEST_PASSTHROUGH",
            "db://unit-test"
        ));
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec![medium_risk_write_base().into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let denied = tool
            .execute(json!({"command": medium_risk_write_command()}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(
            denied
                .error
                .as_deref()
                .unwrap_or("")
                .contains("explicit approval")
        );

        let allowed = tool
            .execute(json!({
                "command": medium_risk_write_command(),
                "approved": true
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success);

        let _ =
            tokio::fs::remove_file(std::env::temp_dir().join("zeroclaw_shell_approval_test")).await;
    }

    // ── shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_can_be_overridden() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_timeout_secs(120);
        assert_eq!(tool.timeout_secs, 120);
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    // ── Non-UTF8 binary output tests ────────────────────

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn decode_output_valid_utf8_roundtrips() {
        let input = "hello 世界 🌍".as_bytes();
        assert_eq!(super::decode_output(input), "hello 世界 🌍");
    }

    #[test]
    fn decode_output_invalid_utf8_uses_replacement_chars() {
        // 0xFF is not valid UTF-8
        let input = b"hello\xFF world";
        let result = super::decode_output(input);
        // Must not panic; non-UTF-8 bytes become replacement characters on non-Windows
        assert!(result.contains("hello"));
        assert!(result.contains("world"));
    }

    #[test]
    fn decode_output_empty_bytes_returns_empty_string() {
        assert_eq!(super::decode_output(b""), "");
    }

    #[test]
    fn windows_code_page_mapping_covers_cjk() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(936), encoding_rs::GBK);
        assert_eq!(windows_code_page_to_encoding(932), encoding_rs::SHIFT_JIS);
        assert_eq!(windows_code_page_to_encoding(949), encoding_rs::EUC_KR);
        assert_eq!(windows_code_page_to_encoding(950), encoding_rs::BIG5);
    }

    #[test]
    fn windows_code_page_mapping_utf8_variants() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(65001), encoding_rs::UTF_8);
        assert_eq!(windows_code_page_to_encoding(20127), encoding_rs::UTF_8);
    }

    #[test]
    fn windows_code_page_mapping_unknown_falls_back_to_utf8() {
        use super::windows_code_page_to_encoding;
        assert_eq!(windows_code_page_to_encoding(99999), encoding_rs::UTF_8);
    }

    #[test]
    fn decode_output_with_cp936_gbk_bytes_transcodes_to_utf8() {
        // GBK encoding of "你好" is [0xC4, 0xE3, 0xBA, 0xC3]
        let gbk_bytes: &[u8] = &[0xC4, 0xE3, 0xBA, 0xC3];
        let decoded = super::decode_output_with_code_page(gbk_bytes, 936);
        assert_eq!(decoded, "你好");
        assert!(!decoded.contains('\u{FFFD}'));
    }

    #[test]
    fn shell_safe_env_vars_excludes_secrets() {
        for var in SAFE_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn shell_safe_env_vars_includes_essentials() {
        assert!(
            SAFE_ENV_VARS.contains(&"PATH"),
            "PATH must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"HOME") || SAFE_ENV_VARS.contains(&"USERPROFILE"),
            "HOME or USERPROFILE must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"TERM"),
            "TERM must be in safe env vars"
        );
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "echo error_msg >&2"}))
            .await
            .unwrap();
        assert!(result.error.as_deref().unwrap_or("").contains("error_msg"));
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = wrapped_shell(security);

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    // ── Sandbox integration tests ────────────────────────

    #[test]
    fn shell_tool_can_be_constructed_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn noop_sandbox_does_not_modify_command() {
        use crate::security::NoopSandbox;

        let sandbox = NoopSandbox;
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("hello");

        let program_before = cmd.get_program().to_os_string();
        let args_before: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();

        sandbox
            .wrap_command(&mut cmd)
            .expect("wrap_command should succeed");

        assert_eq!(cmd.get_program(), program_before);
        assert_eq!(
            cmd.get_args().map(|a| a.to_os_string()).collect::<Vec<_>>(),
            args_before
        );
    }

    #[tokio::test]
    async fn shell_executes_with_sandbox() {
        use crate::security::NoopSandbox;

        let sandbox: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        let tool = ShellTool::new_with_sandbox(
            test_security(AutonomyLevel::Supervised),
            test_runtime(),
            sandbox,
        );
        let result = tool
            .execute(json!({"command": "echo sandbox_test"}))
            .await
            .expect("command with sandbox should succeed");
        assert!(result.success);
        assert!(result.output.contains("sandbox_test"));
    }

    // ── TUI env overlay tests ─────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_is_passed_to_subprocess() {
        // A var that is NOT in SAFE_ENV_VARS and NOT in passthrough —
        // it should only appear if tui_env injects it.
        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert("ZC_TUI_TEST_VAR".to_string(), "tui_injected".to_string());
                m
            }));

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            env_output_contains_assignment(&result.output, "ZC_TUI_TEST_VAR", "tui_injected"),
            "tui_env var should appear in subprocess env, got:\n{}",
            result.output
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_without_tui_env_does_not_inject_extra_vars() {
        // Without tui_env, a non-safe var must NOT appear.
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            !result.output.contains("ZC_TUI_TEST_VAR"),
            "non-safe var must not leak without tui_env"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_overrides_safe_var() {
        // tui_env wins over the process-level value for a var that is also in SAFE_ENV_VARS.
        // This lets the TUI's PATH (e.g. with nix/brew) win over the daemon's PATH.
        let home_key = home_env_key();
        let _guard = EnvGuard::set(home_key, "daemon-home");

        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert(home_key.to_string(), "tui-home".to_string());
                m
            }));

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(
            result.success,
            "env should succeed, got output={:?} error={:?}",
            result.output, result.error
        );
        assert!(
            env_output_contains_assignment(&result.output, home_key, "tui-home"),
            "tui_env {home_key} should override daemon {home_key}, got:\n{}",
            result.output
        );
        assert!(
            !env_output_contains_assignment(&result.output, home_key, "daemon-home"),
            "daemon {home_key} must not leak through when tui_env overrides it, got:\n{}",
            result.output
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_none_behaves_like_existing() {
        // with_tui_env(None) must be identical to no tui_env at all —
        // only SAFE_ENV_VARS + passthrough reach the subprocess.
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(None);

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            !result.output.contains("ZC_TUI_TEST_VAR"),
            "None tui_env must not inject anything extra"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_tui_env_secrets_reach_subprocess_but_not_safe_list() {
        // The whole point: secrets from the TUI env (e.g. SSH_AUTH_SOCK)
        // DO reach the subprocess via tui_env even though they are not
        // in SAFE_ENV_VARS.
        let tool =
            ShellTool::new(test_security_with_env_cmd(), test_runtime()).with_tui_env(Some({
                let mut m = std::collections::HashMap::new();
                m.insert("SSH_AUTH_SOCK".to_string(), "/tmp/fake.sock".to_string());
                m
            }));

        // Confirm SSH_AUTH_SOCK is not in the safe list (would be a bug if it were)
        assert!(
            !SAFE_ENV_VARS.contains(&"SSH_AUTH_SOCK"),
            "SSH_AUTH_SOCK must not be in SAFE_ENV_VARS"
        );

        let result = tool
            .execute(json!({"command": env_print_command()}))
            .await
            .expect("environment print command should succeed");

        assert!(result.success);
        assert!(
            env_output_contains_assignment(&result.output, "SSH_AUTH_SOCK", "/tmp/fake.sock"),
            "SSH_AUTH_SOCK from tui_env must reach subprocess"
        );
    }
}
