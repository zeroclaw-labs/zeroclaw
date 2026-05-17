//! Shell-based tool derived from a skill's `[[tools]]` section.
//!
//! Each `SkillTool` with `kind = "shell"` or `kind = "script"` is converted
//! into a `SkillShellTool` that implements the `Tool` trait. The tool name is
//! prefixed with the skill name (e.g. `my_skill.run_lint`) to avoid collisions
//! with built-in tools.

use crate::security::SecurityPolicy;
use async_trait::async_trait;
#[cfg(unix)]
use libc;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Windows Job Object wrapper for process-tree cleanup on timeout.
///
/// Opens a handle to the child via `OpenProcess`, assigns it to a new Job
/// Object, then closes the process handle (the job holds its own reference).
/// On timeout, `terminate()` kills every process in the tree; the job handle
/// is closed automatically via `Drop` on both the happy path and timeout.
#[cfg(windows)]
mod win_job {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

    pub struct WinJob(HANDLE);

    // SAFETY: Job Object HANDLEs are valid to use from any thread.
    unsafe impl Send for WinJob {}
    unsafe impl Sync for WinJob {}

    impl WinJob {
        /// Open a handle to `pid`, create a Job Object, assign the process to
        /// it, then close the process handle.  Returns `None` if any Win32 call
        /// fails (graceful fallback — timeout still fires, orphans may survive).
        pub fn from_pid(pid: u32) -> Option<Self> {
            let proc = unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, pid) }.ok()?;
            let job = match unsafe { CreateJobObjectW(None, None) } {
                Ok(j) => j,
                Err(_) => {
                    let _ = unsafe { CloseHandle(proc) };
                    return None;
                }
            };
            let ok = unsafe { AssignProcessToJobObject(job, proc) }.is_ok();
            let _ = unsafe { CloseHandle(proc) };
            if ok {
                Some(WinJob(job))
            } else {
                let _ = unsafe { CloseHandle(job) };
                None
            }
        }

        /// Send `TerminateJobObject` to kill the entire process tree.
        pub fn terminate(&self) {
            let _ = unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for WinJob {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// Default execution time for a skill shell command (seconds).
const SKILL_SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1 MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// A tool derived from a skill's `[[tools]]` section that executes shell commands.
pub struct SkillShellTool {
    tool_name: String,
    tool_description: String,
    command_template: String,
    args: HashMap<String, String>,
    timeout_secs: u64,
    security: Arc<SecurityPolicy>,
}

impl SkillShellTool {
    /// Create a new skill shell tool.
    ///
    /// The tool name is prefixed with the skill name (`skill_name.tool_name`)
    /// to prevent collisions with built-in tools.
    pub fn new(
        skill_name: &str,
        tool: &crate::skills::SkillTool,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            tool_name: format!("{}.{}", skill_name, tool.name),
            tool_description: tool.description.clone(),
            command_template: tool.command.clone(),
            args: tool.args.clone(),
            timeout_secs: tool.timeout_secs.unwrap_or(SKILL_SHELL_TIMEOUT_SECS).max(1),
            security,
        }
    }

    fn build_parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for (name, description) in &self.args {
            properties.insert(
                name.clone(),
                serde_json::json!({
                    "type": "string",
                    "description": description
                }),
            );
            required.push(serde_json::Value::String(name.clone()));
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required
        })
    }

    /// Substitute `{{arg_name}}` placeholders in the command template with
    /// the provided argument values. Unknown placeholders are left as-is.
    fn substitute_args(&self, args: &serde_json::Value) -> String {
        let mut command = self.command_template.clone();
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let placeholder = format!("{{{{{}}}}}", key);
                let replacement = value.as_str().unwrap_or_default();
                command = command.replace(&placeholder, replacement);
            }
        }
        command
    }
}

