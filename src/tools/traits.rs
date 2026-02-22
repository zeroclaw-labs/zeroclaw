use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Structured error classification for tool failures.
///
/// Enables the agent loop to detect repeated structural errors (e.g. policy
/// denials) and inject reflection prompts instead of blindly retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Blocked by security policy (e.g. disallowed command, forbidden path).
    PolicyDenied,
    /// File, command, or resource not found.
    NotFound,
    /// OS-level permission denied or symlink escape blocked.
    PermissionDenied,
    /// Rate limit or action budget exceeded.
    RateLimited,
    /// Operation timed out.
    Timeout,
    /// Command returned non-zero exit code or execution error.
    ExecutionFailed,
    /// Invalid or missing parameters.
    InvalidInput,
    /// Task state file was not updated during the session.
    StateNotUpdated,
    /// Unclassified error (default).
    #[default]
    Unknown,
}

impl ErrorKind {
    /// Returns a serde-style label for use in formatted error output.
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::PolicyDenied => "policy_denied",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::RateLimited => "rate_limited",
            Self::Timeout => "timeout",
            Self::ExecutionFailed => "execution_failed",
            Self::InvalidInput => "invalid_input",
            Self::StateNotUpdated => "state_not_updated",
            Self::Unknown => "unknown",
        }
    }
}

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
    /// Structured error classification for agent-loop reflection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ErrorKind>,
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
                error_kind: None,
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
            error_kind: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();

        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn tool_result_with_error_kind_serialization_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("blocked".into()),
            error_kind: Some(ErrorKind::PolicyDenied),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"error_kind\":\"policy_denied\""));

        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error_kind, Some(ErrorKind::PolicyDenied));
    }

    #[test]
    fn tool_result_without_error_kind_deserializes_as_none() {
        let json = r#"{"success":false,"output":"","error":"boom"}"#;
        let parsed: ToolResult = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.error_kind, None);
    }

    #[test]
    fn error_kind_default_is_unknown() {
        assert_eq!(ErrorKind::default(), ErrorKind::Unknown);
    }

    #[test]
    fn error_kind_as_label_returns_correct_strings() {
        assert_eq!(ErrorKind::PolicyDenied.as_label(), "policy_denied");
        assert_eq!(ErrorKind::NotFound.as_label(), "not_found");
        assert_eq!(ErrorKind::PermissionDenied.as_label(), "permission_denied");
        assert_eq!(ErrorKind::RateLimited.as_label(), "rate_limited");
        assert_eq!(ErrorKind::Timeout.as_label(), "timeout");
        assert_eq!(ErrorKind::ExecutionFailed.as_label(), "execution_failed");
        assert_eq!(ErrorKind::InvalidInput.as_label(), "invalid_input");
        assert_eq!(ErrorKind::StateNotUpdated.as_label(), "state_not_updated");
        assert_eq!(ErrorKind::Unknown.as_label(), "unknown");
    }

    #[test]
    fn error_kind_skip_serializing_when_none() {
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
            error_kind: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("error_kind"));
    }
}
