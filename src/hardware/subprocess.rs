//! SubprocessTool — wraps any external binary as a [`Tool`].
//!
//! Plugins do not need to be written in Rust. Any executable that follows the
//! ZeroClaw subprocess protocol is a valid tool:
//!
//! **Protocol (stdin/stdout, one line each):**
//! ```text
//! Host → binary stdin:  {"device":"pico0","pin":5}\n
//! Binary → stdout:      {"success":true,"output":"done","error":null}\n
//! ```
//!
//! Error protocol:
//! - **Timeout (10 s)** — process is killed; `ToolResult::error` contains timeout message.
//! - **Non-zero exit** — process is killed; `ToolResult::error` contains stderr.
//! - **Empty / unparseable stdout** — `ToolResult::error` describes the failure.
//!
//! The schema advertised to the LLM is auto-generated from [`ToolManifest::parameters`].

use super::manifest::ToolManifest;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Subprocess timeout — kill the child process after this many seconds.
const SUBPROCESS_TIMEOUT_SECS: u64 = 10;

/// Timeout for waiting on child process exit after stdout has been read.
/// Prevents a hung cleanup phase from blocking indefinitely.
const PROCESS_EXIT_TIMEOUT_SECS: u64 = 5;

/// A tool backed by an external subprocess.
///
/// The binary receives the LLM-supplied JSON arguments on stdin (one line,
/// `\n`-terminated) and must write a single `ToolResult`-compatible JSON
/// object to stdout before exiting.
pub struct SubprocessTool {
    /// Parsed plugin manifest (tool metadata + parameter definitions).
    manifest: ToolManifest,
    /// Resolved absolute path to the entry-point binary.
    binary_path: PathBuf,
}

impl SubprocessTool {
    /// Create a new `SubprocessTool` from a manifest and resolved binary path.
    pub fn new(manifest: ToolManifest, binary_path: PathBuf) -> Self {
        Self {
            manifest,
            binary_path,
        }
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

    /// Spawn the binary, write args to stdin, read `ToolResult` from stdout.
    ///
    /// Steps:
    /// 1. Serialize `args` to a JSON string.
    /// 2. Spawn `binary_path` with piped stdin/stdout/stderr.
    /// 3. Write `<json>\n` to child stdin; close stdin (signal EOF).
    /// 4. Read one line from child stdout (10 s timeout).
    /// 5. Kill the child process.
    /// 6. Deserialize the line to `ToolResult`.
    /// 7. On timeout → return error `ToolResult`; on empty/bad output → error.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let args_json = serde_json::to_string(&args)
            .map_err(|e| anyhow::anyhow!("failed to serialise args: {}", e))?;

        // Spawn child process.
        let mut child = Command::new(&self.binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to spawn plugin '{}' at {}: {}",
                    self.manifest.tool.name,
                    self.binary_path.display(),
                    e
                )
            })?;

        // Write JSON args + newline to stdin, then drop stdin to signal EOF.
        // BrokenPipe is tolerated — the child may exit before reading stdin
        // (e.g. tools that only use command-line args or produce fixed output).
        if let Some(mut stdin) = child.stdin.take() {
            let write_result = async {
                stdin.write_all(args_json.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                Ok::<(), std::io::Error>(())
            }
            .await;
            if let Err(e) = write_result {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    let _ = child.kill().await;
                    return Err(anyhow::anyhow!(
                        "failed to write args to plugin '{}' stdin: {}",
                        self.manifest.tool.name,
                        e
                    ));
                }
            }
            // stdin dropped here → child receives EOF
        }

        // Take stdout and stderr handles before we move `child`.
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        // Read one line from stdout with a hard timeout.
        let read_result = match stdout_handle {
            None => {
                // No stdout — kill and error.
                let _ = child.kill().await;
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "plugin '{}': could not attach stdout pipe",
                        self.manifest.tool.name
                    )),
                });
            }
            Some(stdout) => {
                let mut reader = BufReader::new(stdout);
                let mut line = String::new();
                timeout(
                    Duration::from_secs(SUBPROCESS_TIMEOUT_SECS),
                    reader.read_line(&mut line),
                )
                .await
                .map(|inner| inner.map(|_| line))
            }
        };

        match read_result {
            // ── Timeout ────────────────────────────────────────────────────
            // The read deadline elapsed — force-kill the plugin and collect
            // any stderr it emitted before dying.
            Err(_elapsed) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let stderr_msg = collect_stderr(stderr_handle).await;
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "plugin '{}' timed out after {}s{}",
                        self.manifest.tool.name,
                        SUBPROCESS_TIMEOUT_SECS,
                        if stderr_msg.is_empty() {
                            String::new()
                        } else {
                            format!("; stderr: {}", stderr_msg)
                        }
                    )),
                })
            }

            // ── I/O error reading stdout ───────────────────────────────────
            Ok(Err(io_err)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                let stderr_msg = collect_stderr(stderr_handle).await;
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "plugin '{}': I/O error reading stdout: {}{}",
                        self.manifest.tool.name,
                        io_err,
                        if stderr_msg.is_empty() {
                            String::new()
                        } else {
                            format!("; stderr: {}", stderr_msg)
                        }
                    )),
                })
            }

            // ── Got a line ────────────────────────────────────────────────
            // Let the process finish naturally — plugins that write their
            // result and then do cleanup should not be interrupted.
            Ok(Ok(line)) => {
                let child_status =
                    timeout(Duration::from_secs(PROCESS_EXIT_TIMEOUT_SECS), child.wait())
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                let stderr_msg = collect_stderr(stderr_handle).await;
                let line = line.trim();

                if line.is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "plugin '{}': empty stdout{}",
                            self.manifest.tool.name,
                            if stderr_msg.is_empty() {
                                String::new()
                            } else {
                                format!("; stderr: {}", stderr_msg)
                            }
                        )),
                    });
                }

                match serde_json::from_str::<ToolResult>(line) {
                    Ok(result) => {
                        // Non-zero exit overrides a parsed result: the plugin
                        // signalled failure even if it wrote a success line.
                        if let Some(status) = child_status {
                            if !status.success() {
                                return Ok(ToolResult {
                                    success: false,
                                    output: String::new(),
                                    error: Some(format!(
                                        "plugin '{}' exited with {}{}",
                                        self.manifest.tool.name,
                                        status,
                                        if stderr_msg.is_empty() {
                                            String::new()
                                        } else {
                                            format!("; stderr: {}", stderr_msg)
                                        }
                                    )),
                                });
                            }
                        }
                        Ok(result)
                    }
                    Err(parse_err) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "plugin '{}': failed to parse output as ToolResult: {} (got: {:?})",
                            self.manifest.tool.name,
                            parse_err,
                            // Truncate oversized output in the error message.
                            // Use char-based truncation to avoid panic on multi-byte UTF-8.
                            if line.chars().count() > 200 {
                                let truncated: String = line.chars().take(200).collect();
                                format!("{}...", truncated)
                            } else {
                                line.to_string()
                            }
                        )),
                    }),
                }
            }
        }
    }
}

