//! Observability trait stubs for Augusta.

use std::time::Duration;

/// Events recorded during agent execution.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ObserverEvent {
    AgentStart {
        provider: String,
        model: String,
    },
    AgentEnd {
        provider: String,
        model: String,
        duration: Duration,
        tokens_used: Option<u64>,
        cost_usd: Option<f64>,
    },
    LlmRequest {
        provider: String,
        model: String,
        messages_count: usize,
    },
    LlmResponse {
        provider: String,
        model: String,
        duration: Duration,
        success: bool,
        error_message: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    ToolCall {
        tool: String,
        duration: Duration,
        success: bool,
    },
    ToolCallStart {
        tool: String,
        arguments: Option<String>,
    },
    TurnComplete,
}

/// Observer trait — record events during agent execution.
pub trait Observer: Send + Sync {
    fn record_event(&self, _event: &ObserverEvent) {}
}

/// Metrics (unused stub).
#[allow(dead_code)]
pub enum ObserverMetric {}
