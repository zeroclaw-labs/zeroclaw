//! DynamicToolFactory trait and v1 factory implementations.
//!
//! This module defines the `DynamicToolFactory` trait for building `Tool`
//! instances from persisted `DynamicToolDef` definitions, along with two
//! concrete factories: `ShellCommandFactory` and `HttpEndpointFactory`.

use super::dynamic_registry::DynamicToolDef;
use super::traits::{Tool, ToolResult};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// ToolBuildContext
// ---------------------------------------------------------------------------

/// Context passed to factories when building a `Tool` from a `DynamicToolDef`.
pub struct ToolBuildContext {
    pub security: Arc<SecurityPolicy>,
    pub workspace_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// DynamicToolFactory trait
// ---------------------------------------------------------------------------

/// Factory trait for constructing `Tool` instances from dynamic definitions.
pub trait DynamicToolFactory: Send + Sync {
    /// The kind string this factory handles (e.g. `"shell_command"`).
    fn kind(&self) -> &'static str;

    /// Validate that the config JSON is structurally and semantically correct.
    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()>;

    /// Build a concrete `Tool` from the given definition and context.
    fn build(&self, def: &DynamicToolDef, ctx: &ToolBuildContext) -> anyhow::Result<Arc<dyn Tool>>;
}

// ---------------------------------------------------------------------------
// ShellCommandFactory
// ---------------------------------------------------------------------------

/// Template for a single argument to a shell command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArgTemplate {
    /// A literal argument value passed as-is.
    Fixed(String),
    /// A parameter name substituted from the tool call arguments.
    Param(String),
}

/// Configuration for a `shell_command` dynamic tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellCommandConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<ArgTemplate>,
    pub working_dir: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// Factory that produces `ShellCommandDynamicTool` instances.
pub struct ShellCommandFactory;

impl DynamicToolFactory for ShellCommandFactory {
    fn kind(&self) -> &'static str {
        "shell_command"
    }

    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()> {
        let cfg: ShellCommandConfig = serde_json::from_value(config.clone())
            .map_err(|e| anyhow::anyhow!("invalid shell_command config: {e}"))?;

        if cfg.command.trim().is_empty() {
            anyhow::bail!("shell_command: command must not be empty");
        }

        if let Some(timeout) = cfg.timeout_secs {
            if timeout > 300 {
                anyhow::bail!("shell_command: timeout_secs must be <= 300, got {timeout}");
            }
        }

        Ok(())
    }

    fn build(&self, def: &DynamicToolDef, ctx: &ToolBuildContext) -> anyhow::Result<Arc<dyn Tool>> {
        self.validate(&def.config)?;

        let cfg: ShellCommandConfig = serde_json::from_value(def.config.clone())?;

        Ok(Arc::new(ShellCommandDynamicTool {
            name: def.name.clone(),
            description: def.description.clone(),
            config: cfg,
            security: ctx.security.clone(),
            workspace_dir: ctx.workspace_dir.clone(),
        }))
    }
}

/// A dynamically built tool that executes a shell command.
struct ShellCommandDynamicTool {
    name: String,
    description: String,
    config: ShellCommandConfig,
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

#[async_trait]
impl Tool for ShellCommandDynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();

        for arg in &self.config.args {
            if let ArgTemplate::Param(param_name) = arg {
                properties.insert(param_name.clone(), serde_json::json!({ "type": "string" }));
                required.push(serde_json::Value::String(param_name.clone()));
            }
        }

        serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Enforce security policy.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "shell_command dynamic tool")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Build arguments by substituting Param values from execute args.
        let mut cmd_args: Vec<String> = Vec::new();
        for arg_tmpl in &self.config.args {
            match arg_tmpl {
                ArgTemplate::Fixed(val) => cmd_args.push(val.clone()),
                ArgTemplate::Param(param_name) => {
                    let value = args
                        .get(param_name)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing required parameter: {param_name}")
                        })?;
                    cmd_args.push(value.to_string());
                }
            }
        }

        // Determine working directory.
        let work_dir = self
            .config
            .working_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.workspace_dir.clone());

        // Determine timeout.
        let timeout_secs = self.config.timeout_secs.unwrap_or(30).min(300);
        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        // Execute via tokio::process::Command.
        let child = tokio::process::Command::new(&self.config.command)
            .args(&cmd_args)
            .current_dir(&work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();

        let child = match child {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to spawn command: {e}")),
                });
            }
        };

        // Wait with timeout.
        let result = tokio::time::timeout(timeout_duration, child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    Ok(ToolResult {
                        success: true,
                        output: stdout,
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: false,
                        output: stdout,
                        error: Some(format!(
                            "command exited with status {}: {}",
                            output.status, stderr
                        )),
                    })
                }
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("command I/O error: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("command timed out after {timeout_secs}s")),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// HttpEndpointFactory
// ---------------------------------------------------------------------------

/// Configuration for an `http_endpoint` dynamic tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpEndpointConfig {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub body_template: Option<String>,
    pub timeout_secs: Option<u64>,
    pub max_response_bytes: Option<usize>,
}

