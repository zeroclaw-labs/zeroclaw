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
                Ok(value) => Some(Ok::<_, Infallible>(
                    Event::default().data(value.to_string()),
                )),
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
    let events = state.event_buffer.snapshot();
    Json(serde_json::json!({ "events": events })).into_response()
}

/// Broadcast observer that forwards events to the SSE broadcast channel.
pub struct BroadcastObserver {
    inner: Box<dyn zeroclaw_runtime::observability::Observer>,
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    buffer: Arc<EventBuffer>,
}

impl BroadcastObserver {
    pub fn new(
        inner: Box<dyn zeroclaw_runtime::observability::Observer>,
        tx: tokio::sync::broadcast::Sender<serde_json::Value>,
        buffer: Arc<EventBuffer>,
    ) -> Self {
        Self { inner, tx, buffer }
    }

    pub fn inner(&self) -> &dyn zeroclaw_runtime::observability::Observer {
        self.inner.as_ref()
    }
}

impl zeroclaw_runtime::observability::Observer for BroadcastObserver {
    fn record_event(&self, event: &zeroclaw_runtime::observability::ObserverEvent) {
        // Forward to inner observer
        self.inner.record_event(event);

        // Broadcast to SSE subscribers
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
            zeroclaw_runtime::observability::ObserverEvent::LlmResponse {
                provider,
                model,
                duration,
                success,
                error_message,
                input_tokens,
                output_tokens,
            } => serde_json::json!({
                "type": "llm_response",
                "provider": provider,
                "model": model,
                "duration_ms": duration.as_millis(),
                "success": success,
                "error_message": error_message,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            zeroclaw_runtime::observability::ObserverEvent::TurnComplete => {
                serde_json::json!({
                    "type": "turn_complete",
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            zeroclaw_runtime::observability::ObserverEvent::CacheHit {
                cache_type,
                tokens_saved,
            } => serde_json::json!({
                "type": "cache_hit",
                "cache_type": cache_type,
                "tokens_saved": tokens_saved,
                "timestamp": chrono::Utc::now().to_rfc3339(),
            }),
            zeroclaw_runtime::observability::ObserverEvent::CacheMiss { cache_type } => {
                serde_json::json!({
                    "type": "cache_miss",
                    "cache_type": cache_type,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            _ => return, // Skip events we don't broadcast
        };

        self.buffer.push(json.clone());
        let _ = self.tx.send(json);
    }

    fn record_metric(&self, metric: &zeroclaw_runtime::observability::traits::ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str {
        "broadcast"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
