use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserUseConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_browser_use_endpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default = "default_browser_use_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_browser_use_max_steps")]
    pub max_steps: u32,
}

fn default_browser_use_endpoint() -> String {
    "http://127.0.0.1:9222".into()
}

fn default_browser_use_timeout_ms() -> u64 {
    60_000
}

fn default_browser_use_max_steps() -> u32 {
    15
}

impl Default for BrowserUseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_browser_use_endpoint(),
            auth_token: None,
            timeout_ms: default_browser_use_timeout_ms(),
            max_steps: default_browser_use_max_steps(),
        }
    }
}

pub struct BrowserUseTool {
    security: Arc<SecurityPolicy>,
    config: BrowserUseConfig,
}

impl BrowserUseTool {
    pub fn new(security: Arc<SecurityPolicy>, config: BrowserUseConfig) -> Self {
        Self { security, config }
    }
}

#[derive(Debug, Clone, Copy)]
enum BrowserUseAction {
    Navigate,
    Extract,
    Form,
    Observe,
    Task,
}

impl BrowserUseAction {
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "navigate" => Ok(Self::Navigate),
            "extract" => Ok(Self::Extract),
            "form" => Ok(Self::Form),
            "observe" => Ok(Self::Observe),
            "task" => Ok(Self::Task),
            other => anyhow::bail!("Unknown browser_use action: {other}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Navigate => "navigate",
            Self::Extract => "extract",
            Self::Form => "form",
            Self::Observe => "observe",
            Self::Task => "task",
        }
    }
}

#[derive(Deserialize)]
struct SidecarResponse {
    success: Option<bool>,
    data: Option<Value>,
    error: Option<String>,
}

#[async_trait]
impl Tool for BrowserUseTool {
    fn name(&self) -> &str {
        "browser_use"
    }

    fn description(&self) -> &str {
        "Agentic browser automation via browser-use sidecar. Accepts natural language goals."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate", "extract", "form", "observe", "task"],
                    "description": "Browser action to perform"
                },
                "goal": {
                    "type": "string",
                    "description": "Natural language goal for the browser agent"
                },
                "url": {
                    "type": "string",
                    "description": "Target URL (optional, used with navigate/extract/form)"
                },
                "max_steps": {
                    "type": "integer",
                    "description": "Maximum steps for the browser agent (overrides config default)"
                },
                "form_data": {
                    "type": "object",
                    "description": "Key-value form field data (used with form action)"
                }
            },
            "required": ["action", "goal"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Security policy denies actions (read-only mode)".into()),
            });
        }

        let action_str = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: action"))?;

        let action = BrowserUseAction::from_str(action_str)?;

        let goal = args
            .get("goal")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: goal"))?;

        let max_steps = args
            .get("max_steps")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(self.config.max_steps);

        let mut body = json!({
            "goal": goal,
            "max_steps": max_steps,
            "metadata": {
                "source": "zeroclaw.browser_use",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        if let Some(url) = args.get("url").and_then(Value::as_str) {
            body["url"] = json!(url);
        }

        if let Some(form_data) = args.get("form_data") {
            body["form_data"] = form_data.clone();
        }

        let endpoint = format!(
            "{}/{}",
            self.config.endpoint.trim_end_matches('/'),
            action.as_str()
        );

        debug!(action = action.as_str(), endpoint = %endpoint, "browser_use request");

        let client = crate::config::build_runtime_proxy_client("tool.browser_use");
        let mut request = client
            .post(&endpoint)
            .timeout(Duration::from_millis(self.config.timeout_ms))
            .json(&body);

        if let Some(token) = self.config.auth_token.as_deref() {
            let token = token.trim();
            if !token.is_empty() {
                request = request.bearer_auth(token);
            }
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Failed to call browser-use sidecar at {endpoint}"))?;

        let status = response.status();
        let response_body = response
            .text()
            .await
            .context("Failed to read browser-use sidecar response body")?;

        if let Ok(parsed) = serde_json::from_str::<SidecarResponse>(&response_body) {
            if status.is_success() && parsed.success.unwrap_or(true) {
                let output = parsed
                    .data
                    .map(|data| serde_json::to_string_pretty(&data).unwrap_or_default())
                    .unwrap_or_else(|| {
                        serde_json::to_string_pretty(&json!({
                            "action": action.as_str(),
                            "ok": true,
                        }))
                        .unwrap_or_default()
                    });

                return Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                });
            }

            let error = parsed.error.or_else(|| {
                if status.is_success() && parsed.success == Some(false) {
                    Some("browser-use sidecar returned success=false".into())
                } else {
                    Some(format!(
                        "browser-use sidecar request failed with status {status}"
                    ))
                }
            });

            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error,
            });
        }

        if status.is_success() {
            return Ok(ToolResult {
                success: true,
                output: response_body,
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "browser-use sidecar request failed with status {status}: {}",
                response_body.trim()
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_roundtrip() {
        for name in &["navigate", "extract", "form", "observe", "task"] {
            let action = BrowserUseAction::from_str(name).unwrap();
            assert_eq!(action.as_str(), *name);
        }
    }

    #[test]
    fn action_rejects_unknown() {
        assert!(BrowserUseAction::from_str("invalid").is_err());
    }

    #[test]
    fn config_default_values() {
        let cfg = BrowserUseConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.endpoint, "http://127.0.0.1:9222");
        assert!(cfg.auth_token.is_none());
        assert_eq!(cfg.timeout_ms, 60_000);
        assert_eq!(cfg.max_steps, 15);
    }

    #[test]
    fn config_serde_roundtrip() {
        let cfg = BrowserUseConfig {
            enabled: true,
            endpoint: "http://127.0.0.1:9333".into(),
            auth_token: Some("test-token".into()),
            timeout_ms: 30_000,
            max_steps: 25,
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        let parsed: BrowserUseConfig = toml::from_str(&toml_str).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.endpoint, "http://127.0.0.1:9333");
        assert_eq!(parsed.auth_token.as_deref(), Some("test-token"));
        assert_eq!(parsed.timeout_ms, 30_000);
        assert_eq!(parsed.max_steps, 25);
    }

    #[test]
    fn schema_has_required_fields() {
        let tool = BrowserUseTool::new(
            Arc::new(SecurityPolicy::default()),
            BrowserUseConfig::default(),
        );
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["goal"].is_object());
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["properties"]["max_steps"].is_object());
        assert!(schema["properties"]["form_data"].is_object());

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
        assert!(required.contains(&json!("goal")));
    }

    #[test]
    fn spec_generation() {
        let tool = BrowserUseTool::new(
            Arc::new(SecurityPolicy::default()),
            BrowserUseConfig::default(),
        );
        let spec = tool.spec();
        assert_eq!(spec.name, "browser_use");
        assert!(!spec.description.is_empty());
        assert!(spec.parameters.is_object());
    }

    #[tokio::test]
    async fn execute_denied_in_readonly() {
        use crate::security::AutonomyLevel;

        let policy = SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        };
        let tool = BrowserUseTool::new(Arc::new(policy), BrowserUseConfig::default());
        let result = tool
            .execute(json!({"action": "navigate", "goal": "go to example.com"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_rejects_missing_action() {
        let tool = BrowserUseTool::new(
            Arc::new(SecurityPolicy::default()),
            BrowserUseConfig::default(),
        );
        let result = tool.execute(json!({"goal": "test"})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_missing_goal() {
        let tool = BrowserUseTool::new(
            Arc::new(SecurityPolicy::default()),
            BrowserUseConfig::default(),
        );
        let result = tool.execute(json!({"action": "navigate"})).await;
        assert!(result.is_err());
    }
}
