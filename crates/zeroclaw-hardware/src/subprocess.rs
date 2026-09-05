//! SubprocessTool — wraps any external binary as a [`Tool`].

use super::manifest::ToolManifest;
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{Duration, Instant, timeout, timeout_at};
use zeroclaw_api::attribution::{ToolKind, ToolProvenance};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(SubprocessTool, ToolKind::Plugin, ToolProvenance::Extension);

/// Subprocess timeout — kill the child process after this many seconds.
const SUBPROCESS_TIMEOUT_SECS: u64 = 10;

/// Timeout for waiting on child process exit after stdout has been read.
/// Prevents a hung cleanup phase from blocking indefinitely.
const PROCESS_EXIT_TIMEOUT_SECS: u64 = 5;

const STDERR_CAPTURE_BYTES: usize = 512;

#[cfg(not(target_os = "windows"))]
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

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

pub struct SubprocessTool {
    /// Parsed plugin manifest (tool metadata + parameter definitions).
    manifest: ToolManifest,
    /// Resolved absolute path to the entry-point binary.
    binary_path: PathBuf,
    first_output_timeout: Duration,
    process_exit_timeout: Duration,
}

impl SubprocessTool {
    /// Create a new `SubprocessTool` from a manifest and resolved binary path.
    pub fn new(manifest: ToolManifest, binary_path: PathBuf) -> Self {
        Self {
            manifest,
            binary_path,
            first_output_timeout: Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
            process_exit_timeout: Duration::from_secs(PROCESS_EXIT_TIMEOUT_SECS),
        }
    }

    #[cfg(test)]
    fn with_timeouts(
        mut self,
        first_output_timeout: Duration,
        process_exit_timeout: Duration,
    ) -> Self {
        self.first_output_timeout = first_output_timeout;
        self.process_exit_timeout = process_exit_timeout;
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary_path);
        configure_plugin_environment(&mut command);
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        command
    }

    /// Build JSON Schema `properties` and `required` arrays from the manifest.
    fn build_schema_properties(
        &self,
    ) -> (
        serde_json::Map<String, serde_json::Value>,
        Vec<serde_json::Value>,
    ) {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for param in &self.manifest.parameters {
            let mut prop = json!({
                "type": param.r#type,
                "description": param.description,
            });

            if let Some(default) = &param.default {
                prop["default"] = default.clone();
            }

            properties.insert(param.name.clone(), prop);

            if param.required {
                required.push(serde_json::Value::String(param.name.clone()));
            }
        }

        (properties, required)
    }
}

#[async_trait]
impl Tool for SubprocessTool {
    fn name(&self) -> &str {
        &self.manifest.tool.name
    }

    fn description(&self) -> &str {
        &self.manifest.tool.description
    }

