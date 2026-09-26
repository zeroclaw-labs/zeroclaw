//! The process-wide observability event bus.
//!
//! [`BroadcastObserver`] turns [`ObserverEvent`]s into the JSON frames that
//! `/api/events` (SSE) and RPC `logs/subscribe` deliver, and keeps the most
//! recent ones in an [`EventBuffer`] for history replay (`/api/events/history`,
//! RPC `events/history`). [`EventBus`] bundles the live sender with that
//! buffer. The daemon owns one bus and installs its observer as the global
//! broadcast hook exactly once, so agent, tool, and LLM frames reach RPC
//! subscribers whether or not the gateway runs; a standalone gateway builds
//! and installs its own.
//!
//! [`ObserverEvent`]: crate::observability::ObserverEvent

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Frames the live broadcast channel holds for slow subscribers.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Frames retained for history replay.
pub const EVENT_HISTORY_CAPACITY: usize = 500;

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
        let mut buf = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(event);
    }

    /// Return a snapshot of all buffered events (oldest first).
    pub fn snapshot(&self) -> Vec<serde_json::Value> {
        let buf = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        buf.iter().cloned().collect()
    }
}

pub struct BroadcastObserver {
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    buffer: Arc<EventBuffer>,
}

impl BroadcastObserver {
    pub fn new(
        tx: tokio::sync::broadcast::Sender<serde_json::Value>,
        buffer: Arc<EventBuffer>,
    ) -> Self {
        Self { tx, buffer }
    }
}

