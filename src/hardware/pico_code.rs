//! Phase 7 — Dynamic code tools: `device_read_code`, `device_write_code`, `device_exec`.
//!
//! These tools let the LLM read, write, and execute code on any connected
//! hardware device.  The `DeviceRuntime` on each device determines which
//! host-side tooling is used:
//!
//! - **MicroPython / CircuitPython** — `mpremote` for code read/write/exec.
//! - **Arduino / Nucleus / Linux** — not yet implemented; returns a clear error.
//!
//! When the `device` parameter is omitted, each tool auto-selects the device
//! only when **exactly one** device is registered.  If multiple devices are
//! present the tool returns an error and requires an explicit `device` parameter.

use super::device::{DeviceRegistry, DeviceRuntime};
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Default timeout for `mpremote` operations (seconds).
const MPREMOTE_TIMEOUT_SECS: u64 = 30;

/// Maximum time to wait for the serial port after a reset (seconds).
const PORT_WAIT_SECS: u64 = 15;

/// Polling interval when waiting for a serial port (ms).
const PORT_POLL_MS: u64 = 200;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Resolve the serial port path and runtime for a device.
///
/// If `device_alias` is provided, look it up; otherwise auto-selects the device
/// only when exactly one device is registered.  With multiple devices present,
/// returns an error requiring an explicit alias.
/// Returns `(alias, port, runtime)` or an error `ToolResult`.
async fn resolve_device_port(
    registry: &RwLock<DeviceRegistry>,
    device_alias: Option<&str>,
) -> Result<(String, String, DeviceRuntime), ToolResult> {
    let reg = registry.read().await;

    let alias: String = match device_alias {
        Some(a) => a.to_string(),
        None => {
            // Auto-select the first device.
            let all_aliases: Vec<String> =
                reg.aliases().into_iter().map(|a| a.to_string()).collect();
            match all_aliases.as_slice() {
                [single] => single.clone(),
                [] => {
                    return Err(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("no device found — is a board connected via USB?".to_string()),
                    });
                }
                multiple => {
                    return Err(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "multiple devices found ({}); specify the \"device\" parameter",
                            multiple.join(", ")
                        )),
                    });
                }
            }
        }
    };

    let device = reg.get_device(&alias).ok_or_else(|| ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!("device '{alias}' not found in registry")),
    })?;

    let runtime = device.runtime;

    let port = device.port().ok_or_else(|| ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "device '{alias}' has no serial port — is it connected?"
        )),
    })?;

    Ok((alias, port.to_string(), runtime))
}

/// Return an unsupported-runtime error `ToolResult` for a given tool name.
fn unsupported_runtime(runtime: DeviceRuntime, tool: &str) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "{runtime} runtime is not yet supported for {tool} — coming soon"
        )),
    }
}

/// Run an `mpremote` command with a timeout and return (stdout, stderr).
async fn run_mpremote(args: &[&str], timeout_secs: u64) -> Result<(String, String), String> {
    use tokio::time::timeout;

    let result = timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("mpremote").args(args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if output.status.success() {
                Ok((stdout, stderr))
            } else {
                Err(format!(
                    "mpremote failed (exit {}): {}",
                    output.status,
                    stderr.trim()
                ))
            }
        }
        Ok(Err(e)) => Err(format!(
            "mpremote not found or could not start ({e}). \
             Install it with: pip install mpremote"
        )),
        Err(_) => Err(format!(
            "mpremote timed out after {timeout_secs}s — \
             the device may be unresponsive"
        )),
    }
}

// ── DeviceReadCodeTool ────────────────────────────────────────────────────────

/// Tool: read the current `main.py` from a connected device.
///
/// The LLM uses this to understand the current program before modifying it.
pub struct DeviceReadCodeTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl DeviceReadCodeTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for DeviceReadCodeTool {
    fn name(&self) -> &str {
        "device_read_code"
    }