    /// JSON Schema Draft 7 — auto-generated from `manifest.parameters`.
    fn parameters_schema(&self) -> serde_json::Value {
        let (properties, required) = self.build_schema_properties();
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let args_json = serde_json::to_string(&args).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": self.manifest.tool.name,
                        "error": format!("{}", e),
                    })),
                "subprocess plugin: failed to serialise tool args"
            );
            anyhow::Error::msg(format!("failed to serialise args: {e}"))
        })?;

        let mut child = self.command().spawn().map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": self.manifest.tool.name,
                        "binary_path": self.binary_path.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "subprocess plugin spawn failed"
            );
            anyhow::Error::msg(format!(
                "failed to spawn plugin '{}' at {}: {e}",
                self.manifest.tool.name,
                self.binary_path.display()
            ))
        })?;

        let Some(stdout) = child.stdout.take() else {
            let cleanup_error = terminate_and_reap(child, self.process_exit_timeout)
                .await
                .into_diagnostic();
            return Ok(failed_tool_result(append_cleanup_diagnostic(
                format!(
                    "plugin '{}': could not attach stdout pipe",
                    self.manifest.tool.name
                ),
                cleanup_error.as_deref(),
            )));
        };
        let Some(stderr) = child.stderr.take() else {
            let cleanup_error = terminate_and_reap(child, self.process_exit_timeout)
                .await
                .into_diagnostic();
            return Ok(failed_tool_result(append_cleanup_diagnostic(
                format!(
                    "plugin '{}': could not attach stderr pipe",
                    self.manifest.tool.name
                ),
                cleanup_error.as_deref(),
            )));
        };
        let (first_stdout, readers) = spawn_output_readers(stdout, stderr);

        let pre_result = async {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "could not attach stdin pipe".to_string())?;
            let write_result = async {
                stdin.write_all(args_json.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                Ok::<(), std::io::Error>(())
            }
            .await;
            if let Err(e) = write_result
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(format!("failed to write args to stdin: {e}"));
            }
            drop(stdin);

            match first_stdout.await {
                Err(_) => Err("stdout reader ended before reporting a result".to_string()),
                Ok(Err(error)) => Err(format!("I/O error reading stdout: {error}")),
                Ok(Ok(line)) => Ok(line),
            }
        };

        let line = match timeout(self.first_output_timeout, pre_result).await {
            Err(_) => {
                return Ok(fail_and_cleanup(
                    child,
                    readers,
                    self.process_exit_timeout,
                    format!(
                        "plugin '{}' timed out before producing a result after {:?}",
                        self.manifest.tool.name, self.first_output_timeout
                    ),
                )
                .await);
            }
            Ok(Err(error)) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": self.manifest.tool.name,
                            "error": error,
                        })),
                    "subprocess plugin failed before producing a result"
                );
                return Ok(fail_and_cleanup(
                    child,
                    readers,
                    self.process_exit_timeout,
                    format!("plugin '{}': {}", self.manifest.tool.name, error),
                )
                .await);
            }
            Ok(Ok(line)) => line,
        };

        let child_status = match timeout(self.process_exit_timeout, child.wait()).await {
            Err(_) => {
                return Ok(fail_and_cleanup(
                    child,
                    readers,
                    self.process_exit_timeout,
                    format!(
                        "plugin '{}' did not exit within {:?} after producing output",
                        self.manifest.tool.name, self.process_exit_timeout
                    ),
                )
                .await);
            }
            Ok(Err(wait_error)) => {
                return Ok(fail_and_cleanup(
                    child,
                    readers,
                    self.process_exit_timeout,
                    format!(
                        "plugin '{}': failed while waiting for process exit: {}",
                        self.manifest.tool.name, wait_error
                    ),
                )
                .await);
            }
            Ok(Ok(status)) => status,
        };

        let line = line.trim();
        if line.is_empty() {
            let report = readers.finish(self.process_exit_timeout).await;
            return Ok(failed_tool_result(append_reader_diagnostics(
                format!("plugin '{}': empty stdout", self.manifest.tool.name),
                &report,
            )));
        }

        let result = match serde_json::from_str::<ToolResult>(line) {
            Ok(result) => result,
            Err(parse_err) => {
                let report = readers.finish(self.process_exit_timeout).await;
                return Ok(failed_tool_result(append_reader_diagnostics(
                    format!(
                        "plugin '{}': failed to parse output as ToolResult: {} (got: {:?})",
                        self.manifest.tool.name,
                        parse_err,
                        if line.chars().count() > 200 {
                            let truncated: String = line.chars().take(200).collect();
                            format!("{}...", truncated)
                        } else {
                            line.to_string()
                        }
                    ),
                    &report,
                )));
            }
        };

        if !child_status.success() {
            let report = readers.finish(self.process_exit_timeout).await;
            return Ok(failed_tool_result(append_reader_diagnostics(
                format!(
                    "plugin '{}' exited with {}",
                    self.manifest.tool.name, child_status
                ),
                &report,
            )));
        }

        let report = readers.finish_success().await;
        if report.error.is_some() {
            return Ok(failed_tool_result(append_reader_diagnostics(
                format!(
                    "plugin '{}': failed while draining process output",
                    self.manifest.tool.name
                ),
                &report,
            )));
        }
        Ok(result)
    }
}

fn configure_plugin_environment(command: &mut Command) {
    command.env_clear();
    for key in SAFE_ENV_VARS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn failed_tool_result(error: String) -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(error),
    }
}

