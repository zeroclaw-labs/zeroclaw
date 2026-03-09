use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Maximum `gws` command execution time before kill.
const GWS_TIMEOUT_SECS: u64 = 30;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Allowed Google Workspace services that gws can target.
const DEFAULT_ALLOWED_SERVICES: &[&str] = &[
    "drive",
    "sheets",
    "gmail",
    "calendar",
    "docs",
    "slides",
    "tasks",
    "people",
    "chat",
    "classroom",
    "forms",
    "keep",
    "meet",
    "events",
];

/// Google Workspace CLI (`gws`) integration tool.
///
/// Wraps the `gws` CLI binary to give the agent structured access to
/// Google Workspace services (Drive, Gmail, Calendar, Sheets, etc.).
/// Requires `gws` to be installed and authenticated (`gws auth login`).
pub struct GoogleWorkspaceTool {
    security: Arc<SecurityPolicy>,
    allowed_services: Vec<String>,
}

impl GoogleWorkspaceTool {
    /// Create a new `GoogleWorkspaceTool`.
    ///
    /// If `allowed_services` is empty, the default service set is used.
    pub fn new(security: Arc<SecurityPolicy>, allowed_services: Vec<String>) -> Self {
        let services = if allowed_services.is_empty() {
            DEFAULT_ALLOWED_SERVICES
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        } else {
            allowed_services
        };
        Self {
            security,
            allowed_services: services,
        }
    }
}

#[async_trait]
impl Tool for GoogleWorkspaceTool {
    fn name(&self) -> &str {
        "google_workspace"
    }

