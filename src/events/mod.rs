//! Agent event bus — global broadcast for real-time streaming of agent activity.
//!
//! All tool executions, assistant messages, thinking content, and lifecycle events
//! are emitted through this bus. Consumers (WebSocket, logging, diagnostics) subscribe
//! to receive events as they happen.
//!
//! Events are sequenced per-run with monotonic `seq` numbers to guarantee ordering.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

// ── Event Types ─────────────────────────────────────────────────

/// Which logical stream an event belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventStream {
    Lifecycle,
    Tool,
    Assistant,
    Thinking,
    Error,
    Custom(String),
}

/// Phase of a tool execution event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Start,
    InputDelta,
    Update,
    Result,
}

/// Phase of a lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    MessageStart,
    MessageStop,
    TurnStart,
    TurnEnd,
    CompactionStart,
    CompactionEnd,
    SessionStart,
    SessionEnd,
}

/// Tool event data — emitted during tool call lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEventData {
    pub phase: ToolPhase,
    pub tool_call_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Assistant text event data — emitted during streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantEventData {
    pub delta: String,
    pub text: String,
}

/// Thinking event data — emitted during extended thinking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingEventData {
    pub delta: String,
}

/// Lifecycle event data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleEventData {
    pub phase: LifecyclePhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

/// The unified event payload broadcast on the event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub run_id: String,
    pub seq: u64,
    pub ts: u64,
    pub stream: EventStream,
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
}

// ── Event Bus ───────────────────────────────────────────────────

type ListenerFn = Arc<dyn Fn(&AgentEvent) + Send + Sync>;

/// Global agent event bus. Thread-safe, lock-free for reads.
pub struct AgentEventBus {
    listeners: RwLock<Vec<(u64, ListenerFn)>>,
    next_listener_id: AtomicU64,
    seq_by_run: Mutex<HashMap<String, u64>>,
}

impl AgentEventBus {
    pub fn new() -> Self {
        Self {
            listeners: RwLock::new(Vec::new()),
            next_listener_id: AtomicU64::new(1),
            seq_by_run: Mutex::new(HashMap::new()),
        }
    }

    /// Emit an event to all subscribers. Assigns monotonic seq per run_id.
    pub fn emit(&self, mut event: AgentEvent) {
        // Assign sequence number
        {
            let mut seqs = self.seq_by_run.lock().unwrap();
            let next_seq = seqs.entry(event.run_id.clone()).or_insert(0);
            *next_seq += 1;
            event.seq = *next_seq;
        }

        // Set timestamp
        event.ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Broadcast to all listeners
        let listeners = self.listeners.read().unwrap();
        for (_, listener) in listeners.iter() {
            listener(&event);
        }
    }

    /// Subscribe to events. Returns a handle ID for unsubscribing.
    pub fn subscribe(&self, listener: impl Fn(&AgentEvent) + Send + Sync + 'static) -> u64 {
        let id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        let mut listeners = self.listeners.write().unwrap();
        listeners.push((id, Arc::new(listener)));
        id
    }

    /// Unsubscribe a listener by handle ID.
    pub fn unsubscribe(&self, id: u64) {
        let mut listeners = self.listeners.write().unwrap();
        listeners.retain(|(lid, _)| *lid != id);
    }

    /// Clear sequence counters for a finished run.
    pub fn clear_run(&self, run_id: &str) {
        let mut seqs = self.seq_by_run.lock().unwrap();
        seqs.remove(run_id);
    }

    /// Get current subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.listeners.read().unwrap().len()
    }

    // ── Convenience emitters ────────────────────────────────────

    /// Emit a tool execution event.
    pub fn emit_tool(&self, run_id: &str, session_key: Option<&str>, data: ToolEventData) {
        self.emit(AgentEvent {
            run_id: run_id.to_string(),
            seq: 0,
            ts: 0,
            stream: EventStream::Tool,
            data: serde_json::to_value(&data).unwrap_or_default(),
            session_key: session_key.map(String::from),
            tenant_id: None,
        });
    }

    /// Emit an assistant text delta.
    pub fn emit_assistant(&self, run_id: &str, session_key: Option<&str>, delta: &str, accumulated: &str) {
        self.emit(AgentEvent {
            run_id: run_id.to_string(),
            seq: 0,
            ts: 0,
            stream: EventStream::Assistant,
            data: serde_json::to_value(&AssistantEventData {
                delta: delta.to_string(),
                text: accumulated.to_string(),
            }).unwrap_or_default(),
            session_key: session_key.map(String::from),
            tenant_id: None,
        });
    }

    /// Emit a thinking delta.
    pub fn emit_thinking(&self, run_id: &str, session_key: Option<&str>, delta: &str) {
        self.emit(AgentEvent {
            run_id: run_id.to_string(),
            seq: 0,
            ts: 0,
            stream: EventStream::Thinking,
            data: serde_json::to_value(&ThinkingEventData {
                delta: delta.to_string(),
            }).unwrap_or_default(),
            session_key: session_key.map(String::from),
            tenant_id: None,
        });
    }

    /// Emit a lifecycle event.
    pub fn emit_lifecycle(&self, run_id: &str, session_key: Option<&str>, data: LifecycleEventData) {
        self.emit(AgentEvent {
            run_id: run_id.to_string(),
            seq: 0,
            ts: 0,
            stream: EventStream::Lifecycle,
            data: serde_json::to_value(&data).unwrap_or_default(),
            session_key: session_key.map(String::from),
            tenant_id: None,
        });
    }

    /// Emit an error event.
    pub fn emit_error(&self, run_id: &str, session_key: Option<&str>, message: &str) {
        self.emit(AgentEvent {
            run_id: run_id.to_string(),
            seq: 0,
            ts: 0,
            stream: EventStream::Error,
            data: serde_json::json!({"message": message}),
            session_key: session_key.map(String::from),
            tenant_id: None,
        });
    }
}

