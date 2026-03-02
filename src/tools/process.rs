use super::shell::collect_allowed_shell_env_vars;
use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use crate::security::SyscallAnomalyDetector;
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::io::AsyncReadExt;

/// Maximum output bytes kept per stream (stdout/stderr): 512KB.
const MAX_OUTPUT_BYTES: usize = 524_288;

/// Maximum concurrent background processes.
const MAX_PROCESSES: usize = 8;

#[derive(Debug, Default, Clone)]
struct OutputBuffer {
    data: String,
    dropped_prefix_bytes: u64,
}

struct ProcessEntry {
    id: usize,
    command: String,
    pid: u32,
    started_at: Instant,
    child: Mutex<tokio::process::Child>,
    stdout_buf: Arc<Mutex<OutputBuffer>>,
    stderr_buf: Arc<Mutex<OutputBuffer>>,
    analyzed_offsets: Mutex<(u64, u64)>,
}

/// Background process management tool.
///
/// Allows the agent to spawn long-running commands, check their output,
/// and terminate them. Complements the synchronous `ShellTool` for commands
/// that need to run beyond the 60-second shell timeout.
pub struct ProcessTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
    processes: Arc<RwLock<HashMap<usize, ProcessEntry>>>,
    next_id: Mutex<usize>,
}

impl ProcessTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self::new_with_syscall_detector(security, runtime, None)
    }

    pub fn new_with_syscall_detector(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
    ) -> Self {
        Self {
            security,
            runtime,
            syscall_detector,
            processes: Arc::new(RwLock::new(HashMap::new())),
            next_id: Mutex::new(0),
        }
    }

    fn handle_spawn(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.runtime.supports_long_running() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Runtime does not support long-running processes".into()),
            });
        }

        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter for spawn action"))?;

        // Check concurrent running process count.
        {
            let processes =
                recover_rwlock_read(&self.processes, "process registry read during spawn");
            let running = processes
                .values()
                .filter(|entry| {
                    let mut child = recover_mutex_lock(
                        &entry.child,
                        "process child state during spawn running-count",
                    );
                    match child.try_wait() {
                        Ok(None) => true,
                        Ok(Some(_)) => false,
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                "Failed to query child status during spawn limit check; treating as running"
                            );
                            true
                        }
                    }
                })
                .count();
            if running >= MAX_PROCESSES {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Maximum concurrent processes ({MAX_PROCESSES}) reached"
                    )),
                });
            }
        }

        // Reuse shell security chain: rate limit → command validation → path check → record.
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if let Err(reason) = self.security.validate_command_execution(command, approved) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(reason),
            });
        }

        if let Some(path) = self.security.forbidden_path_argument(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Build command via runtime adapter.
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

        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to spawn process: {e}")),
                });
            }
        };

        let pid = child.id().unwrap_or(0);

        // Set up background output readers.
        let stdout_buf = Arc::new(Mutex::new(OutputBuffer::default()));
        let stderr_buf = Arc::new(Mutex::new(OutputBuffer::default()));

        if let Some(stdout) = child.stdout.take() {
            spawn_reader_task(stdout, stdout_buf.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader_task(stderr, stderr_buf.clone());
        }

        let id = {
            let mut next = recover_mutex_lock(&self.next_id, "next process id allocation");
            let id = *next;
            *next += 1;
            id
        };

        let entry = ProcessEntry {
            id,
            command: command.to_string(),
            pid,
            started_at: Instant::now(),
            child: Mutex::new(child),
            stdout_buf,
            stderr_buf,
            analyzed_offsets: Mutex::new((0, 0)),
        };

        recover_rwlock_write(&self.processes, "process registry insert").insert(id, entry);

        Ok(ToolResult {
            success: true,
            output: json!({
                "id": id,
                "pid": pid,
                "message": format!("Process started: {command}")
            })
            .to_string(),
            error: None,
        })
    }

    fn handle_list(&self) -> anyhow::Result<ToolResult> {
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "process")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
            });
        }

        let processes = recover_rwlock_read(&self.processes, "process registry list");
        let mut entries = Vec::new();

        for entry in processes.values() {
            let status = {
                let mut child = recover_mutex_lock(&entry.child, "process child state during list");
                match child.try_wait() {
                    Ok(Some(status)) => {
                        format!("exited ({})", status.code().unwrap_or(-1))
                    }
                    Ok(None) => "running".to_string(),
                    Err(e) => format!("error: {e}"),
                }
            };

            entries.push(json!({
                "id": entry.id,
                "command": entry.command,
                "pid": entry.pid,
                "status": status,
                "uptime_secs": entry.started_at.elapsed().as_secs(),
            }));
        }

        match serde_json::to_string_pretty(&entries) {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to serialize process list: {e}")),
            }),
        }
    }

    fn handle_output(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Read, "process")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
            });
        }

        let id = parse_id(args, "output")?;

        let processes = recover_rwlock_read(&self.processes, "process registry output lookup");
        let entry = match processes.get(&id) {
            Some(e) => e,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("No process with id {id}")),
                });
            }
        };

        let stdout_snapshot = snapshot_output_buffer(&entry.stdout_buf);
        let stderr_snapshot = snapshot_output_buffer(&entry.stderr_buf);
        let stdout = stdout_snapshot.data;
        let stderr = stderr_snapshot.data;

        if let Some(detector) = &self.syscall_detector {
            let mut offsets = recover_mutex_lock(&entry.analyzed_offsets, "process output offsets");
            let stdout_delta = slice_unseen_output(
                &stdout,
                stdout_snapshot.dropped_prefix_bytes,
                &mut offsets.0,
            );
            let stderr_delta = slice_unseen_output(
                &stderr,
                stderr_snapshot.dropped_prefix_bytes,
                &mut offsets.1,
            );

            if !stdout_delta.is_empty() || !stderr_delta.is_empty() {
                let _ = detector.inspect_command_output(
                    &entry.command,
                    stdout_delta,
                    stderr_delta,
                    None,
                );
            }
        }

        Ok(ToolResult {
            success: true,
            output: json!({
                "stdout": stdout,
                "stderr": stderr,
            })
            .to_string(),
            error: None,
        })
    }

    fn handle_kill(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "process")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e),
            });
        }

        let id = parse_id(args, "kill")?;

        let pid = {
            let processes = recover_rwlock_read(&self.processes, "process registry kill lookup");
            match processes.get(&id) {
                Some(entry) => entry.pid,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("No process with id {id}")),
                    });
                }
            }
        };

        // Send SIGTERM via kill command.
        let kill_result = std::process::Command::new("kill")
            .arg(pid.to_string())
            .output();

        match kill_result {
            Ok(output) if output.status.success() => Ok(ToolResult {
                success: true,
                output: format!("Sent SIGTERM to process {id} (pid {pid})"),
                error: None,
            }),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to kill process {id} (pid {pid}): {stderr}")),
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute kill command: {e}")),
            }),
        }
    }
}

