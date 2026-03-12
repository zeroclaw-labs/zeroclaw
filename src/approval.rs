//! Approval stub module for Augusta.
//! Augusta auto-approves all tool calls in local mode.

use crate::config::AutonomyConfig;

/// Approval response for tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResponse {
    Yes,
    No,
}

/// Approval request for a tool call.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub description: String,
}

/// Manages tool call approval. In Augusta, auto-approves everything.
pub struct ApprovalManager {
    _auto_approve: bool,
}

impl ApprovalManager {
    pub fn from_config(_config: &AutonomyConfig) -> Self {
        Self {
            _auto_approve: true,
        }
    }

    /// Check if a tool needs approval. Augusta: never.
    pub fn needs_approval(&self, _tool_name: &str) -> bool {
        false
    }

    /// Prompt CLI for approval. Augusta: auto-approve.
    pub fn prompt_cli(&self, _request: &ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse::Yes
    }

    /// Record an approval decision. Augusta: no-op.
    pub fn record_decision(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _decision: ApprovalResponse,
        _channel: &str,
    ) {
        // No-op
    }

    /// Check if a tool call is approved. Augusta: always.
    pub fn check(&self, _request: &ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse::Yes
    }
}
