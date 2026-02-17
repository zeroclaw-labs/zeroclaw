//! Tool trait definition
//!
//! This defines the interface that all robot tools implement.
//! It is compatible with ZeroClaw's Tool trait but standalone.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether the tool executed successfully
    pub success: bool,
    /// Output from the tool (human-readable)
    pub output: String,
    /// Error message if failed
    pub error: Option<String>,
}

impl ToolResult {
    /// Create a successful result
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            error: None,
        }
    }

    /// Create a failed result
    pub fn error(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: String::new(),
            error: Some(error.into()),
        }
    }

    /// Create a failed result with partial output
    pub fn partial(output: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            error: Some(error.into()),
        }
    }
}

/// Description of a tool for LLM function calling
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name (used in function calls)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// JSON Schema for parameters
    pub parameters: Value,
}

/// Core tool trait
///
/// Implement this trait to create a new tool that can be used
/// by an AI agent to interact with the robot hardware.
///
/// # Example
///
/// ```rust,ignore
/// use zeroclaw_robot_kit::{Tool, ToolResult};
/// use async_trait::async_trait;
/// use serde_json::{json, Value};
///
/// pub struct BeepTool;
///
/// #[async_trait]
/// impl Tool for BeepTool {
///     fn name(&self) -> &str { "beep" }
///
///     fn description(&self) -> &str { "Make a beep sound" }
///
///     fn parameters_schema(&self) -> Value {
///         json!({
///             "type": "object",
///             "properties": {
///                 "frequency": { "type": "number", "description": "Hz" }
///             }
///         })
///     }
///
///     async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
///         let freq = args["frequency"].as_f64().unwrap_or(440.0);
///         // Play beep...
///         Ok(ToolResult::success(format!("Beeped at {}Hz", freq)))
///     }
/// }
/// ```
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in LLM function calling)
    fn name(&self) -> &str;

    /// Human-readable description of what this tool does
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's parameters
    ///
    /// This is used by the LLM to understand how to call the tool.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool with the given arguments
    ///
    /// Arguments are passed as JSON matching the parameters_schema.
    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult>;

    /// Get the full specification for LLM registration
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