/// Parse the `id` field from action args, returning a usize.
fn parse_id(args: &serde_json::Value, action: &str) -> anyhow::Result<usize> {
    args.get("id")
        .and_then(|v| v.as_u64())
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| anyhow::anyhow!("Missing 'id' parameter for {action} action"))
}

fn recover_mutex_lock<'a, T>(
    mutex: &'a Mutex<T>,
    context: &'static str,
) -> std::sync::MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(context = context, "Poisoned mutex lock recovered");
            mutex.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn recover_rwlock_read<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> std::sync::RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(context = context, "Poisoned RwLock read recovered");
            lock.clear_poison();
            poisoned.into_inner()
        }
    }
}

fn recover_rwlock_write<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> std::sync::RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!(context = context, "Poisoned RwLock write recovered");
            lock.clear_poison();
            poisoned.into_inner()
        }
    }
}

/// Append data to a bounded buffer, draining oldest bytes when over limit.
fn append_bounded(buf: &Mutex<OutputBuffer>, new_data: &str) {
    let mut guard = recover_mutex_lock(buf, "output buffer append");
    guard.data.push_str(new_data);
    if guard.data.len() > MAX_OUTPUT_BYTES {
        let excess = guard.data.len() - MAX_OUTPUT_BYTES;
        // Find the first valid char boundary at or after `excess`.
        let mut drain_to = excess;
        while drain_to < guard.data.len() && !guard.data.is_char_boundary(drain_to) {
            drain_to += 1;
        }
        guard.data.drain(..drain_to);
        guard.dropped_prefix_bytes = guard
            .dropped_prefix_bytes
            .saturating_add(u64::try_from(drain_to).unwrap_or(u64::MAX));
    }
}

/// Spawn a background task that reads from an async reader into a bounded buffer.
fn spawn_reader_task<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    buf: Arc<Mutex<OutputBuffer>>,
) {
    tokio::spawn(async move {
        let mut chunk = vec![0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(n) if n > 0 => {
                    let text = String::from_utf8_lossy(&chunk[..n]);
                    append_bounded(&buf, &text);
                }
                _ => break,
            }
        }
    });
}