#[async_trait]
impl Tool for SkillShellTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        &self.tool_description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.build_parameters_schema()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = self.substitute_args(&args);

        // Rate limiting is applied by the RateLimitedTool wrapper at
        // registration time (see zeroclaw-runtime::tools::mod). The
        // PathGuardedTool wrapper cannot inspect the substituted command
        // built by substitute_args, so the forbidden_path_argument check
        // below remains tool-local.

        // Security validation — always requires explicit approval (approved=true)
        // since skill tools are user-defined and should be treated as medium-risk.
        match self.security.validate_command_execution(&command, true) {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        if let Some(path) = self.security.forbidden_path_argument(&command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        // Build and execute the command
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&command);
        cmd.current_dir(&self.security.workspace_dir);
        cmd.env_clear();
        // Pipe stdout/stderr so wait_with_output() captures them (unlike
        // output(), spawn() does not set up pipes automatically).
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // On Unix, run the shell in its own process group so a SIGKILL on
        // timeout reaches grandchildren too. On Windows a Job Object (created
        // after spawn) provides the same guarantee. Other platforms fall back
        // to kill_on_drop which only kills the direct child.
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(not(any(unix, windows)))]
        cmd.kill_on_drop(true);

        // Only pass safe environment variables
        for var in &[
            "PATH", "HOME", "TERM", "LANG", "LC_ALL", "USER", "SHELL", "TMPDIR",
        ] {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to execute command: {e}")),
                });
            }
        };
        // Capture platform-specific process-tree kill handles before moving
        // child into wait_with_output().
        #[cfg(unix)]
        let pgid = child.id();
        #[cfg(windows)]
        let win_job = child.id().and_then(win_job::WinJob::from_pid);

        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await;

        match result {
            Ok(Ok(output)) => {
                // win_job dropped here on Windows: CloseHandle with no termination.
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

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

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_elapsed) => {
                // Kill the entire process tree before returning the timeout error.
                #[cfg(unix)]
                if let Some(pid) = pgid {
                    // SAFETY: pid is a valid process group we created above;
                    // negative first arg means "send to process group".
                    unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
                }
                #[cfg(windows)]
                if let Some(ref job) = win_job {
                    job.terminate();
                }
                // win_job dropped here on Windows: CloseHandle after termination.
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Command timed out after {}s and was killed",
                        self.timeout_secs
                    )),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use crate::skills::SkillTool;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    /// Security policy for tests that need to run arbitrary shell constructs
    /// (background operators, file redirects) that the default policy blocks.
    /// Only used in process-kill tests where the point is to verify kill
    /// semantics, not policy enforcement.
    #[cfg(unix)]
    fn test_unrestricted_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["*".to_string()],
            block_high_risk_commands: false,
            ..SecurityPolicy::default()
        })
    }

    fn sample_skill_tool() -> SkillTool {
        let mut args = HashMap::new();
        args.insert("file".to_string(), "The file to lint".to_string());
        args.insert(
            "format".to_string(),
            "Output format (json|text)".to_string(),
        );

        SkillTool {
            name: "run_lint".to_string(),
            description: "Run the linter on a file".to_string(),
            kind: "shell".to_string(),
            command: "lint --file {{file}} --format {{format}}".to_string(),
            args,
            timeout_secs: None,
        }
    }

    #[test]
    fn skill_shell_tool_name_is_prefixed() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.name(), "my_skill.run_lint");
    }

    #[test]
    fn skill_shell_tool_description() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        assert_eq!(tool.description(), "Run the linter on a file");
    }

    #[test]
    fn skill_shell_tool_parameters_schema() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["file"].is_object());
        assert_eq!(schema["properties"]["file"]["type"], "string");
        assert!(schema["properties"]["format"].is_object());

        let required = schema["required"]
            .as_array()
            .expect("required should be array");
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn skill_shell_tool_substitute_args() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let result = tool.substitute_args(&serde_json::json!({
            "file": "src/main.rs",
            "format": "json"
        }));
        assert_eq!(result, "lint --file src/main.rs --format json");
    }

    #[test]
    fn skill_shell_tool_substitute_missing_arg() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let result = tool.substitute_args(&serde_json::json!({"file": "test.rs"}));
        // Missing {{format}} placeholder stays in the command
        assert!(result.contains("{{format}}"));
        assert!(result.contains("test.rs"));
    }

    #[test]
    fn skill_shell_tool_empty_args_schema() {
        let st = SkillTool {
            name: "simple".to_string(),
            description: "Simple tool".to_string(),
            kind: "shell".to_string(),
            command: "echo hello".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("s", &st, test_security());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"].as_object().unwrap().is_empty());
        assert!(schema["required"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skill_shell_tool_executes_echo() {
        let st = SkillTool {
            name: "hello".to_string(),
            description: "Say hello".to_string(),
            kind: "shell".to_string(),
            command: "echo hello-skill".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("hello-skill"));
    }

    #[test]
    fn skill_shell_tool_spec_roundtrip() {
        let tool = SkillShellTool::new("my_skill", &sample_skill_tool(), test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "my_skill.run_lint");
        assert_eq!(spec.description, "Run the linter on a file");
        assert_eq!(spec.parameters["type"], "object");
    }

    #[test]
    fn skill_shell_tool_default_timeout() {
        let st = SkillTool {
            name: "t".to_string(),
            description: "".to_string(),
            kind: "shell".to_string(),
            command: "true".to_string(),
            args: HashMap::new(),
            timeout_secs: None,
        };
        let tool = SkillShellTool::new("s", &st, test_security());
        assert_eq!(tool.timeout_secs, SKILL_SHELL_TIMEOUT_SECS);
    }

    #[test]
    fn skill_shell_tool_custom_timeout() {
        let st = SkillTool {
            name: "t".to_string(),
            description: "".to_string(),
            kind: "shell".to_string(),
            command: "true".to_string(),
            args: HashMap::new(),
            timeout_secs: Some(3600),
        };
        let tool = SkillShellTool::new("s", &st, test_security());
        assert_eq!(tool.timeout_secs, 3600);
    }

    #[tokio::test]
    async fn skill_shell_tool_timeout_enforced() {
        let st = SkillTool {
            name: "slow".to_string(),
            description: "".to_string(),
            kind: "shell".to_string(),
            // tail -f /dev/null blocks indefinitely and is allowed by the security
            // policy (tail is in allowed_commands, /dev/null is always permitted).
            command: "tail -f /dev/null".to_string(),
            args: HashMap::new(),
            timeout_secs: Some(1),
        };
        let tool = SkillShellTool::new("test", &st, test_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("timed out"),
            "expected timeout message, got: {err}"
        );
        assert!(
            err.contains("1s"),
            "expected timeout duration in message, got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_shell_tool_timeout_kills_process() {
        let pid_file = std::env::temp_dir().join("zeroclaw_timeout_kills_test.pid");
        let _ = std::fs::remove_file(&pid_file);
        let pid_path = pid_file.display().to_string();

        let st = SkillTool {
            name: "slow".to_string(),
            description: "".to_string(),
            kind: "shell".to_string(),
            // Background a long-running sleep, record its PID, then wait.
            // The sleep is the grandchild; process-group kill must reach it.
            command: format!("sleep 60 & echo $! > {pid_path} && wait"),
            args: HashMap::new(),
            timeout_secs: Some(1),
        };
        let tool = SkillShellTool::new("test", &st, test_unrestricted_security());
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("timed out"),
            "expected timeout error, got: {err}"
        );

        // Brief pause for SIGKILL delivery and OS reaping.
        tokio::time::sleep(Duration::from_millis(250)).await;

        let pid_str = std::fs::read_to_string(&pid_file)
            .expect("shell did not write grandchild PID before timeout");
        let pid: u32 = pid_str.trim().parse().expect("invalid PID in file");

        // /proc/<pid> exists iff the process is still alive.
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "grandchild sleep process {pid} was not killed on timeout"
        );
        let _ = std::fs::remove_file(&pid_file);
    }

    #[test]
    fn skill_shell_tool_zero_timeout_clamped_to_one() {
        let st = SkillTool {
            name: "t".to_string(),
            description: "".to_string(),
            kind: "shell".to_string(),
            command: "true".to_string(),
            args: HashMap::new(),
            timeout_secs: Some(0),
        };
        let tool = SkillShellTool::new("s", &st, test_security());
        assert_eq!(
            tool.timeout_secs, 1,
            "zero timeout_secs must be clamped to 1"
        );
    }
}
