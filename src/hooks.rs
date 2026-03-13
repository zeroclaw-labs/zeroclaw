//! Hook stubs for Augusta.
//! Augusta does not support lifecycle hooks — these are no-ops.
//! The async signatures are required for API compatibility with the agent loop.

use crate::providers::ChatMessage;
use crate::tools::ToolResult;
use std::time::Duration;

/// Result of a before-tool-call hook.
pub enum HookResult {
    /// Cancel the tool call with a reason.
    Cancel(String),
    /// Continue with (possibly modified) tool name and arguments.
    Continue((String, serde_json::Value)),
}

/// Hook runner — no-op in Augusta.
pub struct HookRunner;

#[allow(clippy::unused_async)]
impl HookRunner {
    /// Fire before an LLM call (void, no-op).
    pub async fn fire_llm_input(&self, _history: &[ChatMessage], _model: &str) {}

    /// Run before a tool call. Always returns Continue (pass-through).
    pub async fn run_before_tool_call(&self, name: String, args: serde_json::Value) -> HookResult {
        HookResult::Continue((name, args))
    }

    /// Fire after a tool call (void, no-op).
    pub async fn fire_after_tool_call(
        &self,
        _tool_name: &str,
        _result: &ToolResult,
        _duration: Duration,
    ) {
    }
}