impl crate::observability::Observer for BroadcastObserver {
    fn record_event(&self, event: &crate::observability::ObserverEvent) {
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
            crate::observability::ObserverEvent::LlmRequest {
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
            crate::observability::ObserverEvent::ToolCall {
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
            crate::observability::ObserverEvent::ToolCallStart {
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
            crate::observability::ObserverEvent::Error { component, message } => {
                serde_json::json!({
                    "type": "error",
                    "source": "observability",
                    "component": component,
                    "message": message,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                })
            }
            crate::observability::ObserverEvent::AgentStart {
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
            crate::observability::ObserverEvent::AgentEnd {
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
            crate::observability::ObserverEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
                channel,
                agent_alias,
                turn_id,
                token_budget,
                tokens_before,
                tokens_after,
                tokens_before_source,
                tokens_after_source,
                unsatisfiable_floor,
            } => {
                let mut json = serde_json::json!({
                    "type": "history_trimmed",
                    "source": "observability",
                    "dropped_messages": dropped_messages,
                    "kept_turns": kept_turns,
                    "reason": reason,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                if let Some(token_budget) = token_budget {
                    json["token_budget"] = (*token_budget).into();
                }
                if let Some(tokens_before) = tokens_before {
                    json["tokens_before"] = (*tokens_before).into();
                }
                if let Some(tokens_after) = tokens_after {
                    json["tokens_after"] = (*tokens_after).into();
                }
                if let Some(tokens_before_source) = tokens_before_source {
                    json["tokens_before_source"] = tokens_before_source.as_str().into();
                }
                if let Some(tokens_after_source) = tokens_after_source {
                    json["tokens_after_source"] = tokens_after_source.as_str().into();
                }
                if let Some(unsatisfiable_floor) = unsatisfiable_floor {
                    json["unsatisfiable_floor"] = (*unsatisfiable_floor).into();
                }
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

    fn record_metric(&self, _metric: &crate::observability::traits::ObserverMetric) {
        // Metrics are not broadcast over SSE; the primary observer records them.
    }

    fn name(&self) -> &str {
        "broadcast"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The live event sender plus its history buffer.
///
/// Cloning shares both halves. [`EventBus::install_hook`] makes this bus the
/// destination of every observer built by
/// [`create_observer`](crate::observability::create_observer); whoever owns
/// the bus installs it once and holds the guard for as long as it runs.
#[derive(Clone)]
pub struct EventBus {
    tx: tokio::sync::broadcast::Sender<serde_json::Value>,
    history: Arc<EventBuffer>,
}

impl EventBus {
    /// A bus with the standard channel and history capacities.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacities(EVENT_CHANNEL_CAPACITY, EVENT_HISTORY_CAPACITY)
    }

    /// A bus with explicit capacities (tests use small ones).
    #[must_use]
    pub fn with_capacities(channel: usize, history: usize) -> Self {
        let (tx, _rx) = tokio::sync::broadcast::channel(channel);
        Self {
            tx,
            history: Arc::new(EventBuffer::new(history)),
        }
    }

    /// The live sender. Other publishers (cron, heartbeat, the log layer)
    /// send on it directly; those frames are live-only and not buffered.
    #[must_use]
    pub fn sender(&self) -> &tokio::sync::broadcast::Sender<serde_json::Value> {
        &self.tx
    }

    /// The history buffer the observer fills.
    #[must_use]
    pub fn history(&self) -> &Arc<EventBuffer> {
        &self.history
    }

    /// Install this bus's [`BroadcastObserver`] as the process-wide broadcast
    /// hook. The hook stays installed until the returned guard drops.
    #[must_use = "hold the guard for as long as the bus should receive observer events"]
    pub fn install_hook(&self) -> crate::observability::BroadcastHookGuard {
        crate::observability::set_scoped_broadcast_hook(Arc::new(BroadcastObserver::new(
            self.tx.clone(),
            Arc::clone(&self.history),
        )))
    }
}

impl EventBus {
    /// The bus a component should publish on.
    ///
    /// `shared` is a bus whose owner already installed its hook (a gateway
    /// under the daemon): it is reused and no second hook is installed, so
    /// each observer event reaches subscribers once and lands in one history.
    /// With no shared bus (a standalone gateway) a fresh bus is created and
    /// installed, and the caller holds the returned guard while it runs.
    #[must_use = "hold the guard for as long as the bus should receive observer events"]
    pub fn shared_or_installed(
        shared: Option<Self>,
    ) -> (Self, Option<crate::observability::BroadcastHookGuard>) {
        match shared {
            Some(bus) => (bus, None),
            None => {
                let bus = Self::new();
                let guard = bus.install_hook();
                (bus, Some(guard))
            }
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a frame may be delivered on a public, non-session stream: every
/// observer frame (tagged `source = "observability"`), plus any frame that is
/// not scoped to a session.
#[must_use]
pub fn is_public_event(event: &serde_json::Value) -> bool {
    if event.get("source").and_then(serde_json::Value::as_str) == Some("observability") {
        return true;
    }
    event
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .is_none()
}

/// Buffered frames that may be replayed, oldest first.
///
/// Pairing credentials are broadcast-only and delivery-once: they are never
/// replayed, so a client that connects after pairing cannot recover the QR
/// payload or pair code. Login events bypass the observer that fills the
/// buffer, so a marked frame should never be here in the first place; this
/// filter fails closed as defense in depth.
#[must_use]
pub fn history_events(buffer: &EventBuffer) -> Vec<serde_json::Value> {
    buffer
        .snapshot()
        .into_iter()
        .filter(is_public_event)
        .filter(|event| !zeroclaw_log::frame_carries_ephemeral_credentials(event))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::{Observer, ObserverEvent};

    #[test]
    fn event_buffer_recovers_after_a_poisoned_writer() {
        let buffer = Arc::new(EventBuffer::new(4));
        let poisoned = buffer.clone();
        let result = std::thread::spawn(move || {
            let mut guard = poisoned.inner.lock().expect("fresh buffer must lock");
            guard.push_back(serde_json::json!({"sequence": 1}));
            panic!("poison event buffer");
        })
        .join();
        assert!(result.is_err());

        buffer.push(serde_json::json!({"sequence": 2}));
        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0]["sequence"], 1);
        assert_eq!(snapshot[1]["sequence"], 2);
    }

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
            token_budget: Some(500_000),
            tokens_before: Some(612_000),
            tokens_after: Some(117_000),
            tokens_before_source: Some(zeroclaw_api::agent::TokenCountSource::Provider),
            tokens_after_source: Some(zeroclaw_api::agent::TokenCountSource::Calibrated),
            unsatisfiable_floor: None,
        });

        let value = rx.try_recv().expect("history_trimmed must broadcast");
        assert_eq!(value["type"], "history_trimmed");
        assert_eq!(value["source"], "observability");
        assert_eq!(value["dropped_messages"], 12);
        assert_eq!(value["kept_turns"], 1);
        assert_eq!(value["reason"], "context token budget exceeded");
        assert_eq!(value["token_budget"], 500_000);
        assert_eq!(value["tokens_before"], 612_000);
        assert_eq!(value["tokens_after"], 117_000);
        assert_eq!(value["tokens_before_source"], "provider");
        assert_eq!(value["tokens_after_source"], "calibrated");
        assert_eq!(value["channel"], "wss");
        assert_eq!(value["agent_alias"], "trimtest");
        assert_eq!(value["turn_id"], "turn-1");
        assert!(is_public_event(&value));
    }

    #[test]
    fn history_trimmed_without_token_accounting_omits_token_fields() {
        // Message-limit trims carry no token data; the broadcast must not emit
        // null placeholder keys, so older clients keep parsing the frame.
        let (obs, mut rx, _buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::HistoryTrimmed {
            dropped_messages: 12,
            kept_turns: 1,
            reason: "history message limit exceeded".into(),
            channel: None,
            agent_alias: None,
            turn_id: None,
            token_budget: None,
            tokens_before: None,
            tokens_after: None,
            tokens_before_source: None,
            tokens_after_source: None,
            unsatisfiable_floor: None,
        });

        let value = rx.try_recv().expect("history_trimmed must broadcast");
        assert_eq!(value["type"], "history_trimmed");
        assert!(value.get("token_budget").is_none());
        assert!(value.get("tokens_before").is_none());
        assert!(value.get("tokens_after").is_none());
        assert!(value.get("tokens_before_source").is_none());
        assert!(value.get("tokens_after_source").is_none());
    }

    #[test]
    fn unmapped_events_are_skipped() {
        let (obs, mut rx, buffer) = make_broadcast();

        obs.record_event(&ObserverEvent::HeartbeatTick);

        assert!(rx.try_recv().is_err(), "heartbeat should not broadcast");
        assert!(buffer.snapshot().is_empty());
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
        let _guard = crate::observability::HOOK_TEST_LOCK.blocking_lock();

        crate::observability::clear_broadcast_hook();

        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let bo: Arc<dyn Observer> = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        crate::observability::set_broadcast_hook(bo);

        // Same factory call site as `process_message` in the agent loop.
        let cfg = zeroclaw_config::schema::ObservabilityConfig {
            backend: zeroclaw_config::schema::ObservabilityBackend::None,
            ..Default::default()
        };
        let observer = crate::observability::create_observer(&cfg);

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

        crate::observability::clear_broadcast_hook();
    }

    /// Pins the `/api/events/history` surface for the turn-lifecycle
    /// brackets: `AgentStart`/`AgentEnd` recorded through a factory-built
    /// observer (the path channel and daemon turns take) must land in the
    /// history buffer as public `agent_start`/`agent_end` frames — the
    /// contract external pollers (e.g. ZeroHome) rely on to detect
    /// in-flight turns.
    #[test]
    fn agent_lifecycle_events_reach_history_payload_via_broadcast_hook() {
        let _guard = crate::observability::HOOK_TEST_LOCK.blocking_lock();

        crate::observability::clear_broadcast_hook();

        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let buffer = Arc::new(EventBuffer::new(16));
        let bo: Arc<dyn Observer> = Arc::new(BroadcastObserver::new(tx, buffer.clone()));
        crate::observability::set_broadcast_hook(bo);

        // Same factory call site as the channel orchestrator and agent loop.
        let cfg = zeroclaw_config::schema::ObservabilityConfig {
            backend: zeroclaw_config::schema::ObservabilityBackend::None,
            ..Default::default()
        };
        let observer = crate::observability::create_observer(&cfg);

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

        let events = history_events(&buffer);
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

        crate::observability::clear_broadcast_hook();
    }

    fn sentinel_tool_call(tool: &str) -> ObserverEvent {
        ObserverEvent::ToolCall {
            parent_agent_alias: None,
            tool: tool.into(),
            tool_call_id: None,
            duration: std::time::Duration::from_millis(1),
            success: true,
            arguments: None,
            result: None,
            channel: None,
            agent_alias: None,
            turn_id: None,
        }
    }

    fn factory_observer() -> Box<dyn Observer> {
        crate::observability::create_observer(&zeroclaw_config::schema::ObservabilityConfig {
            backend: zeroclaw_config::schema::ObservabilityBackend::None,
            ..Default::default()
        })
    }

    /// A gateway under the daemon shares the daemon's bus and installs no
    /// second hook, so an observer event reaches a subscriber exactly once and
    /// is buffered exactly once.
    #[test]
    fn a_shared_bus_adds_no_second_hook() {
        let _guard = crate::observability::HOOK_TEST_LOCK.blocking_lock();
        crate::observability::clear_broadcast_hook();

        let daemon_bus = EventBus::with_capacities(16, 16);
        let _daemon_hook = daemon_bus.install_hook();
        let mut rx = daemon_bus.sender().subscribe();

        let (gateway_bus, gateway_hook) = EventBus::shared_or_installed(Some(daemon_bus.clone()));
        assert!(
            gateway_hook.is_none(),
            "a shared bus must not install a second hook"
        );
        assert!(Arc::ptr_eq(gateway_bus.history(), daemon_bus.history()));

        factory_observer().record_event(&sentinel_tool_call("SENTINEL-ONCE"));

        // Tests running in parallel can record into the same process-wide
        // hook, so count only this test's sentinel.
        let delivered = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|frame| frame["tool"] == "SENTINEL-ONCE")
            .count();
        assert_eq!(
            delivered, 1,
            "the event must be delivered once, not once per installer"
        );
        let buffered = history_events(daemon_bus.history())
            .into_iter()
            .filter(|frame| frame["tool"] == "SENTINEL-ONCE")
            .count();
        assert_eq!(buffered, 1);

        crate::observability::clear_broadcast_hook();
    }

    /// A standalone gateway has no shared bus: it gets a fresh one and owns
    /// its hook for as long as it holds the guard.
    #[test]
    fn a_standalone_bus_installs_and_releases_its_hook() {
        let _guard = crate::observability::HOOK_TEST_LOCK.blocking_lock();
        crate::observability::clear_broadcast_hook();

        let (bus, hook) = EventBus::shared_or_installed(None);
        assert!(hook.is_some(), "a standalone bus must install its own hook");
        let mut rx = bus.sender().subscribe();

        factory_observer().record_event(&sentinel_tool_call("SENTINEL-STANDALONE"));
        // Parallel tests can share the process-wide hook; look for this
        // test's sentinel rather than asserting on the next frame.
        assert!(
            std::iter::from_fn(|| rx.try_recv().ok())
                .any(|frame| frame["tool"] == "SENTINEL-STANDALONE"),
            "the standalone hook delivers"
        );

        drop(hook);
        factory_observer().record_event(&sentinel_tool_call("SENTINEL-AFTER-DROP"));
        assert!(
            !std::iter::from_fn(|| rx.try_recv().ok())
                .any(|frame| frame["tool"] == "SENTINEL-AFTER-DROP"),
            "dropping the guard must uninstall the hook"
        );

        crate::observability::clear_broadcast_hook();
    }
}
