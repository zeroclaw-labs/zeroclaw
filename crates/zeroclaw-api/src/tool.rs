use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Boilerplate-collapsing macro: pair a concrete `Tool` impl with a
/// matching `Attributable` impl that surfaces the supplied `ToolKind`
/// and uses the tool's `name()` as its alias.
///
/// Invoke once per `Tool` struct, in the same module as the struct:
///
/// ```ignore
/// crate::tool_attribution!(ShellTool, ::zeroclaw_api::attribution::ToolKind::Shell);
/// ```
#[macro_export]
macro_rules! tool_attribution {
    ($ty:ty, $kind:expr) => {
        impl $crate::attribution::Attributable for $ty {
            fn role(&self) -> $crate::attribution::Role {
                $crate::attribution::Role::Tool($kind)
            }
            fn alias(&self) -> &str {
                <Self as $crate::tool::Tool>::name(self)
            }
        }
    };
}

/// Bulk-impl `Attributable` for one or more `Tool` mock types in a
/// test module. Every type gets `Role::Tool(ToolKind::Plugin)` and uses
/// the mock's own `name()` as the alias — sufficient for test
/// scaffolding where individual kinds don't matter.
///
/// ```ignore
/// zeroclaw_api::mock_tool_attribution!(CountingTool, FailingTool);
/// ```
#[macro_export]
macro_rules! mock_tool_attribution {
    ($($ty:ty),+ $(,)?) => {
        $(
            $crate::tool_attribution!($ty, $crate::attribution::ToolKind::Plugin);
        )+
    };
}

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

/// Core tool trait — implement for any capability.
///
/// Every `Tool` is `Attributable`: log emissions and audit traces from
/// a tool call carry the same `<kind>.<alias>` composite the rest of
/// the runtime uses for channels, providers, and memory. The supertrait
/// bound makes `&dyn Tool` coerce to `&dyn Attributable` automatically,
/// so dispatch-site logging can attribute without knowing the concrete
/// tool type.
#[async_trait]
pub trait Tool: Send + Sync + crate::attribution::Attributable {
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