    fn description(&self) -> &str {
        "Read the current program (main.py) running on a connected device. \
         Use this before writing new code so you understand the current state."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device alias e.g. pico0, esp0. Auto-selected if only one device is connected."
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let device_alias = args.get("device").and_then(|v| v.as_str());

        let (alias, port, runtime) = match resolve_device_port(&self.registry, device_alias).await {
            Ok(v) => v,
            Err(tool_result) => return Ok(tool_result),
        };

        // Runtime dispatch.
        match runtime {
            DeviceRuntime::MicroPython | DeviceRuntime::CircuitPython => {}
            other => return Ok(unsupported_runtime(other, "device_read_code")),
        }

        tracing::info!(alias = %alias, port = %port, runtime = %runtime, "reading main.py from device");

        match run_mpremote(
            &["connect", &port, "cat", ":main.py"],
            MPREMOTE_TIMEOUT_SECS,
        )
        .await
        {
            Ok((stdout, _stderr)) => Ok(ToolResult {
                success: true,
                output: if stdout.trim().is_empty() {
                    format!("main.py on {alias} is empty or not found.")
                } else {
                    format!(
                        "Current main.py on {alias}:\n\n```python\n{}\n```",
                        stdout.trim()
                    )
                },
                error: None,
            }),
            Err(e) => {
                // mpremote cat fails if main.py doesn't exist — not a fatal error.
                if e.contains("OSError") || e.contains("no such file") || e.contains("ENOENT") {
                    Ok(ToolResult {
                        success: true,
                        output: format!(
                            "No main.py found on {alias} — the device has no program yet."
                        ),
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read code from {alias}: {e}")),
                    })
                }
            }
        }
    }
}

// ── DeviceWriteCodeTool ───────────────────────────────────────────────────────

/// Tool: write a complete program to a device as `main.py`.
///
/// This replaces the current `main.py` on the device and resets it so the new
/// program starts executing immediately.
pub struct DeviceWriteCodeTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl DeviceWriteCodeTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for DeviceWriteCodeTool {
    fn name(&self) -> &str {
        "device_write_code"
    }

    fn description(&self) -> &str {
        "Write a complete program to a device — replaces main.py and restarts the device. \
         Always read the current code first with device_read_code."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device alias e.g. pico0, esp0. Auto-selected if only one device is connected."
                },
                "code": {
                    "type": "string",
                    "description": "Complete program to write as main.py"
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let code = match args.get("code").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: code".to_string()),
                });
            }
        };

        if code.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("code parameter is empty — provide a program to write".to_string()),
            });
        }

        let device_alias = args.get("device").and_then(|v| v.as_str());

        let (alias, port, runtime) = match resolve_device_port(&self.registry, device_alias).await {
            Ok(v) => v,
            Err(tool_result) => return Ok(tool_result),
        };

        // Runtime dispatch.
        match runtime {
            DeviceRuntime::MicroPython | DeviceRuntime::CircuitPython => {}
            other => return Ok(unsupported_runtime(other, "device_write_code")),
        }

        tracing::info!(alias = %alias, port = %port, runtime = %runtime, code_len = code.len(), "writing main.py to device");

        // Write code to an atomic, owner-only temp file via tempfile crate.
        let named_tmp = match tokio::task::spawn_blocking(|| {
            tempfile::Builder::new()
                .prefix("zeroclaw_main_")
                .suffix(".py")
                .tempfile()
        })
        .await
        {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to create temp file: {e}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("temp file task failed: {e}")),
                });
            }
        };
        let tmp_path = named_tmp.path().to_path_buf();
        let tmp_str = tmp_path.to_string_lossy().to_string();

        if let Err(e) = tokio::fs::write(&tmp_path, code).await {
            // named_tmp dropped here — auto-removes the file.
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to write temp file: {e}")),
            });
        }

        // Deploy via mpremote: copy + reset.
        let result = run_mpremote(
            &["connect", &port, "cp", &tmp_str, ":main.py", "+", "reset"],
            MPREMOTE_TIMEOUT_SECS,
        )
        .await;

        // Explicit cleanup — log if removal fails rather than silently ignoring.
        if let Err(e) = named_tmp.close() {
            tracing::warn!(path = %tmp_str, err = %e, "failed to clean up temp file");
        }

        match result {
            Ok((_stdout, _stderr)) => {
                tracing::info!(alias = %alias, "main.py deployed and device reset");

                // Wait for the serial port to reappear after reset.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let port_reappeared = wait_for_port(
                    &port,
                    std::time::Duration::from_secs(PORT_WAIT_SECS),
                    std::time::Duration::from_millis(PORT_POLL_MS),
                )
                .await;

                if port_reappeared {
                    Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Code deployed to {alias} — main.py updated and device reset. \
                             {alias} is back online."
                        ),
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Code deployed to {alias} — main.py updated and device reset. \
                             Note: serial port did not reappear within {PORT_WAIT_SECS}s; \
                             the device may still be booting."
                        ),
                        error: None,
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to deploy code to {alias}: {e}")),
            }),
        }
    }
}

