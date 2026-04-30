use std::time::Duration;

/// Discrete events emitted by the agent runtime for observability.
///
/// Each variant represents a lifecycle event that observers can record,
/// aggregate, or forward to external monitoring systems. Events carry
/// just enough context for tracing and diagnostics without exposing
/// sensitive prompt or response content.
///
/// Marked `#[non_exhaustive]` so out-of-tree observer implementations
/// degrade gracefully when new variants are added in future minor
/// releases — they must include a wildcard arm in their `match`
/// expressions and will simply ignore unknown event kinds.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ObserverEvent {
    /// The agent orchestration loop has started a new session.
    AgentStart { provider: String, model: String },
    /// A request is about to be sent to an LLM provider.
    ///
    /// This is emitted immediately before a provider call so observers can print
    /// user-facing progress without leaking prompt contents.
    LlmRequest {
        provider: String,
        model: String,
        messages_count: usize,
    },
    /// Result of a single LLM provider call.
    LlmResponse {
        provider: String,
        model: String,
        duration: Duration,
        success: bool,
        error_message: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// The agent session has finished.
    ///
    /// Carries aggregate usage data (tokens, cost) when the provider reports it.
    AgentEnd {
        provider: String,
        model: String,
        duration: Duration,
        tokens_used: Option<u64>,
        cost_usd: Option<f64>,
    },
    /// A tool call is about to be executed.
    ToolCallStart {
        tool: String,
        /// Provider-assigned tool call identifier, when the underlying tool
        /// call originated from a native structured tool call block (e.g.
        /// OpenAI `tool_calls[].id`, Anthropic `tool_use.id`). `None` for
        /// text-parsed (XML/markdown) tool calls.
        ///
        /// Observers can correlate `ToolCallStart` → `ToolCall` → the
        /// emitting LLM response via this id.
        tool_call_id: Option<String>,
        /// Full JSON arguments the agent passed to the tool. `None` when
        /// arguments are unavailable at the call site.
        arguments: Option<String>,
    },
    /// A tool call has completed with a success/failure outcome.
    ToolCall {
        tool: String,
        /// Provider-assigned tool call identifier, when present. See
        /// [`ObserverEvent::ToolCallStart::tool_call_id`].
        tool_call_id: Option<String>,
        duration: Duration,
        success: bool,
        /// Full JSON arguments the agent passed to the tool.
        ///
        /// Carried here (in addition to `ToolCallStart`) so observers that
        /// build a single completed span per tool call — e.g. the OTel
        /// exporter — can attach arguments at span-end time without holding
        /// per-call state.
        arguments: Option<String>,
        /// Scrubbed tool output or error reason. Populated for both success
        /// and failure outcomes so backends can show the actual tool result
        /// in trace viewers. Credentials are scrubbed before this field is
        /// emitted.
        result: Option<String>,
    },
    /// A memory recall (search) operation has completed.
    ///
    /// Emitted at the runtime boundary after a hybrid-search query against
    /// the brain DB. Carries an optional `query_summary` for diagnostics —
    /// scrubbed and truncated user text, not a privacy-clean token. The
    /// runtime applies `scrub_credentials` first and then truncates to ≤200
    /// content chars (with a 3-char `...` ellipsis appended when truncation
    /// occurred); short non-credential queries pass through unchanged.
    MemoryRecall {
        /// Scrubbed and truncated query summary. `None` when the caller has
        /// no meaningful query to record (e.g., session-scoped fetches).
        query_summary: Option<String>,
        duration: Duration,
        /// Number of `MemoryEntry` rows returned by the recall call.
        num_entries: usize,
        /// Bounded backend identifier (e.g. `"sqlite"`, `"qdrant"`, `"none"`).
        backend: String,
        success: bool,
    },
    /// A memory store (write) operation has completed.
    ///
    /// Emitted after persisting a memory entry. Carries only bounded
    /// fields — the raw memory `key` is intentionally omitted because
    /// keys can encode high-cardinality identifiers (UUIDs, phone
    /// numbers, message timestamps) that would blow up Prometheus label
    /// series.
    MemoryStore {
        /// Memory category (`"core"`, `"daily"`, `"conversation"`, etc.) —
        /// bounded set, safe to use as a Prometheus label.
        category: String,
        /// Bounded backend identifier (e.g. `"sqlite"`, `"qdrant"`).
        backend: String,
        duration: Duration,
        success: bool,
    },
    /// A RAG retrieval pass has completed.
    ///
    /// Emitted after vector + keyword retrieval against the hardware
    /// datasheet index. Reports cardinalities only; carries an optional
    /// scrubbed-and-truncated query summary on the same terms as
    /// [`MemoryRecall`]. Has no `success` field because the underlying
    /// `rag.retrieve` call is synchronous and infallible.
    RagRetrieve {
        query_summary: Option<String>,
        duration: Duration,
        num_chunks: usize,
        num_boards: usize,
    },
    /// The agent produced a final answer for the current user message.
    TurnComplete,
    /// A message was sent or received through a channel.
    ChannelMessage {
        /// Channel name (e.g., `"telegram"`, `"discord"`).
        channel: String,
        /// `"inbound"` or `"outbound"`.
        direction: String,
    },
    /// Periodic heartbeat tick from the runtime keep-alive loop.
    HeartbeatTick,
    /// Response cache hit — an LLM call was avoided.
    CacheHit {
        /// `"hot"` (in-memory) or `"warm"` (SQLite).
        cache_type: String,
        /// Estimated tokens saved by this cache hit.
        tokens_saved: u64,
    },
    /// Response cache miss — the prompt was not found in cache.
    CacheMiss {
        /// `"response"` cache layer that was checked.
        cache_type: String,
    },
    /// An error occurred in a named component.
    Error {
        /// Subsystem where the error originated (e.g., `"provider"`, `"gateway"`).
        component: String,
        /// Human-readable error description. Must not contain secrets or tokens.
        message: String,
    },
    /// A hand has started execution.
    HandStarted { hand_name: String },
    /// A hand has completed execution successfully.
    HandCompleted {
        hand_name: String,
        duration_ms: u64,
        findings_count: usize,
    },
    /// A hand has failed during execution.
    HandFailed {
        hand_name: String,
        error: String,
        duration_ms: u64,
    },
    /// A deployment has started.
    DeploymentStarted {
        /// Identifier for the deployment (e.g., commit SHA or release tag).
        deploy_id: String,
    },
    /// A deployment has completed successfully.
    DeploymentCompleted {
        deploy_id: String,
        /// Commit SHA that was deployed.
        commit_sha: String,
    },
    /// A deployment has failed.
    DeploymentFailed {
        deploy_id: String,
        /// Human-readable failure reason.
        reason: String,
    },
    /// Recovery from a failed deployment has completed.
    RecoveryCompleted { deploy_id: String },
}

