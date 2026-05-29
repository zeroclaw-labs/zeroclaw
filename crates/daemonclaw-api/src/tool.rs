use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
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

    /// Whether this tool can safely run concurrently with other safe tools.
    ///
    /// Returns `true` (default) for read-only tools. Override to `false` for
    /// tools that mutate shared state (file writes, shell commands, etc.).
    /// The `args` parameter allows input-dependent decisions (e.g., a shell
    /// tool might be safe for `ls` but not for `rm`).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    /// Whether this tool's results come from an untrusted external source
    /// (web, browser, MCP servers, user-provided URLs). When true, the loop
    /// wraps tool results in `<untrusted_tool_result>` tags as a prompt
    /// injection defense layer.
    fn is_untrusted_source(&self) -> bool {
        self.name().starts_with("mcp_")
    }

    /// Get the full spec for LLM registration
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}
