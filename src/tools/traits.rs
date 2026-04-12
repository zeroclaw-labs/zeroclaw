use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of a tool execution.
///
/// `output` should be plain text optimized for LLM consumption.
/// Use readable formatting (not raw JSON) for structured data.
/// Include truncation notices when output is capped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

impl ToolResult {
    /// Convenience constructor for expected error results.
    ///
    /// Use for missing params, invalid input, rate limits, permission denied.
    /// Reserve `Err(anyhow)` for truly unexpected infrastructure failures only.
    pub fn err(msg: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            success: false,
            output: String::new(),
            error: Some(msg.into()),
        })
    }

    /// Convenience constructor for successful results.
    pub fn ok(output: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            success: true,
            output: output.into(),
            error: None,
        })
    }
}

/// Extract a required string parameter from JSON args, returning a `ToolResult`
/// error (not an `Err(anyhow)`) if missing. Use in `Tool::execute()` methods:
///
/// ```ignore
/// let path = require_str!(args, "path");
/// ```
#[macro_export]
macro_rules! require_str {
    ($args:expr, $param:expr) => {
        match $args.get($param).and_then(|v| v.as_str()) {
            Some(v) if !v.is_empty() => v,
            _ => {
                return $crate::tools::traits::ToolResult::err(format!(
                    "Missing required parameter '{}'",
                    $param
                ));
            }
        }
    };
}

/// Shared security gate for mutating tool actions.
pub fn enforce_security_policy(
    security: &crate::security::SecurityPolicy,
    action: &str,
) -> Option<ToolResult> {
    if !security.can_act() {
        return Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "Security policy: read-only mode, cannot perform '{action}'"
            )),
        });
    }

    if security.is_rate_limited() {
        return Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
        });
    }

    if !security.record_action() {
        return Some(ToolResult {
            success: false,
            output: String::new(),
            error: Some("Rate limit exceeded: action budget exhausted".to_string()),
        });
    }

    None
}

/// Description of a tool for the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Core tool trait — implement for any capability
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in LLM function calling)
    fn name(&self) -> &str;

    /// Human-readable description
    fn description(&self) -> &str;

    /// JSON schema for parameters
    fn parameters_schema(&self) -> serde_json::Value;

    /// Execute the tool with given arguments
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Get the full spec for LLM registration
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy_tool"
        }

        fn description(&self) -> &str {
            "A deterministic test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    #[test]
    fn spec_uses_tool_metadata_and_schema() {
        let tool = DummyTool;
        let spec = tool.spec();

        assert_eq!(spec.name, "dummy_tool");
        assert_eq!(spec.description, "A deterministic test tool");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["properties"]["value"]["type"], "string");
    }

    #[tokio::test]
    async fn execute_returns_expected_output() {
        let tool = DummyTool;
        let result = tool
            .execute(serde_json::json!({ "value": "hello-tool" }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "hello-tool");
        assert!(result.error.is_none());
    }

    #[test]
    fn tool_result_serialization_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("boom".into()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();

        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }
}
