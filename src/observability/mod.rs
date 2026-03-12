//! Observability stubs — no-op implementations for Augusta.

pub mod traits;

pub use traits::{Observer, ObserverEvent, ObserverMetric};

use crate::config::ObservabilityConfig;

/// No-op observer — discards all events.
pub struct NoopObserver;

impl Observer for NoopObserver {}

/// Create an observer from config. Always returns NoopObserver for Augusta.
pub fn create_observer(_config: &ObservabilityConfig) -> NoopObserver {
    NoopObserver
}

/// Runtime trace — no-op module for Augusta.
pub mod runtime_trace {
    /// Record a runtime trace event (no-op).
    pub fn record_event(
        _event_type: &str,
        _channel: Option<&str>,
        _provider: Option<&str>,
        _model: Option<&str>,
        _turn_id: Option<&str>,
        _tool: Option<&str>,
        _error: Option<&str>,
        _data: serde_json::Value,
    ) {
        // No-op in Augusta
    }
}