/// Numeric metrics emitted by the agent runtime.
///
/// Observers can aggregate these into dashboards, alerts, or structured logs.
/// Each variant carries a single scalar value with implicit units.
#[derive(Debug, Clone)]
pub enum ObserverMetric {
    /// Time elapsed for a single LLM or tool request.
    RequestLatency(Duration),
    /// Number of tokens consumed by an LLM call.
    TokensUsed(u64),
    /// Current number of active concurrent sessions.
    ActiveSessions(u64),
    /// Current depth of the inbound message queue.
    QueueDepth(u64),
    /// Duration of a single hand run.
    HandRunDuration {
        hand_name: String,
        duration: Duration,
    },
    /// Number of findings produced by a hand run.
    HandFindingsCount { hand_name: String, count: u64 },
    /// Records a hand run outcome for success-rate tracking.
    HandSuccessRate { hand_name: String, success: bool },
    /// Time elapsed from commit to deployment (lead time for changes).
    DeploymentLeadTime(Duration),
    /// Time elapsed to recover from a failed deployment.
    RecoveryTime(Duration),
}

/// Core observability trait for recording agent runtime telemetry.
///
/// Implement this trait to integrate with any monitoring backend (structured
/// logging, Prometheus, OpenTelemetry, etc.). The agent runtime holds one or
/// more `Observer` instances and calls [`record_event`](Observer::record_event)
/// and [`record_metric`](Observer::record_metric) at key lifecycle points.
///
/// Implementations must be `Send + Sync + 'static` because the observer is
/// shared across async tasks via `Arc`.
pub trait Observer: Send + Sync + 'static {
    /// Record a discrete lifecycle event.
    ///
    /// Called synchronously on the hot path; implementations should avoid
    /// blocking I/O. Buffer events internally and flush asynchronously
    /// when possible.
    fn record_event(&self, event: &ObserverEvent);

    /// Record a numeric metric sample.
    ///
    /// Called synchronously; same non-blocking guidance as
    /// [`record_event`](Observer::record_event).
    fn record_metric(&self, metric: &ObserverMetric);

    /// Flush any buffered telemetry data to the backend.
    ///
    /// The runtime calls this during graceful shutdown. The default
    /// implementation is a no-op, which is appropriate for backends
    /// that write synchronously.
    fn flush(&self) {}

    /// Return the human-readable name of this observer backend.
    ///
    /// Used in logs and diagnostics (e.g., `"console"`, `"prometheus"`,
    /// `"opentelemetry"`).
    fn name(&self) -> &str;

    /// Downcast to `Any` for backend-specific operations.
    ///
    /// Enables callers to access concrete observer types when needed
    /// (e.g., retrieving a Prometheus registry handle for custom metrics).
    fn as_any(&self) -> &dyn std::any::Any;
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct DummyObserver {
        events: Mutex<u64>,
        metrics: Mutex<u64>,
    }

    impl Observer for DummyObserver {
        fn record_event(&self, _event: &ObserverEvent) {
            let mut guard = self.events.lock();
            *guard += 1;
        }

        fn record_metric(&self, _metric: &ObserverMetric) {
            let mut guard = self.metrics.lock();
            *guard += 1;
        }

        fn name(&self) -> &str {
            "dummy-observer"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn observer_records_events_and_metrics() {
        let observer = DummyObserver::default();

        observer.record_event(&ObserverEvent::HeartbeatTick);
        observer.record_event(&ObserverEvent::Error {
            component: "test".into(),
            message: "boom".into(),
        });
        observer.record_metric(&ObserverMetric::TokensUsed(42));

        assert_eq!(*observer.events.lock(), 2);
        assert_eq!(*observer.metrics.lock(), 1);
    }

    #[test]
    fn observer_default_flush_and_as_any_work() {
        let observer = DummyObserver::default();

        observer.flush();
        assert_eq!(observer.name(), "dummy-observer");
        assert!(observer.as_any().downcast_ref::<DummyObserver>().is_some());
    }

    #[test]
    fn observer_event_and_metric_are_cloneable() {
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: Some("call_abc123".into()),
            duration: Duration::from_millis(10),
            success: true,
            arguments: Some(r#"{"command":"date"}"#.into()),
            result: Some("Mon Apr 22 12:00:00 UTC 2026\n".into()),
        };
        let metric = ObserverMetric::RequestLatency(Duration::from_millis(8));

        let cloned_event = event.clone();
        let cloned_metric = metric.clone();

        assert!(matches!(cloned_event, ObserverEvent::ToolCall { .. }));
        assert!(matches!(cloned_metric, ObserverMetric::RequestLatency(_)));
    }

    #[test]
    fn memory_event_variants_are_cloneable() {
        let recall = ObserverEvent::MemoryRecall {
            query_summary: Some("user asked about coffee orders".into()),
            duration: Duration::from_millis(40),
            num_entries: 3,
            backend: "sqlite".into(),
            success: true,
        };
        let store = ObserverEvent::MemoryStore {
            category: "conversation".into(),
            backend: "sqlite".into(),
            duration: Duration::from_millis(8),
            success: true,
        };
        let rag = ObserverEvent::RagRetrieve {
            query_summary: None,
            duration: Duration::from_millis(120),
            num_chunks: 5,
            num_boards: 2,
        };

        assert!(matches!(recall.clone(), ObserverEvent::MemoryRecall { .. }));
        assert!(matches!(store.clone(), ObserverEvent::MemoryStore { .. }));
        assert!(matches!(rag.clone(), ObserverEvent::RagRetrieve { .. }));
    }

    #[test]
    fn hand_events_recordable() {
        let observer = DummyObserver::default();

        observer.record_event(&ObserverEvent::HandStarted {
            hand_name: "review".into(),
        });
        observer.record_event(&ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 1500,
            findings_count: 3,
        });
        observer.record_event(&ObserverEvent::HandFailed {
            hand_name: "review".into(),
            error: "timeout".into(),
            duration_ms: 5000,
        });

        assert_eq!(*observer.events.lock(), 3);
    }

    #[test]
    fn hand_metrics_recordable() {
        let observer = DummyObserver::default();

        observer.record_metric(&ObserverMetric::HandRunDuration {
            hand_name: "review".into(),
            duration: Duration::from_millis(1500),
        });
        observer.record_metric(&ObserverMetric::HandFindingsCount {
            hand_name: "review".into(),
            count: 3,
        });
        observer.record_metric(&ObserverMetric::HandSuccessRate {
            hand_name: "review".into(),
            success: true,
        });

        assert_eq!(*observer.metrics.lock(), 3);
    }

    #[test]
    fn hand_event_and_metric_are_cloneable() {
        let event = ObserverEvent::HandCompleted {
            hand_name: "review".into(),
            duration_ms: 500,
            findings_count: 2,
        };
        let metric = ObserverMetric::HandRunDuration {
            hand_name: "review".into(),
            duration: Duration::from_millis(500),
        };

        let cloned_event = event.clone();
        let cloned_metric = metric.clone();

        assert!(matches!(cloned_event, ObserverEvent::HandCompleted { .. }));
        assert!(matches!(
            cloned_metric,
            ObserverMetric::HandRunDuration { .. }
        ));
    }
}