// ── Global Singleton ────────────────────────────────────────────

static EVENT_BUS: std::sync::OnceLock<AgentEventBus> = std::sync::OnceLock::new();

/// Get the global event bus instance.
pub fn event_bus() -> &'static AgentEventBus {
    EVENT_BUS.get_or_init(AgentEventBus::new)
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn emit_assigns_monotonic_seq() {
        let bus = AgentEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let rx = received.clone();
        bus.subscribe(move |evt| {
            rx.lock().unwrap().push(evt.clone());
        });

        for _ in 0..5 {
            bus.emit(AgentEvent {
                run_id: "run-1".into(),
                seq: 0,
                ts: 0,
                stream: EventStream::Assistant,
                data: serde_json::json!({}),
                session_key: None,
                tenant_id: None,
            });
        }

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 5);
        for (i, evt) in events.iter().enumerate() {
            assert_eq!(evt.seq, (i + 1) as u64);
        }
    }

    #[test]
    fn separate_runs_have_independent_seqs() {
        let bus = AgentEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let rx = received.clone();
        bus.subscribe(move |evt| {
            rx.lock().unwrap().push(evt.clone());
        });

        bus.emit(AgentEvent {
            run_id: "run-a".into(), seq: 0, ts: 0,
            stream: EventStream::Tool, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });
        bus.emit(AgentEvent {
            run_id: "run-b".into(), seq: 0, ts: 0,
            stream: EventStream::Tool, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });
        bus.emit(AgentEvent {
            run_id: "run-a".into(), seq: 0, ts: 0,
            stream: EventStream::Tool, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });

        let events = received.lock().unwrap();
        assert_eq!(events[0].seq, 1); // run-a seq 1
        assert_eq!(events[1].seq, 1); // run-b seq 1
        assert_eq!(events[2].seq, 2); // run-a seq 2
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let bus = AgentEventBus::new();
        let called = Arc::new(AtomicBool::new(false));

        let c = called.clone();
        let id = bus.subscribe(move |_| {
            c.store(true, Ordering::Relaxed);
        });

        bus.unsubscribe(id);

        bus.emit(AgentEvent {
            run_id: "run-x".into(), seq: 0, ts: 0,
            stream: EventStream::Lifecycle, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });

        assert!(!called.load(Ordering::Relaxed));
    }

    #[test]
    fn emit_tool_convenience() {
        let bus = AgentEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let rx = received.clone();
        bus.subscribe(move |evt| {
            rx.lock().unwrap().push(evt.clone());
        });

        bus.emit_tool("run-1", Some("sess-1"), ToolEventData {
            phase: ToolPhase::Start,
            tool_call_id: "toolu_abc".into(),
            name: "exec".into(),
            args: Some(serde_json::json!({"cmd": "ls"})),
            partial_json: None,
            partial_result: None,
            result: None,
            error: None,
            is_error: None,
            duration_ms: None,
        });

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].stream, EventStream::Tool);
        assert_eq!(events[0].session_key.as_deref(), Some("sess-1"));
    }

    #[test]
    fn clear_run_resets_seq() {
        let bus = AgentEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let rx = received.clone();
        bus.subscribe(move |evt| {
            rx.lock().unwrap().push(evt.clone());
        });

        bus.emit(AgentEvent {
            run_id: "run-1".into(), seq: 0, ts: 0,
            stream: EventStream::Lifecycle, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });

        bus.clear_run("run-1");

        bus.emit(AgentEvent {
            run_id: "run-1".into(), seq: 0, ts: 0,
            stream: EventStream::Lifecycle, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });

        let events = received.lock().unwrap();
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[1].seq, 1); // Reset back to 1
    }

    #[test]
    fn event_stream_serialization() {
        let json = serde_json::to_string(&EventStream::Tool).unwrap();
        assert_eq!(json, "\"tool\"");

        let json = serde_json::to_string(&EventStream::Custom("my_stream".into())).unwrap();
        assert!(json.contains("my_stream"));
    }

    #[test]
    fn subscriber_count_tracks() {
        let bus = AgentEventBus::new();
        assert_eq!(bus.subscriber_count(), 0);

        let id1 = bus.subscribe(|_| {});
        assert_eq!(bus.subscriber_count(), 1);

        let _id2 = bus.subscribe(|_| {});
        assert_eq!(bus.subscriber_count(), 2);

        bus.unsubscribe(id1);
        assert_eq!(bus.subscriber_count(), 1);
    }

    #[test]
    fn timestamps_are_populated() {
        let bus = AgentEventBus::new();
        let received = Arc::new(Mutex::new(Vec::new()));

        let rx = received.clone();
        bus.subscribe(move |evt| {
            rx.lock().unwrap().push(evt.clone());
        });

        bus.emit(AgentEvent {
            run_id: "run-ts".into(), seq: 0, ts: 0,
            stream: EventStream::Lifecycle, data: serde_json::json!({}),
            session_key: None, tenant_id: None,
        });

        let events = received.lock().unwrap();
        assert!(events[0].ts > 0);
    }
}