// ── DeviceExecTool ────────────────────────────────────────────────────────────

/// Tool: run a one-off code snippet on a device without modifying `main.py`.
///
/// Good for one-time commands, sensor reads, and testing code before committing.
pub struct DeviceExecTool {
    registry: Arc<RwLock<DeviceRegistry>>,
}

impl DeviceExecTool {
    pub fn new(registry: Arc<RwLock<DeviceRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for DeviceExecTool {
    fn name(&self) -> &str {
        "device_exec"
    }

    fn description(&self) -> &str {
        "Execute a code snippet on a connected device without modifying main.py. \
         Good for one-time actions, sensor reads, and testing before writing permanent code."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device alias e.g. pico0, esp0. Auto-selected if only one device is connected."
                },
                "code": {
                    "type": "string",
                    "description": "Code to execute. Output is captured and returned."
                }
            },
            "required": ["code"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let code = match args.get("code").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("missing required parameter: code".to_string()),
                });
            }
        };

        if code.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "code parameter is empty — provide a code snippet to execute".to_string(),
                ),
            });
        }

        let device_alias = args.get("device").and_then(|v| v.as_str());

        let (alias, port, runtime) = match resolve_device_port(&self.registry, device_alias).await {
            Ok(v) => v,
            Err(tool_result) => return Ok(tool_result),
        };

        // Runtime dispatch.
        match runtime {
            DeviceRuntime::MicroPython | DeviceRuntime::CircuitPython => {}
            other => return Ok(unsupported_runtime(other, "device_exec")),
        }

        tracing::info!(alias = %alias, port = %port, runtime = %runtime, code_len = code.len(), "executing snippet on device");

        // Write snippet to an atomic, owner-only temp file via tempfile crate.
        let named_tmp = match tokio::task::spawn_blocking(|| {
            tempfile::Builder::new()
                .prefix("zeroclaw_exec_")
                .suffix(".py")
                .tempfile()
        })
        .await
        {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to create temp file: {e}")),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("temp file task failed: {e}")),
                });
            }
        };
        let tmp_path = named_tmp.path().to_path_buf();
        let tmp_str = tmp_path.to_string_lossy().to_string();

        if let Err(e) = tokio::fs::write(&tmp_path, code).await {
            // named_tmp dropped here — auto-removes the file.
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("failed to write temp file: {e}")),
            });
        }

        // Execute via mpremote run (does NOT modify main.py).
        let result =
            run_mpremote(&["connect", &port, "run", &tmp_str], MPREMOTE_TIMEOUT_SECS).await;

        // Explicit cleanup — log if removal fails rather than silently ignoring.
        if let Err(e) = named_tmp.close() {
            tracing::warn!(path = %tmp_str, err = %e, "failed to clean up temp file");
        }

        match result {
            Ok((stdout, stderr)) => {
                let output = if stdout.trim().is_empty() && !stderr.trim().is_empty() {
                    // Some MicroPython output goes to stderr (e.g. exceptions).
                    stderr.trim().to_string()
                } else {
                    stdout.trim().to_string()
                };

                Ok(ToolResult {
                    success: true,
                    output: if output.is_empty() {
                        format!("Code executed on {alias} — no output produced.")
                    } else {
                        format!("Output from {alias}:\n{output}")
                    },
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute code on {alias}: {e}")),
            }),
        }
    }
}

