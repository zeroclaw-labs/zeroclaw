//! Server-Sent Events (SSE) stream for real-time event delivery.
//!
//! Wraps the broadcast channel in AppState to deliver events to web dashboard clients.

use super::AppState;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

/// Thread-safe ring buffer that retains recent events for history replay.
pub struct EventBuffer {
    inner: Mutex<VecDeque<serde_json::Value>>,
    capacity: usize,
}

impl EventBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Push an event into the buffer, evicting the oldest if at capacity.
    pub fn push(&self, event: serde_json::Value) {
        let mut buf = self.inner.lock().unwrap();
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(event);
    }

    /// Return a snapshot of all buffered events (oldest first).
    pub fn snapshot(&self) -> Vec<serde_json::Value> {
        self.inner.lock().unwrap().iter().cloned().collect()
    }
}

/// GET /api/events — SSE event stream
pub async fn handle_sse_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Auth check
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .unwrap_or("");

        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token>",
            )
                .into_response();
        }
    }

    let rx = state.event_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(
        |result: Result<
            serde_json::Value,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >| {
            match result {
                Ok(value) if is_public_sse_event(&value) => Some(Ok::<_, Infallible>(
                    Event::default().data(value.to_string()),
                )),
                Ok(_) => None,
                Err(_) => None, // Skip lagged messages
            }
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// GET /api/events/history — return buffered recent events as JSON.
pub async fn handle_events_history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = super::api::require_auth(&state, &headers) {
        return e.into_response();
    }
    let events: Vec<_> = state
        .event_buffer
        .snapshot()
        .into_iter()
        .filter(is_public_sse_event)
        .collect();
    Json(serde_json::json!({ "events": events })).into_response()
}

/// Returns true for events that should be visible on the global SSE stream.
///
/// Contract: broadcast events must not include `session_id` unless they are
/// intentionally scoped to that session and hidden from global `/api/events`.
fn is_public_sse_event(event: &serde_json::Value) -> bool {
    event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .is_none()
}

/// Broadcast observer that fans events out to SSE subscribers.
///
/// Installed as the process-wide broadcast hook by [`crate::run_gateway`] so
/// that events recorded by *any* observer built through
/// `observability::create_observer` — including the per-call observer the
/// agent loop creates inside `process_message` — also reach `/api/events`
/// clients.
///
/// Crate-private: the constructor signature is intentionally not part of any
/// stable surface, since it is wired directly into `run_gateway`.
pub(crate) struct BroadcastObserver {
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    buffer: Arc<EventBuffer>,
}

impl BroadcastObserver {
    pub(crate) fn new(
        tx: tokio::sync::broadcast::Sender<serde_json::Value>,
        buffer: Arc<EventBuffer>,
    ) -> Self {
        Self { tx, buffer }
    }
}

impl zeroclaw_runtime::observability::Observer for BroadcastObserver {
    fn record_event(&self, event: &zeroclaw_runtime::observability::ObserverEvent) {
        // Recording into the primary observer (logs / Prometheus) is the
        // responsibility of whoever built the event source; `TeeObserver`
        // takes care of that fan-out. Here we only translate to JSON and
        // ship to SSE subscribers.
        let json = match event {
            zeroclaw_runtime::observability::ObserverEvent::LlmRequest {
                provider, model, ..
            } => serde_json::json!({
                "type": "llm_request",
                "provider": provider,
                "model": model,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            zeroclaw_runtime::observability::ObserverEvent::ToolCall {
                tool,
                duration,
                success,
            } => serde_json::json!({
                "type": "tool_call",
                "tool": tool,
                "duration_ms": duration.as_millis(),
                "success": success,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            zeroclaw_runtime::observability::ObserverEvent::ToolCallStart { tool, .. } => {
                serde_json::json!({
                    "type": "tool_call_start",
                    "tool": tool,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            zeroclaw_runtime::observability::ObserverEvent::Error { component, message } => {
                serde_json::json!({
                    "type": "error",
                    "component": component,
                    "message": message,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            zeroclaw_runtime::observability::ObserverEvent::AgentStart { provider, model } => {
                serde_json::json!({
                    "type": "agent_start",
                    "provider": provider,
                    "model": model,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            zeroclaw_runtime::observability::ObserverEvent::AgentEnd {
                provider,
                model,
                duration,
                tokens_used,
                cost_usd,
            } => serde_json::json!({
                "type": "agent_end",
                "provider": provider,
                "model": model,
                "duration_ms": duration.as_millis(),
                "tokens_used": tokens_used,
                "cost_usd": cost_usd,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            _ => return, // Skip events we don't broadcast
        };

        self.buffer.push(json.clone());
        let _ = self.tx.send(json);
    }

    fn record_metric(&self, _metric: &zeroclaw_runtime::observability::traits::ObserverMetric) {
        // Metrics are not broadcast over SSE; the primary observer records them.
    }

    fn name(&self) -> &str {
        "broadcast"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_runtime::observability::{Observer, ObserverEvent};

    fn make_broadcast() -> (
        Arc<BroadcastObserver>,
        tokio::sync::broadcast::Receiver<serde_json::Value>,
        Arc<EventBuffer>,
    ) {
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let obs = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        (obs, rx, buffer)
    }

    #[test]
    fn tool_call_event_is_broadcast_and_buffered() {
        let (obs, mut rx, buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: std::time::Duration::from_millis(42),
            success: true,
        });

        let value = rx.try_recv().expect("event should be broadcast");
        assert_eq!(value["type"], "tool_call");
        assert_eq!(value["tool"], "shell");
        assert_eq!(value["success"], true);

        let snap = buffer.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["type"], "tool_call");
    }

    #[test]
    fn tool_call_start_event_is_broadcast() {
        let (obs, mut rx, _buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "mcp_filesystem__read_file".into(),
            arguments: None,
        });

        let value = rx.try_recv().expect("event should be broadcast");
        assert_eq!(value["type"], "tool_call_start");
        assert_eq!(value["tool"], "mcp_filesystem__read_file");
    }

    #[test]
    fn unmapped_events_are_skipped() {
        let (obs, mut rx, buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::HeartbeatTick);

        assert!(rx.try_recv().is_err(), "heartbeat should not broadcast");
        assert!(buffer.snapshot().is_empty());
    }

    #[test]
    fn session_scoped_events_are_not_public_sse_events() {
        let session_event = serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "content": "private session notification"
        });
        let global_event = serde_json::json!({
            "type": "tool_call",
            "tool": "shell"
        });

        assert!(!is_public_sse_event(&session_event));
        assert!(is_public_sse_event(&global_event));
    }

    /// End-to-end coverage of the wiring `run_gateway` performs at startup:
    /// installing `BroadcastObserver` as the process-wide broadcast hook and
    /// then building an observer through `create_observer` (the path the
    /// agent loop takes inside `process_message`) must surface events on the
    /// SSE broadcast channel. Codifies the load-bearing ordering so that
    /// reordering or dropping `set_broadcast_hook` in `run_gateway` is caught
    /// by `cargo test`, not by a silent regression in production.
    #[test]
    fn factory_observer_events_reach_broadcast_hook() {
        // The broadcast hook is process-wide; serialize hook-touching tests
        // within this test binary so they don't observe each other's state.
        static HOOK_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _guard = HOOK_TEST_LOCK.lock();

        zeroclaw_runtime::observability::clear_broadcast_hook();

        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let bo: Arc<dyn Observer> = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        zeroclaw_runtime::observability::set_broadcast_hook(bo);

        // Same factory call site as `process_message` in the agent loop.
        let cfg = zeroclaw_config::schema::ObservabilityConfig {
            backend: "noop".into(),
            ..Default::default()
        };
        let observer = zeroclaw_runtime::observability::create_observer(&cfg);

        observer.record_event(&ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: std::time::Duration::from_millis(7),
            success: true,
        });

        let value = rx
            .try_recv()
            .expect("factory-built observer event must reach the SSE broadcast channel");
        assert_eq!(value["type"], "tool_call");
        assert_eq!(value["tool"], "shell");
        assert_eq!(value["success"], true);

        let snap = buffer.snapshot();
        assert_eq!(
            snap.len(),
            1,
            "broadcast events must also land in the buffer"
        );

        zeroclaw_runtime::observability::clear_broadcast_hook();
    }
}