const MAX_HTTP_TIMEOUT_SECS: u64 = 60;
const MAX_HTTP_RESPONSE_BYTES: usize = 1_048_576;

/// Factory that produces `HttpEndpointDynamicTool` instances.
pub struct HttpEndpointFactory;

impl DynamicToolFactory for HttpEndpointFactory {
    fn kind(&self) -> &'static str {
        "http_endpoint"
    }

    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()> {
        let cfg: HttpEndpointConfig = serde_json::from_value(config.clone())
            .map_err(|e| anyhow::anyhow!("invalid http_endpoint config: {e}"))?;

        if cfg.url.trim().is_empty() {
            anyhow::bail!("http_endpoint: url must not be empty");
        }

        let method_upper = cfg.method.to_uppercase();
        if !matches!(method_upper.as_str(), "GET" | "POST" | "PUT" | "DELETE") {
            anyhow::bail!(
                "http_endpoint: method must be one of GET, POST, PUT, DELETE; got '{}'",
                cfg.method
            );
        }

        if let Some(timeout) = cfg.timeout_secs {
            if timeout > MAX_HTTP_TIMEOUT_SECS {
                anyhow::bail!(
                    "http_endpoint: timeout_secs must be <= {MAX_HTTP_TIMEOUT_SECS}, got {timeout}"
                );
            }
        }

        if let Some(max_bytes) = cfg.max_response_bytes {
            if max_bytes > MAX_HTTP_RESPONSE_BYTES {
                anyhow::bail!(
                    "http_endpoint: max_response_bytes must be <= {MAX_HTTP_RESPONSE_BYTES}, got {max_bytes}"
                );
            }
        }

        Ok(())
    }

    fn build(&self, def: &DynamicToolDef, ctx: &ToolBuildContext) -> anyhow::Result<Arc<dyn Tool>> {
        self.validate(&def.config)?;

        let mut cfg: HttpEndpointConfig = serde_json::from_value(def.config.clone())?;
        // Normalize method to uppercase.
        cfg.method = cfg.method.to_uppercase();

        Ok(Arc::new(HttpEndpointDynamicTool {
            name: def.name.clone(),
            description: def.description.clone(),
            config: cfg,
            security: ctx.security.clone(),
        }))
    }
}

/// A dynamically built tool that calls an HTTP endpoint.
struct HttpEndpointDynamicTool {
    name: String,
    description: String,
    config: HttpEndpointConfig,
    security: Arc<SecurityPolicy>,
}

