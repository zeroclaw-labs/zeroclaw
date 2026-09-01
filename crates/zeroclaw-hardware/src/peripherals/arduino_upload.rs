//! Arduino upload tool — agent generates code, uploads via arduino-cli.

use async_trait::async_trait;
use serde_json::{Value, json};
#[cfg(test)]
use std::ffi::OsString;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;
use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

tool_attribution!(ArduinoUploadTool, ToolKind::Plugin);

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const COMPILE_TIMEOUT: Duration = Duration::from_secs(120);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);
const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct CommandTimeouts {
    version: Duration,
    compile: Duration,
    upload: Duration,
}

impl Default for CommandTimeouts {
    fn default() -> Self {
        Self {
            version: VERSION_TIMEOUT,
            compile: COMPILE_TIMEOUT,
            upload: UPLOAD_TIMEOUT,
        }
    }
}

enum ProcessFailure {
    Spawn(std::io::Error),
    Io(std::io::Error),
    Lifecycle(String),
    Timeout(Duration),
}

#[derive(Clone, Copy)]
enum OutputCapture {
    Discard,
    Collect,
}

/// Tool: upload Arduino sketch (agent-generated code) to the board.
pub struct ArduinoUploadTool {
    /// Serial port path (e.g. /dev/cu.usbmodem33000283452)
    pub port: String,
    cli_path: PathBuf,
    timeouts: CommandTimeouts,
    #[cfg(test)]
    test_env: Vec<(OsString, OsString)>,
    #[cfg(test)]
    cli_prefix_args: Vec<OsString>,
}

impl ArduinoUploadTool {
    pub fn new(port: String) -> Self {
        Self {
            port,
            cli_path: PathBuf::from("arduino-cli"),
            timeouts: CommandTimeouts::default(),
            #[cfg(test)]
            test_env: Vec::new(),
            #[cfg(test)]
            cli_prefix_args: Vec::new(),
        }
    }

    async fn run_cli(
        &self,
        args: &[&str],
        deadline: Duration,
        capture: OutputCapture,
    ) -> Result<Output, ProcessFailure> {
        let mut command = Command::new(&self.cli_path);
        #[cfg(test)]
        command.args(&self.cli_prefix_args);
        command.args(args).stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(test)]
        command.envs(self.test_env.iter().cloned());

        let capture_files = match capture {
            OutputCapture::Discard => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
                None
            }
            OutputCapture::Collect => {
                let stdout_writer = tempfile::tempfile().map_err(ProcessFailure::Io)?;
                let stderr_writer = tempfile::tempfile().map_err(ProcessFailure::Io)?;
                let stdout_reader = stdout_writer.try_clone().map_err(ProcessFailure::Io)?;
                let stderr_reader = stderr_writer.try_clone().map_err(ProcessFailure::Io)?;
                command
                    .stdout(Stdio::from(stdout_writer))
                    .stderr(Stdio::from(stderr_writer));
                Some((stdout_reader, stderr_reader))
            }
        };

        let mut child = command.spawn().map_err(ProcessFailure::Spawn)?;
        let status = match timeout(deadline, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                if let Err(cleanup) = Self::terminate_and_reap(&mut child).await {
                    return Err(ProcessFailure::Lifecycle(format!(
                        "wait failed ({error}); {cleanup}"
                    )));
                }
                return Err(ProcessFailure::Io(error));
            }
            Err(_) => {
                if let Err(cleanup) = Self::terminate_and_reap(&mut child).await {
                    return Err(ProcessFailure::Lifecycle(format!(
                        "deadline elapsed after {} seconds; {cleanup}",
                        deadline.as_secs_f64()
                    )));
                }
                return Err(ProcessFailure::Timeout(deadline));
            }
        };

        match capture_files {
            None => Ok(Output {
                status,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            Some((stdout_reader, stderr_reader)) => tokio::task::spawn_blocking(move || {
                Self::read_captured_output(status, stdout_reader, stderr_reader)
            })
            .await
            .map_err(|error| {
                ProcessFailure::Io(std::io::Error::other(format!(
                    "output reader task failed: {error}"
                )))
            })?
            .map_err(ProcessFailure::Io),
        }
    }

    async fn terminate_and_reap(child: &mut tokio::process::Child) -> Result<(), String> {
        if let Err(error) = child.start_kill()
            && error.kind() != std::io::ErrorKind::InvalidInput
        {
            return Err(format!("failed to terminate child: {error}"));
        }
        match timeout(PROCESS_REAP_TIMEOUT, child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(format!("failed to reap child: {error}")),
            Err(_) => Err(format!(
                "child was not reaped within {} seconds",
                PROCESS_REAP_TIMEOUT.as_secs()
            )),
        }
    }

    fn read_captured_output(
        status: std::process::ExitStatus,
        mut stdout_reader: std::fs::File,
        mut stderr_reader: std::fs::File,
    ) -> std::io::Result<Output> {
        stdout_reader.seek(SeekFrom::Start(0))?;
        stderr_reader.seek(SeekFrom::Start(0))?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        stdout_reader.read_to_end(&mut stdout)?;
        stderr_reader.read_to_end(&mut stderr)?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    fn timeout_result(stage: &str, duration: Duration) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new().into(),
            error: Some(format!(
                "Arduino {stage} timed out after {} seconds",
                duration.as_secs_f64()
            )),
        }
    }

    fn lifecycle_result(stage: &str, error: String) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new().into(),
            error: Some(format!("Arduino {stage} process lifecycle failed: {error}")),
        }
    }
}