fn snapshot_output_buffer(buf: &Mutex<OutputBuffer>) -> OutputBuffer {
    recover_mutex_lock(buf, "output buffer snapshot").clone()
}

fn slice_unseen_output<'a>(
    current: &'a str,
    dropped_prefix_bytes: u64,
    analyzed: &mut u64,
) -> &'a str {
    let len_u64 = u64::try_from(current.len()).unwrap_or(u64::MAX);
    let available_end = dropped_prefix_bytes.saturating_add(len_u64);

    let start = if *analyzed <= dropped_prefix_bytes {
        0
    } else {
        usize::try_from(analyzed.saturating_sub(dropped_prefix_bytes))
            .unwrap_or(current.len())
            .min(current.len())
    };

    let mut boundary = start;
    while boundary < current.len() && !current.is_char_boundary(boundary) {
        boundary += 1;
    }

    *analyzed = available_end;
    &current[boundary..]
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Manage background processes: spawn long-running commands, check output, and terminate them"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["spawn", "list", "output", "kill"],
                    "description": "Action to perform: spawn a process, list all, get output, or kill"
                },
                "command": {
                    "type": "string",
                    "description": "Shell command to run in background (required for 'spawn')"
                },
                "id": {
                    "type": "integer",
                    "description": "Process ID returned by spawn (required for 'output' and 'kill')"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Approve medium/high-risk commands (for 'spawn')",
                    "default": false
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

        match action {
            "spawn" => self.handle_spawn(&args),
            "list" => self.handle_list(),
            "output" => self.handle_output(&args),
            "kill" => self.handle_kill(&args),
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Use: spawn, list, output, kill"
                )),
            }),
        }
    }
}

