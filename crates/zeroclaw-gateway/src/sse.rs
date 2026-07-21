//! Server-Sent Events (SSE) stream for real-time event delivery.
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

/// GET /api/events — SSE event stream.
///
/// Pairing credentials (QR payloads, one-shot pair codes) are **broadcast-only
/// and delivery-once**: they ride the live `event_tx` fan-out and are never
/// written to the history buffer or the persisted JSONL. A subscriber must be
/// connected *before* pairing to observe them; a client that connects late,
/// reconnects, or lags past the broadcast ring (the discarded
/// `BroadcastStreamRecvError` below) deliberately cannot recover the credential
/// — that is the non-persistent boundary, not a bug. Recovery would require
/// buffering the secret, which the credential boundary forbids.
pub async fn handle_sse_events(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Auth check. When pairing is enabled every subscriber that reaches the
    // stream below has passed the bearer check, so the stream is authenticated;
    // when it is disabled no subscriber is authenticated. That posture decides
    // whether broadcast-only pairing secrets may ride the stream (see
    // `sse_frame_for_stream`).
    let auth_enforced = state.pairing.require_pairing();
    if auth_enforced {
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
        move |result: Result<
            serde_json::Value,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >| {
            match result {
                Ok(value) => sse_frame_for_stream(value, auth_enforced)
                    .map(|v| Ok::<_, Infallible>(Event::default().data(v.to_string()))),
                Err(_) => None, // Skip lagged messages
            }
        },
    );

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Decide the deliverable form of a broadcast frame for an SSE stream with the
/// given authentication posture. Returns `None` to withhold the frame.
///
/// Fail-closed contract: a frame carrying broadcast-only pairing secrets
/// (stamped [`zeroclaw_log::EPHEMERAL_BROADCAST_MARKER`] by the log layer when
/// it merges `ephemeral_attributes` — QR payloads, pair codes) is withheld
/// entirely unless `auth_enforced` is true. This keeps the credential off any
/// unauthenticated `/api/events` stream even though the pre-existing handler
/// skips the bearer check when pairing is disabled. The internal marker is
/// stripped before delivery so the public event shape is unchanged.
fn sse_frame_for_stream(
    mut value: serde_json::Value,
    auth_enforced: bool,
) -> Option<serde_json::Value> {
    if !is_public_sse_event(&value) {
        return None;
    }
    if zeroclaw_log::frame_carries_ephemeral_credentials(&value) && !auth_enforced {
        return None;
    }
    // Strip the internal marker so the delivered public shape is unchanged.
    // Shared with every other broadcast consumer (RPC `logs/subscribe`) so the
    // credential boundary is enforced identically across the bus.
    zeroclaw_log::strip_ephemeral_broadcast_marker(&mut value);
    Some(value)
}

/// GET /api/events/history — return buffered recent events as JSON.
pub async fn handle_events_history(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = super::api::require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(history_events_payload(&state.event_buffer)).into_response()
}

fn history_events_payload(buffer: &EventBuffer) -> serde_json::Value {
    let events: Vec<_> = buffer
        .snapshot()
        .into_iter()
        .filter(is_public_sse_event)
        // Pairing credentials are broadcast-only and delivery-once: they are
        // never replayed from history, so a client that connects after pairing
        // cannot recover the QR payload / pair code. Login events bypass the
        // observer that fills this buffer, so a marked frame should never be
        // here in the first place — this filter fails closed as defense in
        // depth for the non-persistent credential boundary.
        .filter(|event| !zeroclaw_log::frame_carries_ephemeral_credentials(event))
        .collect();
    serde_json::json!({ "events": events })
}

fn is_public_sse_event(event: &serde_json::Value) -> bool {
    if event.get("source").and_then(serde_json::Value::as_str) == Some("observability") {
        return true;
    }
    event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .is_none()
}

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
        // Helper for optional string fields
        fn add_optional_string(json: &mut serde_json::Value, key: &str, value: &Option<String>) {
            if let Some(value) = value {
                json[key] = serde_json::Value::String(value.clone());
            }
        }

        // Recording into the primary observer (logs / Prometheus) is the
        // responsibility of whoever built the event source; `TeeObserver`
        // takes care of that fan-out. Here we only translate to JSON and
        // ship to SSE subscribers.
        let json = match event {
            zeroclaw_runtime::observability::ObserverEvent::LlmRequest {
                model_provider,
                model,
                messages_count,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
            } => {
                let mut json = serde_json::json!({
                    "type": "llm_request",
                    "source": "observability",
                    "model_provider": model_provider,
                    "model": model,
                    "messages_count": messages_count,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "parent_agent_alias", parent_agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
            zeroclaw_runtime::observability::ObserverEvent::ToolCall {
                tool,
                duration,
                success,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
                ..
            } => {
                let mut json = serde_json::json!({
                    "type": "tool_call",
                    "source": "observability",
                    "tool": tool,
                    "duration_ms": duration.as_millis(),
                    "success": success,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "parent_agent_alias", parent_agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
            zeroclaw_runtime::observability::ObserverEvent::ToolCallStart {
                tool,
                channel,
                agent_alias,
                parent_agent_alias,
                turn_id,
                ..
            } => {
                let mut json = serde_json::json!({
                    "type": "tool_call_start",
                    "source": "observability",
                    "tool": tool,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "parent_agent_alias", parent_agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
            zeroclaw_runtime::observability::ObserverEvent::Error { component, message } => {
                serde_json::json!({
                    "type": "error",
                    "source": "observability",
                    "component": component,
                    "message": message,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            zeroclaw_runtime::observability::ObserverEvent::AgentStart {
                model_provider,
                model,
                channel,
                agent_alias,
                turn_id,
            } => {
                let mut json = serde_json::json!({
                    "type": "agent_start",
                    "source": "observability",
                    "model_provider": model_provider,
                    "model": model,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
            zeroclaw_runtime::observability::ObserverEvent::AgentEnd {
                model_provider,
                model,
                duration,
                tokens_used,
                cost_usd,
                channel,
                agent_alias,
                turn_id,
            } => {
                let (tokens_total, input_tokens, output_tokens) = tokens_used
                    .as_ref()
                    .map(|usage| {
                        (
                            Some(usage.input_tokens.saturating_add(usage.output_tokens)),
                            Some(usage.input_tokens),
                            Some(usage.output_tokens),
                        )
                    })
                    .unwrap_or((None, None, None));
                let mut json = serde_json::json!({
                    "type": "agent_end",
                    "source": "observability",
                    "model_provider": model_provider,
                    "model": model,
                    "duration_ms": duration.as_millis(),
                    "tokens_used": tokens_total,
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "cost_usd": cost_usd,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
            zeroclaw_runtime::observability::ObserverEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
                channel,
                agent_alias,
                turn_id,
            } => {
                let mut json = serde_json::json!({
                    "type": "history_trimmed",
                    "source": "observability",
                    "dropped_messages": dropped_messages,
                    "kept_turns": kept_turns,
                    "reason": reason,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                add_optional_string(&mut json, "channel", channel);
                add_optional_string(&mut json, "agent_alias", agent_alias);
                add_optional_string(&mut json, "turn_id", turn_id);
                json
            }
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

    // The broadcast hook is process-wide; serialize hook-touching tests
    // within this test binary so they don't observe each other's state.
    static HOOK_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

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
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: None,
            duration: std::time::Duration::from_millis(42),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
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
            parent_agent_alias: None,
            tool: "mcp_filesystem__read_file".into(),
            tool_call_id: None,
            arguments: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        });

        let value = rx.try_recv().expect("event should be broadcast");
        assert_eq!(value["type"], "tool_call_start");
        assert_eq!(value["tool"], "mcp_filesystem__read_file");
    }

    #[test]
    fn history_trimmed_event_is_broadcast_with_cut_accounting() {
        let (obs, mut rx, _buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::HistoryTrimmed {
            dropped_messages: 12,
            kept_turns: 1,
            reason: "context token budget exceeded".into(),
            channel: Some("wss".into()),
            agent_alias: Some("trimtest".into()),
            turn_id: Some("turn-1".into()),
        });

        let value = rx.try_recv().expect("history_trimmed must broadcast");
        assert_eq!(value["type"], "history_trimmed");
        assert_eq!(value["source"], "observability");
        assert_eq!(value["dropped_messages"], 12);
        assert_eq!(value["kept_turns"], 1);
        assert_eq!(value["reason"], "context token budget exceeded");
        assert_eq!(value["channel"], "wss");
        assert_eq!(value["agent_alias"], "trimtest");
        assert_eq!(value["turn_id"], "turn-1");
        assert!(is_public_sse_event(&value));
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

    #[test]
    fn history_payload_returns_only_public_events() {
        let buffer = EventBuffer::new(8);
        buffer.push(serde_json::json!({
            "type": "message",
            "session_id": "operator-1",
            "content": "private session notification"
        }));
        buffer.push(serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model_provider": "test",
            "model": "test-model"
        }));
        buffer.push(serde_json::json!({
            "type": "gateway_lifecycle",
            "phase": "ready"
        }));

        let payload = history_events_payload(&buffer);
        let events = payload["events"].as_array().expect("events array");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "agent_start");
        assert_eq!(events[1]["type"], "gateway_lifecycle");
    }

    /// Build a broadcast frame stamped the way `zeroclaw_log::record_event`
    /// stamps a credential-bearing login event (ephemeral attrs merged into
    /// `attributes.login`, plus the fail-closed marker).
    fn credential_login_frame() -> serde_json::Value {
        serde_json::json!({
            "source": "observability",
            "attributes": { "login": { "state": "qr", "qr_payload": "SECRET-QR-PAYLOAD" } },
            zeroclaw_log::EPHEMERAL_BROADCAST_MARKER: true,
        })
    }

    #[test]
    fn ephemeral_credential_frame_is_withheld_from_unauthenticated_stream() {
        // Pairing disabled ⇒ the `/api/events` handler skips the bearer check,
        // so the stream is unauthenticated. The credential frame must be
        // withheld entirely rather than fanned out to an anonymous subscriber.
        let frame = credential_login_frame();
        assert!(
            sse_frame_for_stream(frame, /* auth_enforced */ false).is_none(),
            "pairing secret must never ride an unauthenticated /api/events stream"
        );
    }

    #[test]
    fn ephemeral_credential_frame_reaches_authenticated_stream_without_marker() {
        // Pairing enabled ⇒ every subscriber passed the bearer check, so the
        // credential may be delivered; the internal marker is stripped first.
        let delivered =
            sse_frame_for_stream(credential_login_frame(), /* auth_enforced */ true)
                .expect("authenticated stream should receive the credential frame");
        assert_eq!(
            delivered["attributes"]["login"]["qr_payload"], "SECRET-QR-PAYLOAD",
            "authenticated stream still renders the QR payload"
        );
        assert!(
            delivered
                .get(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER)
                .is_none(),
            "internal fail-closed marker must be stripped before delivery"
        );
    }

    #[test]
    fn credential_free_frame_flows_on_unauthenticated_stream() {
        // A lifecycle frame with no ephemeral secret is unmarked and still
        // flows when auth is disabled (unchanged behavior for non-secret data).
        let frame = serde_json::json!({
            "source": "observability",
            "attributes": { "login": { "state": "connected" } },
        });
        assert!(
            sse_frame_for_stream(frame, /* auth_enforced */ false).is_some(),
            "credential-free lifecycle frames are unaffected"
        );
    }

    #[test]
    fn session_scoped_frame_is_withheld_regardless_of_auth() {
        let frame = serde_json::json!({ "type": "message", "session_id": "operator-1" });
        assert!(sse_frame_for_stream(frame.clone(), true).is_none());
        assert!(sse_frame_for_stream(frame, false).is_none());
    }

    #[test]
    fn observability_tagged_events_are_public_even_without_session_id() {
        // After observability frames keep the SSE pathway open even
        // though they would not otherwise carry a session_id discriminator.
        let obs = serde_json::json!({
            "type": "tool_call",
            "source": "observability",
            "tool": "shell",
        });
        assert!(is_public_sse_event(&obs));
    }

    #[test]
    fn broadcast_agent_end_includes_turn_metadata_and_token_total() {
        let (obs, mut rx, _buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::AgentEnd {
            model_provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            duration: std::time::Duration::from_millis(42),
            tokens_used: Some(zeroclaw_api::observability_traits::TurnTokenUsage {
                input_tokens: 12,
                output_tokens: 34,
            }),
            cost_usd: Some(0.001),
            channel: Some("wss".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });

        let value = rx.try_recv().expect("event should be broadcast");
        assert_eq!(value["type"], "agent_end");
        assert_eq!(value["source"], "observability");
        assert_eq!(value["tokens_used"], 46);
        assert_eq!(value["input_tokens"], 12);
        assert_eq!(value["output_tokens"], 34);
        assert_eq!(value["channel"], "wss");
        assert_eq!(value["agent_alias"], "default");
        assert_eq!(value["turn_id"], "turn-1");
    }

    #[test]
    fn broadcast_observer_tags_every_event_with_observability_source() {
        // The chat-WS filter relies on this tag as a defense-in-depth check
        // (any future emitter that forgets to set session_id still gets
        // routed correctly). Cover every variant the observer broadcasts.
        let (obs, mut rx, _buffer) = make_broadcast();

        let cases: Vec<ObserverEvent> = vec![
            ObserverEvent::LlmRequest {
                parent_agent_alias: None,
                model_provider: "p".into(),
                model: "m".into(),
                messages_count: 0,
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
            ObserverEvent::ToolCall {
                parent_agent_alias: None,
                tool: "shell".into(),
                tool_call_id: None,
                duration: std::time::Duration::from_millis(1),
                success: true,
                arguments: None,
                result: None,
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
            ObserverEvent::ToolCallStart {
                parent_agent_alias: None,
                tool: "shell".into(),
                tool_call_id: None,
                arguments: None,
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
            ObserverEvent::Error {
                component: "any".into(),
                message: "boom".into(),
            },
            ObserverEvent::AgentStart {
                model_provider: "p".into(),
                model: "m".into(),
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
            ObserverEvent::AgentEnd {
                model_provider: "p".into(),
                model: "m".into(),
                duration: std::time::Duration::from_millis(1),
                tokens_used: None,
                cost_usd: None,
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
        ];
        for ev in cases {
            obs.record_event(&ev);
            let v = rx.try_recv().expect("event must broadcast");
            assert_eq!(
                v["source"], "observability",
                "every BroadcastObserver event must be tagged source=observability: {v}"
            );
        }
    }

    #[test]
    fn factory_observer_events_reach_broadcast_hook() {
        let _guard = HOOK_TEST_LOCK.lock();

        zeroclaw_runtime::observability::clear_broadcast_hook();

        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let bo: Arc<dyn Observer> = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        zeroclaw_runtime::observability::set_broadcast_hook(bo);

        // Same factory call site as `process_message` in the agent loop.
        let cfg = zeroclaw_config::schema::ObservabilityConfig {
            backend: zeroclaw_config::schema::ObservabilityBackend::None,
            ..Default::default()
        };
        let observer = zeroclaw_runtime::observability::create_observer(&cfg);

        observer.record_event(&ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: "shell".into(),
            tool_call_id: None,
            duration: std::time::Duration::from_millis(7),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
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

    /// Pins the `/api/events/history` surface for the turn-lifecycle
    /// brackets: `AgentStart`/`AgentEnd` recorded through a factory-built
    /// observer (the path channel and daemon turns take) must land in the
    /// history buffer as public `agent_start`/`agent_end` frames — the
    /// contract external pollers (e.g. ZeroHome) rely on to detect
    /// in-flight turns.
    #[test]
    fn agent_lifecycle_events_reach_history_payload_via_broadcast_hook() {
        let _guard = HOOK_TEST_LOCK.lock();

        zeroclaw_runtime::observability::clear_broadcast_hook();

        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let bo: Arc<dyn Observer> = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        zeroclaw_runtime::observability::set_broadcast_hook(bo);

        // Same factory call site as the channel orchestrator and agent loop.
        let cfg = zeroclaw_config::schema::ObservabilityConfig {
            backend: zeroclaw_config::schema::ObservabilityBackend::None,
            ..Default::default()
        };
        let observer = zeroclaw_runtime::observability::create_observer(&cfg);

        observer.record_event(&ObserverEvent::AgentStart {
            model_provider: "p".into(),
            model: "m".into(),
            channel: Some("telegram".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });
        observer.record_event(&ObserverEvent::AgentEnd {
            model_provider: "p".into(),
            model: "m".into(),
            duration: std::time::Duration::from_millis(5),
            tokens_used: None,
            cost_usd: None,
            channel: Some("telegram".into()),
            agent_alias: Some("default".into()),
            turn_id: Some("turn-1".into()),
        });

        let payload = history_events_payload(&buffer);
        let events = payload["events"].as_array().expect("events array");
        assert_eq!(
            events.len(),
            2,
            "both brackets must be retained: {events:?}"
        );
        assert_eq!(events[0]["type"], "agent_start");
        assert_eq!(events[1]["type"], "agent_end");
        for event in events {
            assert_eq!(event["source"], "observability");
            assert!(
                event["timestamp"].is_string(),
                "history frames must carry a timestamp: {event}"
            );
            assert_eq!(event["turn_id"], "turn-1");
            assert_eq!(event["channel"], "telegram");
        }

        zeroclaw_runtime::observability::clear_broadcast_hook();
    }

    /// Warning follow-up (non-persistent boundary): even if a credential-marked
    /// login frame reached the replay buffer, `/api/events/history` must
    /// withhold it. Pairing secrets are delivery-once — a client that connects
    /// after pairing cannot recover them from history.
    #[test]
    fn history_payload_never_recovers_pairing_credentials() {
        let buffer = EventBuffer::new(8);
        buffer.push(credential_login_frame());
        buffer.push(serde_json::json!({
            "type": "agent_start",
            "source": "observability",
            "model_provider": "test",
            "model": "test-model",
        }));

        let payload = history_events_payload(&buffer);
        let events = payload["events"].as_array().expect("events array");
        assert_eq!(
            events.len(),
            1,
            "credential-bearing frame must be withheld from history: {events:?}"
        );
        assert_eq!(events[0]["type"], "agent_start");
        let dump = payload.to_string();
        assert!(
            !dump.contains("SECRET-QR-PAYLOAD"),
            "history must not expose a pairing secret: {dump}"
        );
        assert!(!dump.contains(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER));
    }

    // ── Route-level `/api/events` credential-boundary regressions ─────────
    //
    // These drive the real axum handler through the router — PairingGuard,
    // bearer parsing, the early 401, subscription wiring, and the routed SSE
    // response — rather than calling `sse_frame_for_stream` in isolation, so
    // they catch the auth check being removed or disconnected from the filter.

    fn events_app(state: AppState) -> axum::Router {
        axum::Router::new()
            .route("/api/events", axum::routing::get(handle_sse_events))
            .with_state(state)
    }

    /// Drive the SSE response body, accumulating delivered bytes until `needle`
    /// is seen or the budget elapses (avoids blocking on the keep-alive).
    async fn read_stream_until(
        body: axum::body::Body,
        needle: &str,
        budget: std::time::Duration,
    ) -> String {
        use http_body_util::BodyExt;
        let mut body = body;
        let mut acc = String::new();
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(bytes) = frame.into_data() {
                        acc.push_str(&String::from_utf8_lossy(&bytes));
                        if acc.contains(needle) {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        acc
    }

    fn pairing_guard(
        require: bool,
        tokens: &[&str],
    ) -> Arc<zeroclaw_runtime::security::pairing::PairingGuard> {
        let owned: Vec<String> = tokens.iter().map(|t| (*t).to_string()).collect();
        Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
            require, &owned,
        ))
    }

    /// Pairing disabled ⇒ the handler skips the bearer check, so the stream is
    /// unauthenticated. A credential frame must be withheld end-to-end while a
    /// credential-free frame still flows (proving the stream is live).
    #[tokio::test]
    async fn route_withholds_credential_frame_on_unauthenticated_events_stream() {
        use tower::ServiceExt as _;

        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(false, &[]);
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        state.event_tx = tx.clone();

        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let _ = tx.send(credential_login_frame());
        let _ = tx.send(serde_json::json!({
            "source": "observability",
            "type": "tool_call",
            "tool": "SENTINEL-LIVE",
        }));

        let body = read_stream_until(
            response.into_body(),
            "SENTINEL-LIVE",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            body.contains("SENTINEL-LIVE"),
            "the stream must stay live for non-secret frames: {body:?}"
        );
        assert!(
            !body.contains("SECRET-QR-PAYLOAD"),
            "pairing secret must never reach an unauthenticated /api/events client: {body:?}"
        );
    }

    /// Pairing enabled ⇒ the handler enforces the bearer check and returns 401
    /// before any subscription for a missing or bad token.
    #[tokio::test]
    async fn route_returns_401_when_pairing_enabled_without_valid_token() {
        use tower::ServiceExt as _;

        // No token.
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);

        // Bad token.
        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .header(axum::http::header::AUTHORIZATION, "Bearer wrong-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    /// Pairing enabled + valid bearer ⇒ every subscriber is authenticated, so
    /// the credential is delivered — with the internal marker stripped.
    #[tokio::test]
    async fn route_delivers_credential_without_marker_to_authenticated_client() {
        use tower::ServiceExt as _;

        let mut state = crate::api::test_state(zeroclaw_config::schema::Config::default());
        state.pairing = pairing_guard(true, &["valid-token"]);
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        state.event_tx = tx.clone();

        let response = events_app(state)
            .oneshot(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/events")
                    .header(axum::http::header::AUTHORIZATION, "Bearer valid-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let _ = tx.send(credential_login_frame());

        let body = read_stream_until(
            response.into_body(),
            "SECRET-QR-PAYLOAD",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(
            body.contains("SECRET-QR-PAYLOAD"),
            "an authenticated client should receive the QR payload: {body:?}"
        );
        assert!(
            !body.contains(zeroclaw_log::EPHEMERAL_BROADCAST_MARKER),
            "the internal fail-closed marker must be stripped before delivery: {body:?}"
        );
    }
}