#[async_trait]
impl Tool for HttpEndpointDynamicTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Enforce security policy.
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "http_endpoint dynamic tool")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let timeout_secs = self
            .config
            .timeout_secs
            .unwrap_or(MAX_HTTP_TIMEOUT_SECS)
            .min(MAX_HTTP_TIMEOUT_SECS);
        let max_bytes = self
            .config
            .max_response_bytes
            .unwrap_or(MAX_HTTP_RESPONSE_BYTES)
            .min(MAX_HTTP_RESPONSE_BYTES);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;

        let mut request = match self.config.method.as_str() {
            "GET" => client.get(&self.config.url),
            "POST" => client.post(&self.config.url),
            "PUT" => client.put(&self.config.url),
            "DELETE" => client.delete(&self.config.url),
            other => anyhow::bail!("unsupported HTTP method: {other}"),
        };

        for (key, value) in &self.config.headers {
            request = request.header(key.as_str(), value.as_str());
        }

        if let Some(body) = &self.config.body_template {
            request = request.body(body.clone());
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("HTTP request failed: {e}")),
                });
            }
        };

        let status = response.status();
        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("failed to read response body: {e}")),
                });
            }
        };

        // Truncate to max_response_bytes.
        let truncated = if body_bytes.len() > max_bytes {
            &body_bytes[..max_bytes]
        } else {
            &body_bytes[..]
        };

        let body_str = String::from_utf8_lossy(truncated).to_string();

        if status.is_success() {
            Ok(ToolResult {
                success: true,
                output: body_str,
                error: None,
            })
        } else {
            Ok(ToolResult {
                success: false,
                output: body_str,
                error: Some(format!("HTTP {status}")),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Factory registry
// ---------------------------------------------------------------------------

/// Build the default factory registry with all built-in dynamic tool factories.
pub fn default_factory_registry() -> HashMap<String, Box<dyn DynamicToolFactory>> {
    let mut map: HashMap<String, Box<dyn DynamicToolFactory>> = HashMap::new();
    map.insert("shell_command".into(), Box::new(ShellCommandFactory));
    map.insert("http_endpoint".into(), Box::new(HttpEndpointFactory));
    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn sample_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            max_actions_per_hour: 100,
            ..SecurityPolicy::default()
        })
    }

    fn sample_build_ctx() -> ToolBuildContext {
        ToolBuildContext {
            security: sample_security(),
            workspace_dir: PathBuf::from("/tmp/zeroclaw_test_workspace"),
        }
    }

    fn sample_shell_tool_def() -> DynamicToolDef {
        DynamicToolDef {
            id: "tool-shell-001".into(),
            name: "run_echo".into(),
            description: "Runs echo with arguments".into(),
            kind: "shell_command".into(),
            config: serde_json::json!({
                "command": "echo",
                "args": [
                    { "Fixed": "hello" },
                    { "Param": "message" }
                ],
                "timeout_secs": 10
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: Some("zeroclaw_user".into()),
        }
    }

    fn sample_http_tool_def() -> DynamicToolDef {
        DynamicToolDef {
            id: "tool-http-001".into(),
            name: "fetch_status".into(),
            description: "Fetches a status endpoint".into(),
            kind: "http_endpoint".into(),
            config: serde_json::json!({
                "url": "https://example.com/status",
                "method": "GET",
                "headers": { "Accept": "application/json" },
                "timeout_secs": 10
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: Some("zeroclaw_user".into()),
        }
    }

    // ------------------------------------------------------------------
    // 1. shell_command_factory_validates_valid_config
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_factory_validates_valid_config() {
        let factory = ShellCommandFactory;
        let config = serde_json::json!({
            "command": "echo",
            "args": [{ "Fixed": "hello" }],
            "timeout_secs": 30
        });
        assert!(factory.validate(&config).is_ok());
    }

    // ------------------------------------------------------------------
    // 2. shell_command_factory_rejects_empty_command
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_factory_rejects_empty_command() {
        let factory = ShellCommandFactory;
        let config = serde_json::json!({
            "command": "",
            "args": []
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "expected empty command error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // 3. shell_command_factory_rejects_excessive_timeout
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_factory_rejects_excessive_timeout() {
        let factory = ShellCommandFactory;
        let config = serde_json::json!({
            "command": "echo",
            "timeout_secs": 999
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("timeout_secs must be <= 300"),
            "expected timeout error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // 4. shell_command_factory_builds_tool
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_factory_builds_tool() {
        let factory = ShellCommandFactory;
        let def = sample_shell_tool_def();
        let ctx = sample_build_ctx();
        let tool = factory.build(&def, &ctx);
        assert!(tool.is_ok(), "build should succeed");
    }

    // ------------------------------------------------------------------
    // 5. shell_command_tool_has_correct_name
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_tool_has_correct_name() {
        let factory = ShellCommandFactory;
        let def = sample_shell_tool_def();
        let ctx = sample_build_ctx();
        let tool = factory.build(&def, &ctx).unwrap();
        assert_eq!(tool.name(), "run_echo");
        assert_eq!(tool.description(), "Runs echo with arguments");
    }

    // ------------------------------------------------------------------
    // 6. shell_command_tool_generates_param_schema
    // ------------------------------------------------------------------
    #[test]
    fn shell_command_tool_generates_param_schema() {
        let factory = ShellCommandFactory;
        let def = sample_shell_tool_def();
        let ctx = sample_build_ctx();
        let tool = factory.build(&def, &ctx).unwrap();
        let schema = tool.parameters_schema();

        assert_eq!(schema["type"], "object");
        assert!(
            schema["properties"]["message"]["type"] == "string",
            "Param 'message' should generate a string property"
        );
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.contains(&serde_json::Value::String("message".into())),
            "Param 'message' should be in required list"
        );
    }

    // ------------------------------------------------------------------
    // 7. http_endpoint_factory_validates_valid_config
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_validates_valid_config() {
        let factory = HttpEndpointFactory;
        let config = serde_json::json!({
            "url": "https://example.com/api",
            "method": "post",
            "headers": { "Content-Type": "application/json" },
            "body_template": "{\"key\": \"value\"}",
            "timeout_secs": 30
        });
        assert!(factory.validate(&config).is_ok());
    }

    // ------------------------------------------------------------------
    // 8. http_endpoint_factory_rejects_invalid_method
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_rejects_invalid_method() {
        let factory = HttpEndpointFactory;
        let config = serde_json::json!({
            "url": "https://example.com",
            "method": "PATCH"
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("must be one of"),
            "expected method error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // 9. http_endpoint_factory_rejects_excessive_timeout
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_rejects_excessive_timeout() {
        let factory = HttpEndpointFactory;
        let config = serde_json::json!({
            "url": "https://example.com",
            "method": "GET",
            "timeout_secs": 120
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("timeout_secs must be <= 60"),
            "expected timeout error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // 10. http_endpoint_factory_builds_tool
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_builds_tool() {
        let factory = HttpEndpointFactory;
        let def = sample_http_tool_def();
        let ctx = sample_build_ctx();
        let tool = factory.build(&def, &ctx);
        assert!(tool.is_ok(), "build should succeed");
        let tool = tool.unwrap();
        assert_eq!(tool.name(), "fetch_status");
        assert_eq!(tool.description(), "Fetches a status endpoint");
    }

    // ------------------------------------------------------------------
    // 11. factory_registry_has_expected_kinds
    // ------------------------------------------------------------------
    #[test]
    fn factory_registry_has_expected_kinds() {
        let registry = default_factory_registry();
        assert!(registry.contains_key("shell_command"));
        assert!(registry.contains_key("http_endpoint"));
        assert_eq!(registry.len(), 2);

        assert_eq!(registry["shell_command"].kind(), "shell_command");
        assert_eq!(registry["http_endpoint"].kind(), "http_endpoint");
    }

    // ------------------------------------------------------------------
    // 12. shell_command_tool_execute_echo (async - actually runs echo)
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn shell_command_tool_execute_echo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let factory = ShellCommandFactory;
        let def = DynamicToolDef {
            id: "tool-echo-test".into(),
            name: "echo_test".into(),
            description: "Echoes hello".into(),
            kind: "shell_command".into(),
            config: serde_json::json!({
                "command": "echo",
                "args": [{ "Fixed": "hello" }],
                "timeout_secs": 5
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: None,
        };

        let ctx = ToolBuildContext {
            security: sample_security(),
            workspace_dir: tmp.path().to_path_buf(),
        };
        let tool = factory.build(&def, &ctx).unwrap();
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(result.success, "echo should succeed: {:?}", result.error);
        assert_eq!(result.output.trim(), "hello");
        assert!(result.error.is_none());
    }

    // ------------------------------------------------------------------
    // Extra: http_endpoint_factory_rejects_empty_url
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_rejects_empty_url() {
        let factory = HttpEndpointFactory;
        let config = serde_json::json!({
            "url": "",
            "method": "GET"
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("url must not be empty"),
            "expected url error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // Extra: http_endpoint_factory_rejects_excessive_response_bytes
    // ------------------------------------------------------------------
    #[test]
    fn http_endpoint_factory_rejects_excessive_response_bytes() {
        let factory = HttpEndpointFactory;
        let config = serde_json::json!({
            "url": "https://example.com",
            "method": "GET",
            "max_response_bytes": 2_000_000
        });
        let err = factory.validate(&config).unwrap_err();
        assert!(
            err.to_string().contains("max_response_bytes must be <="),
            "expected max_response_bytes error, got: {err}"
        );
    }

    // ------------------------------------------------------------------
    // Extra: shell_command_tool_blocked_in_readonly_mode
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn shell_command_tool_blocked_in_readonly_mode() {
        use crate::security::AutonomyLevel;

        let readonly_security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });

        let factory = ShellCommandFactory;
        let def = DynamicToolDef {
            id: "tool-readonly-test".into(),
            name: "readonly_test".into(),
            description: "Should be blocked".into(),
            kind: "shell_command".into(),
            config: serde_json::json!({
                "command": "echo",
                "args": [{ "Fixed": "test" }]
            }),
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            created_by: None,
        };

        let ctx = ToolBuildContext {
            security: readonly_security,
            workspace_dir: PathBuf::from("/tmp"),
        };

        let tool = factory.build(&def, &ctx).unwrap();
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode"),
            "expected read-only error, got: {:?}",
            result.error
        );
    }
}