impl Drop for ProcessTool {
    fn drop(&mut self) {
        let processes = recover_rwlock_read(&self.processes, "process registry drop cleanup");
        for entry in processes.values() {
            let mut child = recover_mutex_lock(&entry.child, "process child lock during drop");
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuditConfig, SyscallAnomalyConfig};
    use crate::runtime::NativeRuntime;
    use crate::security::{AutonomyLevel, SecurityPolicy, SyscallAnomalyDetector};
    use std::panic::AssertUnwindSafe;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        let mut policy = SecurityPolicy::default();
        policy.autonomy = AutonomyLevel::Full;
        policy.workspace_dir = std::env::temp_dir();
        policy.allowed_commands.push("sleep".into());
        Arc::new(policy)
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    fn test_syscall_detector(tmp: &TempDir) -> Arc<SyscallAnomalyDetector> {
        let log_path = tmp.path().join("process-syscall-anomalies.log");
        let cfg = SyscallAnomalyConfig {
            baseline_syscalls: vec!["read".into(), "write".into()],
            log_path: log_path.to_string_lossy().to_string(),
            alert_cooldown_secs: 1,
            max_alerts_per_minute: 50,
            ..SyscallAnomalyConfig::default()
        };
        let audit = AuditConfig {
            enabled: false,
            ..AuditConfig::default()
        };
        Arc::new(SyscallAnomalyDetector::new(cfg, tmp.path(), audit))
    }

    fn make_tool() -> ProcessTool {
        ProcessTool::new(test_security(), test_runtime())
    }

    #[test]
    fn process_tool_name() {
        assert_eq!(make_tool().name(), "process");
    }

    #[test]
    fn process_tool_description_not_empty() {
        assert!(!make_tool().description().is_empty());
    }

    #[test]
    fn process_tool_schema_has_action() {
        let schema = make_tool().parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("action")));
    }

    #[test]
    fn constants_are_correct() {
        assert_eq!(MAX_OUTPUT_BYTES, 524_288);
        assert_eq!(MAX_PROCESSES, 8);
    }

    #[tokio::test]
    async fn spawn_starts_background_process() {
        let tool = make_tool();
        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo hello_process_test"
            }))
            .await
            .unwrap();
        assert!(result.success, "spawn should succeed: {:?}", result.error);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(output["id"].is_number());
        assert!(output["pid"].is_number());
    }

    #[tokio::test]
    async fn list_shows_spawned_process() {
        let tool = make_tool();
        tool.execute(json!({
            "action": "spawn",
            "command": "echo list_test"
        }))
        .await
        .unwrap();

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("list_test"));
    }

    #[tokio::test]
    async fn output_returns_stdout() {
        let tool = make_tool();
        let spawn_result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo output_capture_test"
            }))
            .await
            .unwrap();

        let spawn_output: serde_json::Value = serde_json::from_str(&spawn_result.output).unwrap();
        let id = spawn_output["id"].as_u64().unwrap();

        // Wait for the process to finish and output to be captured.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let result = tool
            .execute(json!({
                "action": "output",
                "id": id
            }))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("output_capture_test"));
    }

    #[tokio::test]
    async fn kill_terminates_process() {
        let tool = make_tool();
        let spawn_result = tool
            .execute(json!({
                "action": "spawn",
                "command": "sleep 60"
            }))
            .await
            .unwrap();
        assert!(spawn_result.success);

        let spawn_output: serde_json::Value = serde_json::from_str(&spawn_result.output).unwrap();
        let id = spawn_output["id"].as_u64().unwrap();

        let kill_result = tool
            .execute(json!({
                "action": "kill",
                "id": id
            }))
            .await
            .unwrap();
        assert!(kill_result.success);
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let tool = make_tool();
        let result = tool.execute(json!({"action": "restart"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn spawn_blocks_disallowed_command() {
        let tool = make_tool();
        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "rm -rf /"
            }))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn spawn_blocks_forbidden_path() {
        let tool = make_tool();
        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "cat /etc/passwd"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Path blocked"));
    }

    #[tokio::test]
    async fn kill_blocks_readonly() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ProcessTool::new(security, test_runtime());
        let result = tool
            .execute(json!({
                "action": "kill",
                "id": 0
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn output_missing_id_returns_error() {
        let tool = make_tool();
        let result = tool.execute(json!({"action": "output"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn output_nonexistent_id_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(json!({
                "action": "output",
                "id": 9999
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("No process"));
    }

    #[tokio::test]
    async fn spawn_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ProcessTool::new(security, test_runtime());
        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo test"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Rate limit"));
    }

    struct NoLongRunningRuntime;

    impl RuntimeAdapter for NoLongRunningRuntime {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn name(&self) -> &str {
            "test-restricted"
        }
        fn has_shell_access(&self) -> bool {
            true
        }
        fn has_filesystem_access(&self) -> bool {
            true
        }
        fn storage_path(&self) -> PathBuf {
            PathBuf::from("/tmp")
        }
        fn supports_long_running(&self) -> bool {
            false
        }
        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(cmd)
        }
    }

    #[tokio::test]
    async fn spawn_rejects_when_runtime_unsupported() {
        let tool = ProcessTool::new(test_security(), Arc::new(NoLongRunningRuntime));
        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo test"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("long-running"));
    }

    #[test]
    fn append_bounded_truncates_old_data() {
        let buf = Mutex::new(OutputBuffer::default());
        let data = "x".repeat(MAX_OUTPUT_BYTES + 100);
        append_bounded(&buf, &data);
        let guard = buf.lock().unwrap();
        assert!(guard.data.len() <= MAX_OUTPUT_BYTES);
        assert!(guard.dropped_prefix_bytes >= 100);
    }

    #[test]
    fn append_bounded_recovers_from_poisoned_lock() {
        let buf = Mutex::new(OutputBuffer::default());
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = buf.lock().expect("buffer lock should be available");
            panic!("poison output buffer mutex");
        }));

        append_bounded(&buf, "hello");
        assert!(
            buf.lock().is_ok(),
            "output buffer mutex poison should be cleared"
        );
        let guard = buf
            .lock()
            .expect("output buffer lock should be available after recovery");
        assert!(guard.data.contains("hello"));
    }

    #[test]
    fn snapshot_output_buffer_recovers_from_poisoned_lock() {
        let buf = Mutex::new(OutputBuffer {
            data: "seed".to_string(),
            dropped_prefix_bytes: 0,
        });
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = buf.lock().expect("buffer lock should be available");
            panic!("poison output buffer mutex");
        }));

        let snapshot = snapshot_output_buffer(&buf);
        assert!(
            buf.lock().is_ok(),
            "output buffer mutex poison should be cleared"
        );
        assert_eq!(snapshot.data, "seed");
    }

    #[tokio::test]
    async fn spawn_recovers_from_poisoned_next_id_lock() {
        let tool = make_tool();
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = tool
                .next_id
                .lock()
                .expect("next_id lock should be available");
            panic!("poison next_id mutex");
        }));

        let result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo recovered_spawn"
            }))
            .await
            .expect("spawn should return ToolResult instead of panicking");

        assert!(result.success, "spawn should recover: {:?}", result.error);
        assert!(
            tool.next_id.lock().is_ok(),
            "next_id mutex poison should be cleared"
        );
    }

    #[tokio::test]
    async fn list_recovers_from_poisoned_processes_lock() {
        let tool = make_tool();
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = tool
                .processes
                .write()
                .expect("processes lock should be available");
            panic!("poison processes lock");
        }));

        let result = tool
            .execute(json!({"action": "list"}))
            .await
            .expect("list should return ToolResult instead of panicking");

        assert!(result.success, "list should recover: {:?}", result.error);
        assert!(
            tool.processes.read().is_ok(),
            "processes lock poison should be cleared"
        );
    }

    #[tokio::test]
    async fn output_recovers_from_poisoned_offsets_lock() {
        let tmp = tempfile::tempdir().expect("temp dir should be created");
        let tool = ProcessTool::new_with_syscall_detector(
            test_security(),
            test_runtime(),
            Some(test_syscall_detector(&tmp)),
        );
        let spawn_result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo poison_offsets"
            }))
            .await
            .expect("spawn should return result");
        assert!(spawn_result.success);

        let spawn_output: serde_json::Value =
            serde_json::from_str(&spawn_result.output).expect("spawn output should be json");
        let id = spawn_output["id"]
            .as_u64()
            .expect("process id should exist in spawn output");
        let id = usize::try_from(id).expect("process id should fit in usize");

        {
            let processes = tool
                .processes
                .read()
                .expect("processes lock should be available");
            let entry = processes.get(&id).expect("spawned entry should exist");
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _guard = entry
                    .analyzed_offsets
                    .lock()
                    .expect("offsets lock should be available");
                panic!("poison analyzed_offsets lock");
            }));
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut result = tool
            .execute(json!({"action": "output", "id": id}))
            .await
            .expect("output should return ToolResult instead of panicking");
        while result.success
            && !result.output.contains("poison_offsets")
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            result = tool
                .execute(json!({"action": "output", "id": id}))
                .await
                .expect("output should return ToolResult instead of panicking");
        }
        assert!(result.success, "output should recover: {:?}", result.error);
        assert!(
            result.output.contains("poison_offsets"),
            "timed out waiting for captured output: {:?}",
            result.error
        );
        let processes = tool
            .processes
            .read()
            .expect("processes lock should be available after recovery");
        let entry = processes
            .get(&id)
            .expect("spawned entry should still exist for poison assertion");
        assert!(
            entry.analyzed_offsets.lock().is_ok(),
            "analyzed_offsets mutex poison should be cleared"
        );
    }

    #[test]
    fn slice_unseen_output_tracks_new_tail_after_rollover() {
        let mut analyzed = u64::try_from(MAX_OUTPUT_BYTES).expect("size should fit in u64");
        let current = format!("{}tail", "x".repeat(MAX_OUTPUT_BYTES.saturating_sub(4)));
        let dropped = 4_u64;

        let delta = slice_unseen_output(&current, dropped, &mut analyzed);

        assert_eq!(delta, "tail");
        assert_eq!(
            analyzed,
            dropped.saturating_add(u64::try_from(current.len()).expect("len should fit in u64"))
        );
    }

    #[tokio::test]
    async fn process_output_runs_syscall_detector_incrementally() {
        let tmp = tempfile::tempdir().expect("temp dir should be created");
        let log_path = tmp.path().join("process-syscall-anomalies.log");
        let tool = ProcessTool::new_with_syscall_detector(
            test_security(),
            test_runtime(),
            Some(test_syscall_detector(&tmp)),
        );

        let spawn_result = tool
            .execute(json!({
                "action": "spawn",
                "command": "echo seccomp denied syscall=openat"
            }))
            .await
            .expect("spawn should return result");
        assert!(spawn_result.success);
        let spawn_output: serde_json::Value =
            serde_json::from_str(&spawn_result.output).expect("spawn output should be json");
        let id = spawn_output["id"]
            .as_u64()
            .expect("process id should exist");

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let first_output = tool
            .execute(json!({"action": "output", "id": id}))
            .await
            .expect("first output should return result");
        assert!(first_output.success);

        let first_log = tokio::fs::read_to_string(&log_path)
            .await
            .expect("first anomaly log should exist");
        let first_lines = first_log.lines().count();
        assert!(first_lines >= 1);
        assert!(first_log.contains("\"kind\":\"unknown_syscall\""));

        let second_output = tool
            .execute(json!({"action": "output", "id": id}))
            .await
            .expect("second output should return result");
        assert!(second_output.success);

        let second_log = tokio::fs::read_to_string(&log_path)
            .await
            .expect("second anomaly log should still exist");
        let second_lines = second_log.lines().count();
        assert_eq!(
            second_lines, first_lines,
            "incremental offsets should prevent duplicate detector emissions for unchanged output"
        );
    }
}