// ── port wait helper ──────────────────────────────────────────────────────────

/// Poll for a specific serial port to reappear after a device reset.
///
/// Returns `true` if the port exists within the timeout, `false` otherwise.
async fn wait_for_port(
    port_path: &str,
    timeout: std::time::Duration,
    interval: std::time::Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if std::path::Path::new(port_path).exists() {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

/// Factory function: create all Phase 7 dynamic code tools.
pub fn device_code_tools(registry: Arc<RwLock<DeviceRegistry>>) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(DeviceReadCodeTool::new(registry.clone())),
        Box::new(DeviceWriteCodeTool::new(registry.clone())),
        Box::new(DeviceExecTool::new(registry)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> Arc<RwLock<DeviceRegistry>> {
        Arc::new(RwLock::new(DeviceRegistry::new()))
    }

    // ── DeviceReadCodeTool ───────────────────────────────────────────

    #[test]
    fn device_read_code_name() {
        let tool = DeviceReadCodeTool::new(empty_registry());
        assert_eq!(tool.name(), "device_read_code");
    }

    #[test]
    fn device_read_code_schema_valid() {
        let tool = DeviceReadCodeTool::new(empty_registry());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["device"].is_object());
    }

    #[tokio::test]
    async fn device_read_code_no_device_returns_error() {
        let tool = DeviceReadCodeTool::new(empty_registry());
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap_or("").contains("no device"),
            "expected 'no device' error; got: {:?}",
            result.error
        );
    }

    // ── DeviceWriteCodeTool ──────────────────────────────────────────

    #[test]
    fn device_write_code_name() {
        let tool = DeviceWriteCodeTool::new(empty_registry());
        assert_eq!(tool.name(), "device_write_code");
    }

    #[test]
    fn device_write_code_schema_requires_code() {
        let tool = DeviceWriteCodeTool::new(empty_registry());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("code")),
            "code should be required"
        );
    }

    #[tokio::test]
    async fn device_write_code_empty_code_rejected() {
        let tool = DeviceWriteCodeTool::new(empty_registry());
        let result = tool.execute(json!({"code": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("empty"));
    }

    #[tokio::test]
    async fn device_write_code_no_device_returns_error() {
        let tool = DeviceWriteCodeTool::new(empty_registry());
        let result = tool
            .execute(json!({"code": "print('hello')"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("no device"),);
    }

    // ── DeviceExecTool ───────────────────────────────────────────────

    #[test]
    fn device_exec_name() {
        let tool = DeviceExecTool::new(empty_registry());
        assert_eq!(tool.name(), "device_exec");
    }

    #[test]
    fn device_exec_schema_requires_code() {
        let tool = DeviceExecTool::new(empty_registry());
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("code")),
            "code should be required"
        );
    }

    #[tokio::test]
    async fn device_exec_empty_code_rejected() {
        let tool = DeviceExecTool::new(empty_registry());
        let result = tool.execute(json!({"code": "  "})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("empty"));
    }

    #[tokio::test]
    async fn device_exec_no_device_returns_error() {
        let tool = DeviceExecTool::new(empty_registry());
        let result = tool.execute(json!({"code": "print(1+1)"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("no device"),);
    }

    // ── Factory ──────────────────────────────────────────────────────

    #[test]
    fn factory_returns_three_tools() {
        let reg = empty_registry();
        let tools = device_code_tools(reg);
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"device_read_code"));
        assert!(names.contains(&"device_write_code"));
        assert!(names.contains(&"device_exec"));
    }

    #[test]
    fn all_specs_valid() {
        let reg = empty_registry();
        let tools = device_code_tools(reg);
        for tool in &tools {
            let spec = tool.spec();
            assert!(!spec.name.is_empty());
            assert!(!spec.description.is_empty());
            assert_eq!(spec.parameters["type"], "object");
        }
    }
}
