use super::traits::{Tool, ToolResult};
use crate::runtime::{RuntimeAdapter, WasmCapabilities, WasmRuntime};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for listing and executing sandboxed WASM modules.
pub struct WasmModuleTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
}

impl WasmModuleTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self { security, runtime }
    }

    fn wasm_runtime(&self) -> Option<&WasmRuntime> {
        self.runtime.as_any().downcast_ref::<WasmRuntime>()
    }

    fn parse_caps(args: &serde_json::Value) -> anyhow::Result<WasmCapabilities> {
        let read_workspace = args
            .get("read_workspace")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let write_workspace = args
            .get("write_workspace")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let fuel_override = args
            .get("fuel_override")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let memory_override_mb = args
            .get("memory_override_mb")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);

        let allowed_hosts = match args.get("allowed_hosts") {
            Some(value) => {
                let arr = value.as_array().ok_or_else(|| {
                    anyhow::anyhow!("'allowed_hosts' must be an array of strings")
                })?;
                let mut hosts = Vec::with_capacity(arr.len());
                for entry in arr {
                    let host = entry
                        .as_str()
                        .ok_or_else(|| {
                            anyhow::anyhow!("'allowed_hosts' must be an array of strings")
                        })?
                        .trim()
                        .to_string();
                    if !host.is_empty() {
                        hosts.push(host);
                    }
                }
                hosts
            }
            None => Vec::new(),
        };

        Ok(WasmCapabilities {
            read_workspace,
            write_workspace,
            allowed_hosts,
            fuel_override,
            memory_override_mb,
        })
    }
}

#[async_trait]
impl Tool for WasmModuleTool {
    fn name(&self) -> &str {
        "wasm_module"
    }

    fn description(&self) -> &str {
        "List or execute sandboxed WASM modules from runtime.wasm.tools_dir"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "run"],
                    "description": "Action to perform: list modules or run a module"
                },
                "module": {
                    "type": "string",
                    "description": "WASM module name (without .wasm extension), required when action=run"
                },
                "read_workspace": {
                    "type": "boolean",
                    "description": "Request read_workspace capability (must be allowed by runtime policy)"
                },
                "write_workspace": {
                    "type": "boolean",
                    "description": "Request write_workspace capability (must be allowed by runtime policy)"
                },
                "allowed_hosts": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Requested host allowlist subset for this invocation"
                },
                "fuel_override": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional fuel override; cannot exceed runtime.wasm.fuel_limit"
                },
                "memory_override_mb": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional memory override in MB; cannot exceed runtime.wasm.memory_limit_mb"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let Some(wasm_runtime) = self.wasm_runtime() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "wasm_module tool is only available when runtime.kind = \"wasm\"".into(),
                ),
            });
        };

        match action {
            "list" => match wasm_runtime.list_modules(&self.security.workspace_dir) {
                Ok(modules) => Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&json!({ "modules": modules }))?,
                    error: None,
                }),
                Err(err) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(err.to_string()),
                }),
            },
            "run" => {
                let module = args
                    .get("module")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Missing 'module' parameter for action=run"))?;
                let caps = Self::parse_caps(&args)?;
                match wasm_runtime.execute_module(module, &self.security.workspace_dir, &caps) {
                    Ok(result) => {
                        let output = serde_json::to_string_pretty(&json!({
                            "module": module,
                            "module_sha256": result.module_sha256,
                            "exit_code": result.exit_code,
                            "fuel_consumed": result.fuel_consumed,
                            "stdout": result.stdout,
                            "stderr": result.stderr
                        }))?;
                        let success = result.exit_code == 0;
                        let error = if success {
                            None
                        } else if result.stderr.is_empty() {
                            Some(format!("WASM module exited with code {}", result.exit_code))
                        } else {
                            Some(result.stderr)
                        };

                        Ok(ToolResult {
                            success,
                            output,
                            error,
                        })
                    }
                    Err(err) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(err.to_string()),
                    }),
                }
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported action '{other}'. Use 'list' or 'run'."
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WasmRuntimeConfig;
    use crate::runtime::NativeRuntime;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(workspace_dir: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir,
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn wasm_module_tool_name() {
        let dir = tempfile::tempdir().unwrap();
        let security = test_security(dir.path().to_path_buf());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tool = WasmModuleTool::new(security, runtime);
        assert_eq!(tool.name(), "wasm_module");
    }

    #[tokio::test]
    async fn list_action_returns_modules() {
        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("alpha.wasm"), b"\0asm").unwrap();
        std::fs::write(tools_dir.join("beta.wasm"), b"\0asm").unwrap();
        std::fs::write(tools_dir.join("bad$name.wasm"), b"\0asm").unwrap();

        let security = test_security(dir.path().to_path_buf());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tool = WasmModuleTool::new(security, runtime);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("alpha"));
        assert!(result.output.contains("beta"));
        assert!(!result.output.contains("bad$name"));
    }

    #[tokio::test]
    async fn run_action_requires_module() {
        let dir = tempfile::tempdir().unwrap();
        let security = test_security(dir.path().to_path_buf());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tool = WasmModuleTool::new(security, runtime);

        let result = tool.execute(json!({"action": "run"})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("module"));
    }

    #[tokio::test]
    async fn run_action_errors_without_runtime_wasm_feature() {
        if WasmRuntime::is_available() {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let tools_dir = dir.path().join("tools/wasm");
        std::fs::create_dir_all(&tools_dir).unwrap();
        std::fs::write(tools_dir.join("hello.wasm"), b"\0asm\x01\0\0\0").unwrap();

        let security = test_security(dir.path().to_path_buf());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tool = WasmModuleTool::new(security, runtime);

        let result = tool
            .execute(json!({"action": "run", "module": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap_or_default().contains("not available"));
    }

    #[tokio::test]
    async fn tool_rejects_non_wasm_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let security = test_security(dir.path().to_path_buf());
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
        let tool = WasmModuleTool::new(security, runtime);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .unwrap_or_default()
            .contains("runtime.kind = \"wasm\""));
    }
}