    fn description(&self) -> &str {
        "Interact with Google Workspace services (Drive, Gmail, Calendar, Sheets, Docs, etc.) \
         via the gws CLI. Requires gws to be installed and authenticated."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "service": {
                    "type": "string",
                    "description": "Google Workspace service (e.g. drive, gmail, calendar, sheets, docs, slides, tasks, people, chat, forms, keep, meet)"
                },
                "resource": {
                    "type": "string",
                    "description": "Service resource (e.g. files, messages, events, spreadsheets)"
                },
                "method": {
                    "type": "string",
                    "description": "Method to call on the resource (e.g. list, get, create, update, delete)"
                },
                "sub_resource": {
                    "type": "string",
                    "description": "Optional sub-resource for nested operations"
                },
                "params": {
                    "type": "object",
                    "description": "URL/query parameters as key-value pairs (passed as --params JSON)"
                },
                "body": {
                    "type": "object",
                    "description": "Request body for POST/PATCH/PUT operations (passed as --json JSON)"
                },
                "format": {
                    "type": "string",
                    "enum": ["json", "table", "yaml", "csv"],
                    "description": "Output format (default: json)"
                },
                "page_all": {
                    "type": "boolean",
                    "description": "Auto-paginate through all results"
                },
                "page_limit": {
                    "type": "integer",
                    "description": "Max pages to fetch when using page_all (default: 10)"
                }
            },
            "required": ["service", "resource", "method"]
        })
    }

    /// Execute a Google Workspace CLI command with input validation and security enforcement.
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let service = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'service' parameter"))?;
        let resource = args
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'resource' parameter"))?;
        let method = args
            .get("method")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'method' parameter"))?;

        // Security checks
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        // Validate service is in the allowlist
        if !self.allowed_services.iter().any(|s| s == service) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Service '{service}' is not in the allowed services list. \
                     Allowed: {}",
                    self.allowed_services.join(", ")
                )),
            });
        }

        // Validate inputs contain no shell metacharacters
        for (label, value) in [
            ("service", service),
            ("resource", resource),
            ("method", method),
        ] {
            if !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Invalid characters in '{label}': only alphanumeric, underscore, and hyphen are allowed"
                    )),
                });
            }
        }

        // Build the gws command — validate all optional fields before consuming budget
        let mut cmd_args: Vec<String> = vec![service.to_string(), resource.to_string()];

        if let Some(sub_resource_value) = args.get("sub_resource") {
            let sub_resource = match sub_resource_value.as_str() {
                Some(s) => s,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'sub_resource' must be a string".into()),
                    })
                }
            };
            if !sub_resource
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Invalid characters in 'sub_resource': only alphanumeric, underscore, and hyphen are allowed"
                            .into(),
                    ),
                });
            }
            cmd_args.push(sub_resource.to_string());
        }

        cmd_args.push(method.to_string());

        if let Some(params) = args.get("params") {
            if !params.is_object() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'params' must be an object".into()),
                });
            }
            cmd_args.push("--params".into());
            cmd_args.push(params.to_string());
        }

        if let Some(body) = args.get("body") {
            if !body.is_object() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'body' must be an object".into()),
                });
            }
            cmd_args.push("--json".into());
            cmd_args.push(body.to_string());
        }

        if let Some(format_value) = args.get("format") {
            let format = match format_value.as_str() {
                Some(s) => s,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'format' must be a string".into()),
                    })
                }
            };
            match format {
                "json" | "table" | "yaml" | "csv" => {
                    cmd_args.push("--format".into());
                    cmd_args.push(format.to_string());
                }
                _ => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Invalid format '{format}': must be json, table, yaml, or csv"
                        )),
                    });
                }
            }
        }

        let page_all = match args.get("page_all") {
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'page_all' must be a boolean".into()),
                    })
                }
            },
            None => false,
        };
        if page_all {
            cmd_args.push("--page-all".into());
        }

        let page_limit = match args.get("page_limit") {
            Some(v) => match v.as_u64() {
                Some(n) => Some(n),
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'page_limit' must be a non-negative integer".into()),
                    })
                }
            },
            None => None,
        };
        if page_all || page_limit.is_some() {
            cmd_args.push("--page-limit".into());
            cmd_args.push(page_limit.unwrap_or(10).to_string());
        }

        // Charge action budget only after all validation passes
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        let mut cmd = tokio::process::Command::new("gws");
        cmd.args(&cmd_args);
        cmd.env_clear();
        // gws needs PATH to find itself and HOME/APPDATA for credential storage
        for key in &["PATH", "HOME", "APPDATA", "USERPROFILE", "LANG", "TERM"] {
            if let Ok(val) = std::env::var(key) {
                cmd.env(key, val);
            }
        }

        let result =
            tokio::time::timeout(Duration::from_secs(GWS_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if stdout.len() > MAX_OUTPUT_BYTES {
                    // Find a valid char boundary at or before MAX_OUTPUT_BYTES
                    let mut boundary = MAX_OUTPUT_BYTES;
                    while boundary > 0 && !stdout.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    stdout.truncate(boundary);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    let mut boundary = MAX_OUTPUT_BYTES;
                    while boundary > 0 && !stderr.is_char_boundary(boundary) {
                        boundary -= 1;
                    }
                    stderr.truncate(boundary);
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
                error: Some(format!(
                    "Failed to execute gws: {e}. Is gws installed? Run: npm install -g @googleworkspace/cli"
                )),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "gws command timed out after {GWS_TIMEOUT_SECS}s and was killed"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn tool_name() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        assert_eq!(tool.name(), "google_workspace");
    }

    #[test]
    fn tool_description_non_empty() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn tool_schema_has_required_fields() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["service"].is_object());
        assert!(schema["properties"]["resource"].is_object());
        assert!(schema["properties"]["method"].is_object());
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        assert!(required.contains(&json!("service")));
        assert!(required.contains(&json!("resource")));
        assert!(required.contains(&json!("method")));
    }

    #[test]
    fn default_allowed_services_populated() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        assert!(!tool.allowed_services.is_empty());
        assert!(tool.allowed_services.contains(&"drive".to_string()));
        assert!(tool.allowed_services.contains(&"gmail".to_string()));
        assert!(tool.allowed_services.contains(&"calendar".to_string()));
    }

    #[test]
    fn custom_allowed_services_override_defaults() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec!["drive".into(), "sheets".into()]);
        assert_eq!(tool.allowed_services.len(), 2);
        assert!(tool.allowed_services.contains(&"drive".to_string()));
        assert!(tool.allowed_services.contains(&"sheets".to_string()));
        assert!(!tool.allowed_services.contains(&"gmail".to_string()));
    }

    #[tokio::test]
    async fn rejects_disallowed_service() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec!["drive".into()]);
        let result = tool
            .execute(json!({
                "service": "gmail",
                "resource": "users",
                "method": "list"
            }))
            .await
            .expect("disallowed service should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not in the allowed"));
    }

    #[tokio::test]
    async fn rejects_shell_injection_in_service() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec!["drive; rm -rf /".into()]);
        let result = tool
            .execute(json!({
                "service": "drive; rm -rf /",
                "resource": "files",
                "method": "list"
            }))
            .await
            .expect("shell injection should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Invalid characters"));
    }

    #[tokio::test]
    async fn rejects_shell_injection_in_resource() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files$(whoami)",
                "method": "list"
            }))
            .await
            .expect("shell injection should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Invalid characters"));
    }

    #[tokio::test]
    async fn rejects_invalid_format() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list",
                "format": "xml"
            }))
            .await
            .expect("invalid format should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Invalid format"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_params() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list",
                "params": "not_an_object"
            }))
            .await
            .expect("wrong type params should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("'params' must be an object"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_body() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "create",
                "body": "not_an_object"
            }))
            .await
            .expect("wrong type body should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("'body' must be an object"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_page_all() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list",
                "page_all": "yes"
            }))
            .await
            .expect("wrong type page_all should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("'page_all' must be a boolean"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_page_limit() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list",
                "page_limit": "ten"
            }))
            .await
            .expect("wrong type page_limit should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("'page_limit' must be a non-negative integer"));
    }

    #[tokio::test]
    async fn rejects_wrong_type_sub_resource() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list",
                "sub_resource": 123
            }))
            .await
            .expect("wrong type sub_resource should return a result");
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("'sub_resource' must be a string"));
    }

    #[tokio::test]
    async fn missing_required_param_returns_error() {
        let tool = GoogleWorkspaceTool::new(test_security(), vec![]);
        let result = tool.execute(json!({"service": "drive"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rate_limited_returns_error() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = GoogleWorkspaceTool::new(security, vec![]);
        let result = tool
            .execute(json!({
                "service": "drive",
                "resource": "files",
                "method": "list"
            }))
            .await
            .expect("rate-limited should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[test]
    fn gws_timeout_is_reasonable() {
        assert_eq!(GWS_TIMEOUT_SECS, 30);
    }
}