struct CleanupReport {
    diagnostic: Option<String>,
    reaper: Option<JoinHandle<()>>,
}

impl CleanupReport {
    fn into_diagnostic(mut self) -> Option<String> {
        drop(self.reaper.take());
        self.diagnostic
    }
}

async fn terminate_and_reap(child: Child, deadline: Duration) -> CleanupReport {
    terminate_and_reap_after(child, deadline, std::future::ready(())).await
}

async fn terminate_and_reap_after<F>(
    mut child: Child,
    deadline: Duration,
    before_wait: F,
) -> CleanupReport
where
    F: std::future::Future<Output = ()>,
{
    let mut errors = Vec::new();
    match child.try_wait() {
        Ok(Some(_)) => {
            return CleanupReport {
                diagnostic: None,
                reaper: None,
            };
        }
        Ok(None) => {}
        Err(error) => errors.push(format!("failed to inspect child before cleanup: {error}")),
    }

    if let Err(error) = child.start_kill() {
        errors.push(format!("failed to terminate child: {error}"));
    }
    let wait_result = timeout(deadline, async {
        before_wait.await;
        child.wait().await
    })
    .await;
    let reaper = match wait_result {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => {
            errors.push(format!("failed to reap child: {error}"));
            Some(spawn_background_reaper(child))
        }
        Err(_) => {
            errors.push(format!("child cleanup timed out after {deadline:?}"));
            Some(spawn_background_reaper(child))
        }
    };

    CleanupReport {
        diagnostic: (!errors.is_empty()).then(|| errors.join("; ")),
        reaper,
    }
}

fn spawn_background_reaper(mut child: Child) -> JoinHandle<()> {
    let pid = child.id();
    zeroclaw_spawn::spawn!(async move {
        if let Err(error) = child.wait().await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "pid": pid,
                        "error": format!("{}", error),
                    })),
                "subprocess plugin background reap failed"
            );
        }
    })
}

async fn fail_and_cleanup(
    child: Child,
    readers: OutputReaderTasks,
    reader_deadline: Duration,
    message: String,
) -> ToolResult {
    let cleanup_error = terminate_and_reap(child, reader_deadline)
        .await
        .into_diagnostic();
    let report = readers.finish(reader_deadline).await;
    failed_tool_result(append_reader_diagnostics(
        append_cleanup_diagnostic(message, cleanup_error.as_deref()),
        &report,
    ))
}

fn spawn_output_readers(
    stdout: ChildStdout,
    stderr: ChildStderr,
) -> (oneshot::Receiver<Result<String, String>>, OutputReaderTasks) {
    let (first_line_tx, first_line_rx) = oneshot::channel();
    let stdout_task = zeroclaw_spawn::spawn!(async move {
        let mut stdout = BufReader::new(stdout);
        let mut line = String::new();
        match stdout.read_line(&mut line).await {
            Ok(_) => {
                let _ = first_line_tx.send(Ok(line));
                tokio::io::copy(&mut stdout, &mut tokio::io::sink())
                    .await
                    .map(|_| ())
            }
            Err(error) => {
                let message = error.to_string();
                let _ = first_line_tx.send(Err(message));
                Err(error)
            }
        }
    });

    let stderr_task = zeroclaw_spawn::spawn!(async move {
        let mut stderr = stderr;
        let mut captured = Vec::with_capacity(STDERR_CAPTURE_BYTES);
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let read = stderr.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            let remaining = STDERR_CAPTURE_BYTES.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
        Ok(captured)
    });

    (
        first_line_rx,
        OutputReaderTasks {
            stdout_task,
            stderr_task,
        },
    )
}

