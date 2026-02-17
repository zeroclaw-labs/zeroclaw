use std::time::Duration;

/// Events the observer can record
#[derive(Debug, Clone)]
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
    },
    ToolCallStart {
        tool: String,
    },
    ToolCall {
        tool: String,
        duration: Duration,
        success: bool,
    },
    TurnComplete,
    ChannelMessage {
        channel: String,
        direction: String,
    },
    HeartbeatTick,
    Error {
        component: String,
        message: String,
    },
    // LLM request/response tracking
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
    },
}

/// Numeric metrics
#[derive(Debug, Clone)]
pub enum ObserverMetric {
    RequestLatency(Duration),
    TokensUsed(u64),
    ActiveSessions(u64),
    QueueDepth(u64),
}

/// Core observability trait — implement for any backend
pub trait Observer: Send + Sync {
    /// Record a discrete event
    fn record_event(&self, event: &ObserverEvent);

    /// Record a numeric metric
    fn record_metric(&self, metric: &ObserverMetric);

    /// Flush any buffered data (no-op for most backends)
    fn flush(&self) {}

    /// Human-readable name of this observer
    fn name(&self) -> &str;

    /// Downcast support for backend-specific operations (e.g. Prometheus encoding)
    fn as_any(&self) -> &dyn std::any::Any;
}