/// Collect up to 512 bytes from an optional stderr handle.
/// Used to enrich error messages when a plugin writes nothing to stdout.
async fn collect_stderr(handle: Option<tokio::process::ChildStderr>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut stderr) = handle else {
        return String::new();
    };
    let mut buf = vec![0u8; 512];
    match stderr.read(&mut buf).await {
        Ok(n) if n > 0 => String::from_utf8_lossy(&buf[..n]).trim().to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::manifest::{ExecConfig, ParameterDef, ToolManifest, ToolMeta};

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

    /// Verify that a binary which exits 0 with valid ToolResult JSON on stdout
    /// is deserialised correctly.
    #[tokio::test]
    async fn execute_successful_subprocess() {
        // Use `echo` to emit a valid ToolResult on stdout.
        // `echo` prints its argument + newline and exits 0.
        let result_json = r#"{"success":true,"output":"ok","error":null}"#;

        // Build a manifest pointing at `echo`.
        let m = make_manifest("echo_tool", vec![]);

        // Construct an `echo` invocation as the binary with the JSON pre-set.
        // We use `sh -c 'echo <json>'` because the SubprocessTool feeds the
        // manifest binary with args on stdin — echo just ignores stdin.
        let script = format!("echo '{}'", result_json);
        let binary = PathBuf::from("sh");
        // Override binary to `sh` and pass `-c` + script via a wrapper.
        // Simpler: write a temp script.
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("tool.sh");
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\ncat > /dev/null\necho '{}'\n", result_json),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let tool = SubprocessTool::new(m, script_path.clone());
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("execute should not return Err");

        assert!(result.success, "expected success=true, got: {:?}", result);
        assert_eq!(result.output, "ok");
        assert!(result.error.is_none());

        let _ = script;
        let _ = binary;
    }

    /// A binary that hangs forever should be killed and return a timeout error.
    #[tokio::test]
    #[ignore = "slow: waits SUBPROCESS_TIMEOUT_SECS (~10 s) to elapse — run manually"]
    async fn execute_timeout_kills_process_and_returns_error() {
        // Script sleeps forever — SubprocessTool should kill it and return a
        // "timed out" error once SUBPROCESS_TIMEOUT_SECS elapses.
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("tool.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 60\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let m = make_manifest("sleep_tool", vec![]);
        let tool = SubprocessTool::new(m, script_path);
        let result = tool
            .execute(serde_json::json!({}))
            .await
            .expect("should not propagate Err");

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("timed out"),
            "expected 'timed out' in error, got: {}",
            err
        );
    }
}