struct OutputReaderTasks {
    stdout_task: JoinHandle<std::io::Result<()>>,
    stderr_task: JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drop for OutputReaderTasks {
    fn drop(&mut self) {
        self.stdout_task.abort();
        self.stderr_task.abort();
    }
}

struct OutputReaderReport {
    stderr: String,
    error: Option<String>,
}

impl OutputReaderReport {
    fn from_reader_results(
        stdout_result: Option<Result<std::io::Result<()>, JoinError>>,
        stderr_result: Option<Result<std::io::Result<Vec<u8>>, JoinError>>,
    ) -> Self {
        let mut errors = Vec::new();
        if let Some(stdout_result) = stdout_result {
            match stdout_result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(format!("stdout: {error}")),
                Err(error) => errors.push(format!("stdout task: {error}")),
            }
        }
        let stderr = match stderr_result {
            None => String::new(),
            Some(Ok(Ok(bytes))) => String::from_utf8_lossy(&bytes).trim().to_string(),
            Some(Ok(Err(error))) => {
                errors.push(format!("stderr: {error}"));
                String::new()
            }
            Some(Err(error)) => {
                errors.push(format!("stderr task: {error}"));
                String::new()
            }
        };

        Self {
            stderr,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }
    }
}

fn classify_reader_result<T>(
    abort_requested: bool,
    result: Result<std::io::Result<T>, JoinError>,
) -> Option<Result<std::io::Result<T>, JoinError>> {
    match result {
        Err(error) if abort_requested && error.is_cancelled() => None,
        result => Some(result),
    }
}

impl OutputReaderTasks {
    async fn finish_success(mut self) -> OutputReaderReport {
        let stdout_abort_requested = !self.stdout_task.is_finished();
        if stdout_abort_requested {
            self.stdout_task.abort();
        }
        let stderr_abort_requested = !self.stderr_task.is_finished();
        if stderr_abort_requested {
            self.stderr_task.abort();
        }

        let stdout_result =
            classify_reader_result(stdout_abort_requested, (&mut self.stdout_task).await);
        let stderr_result =
            classify_reader_result(stderr_abort_requested, (&mut self.stderr_task).await);

        OutputReaderReport::from_reader_results(stdout_result, stderr_result)
    }

    async fn finish(mut self, deadline: Duration) -> OutputReaderReport {
        let drain_deadline = Instant::now() + deadline;
        let stdout_result = match timeout_at(drain_deadline, &mut self.stdout_task).await {
            Ok(result) => result,
            Err(_) => {
                self.stdout_task.abort();
                self.stderr_task.abort();
                let _ = (&mut self.stdout_task).await;
                let _ = (&mut self.stderr_task).await;
                return OutputReaderReport {
                    stderr: String::new(),
                    error: Some(format!("output drain timed out after {deadline:?}")),
                };
            }
        };
        let stderr_result = match timeout_at(drain_deadline, &mut self.stderr_task).await {
            Ok(result) => result,
            Err(_) => {
                self.stderr_task.abort();
                let _ = (&mut self.stderr_task).await;
                return OutputReaderReport {
                    stderr: String::new(),
                    error: Some(format!("output drain timed out after {deadline:?}")),
                };
            }
        };

        OutputReaderReport::from_reader_results(Some(stdout_result), Some(stderr_result))
    }
}

fn append_reader_diagnostics(mut message: String, report: &OutputReaderReport) -> String {
    if !report.stderr.is_empty() {
        message.push_str("; stderr: ");
        message.push_str(&report.stderr);
    }
    if let Some(error) = &report.error {
        message.push_str("; reader: ");
        message.push_str(error);
    }
    message
}