#[async_trait]
impl Tool for ArduinoUploadTool {
    fn name(&self) -> &str {
        "arduino_upload"
    }

    fn description(&self) -> &str {
        "Generate Arduino sketch code and upload it to the connected Arduino. Use when: user asks to 'make a heart', 'blink LED', or run any custom pattern on Arduino. You MUST write the full .ino sketch code (setup + loop). Arduino Uno: pin 13 = built-in LED. Saves to temp dir, runs arduino-cli compile and upload. Requires arduino-cli installed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": {
                    "type": "string",
                    "description": "Full Arduino sketch code (complete .ino file content)"
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let code = args.get("code").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"param": "code"})),
                "arduino_upload tool: missing parameter"
            );
            anyhow::Error::msg("Missing 'code' parameter")
        })?;

        if code.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new().into(),
                error: Some("Code cannot be empty".into()),
            });
        }

        match self
            .run_cli(&["version"], self.timeouts.version, OutputCapture::Discard)
            .await
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(format!(
                        "arduino-cli preflight failed with {}",
                        output.status
                    )),
                });
            }
            Err(ProcessFailure::Timeout(duration)) => {
                return Ok(Self::timeout_result("CLI preflight", duration));
            }
            Err(ProcessFailure::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(
                        "arduino-cli not found. Install it: https://arduino.github.io/arduino-cli/"
                            .into(),
                    ),
                });
            }
            Err(ProcessFailure::Spawn(error) | ProcessFailure::Io(error)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new().into(),
                    error: Some(format!("arduino-cli preflight failed: {error}")),
                });
            }
            Err(ProcessFailure::Lifecycle(error)) => {
                return Ok(Self::lifecycle_result("CLI preflight", error));
            }
        }

        let sketch_name = "zeroclaw_sketch";
        let temp_dir = match tempfile::Builder::new().prefix("zeroclaw_").tempdir() {
            Ok(dir) => dir,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("Failed to create sketch dir: {e}").into(),
                    error: Some(e.to_string()),
                });
            }
        };
        let sketch_dir = temp_dir.path().join(sketch_name);
        let ino_path = sketch_dir.join(format!("{}.ino", sketch_name));

        if let Err(e) = tokio::fs::create_dir_all(&sketch_dir).await {
            return Ok(ToolResult {
                success: false,
                output: format!("Failed to create sketch dir: {}", e).into(),
                error: Some(e.to_string()),
            });
        }

        if let Err(e) = tokio::fs::write(&ino_path, code).await {
            return Ok(ToolResult {
                success: false,
                output: format!("Failed to write sketch: {}", e).into(),
                error: Some(e.to_string()),
            });
        }

        let sketch_path = sketch_dir.to_string_lossy();
        let fqbn = "arduino:avr:uno";

        // Compile
        let compile_output = match self
            .run_cli(
                &["compile", "--fqbn", fqbn, &sketch_path],
                self.timeouts.compile,
                OutputCapture::Collect,
            )
            .await
        {
            Ok(output) => output,
            Err(ProcessFailure::Timeout(duration)) => {
                return Ok(Self::timeout_result("compile", duration));
            }
            Err(ProcessFailure::Spawn(e) | ProcessFailure::Io(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("arduino-cli compile failed: {}", e).into(),
                    error: Some(e.to_string()),
                });
            }
            Err(ProcessFailure::Lifecycle(error)) => {
                return Ok(Self::lifecycle_result("compile", error));
            }
        };

        if !compile_output.status.success() {
            let stderr = String::from_utf8_lossy(&compile_output.stderr);
            return Ok(ToolResult {
                success: false,
                output: format!("Compile failed:\n{}", stderr).into(),
                error: Some("Arduino compile error".into()),
            });
        }

        // Upload
        let upload_output = match self
            .run_cli(
                &["upload", "-p", &self.port, "--fqbn", fqbn, &sketch_path],
                self.timeouts.upload,
                OutputCapture::Collect,
            )
            .await
        {
            Ok(output) => output,
            Err(ProcessFailure::Timeout(duration)) => {
                return Ok(Self::timeout_result("upload", duration));
            }
            Err(ProcessFailure::Spawn(e) | ProcessFailure::Io(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: format!("arduino-cli upload failed: {}", e).into(),
                    error: Some(e.to_string()),
                });
            }
            Err(ProcessFailure::Lifecycle(error)) => {
                return Ok(Self::lifecycle_result("upload", error));
            }
        };

        if !upload_output.status.success() {
            let stderr = String::from_utf8_lossy(&upload_output.stderr);
            return Ok(ToolResult {
                success: false,
                output: format!("Upload failed:\n{}", stderr).into(),
                error: Some("Arduino upload error".into()),
            });
        }

        Ok(ToolResult {
            success: true,
            output:
                "Sketch compiled and uploaded successfully. The Arduino is now running your code."
                    .into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    struct FakeCli {
        _dir: tempfile::TempDir,
        cli_path: PathBuf,
        cli_prefix_args: Vec<OsString>,
        log: PathBuf,
        sketch_path: PathBuf,
        pid: PathBuf,
        stdin_state: PathBuf,
    }

    impl FakeCli {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (cli_path, cli_prefix_args) = Self::create_executable(dir.path());
            let log = dir.path().join("calls.log");
            let sketch_path = dir.path().join("sketch-path");
            let pid = dir.path().join("pid");
            let stdin_state = dir.path().join("stdin-state");

            Self {
                _dir: dir,
                cli_path,
                cli_prefix_args,
                log,
                sketch_path,
                pid,
                stdin_state,
            }
        }

        #[cfg(unix)]
        fn create_executable(dir: &std::path::Path) -> (PathBuf, Vec<OsString>) {
            let script = dir.join("arduino-cli");
            std::fs::write(
                &script,
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$ZC_FAKE_LOG"
if IFS= read -r ignored; then
  stdin_state='data'
else
  stdin_state='eof'
fi
printf '%s:%s\n' "$1" "$stdin_state" >> "$ZC_STDIN_STATE"
case "$1" in
  version)
    if [ "${ZC_VERSION_SLEEP:-0}" = "1" ]; then
      printf '%s' "$$" > "$ZC_CHILD_PID"
      exec sleep 30
    fi
    exit "${ZC_VERSION_EXIT:-0}"
    ;;
  compile)
    printf '%s' "$4" > "$ZC_SKETCH_PATH"
    if [ "${ZC_COMPILE_SLEEP:-0}" = "1" ]; then
      printf '%s' "$$" > "$ZC_CHILD_PID"
      exec sleep 30
    fi
    printf '%s' "compile error" >&2
    exit "${ZC_COMPILE_EXIT:-0}"
    ;;
  upload)
    if [ "${ZC_UPLOAD_SLEEP:-0}" = "1" ]; then
      printf '%s' "$$" > "$ZC_CHILD_PID"
      exec sleep 30
    fi
    printf '%s' "upload error" >&2
    exit "${ZC_UPLOAD_EXIT:-0}"
    ;;
esac
exit 2
"#,
            )
            .unwrap();
            let mut permissions = std::fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&script, permissions).unwrap();
            (script, Vec::new())
        }

        #[cfg(windows)]
        fn create_executable(dir: &std::path::Path) -> (PathBuf, Vec<OsString>) {
            let script = dir.join("arduino-cli.ps1");
            std::fs::write(
                &script,
                r#"param([Parameter(ValueFromRemainingArguments = $true)][string[]]$CliArgs)
Add-Content -LiteralPath $env:ZC_FAKE_LOG -Value ($CliArgs -join ' ')
$stdinValue = [Console]::In.ReadLine()
if ($null -eq $stdinValue) {
  $stdinState = 'eof'
} else {
  $stdinState = 'data'
}
Add-Content -LiteralPath $env:ZC_STDIN_STATE -Value "$($CliArgs[0]):$stdinState"
switch ($CliArgs[0]) {
  'version' {
    if ($env:ZC_VERSION_SLEEP -eq '1') {
      Set-Content -LiteralPath $env:ZC_CHILD_PID -NoNewline -Value $PID
      Start-Sleep -Seconds 30
    }
    $code = if ($env:ZC_VERSION_EXIT) { [int]$env:ZC_VERSION_EXIT } else { 0 }
    exit $code
  }
  'compile' {
    Set-Content -LiteralPath $env:ZC_SKETCH_PATH -NoNewline -Value $CliArgs[3]
    if ($env:ZC_COMPILE_SLEEP -eq '1') {
      Set-Content -LiteralPath $env:ZC_CHILD_PID -NoNewline -Value $PID
      Start-Sleep -Seconds 30
    }
    [Console]::Error.Write('compile error')
    $code = if ($env:ZC_COMPILE_EXIT) { [int]$env:ZC_COMPILE_EXIT } else { 0 }
    exit $code
  }
  'upload' {
    if ($env:ZC_UPLOAD_SLEEP -eq '1') {
      Set-Content -LiteralPath $env:ZC_CHILD_PID -NoNewline -Value $PID
      Start-Sleep -Seconds 30
    }
    [Console]::Error.Write('upload error')
    $code = if ($env:ZC_UPLOAD_EXIT) { [int]$env:ZC_UPLOAD_EXIT } else { 0 }
    exit $code
  }
}
exit 2
"#,
            )
            .unwrap();
            (
                PathBuf::from("powershell.exe"),
                vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    script.into_os_string(),
                ],
            )
        }

        fn tool(&self, extra_env: &[(&str, &str)]) -> ArduinoUploadTool {
            let mut test_env = vec![
                ("ZC_FAKE_LOG".into(), self.log.as_os_str().to_owned()),
                (
                    "ZC_SKETCH_PATH".into(),
                    self.sketch_path.as_os_str().to_owned(),
                ),
                ("ZC_CHILD_PID".into(), self.pid.as_os_str().to_owned()),
                (
                    "ZC_STDIN_STATE".into(),
                    self.stdin_state.as_os_str().to_owned(),
                ),
            ];
            test_env.extend(
                extra_env
                    .iter()
                    .map(|(key, value)| (OsString::from(key), OsString::from(value))),
            );
            ArduinoUploadTool {
                port: "/dev/fake-arduino".into(),
                cli_path: self.cli_path.clone(),
                timeouts: CommandTimeouts {
                    version: Duration::from_secs(10),
                    compile: Duration::from_secs(10),
                    upload: Duration::from_secs(10),
                },
                test_env,
                cli_prefix_args: self.cli_prefix_args.clone(),
            }
        }
    }

    #[cfg(unix)]
    fn process_is_alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    fn process_is_alive(pid: &str) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid.trim()), "/NH"])
            .output()
            .is_ok_and(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .any(|field| field == pid.trim())
            })
    }

    #[tokio::test]
    async fn fake_cli_success_runs_all_stages_and_cleans_workspace() {
        let fake = FakeCli::new();
        let result = fake
            .tool(&[])
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(result.success, "result: {result:?}");
        let calls = std::fs::read_to_string(&fake.log).unwrap();
        assert_eq!(
            std::fs::read_to_string(&fake.stdin_state)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["version:eof", "compile:eof", "upload:eof"]
        );
        assert!(calls.lines().any(|line| line == "version"));
        assert!(
            calls
                .lines()
                .any(|line| line.starts_with("compile --fqbn "))
        );
        assert!(calls.lines().any(|line| line.starts_with("upload -p ")));
        let sketch_path = std::fs::read_to_string(&fake.sketch_path).unwrap();
        assert!(!std::path::Path::new(&sketch_path).exists());
    }

    #[tokio::test]
    async fn compile_failure_reports_stderr_and_cleans_workspace() {
        let fake = FakeCli::new();
        let result = fake
            .tool(&[("ZC_COMPILE_EXIT", "7")])
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.to_string().contains("compile error"));
        let sketch_path = std::fs::read_to_string(&fake.sketch_path).unwrap();
        assert!(!std::path::Path::new(&sketch_path).exists());
    }

    #[tokio::test]
    async fn failed_preflight_does_not_compile_or_upload() {
        let fake = FakeCli::new();
        let result = fake
            .tool(&[("ZC_VERSION_EXIT", "7")])
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("arduino-cli preflight failed")
        );
        assert_eq!(std::fs::read_to_string(&fake.log).unwrap(), "version\n");
        assert_eq!(
            std::fs::read_to_string(&fake.stdin_state).unwrap().trim(),
            "version:eof"
        );
        assert!(!fake.sketch_path.exists());
    }

    #[tokio::test]
    async fn non_not_found_spawn_error_keeps_its_cause() {
        let fake = FakeCli::new();
        let mut tool = fake.tool(&[]);
        tool.cli_path = fake._dir.path().to_path_buf();
        tool.cli_prefix_args.clear();

        let result = tool
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.unwrap();
        assert!(error.contains("arduino-cli preflight failed"));
        assert!(!error.contains("Install it"));
    }

    #[tokio::test]
    async fn compile_timeout_kills_reaps_and_cleans_workspace() {
        let fake = FakeCli::new();
        let mut tool = fake.tool(&[("ZC_COMPILE_SLEEP", "1")]);
        tool.timeouts.compile = Duration::from_secs(1);
        let result = tool
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("compile timed out"),
            "unexpected timeout error: {error}"
        );
        let pid = std::fs::read_to_string(&fake.pid).unwrap();
        assert!(
            !process_is_alive(&pid),
            "timed-out child {pid} is still alive"
        );
        let sketch_path = std::fs::read_to_string(&fake.sketch_path).unwrap();
        assert!(!std::path::Path::new(&sketch_path).exists());
    }

    #[tokio::test]
    async fn preflight_timeout_reports_its_stage() {
        let fake = FakeCli::new();
        let mut tool = fake.tool(&[("ZC_VERSION_SLEEP", "1")]);
        tool.timeouts.version = Duration::from_secs(1);
        let result = tool
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("CLI preflight timed out"),
            "unexpected timeout error: {error}"
        );
        assert!(!fake.sketch_path.exists());
    }

    #[tokio::test]
    async fn upload_timeout_reports_its_stage_and_cleans_workspace() {
        let fake = FakeCli::new();
        let mut tool = fake.tool(&[("ZC_UPLOAD_SLEEP", "1")]);
        tool.timeouts.upload = Duration::from_secs(1);
        let result = tool
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or_default();
        assert!(
            error.contains("upload timed out"),
            "unexpected timeout error: {error}"
        );
        let sketch_path = std::fs::read_to_string(&fake.sketch_path).unwrap();
        assert!(!std::path::Path::new(&sketch_path).exists());
    }

    #[tokio::test]
    async fn upload_failure_reports_stderr_and_cleans_workspace() {
        let fake = FakeCli::new();
        let result = fake
            .tool(&[("ZC_UPLOAD_EXIT", "9")])
            .execute(json!({"code": "void setup() {}\nvoid loop() {}"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.to_string().contains("upload error"));
        assert_eq!(result.error.as_deref(), Some("Arduino upload error"));
        let sketch_path = std::fs::read_to_string(&fake.sketch_path).unwrap();
        assert!(!std::path::Path::new(&sketch_path).exists());
    }
}