fn append_cleanup_diagnostic(mut message: String, cleanup_error: Option<&str>) -> String {
    if let Some(error) = cleanup_error {
        message.push_str("; cleanup: ");
        message.push_str(error);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ExecConfig, ParameterDef, ToolManifest, ToolMeta};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use zeroclaw_api::attribution::Attributable;

    fn make_manifest(name: &str, params: Vec<ParameterDef>) -> ToolManifest {
        ToolManifest {
            tool: ToolMeta {
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: format!("Test tool: {}", name),
            },
            exec: ExecConfig {
                binary: "tool".to_string(),
            },
            transport: None,
            parameters: params,
        }
    }

    fn make_param(name: &str, ty: &str, required: bool) -> ParameterDef {
        ParameterDef {
            name: name.to_string(),
            r#type: ty.to_string(),
            description: format!("param {}", name),
            required,
            default: None,
        }
    }

    #[test]
    fn name_and_description_come_from_manifest() {
        let m = make_manifest("gpio_test", vec![]);
        let tool = SubprocessTool::new(m, PathBuf::from("/bin/true"));
        assert_eq!(tool.name(), "gpio_test");
        assert_eq!(tool.description(), "Test tool: gpio_test");
    }

    #[test]
    fn manifest_loaded_subprocess_is_an_extension() {
        let tool =
            SubprocessTool::new(make_manifest("browser", vec![]), PathBuf::from("/bin/true"));

        assert_eq!(tool.tool_provenance(), ToolProvenance::Extension);
    }

    #[test]
    fn schema_reflects_parameter_definitions() {
        let params = vec![
            make_param("device", "string", true),
            make_param("pin", "integer", true),
            make_param("value", "integer", false),
        ];
        let m = make_manifest("gpio_write", params);
        let tool = SubprocessTool::new(m, PathBuf::from("/bin/true"));
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["device"]["type"], "string");
        assert_eq!(schema["properties"]["pin"]["type"], "integer");

        let required = schema["required"].as_array().unwrap();
        let req_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req_names.contains(&"device"));
        assert!(req_names.contains(&"pin"));
        assert!(!req_names.contains(&"value"));
    }

    #[test]
    fn schema_parameterless_tool_has_empty_required() {
        let m = make_manifest("noop", vec![]);
        let tool = SubprocessTool::new(m, PathBuf::from("/bin/true"));
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.is_empty());
    }

    #[test]
    fn plugin_command_environment_is_allowlisted() {
        let mut command = Command::new("unused-test-binary");
        command.env("ZEROCLAW_SUBPROCESS_TEST_SECRET", "must-not-survive");
        configure_plugin_environment(&mut command);

        let explicit_environment: Vec<String> = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| value.map(|_| key.to_string_lossy().into_owned()))
            .collect();

        assert!(
            explicit_environment
                .iter()
                .all(|key| SAFE_ENV_VARS.contains(&key.as_str())),
            "unexpected environment keys: {explicit_environment:?}"
        );
        assert!(
            !explicit_environment
                .iter()
                .any(|key| { key.eq_ignore_ascii_case("ZEROCLAW_SUBPROCESS_TEST_SECRET") })
        );
    }

    #[tokio::test]
    async fn execute_successful_subprocess() {
        let result_json = r#"{"success":true,"output":"ok","error":null}"#;
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_protocol_helper(dir.path(), &protocol_helper_script(result_json));
        let tool = SubprocessTool::new(make_manifest("echo_tool", vec![]), script_path);
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("execute should not return Err");

        assert!(result.success, "expected success=true, got: {:?}", result);
        assert_eq!(result.output, "ok");
        assert!(result.error.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_returns_success_before_descendant_closes_output_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("descendant-pid");
        let result_json = r#"{"success":true,"output":"ok","error":null}"#;
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\nsleep 60 &\ndescendant=$!\nprintf '%s\\n' \"$descendant\" > \"{}\"\nprintf '%s\\n' '{}'\nexit 0\n",
            pid_path.display(),
            result_json
        );
        let script_path = write_protocol_helper(dir.path(), &script);
        let tool = SubprocessTool::new(make_manifest("pipe_holder_tool", vec![]), script_path)
            .with_timeouts(Duration::from_secs(1), Duration::from_millis(100));
        let execution_result =
            timeout(Duration::from_secs(1), tool.execute(serde_json::json!({}))).await;
        let descendant_pid = wait_for_pid_file(&pid_path).await;

        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", descendant_pid.trim()])
            .status();

        let result = execution_result
            .expect("pipe-holding plugin must return before the test deadline")
            .expect("execute should not return Err");
        assert!(
            result.success,
            "expected the direct child's ToolResult: {result:?}"
        );
        assert_eq!(result.output, "ok");
        assert!(result.error.is_none());
    }

    #[cfg(windows)]
    fn protocol_helper_path(dir: &std::path::Path) -> PathBuf {
        dir.join("tool.cmd")
    }

    #[cfg(not(windows))]
    fn protocol_helper_path(dir: &std::path::Path) -> PathBuf {
        dir.join("tool.sh")
    }

    #[cfg(windows)]
    fn protocol_helper_script(result_json: &str) -> String {
        format!("@echo off\r\nset /p _zc_args=\r\necho {result_json}\r\n")
    }

    #[cfg(not(windows))]
    fn protocol_helper_script(result_json: &str) -> String {
        format!("#!/bin/sh\ncat > /dev/null\necho '{}'\n", result_json)
    }

    #[cfg(windows)]
    fn stderr_flood_script(result_json: &str) -> String {
        format!(
            "@echo off\r\nset /p _zc_args=\r\nfor /L %%i in (1,1,4096) do @echo 01234567890123456789012345678901234567890123456789012345678901234567890123456789 1>&2\r\necho {result_json}\r\n"
        )
    }

    #[cfg(not(windows))]
    fn stderr_flood_script(result_json: &str) -> String {
        format!(
            "#!/bin/sh\ncat > /dev/null\ni=0\nwhile [ \"$i\" -lt 4096 ]; do\n  printf '%080d\\n' \"$i\" >&2\n  i=$((i + 1))\ndone\necho '{}'\n",
            result_json
        )
    }

    fn write_protocol_helper(dir: &std::path::Path, script: &str) -> PathBuf {
        let script_path = protocol_helper_path(dir);
        std::fs::write(&script_path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script_path
    }

    #[cfg(unix)]
    async fn wait_for_pid_file(path: &std::path::Path) -> String {
        let mut last_observed = None;
        let result = timeout(Duration::from_secs(4), async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(path) {
                    if pid.trim().parse::<u32>().is_ok_and(|pid| pid > 0) {
                        break pid;
                    }
                    last_observed = Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        result.unwrap_or_else(|_| {
            panic!(
                "fixture must record a positive PID before the readiness deadline; last observed: {last_observed:?}"
            )
        })
    }

    #[tokio::test]
    async fn execute_drains_stderr_before_reading_stdout() {
        let result_json = r#"{"success":true,"output":"ok","error":null}"#;
        let dir = tempfile::tempdir().unwrap();
        let script_path = write_protocol_helper(dir.path(), &stderr_flood_script(result_json));
        let tool = SubprocessTool::new(make_manifest("stderr_tool", vec![]), script_path);

        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(result.success, "stderr flood must not deadlock: {result:?}");
        assert_eq!(result.output, "ok");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_timeout_kills_process_and_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\necho $$ > \"{}\"\nexec sleep 60\n",
            pid_path.display()
        );
        let script_path = write_protocol_helper(dir.path(), &script);

        let m = make_manifest("sleep_tool", vec![]);
        let tool = SubprocessTool::new(m, script_path)
            .with_timeouts(Duration::from_secs(5), Duration::from_secs(1));
        let execution =
            zeroclaw_spawn::spawn!(async move { tool.execute(serde_json::json!({})).await });
        let pid = wait_for_pid_file(&pid_path).await;
        let result = execution
            .await
            .expect("test execution task should complete")
            .expect("should not propagate Err");

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("timed out"),
            "expected 'timed out' in error, got: {}",
            err
        );

        let status = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap();
        assert!(!status.success(), "timed-out plugin process must be reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_times_out_while_child_leaves_large_stdin_unread() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let script = format!(
            "#!/bin/sh\necho $$ > \"{}\"\nexec sleep 60\n",
            pid_path.display()
        );
        let script_path = write_protocol_helper(dir.path(), &script);
        let tool = SubprocessTool::new(make_manifest("blocked_stdin_tool", vec![]), script_path)
            .with_timeouts(Duration::from_secs(5), Duration::from_secs(1));

        let execution = zeroclaw_spawn::spawn!(async move {
            tool.execute(serde_json::json!({"payload": "x".repeat(1024 * 1024)}))
                .await
        });
        let pid = wait_for_pid_file(&pid_path).await;
        let result = execution
            .await
            .expect("test execution task should complete")
            .expect("blocked stdin writes should return a failed ToolResult");

        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap().contains("timed out"),
            "the first-result deadline must include request writes: {result:?}"
        );
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap();
        assert!(!status.success(), "blocked-stdin plugin must be reaped");
    }

    struct CancellationMarker(Arc<AtomicBool>);

    impl Drop for CancellationMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn dropping_output_readers_aborts_both_tasks() {
        let stdout_cancelled = Arc::new(AtomicBool::new(false));
        let stderr_cancelled = Arc::new(AtomicBool::new(false));
        let stdout_marker = CancellationMarker(Arc::clone(&stdout_cancelled));
        let stderr_marker = CancellationMarker(Arc::clone(&stderr_cancelled));

        let readers = OutputReaderTasks {
            stdout_task: zeroclaw_spawn::spawn!(async move {
                let _marker = stdout_marker;
                std::future::pending::<std::io::Result<()>>().await
            }),
            stderr_task: zeroclaw_spawn::spawn!(async move {
                let _marker = stderr_marker;
                std::future::pending::<std::io::Result<Vec<u8>>>().await
            }),
        };
        tokio::task::yield_now().await;

        drop(readers);
        timeout(Duration::from_secs(1), async {
            while !(stdout_cancelled.load(Ordering::SeqCst)
                && stderr_cancelled.load(Ordering::SeqCst))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted reader tasks must be dropped promptly");

        assert!(stdout_cancelled.load(Ordering::SeqCst));
        assert!(stderr_cancelled.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn reader_result_classification_preserves_race_winner_and_suppresses_cancellation() {
        let retained = classify_reader_result(
            true,
            Ok::<std::io::Result<()>, JoinError>(Err(std::io::Error::other(
                "completed reader I/O error",
            ))),
        );
        let retained_error = match retained {
            Some(Ok(Err(error))) => error,
            other => panic!("race-winning reader error must be retained: {other:?}"),
        };
        assert_eq!(retained_error.to_string(), "completed reader I/O error");

        let pending_task =
            zeroclaw_spawn::spawn!(async { std::future::pending::<std::io::Result<()>>().await });
        pending_task.abort();
        let cancelled = pending_task.await;

        assert!(matches!(&cancelled, Err(error) if error.is_cancelled()));
        assert!(classify_reader_result(true, cancelled).is_none());
    }

    #[tokio::test]
    async fn reader_finish_does_not_repoll_completed_handle_after_peer_timeout() {
        let readers = OutputReaderTasks {
            stdout_task: zeroclaw_spawn::spawn!(async { Ok(()) }),
            stderr_task: zeroclaw_spawn::spawn!(async {
                std::future::pending::<std::io::Result<Vec<u8>>>().await
            }),
        };

        let report = readers.finish(Duration::from_millis(10)).await;

        assert_eq!(
            report.error.as_deref(),
            Some("output drain timed out after 10ms")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_output_then_exit_timeout_kills_and_reaps_process() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("pid");
        let result_json = r#"{"success":true,"output":"premature","error":null}"#;
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\necho $$ > \"{}\"\necho '{}'\nexec sleep 60\n",
            pid_path.display(),
            result_json
        );
        let script_path = write_protocol_helper(dir.path(), &script);
        let tool = SubprocessTool::new(make_manifest("late_exit_tool", vec![]), script_path)
            .with_timeouts(Duration::from_secs(5), Duration::from_millis(100));

        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap().contains("did not exit"),
            "a parsed line must not hide an exit timeout: {result:?}"
        );
        let pid = std::fs::read_to_string(pid_path).unwrap();
        let status = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap();
        assert!(!status.success(), "exit-timeout plugin must be reaped");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_timeout_transfers_child_to_background_reaper() {
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().expect("spawned child must have a PID");

        let CleanupReport { diagnostic, reaper } =
            terminate_and_reap_after(child, Duration::from_millis(10), std::future::pending())
                .await;

        assert_eq!(
            diagnostic.as_deref(),
            Some("child cleanup timed out after 10ms")
        );
        timeout(
            Duration::from_secs(1),
            reaper.expect("timed-out cleanup must retain a background reaper"),
        )
        .await
        .expect("background reaper must finish promptly")
        .expect("background reaper task must not panic");

        let status = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap();
        assert!(!status.success(), "background reaper must reap the child");
    }
}
