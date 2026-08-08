//! Cross-entry conversation attribution integration tests.
//!
//! Drives the embedded `Agent` API and the durable session backend
//! end-to-end to assert the cross-cutting conversation-attribution
//! contract that every entry boundary (gateway, CLI, RPC, ACP, channel,
//! embedded caller) shares: a caller-owned conversation id is stable
//! across turns within one conversation, isolated across concurrent
//! conversations, rotates atomically on `/clear`, converges to a single
//! UUID under concurrent first-resolve, and never leaks routing,
//! history-key, storage, path, sender, or memory-scope values into the
//! telemetry identity slot.
//!
//! The per-owner unit tests (runtime, gateway, channels, infra) cover each
//! owner's specific minting/reuse path. These tests assert the contract
//! HOLDS across owners by exercising the two most testable surfaces - the
//! embedded `Agent` (the common propagation core) and the `SessionBackend`
//! (the durable source of truth) - and are written against the public
//! `zeroclaw` crate API only.

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;

use zeroclaw::agent::agent::Agent;
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::observability::{Observer, ObserverEvent};
use zeroclaw::providers::ToolCall;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::observability_traits::ObserverMetric;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

use crate::support::helpers::{make_memory, text_response, tool_response};
use crate::support::{EchoTool, MockModelProvider};

// ─────────────────────────────────────────────────────────────────────────────
// Capturing observer
// ─────────────────────────────────────────────────────────────────────────────

/// Records every observer event in arrival order so a test can assert the
/// `(conversation_id, turn_id)` attribution stamped on each turn-scoped
/// lifecycle event. Stored behind `parking_lot::Mutex` so concurrent agents
/// each see only their own stream.
#[derive(Default)]
struct CapturingObserver {
    events: Mutex<Vec<ObserverEvent>>,
}

impl CapturingObserver {
    fn events(&self) -> Vec<ObserverEvent> {
        self.events.lock().clone()
    }
}

impl Observer for CapturingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().push(event.clone());
    }
    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str {
        "capturing"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn flush(&self) {}
}

/// Build an embedded `Agent` whose observer captures every event, with the
/// given caller-owned `conversation_id` and `memory_session_id` and a mock
/// model that replays `responses` in order. `auto_save(true)` makes the
/// store path fire so the `MemoryStore` attributed variant is exercised.
fn build_capturing_agent(
    capturing: Arc<CapturingObserver>,
    conversation_id: Option<&str>,
    memory_session_id: Option<&str>,
    responses: Vec<zeroclaw::providers::ChatResponse>,
) -> Agent {
    let observer: Arc<dyn Observer> = capturing;
    Agent::builder()
        .model_provider(Box::new(MockModelProvider::new(responses)))
        .tools(vec![Box::new(EchoTool)])
        .memory(make_memory())
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::env::temp_dir())
        .agent_alias("test-agent".into())
        .auto_save(true)
        .memory_session_id(memory_session_id.map(str::to_string))
        .conversation_id(conversation_id.map(str::to_string))
        .build()
        .expect("agent builder should succeed with a valid config")
}

/// Open a fresh tempfile-backed SQLite session backend.
fn temp_backend() -> (tempfile::TempDir, SqliteSessionBackend) {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let backend = SqliteSessionBackend::new(tmp.path()).expect("sqlite session backend");
    (tmp, backend)
}

/// True when `s` has the 8-4-4-4-12 UUID structure the durable backend mints.
fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Unified extractor over the nine turn-attributed variants
// ─────────────────────────────────────────────────────────────────────────────

/// Extract `(conversation_id, turn_id)` from any of the nine turn-attributed
/// observer events (`AgentStart`, `AgentEnd`, `LlmRequest`, `LlmResponse`,
/// `ToolCallStart`, `ToolCall`, `MemoryRecall`, `MemoryStore`,
/// `RagRetrieve`). Returns `(None, None)` for events outside that set
/// (heartbeat, cache, channel message, deployment, memory audit,
/// `HistoryTrimmed`, ...) so callers can filter to the attributed subset.
fn conversation_id_and_turn(event: &ObserverEvent) -> (Option<&str>, Option<&str>) {
    match event {
        ObserverEvent::AgentStart {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::AgentEnd {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::LlmRequest {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::LlmResponse {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::ToolCallStart {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::ToolCall {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::MemoryRecall {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::MemoryStore {
            conversation_id,
            turn_id,
            ..
        }
        | ObserverEvent::RagRetrieve {
            conversation_id,
            turn_id,
            ..
        } => (conversation_id.as_deref(), turn_id.as_deref()),
        _ => (None, None),
    }
}

/// Build one of each of the nine turn-attributed variants carrying the same
/// neutral `(conversation_id, turn_id)` pair, so the extractor is verified
/// against every arm without depending on runtime emission.
fn nine_attributed_events(conversation: &str, turn: &str) -> Vec<ObserverEvent> {
    vec![
        ObserverEvent::AgentStart {
            model_provider: "opaque-provider".into(),
            model: "opaque-model".into(),
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::LlmRequest {
            model_provider: "opaque-provider".into(),
            model: "opaque-model".into(),
            messages_count: 1,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            parent_agent_alias: None,
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::LlmResponse {
            model_provider: "opaque-provider".into(),
            model: "opaque-model".into(),
            duration: Duration::from_millis(1),
            success: true,
            error_message: None,
            input_tokens: None,
            output_tokens: None,
            messages: None,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            parent_agent_alias: None,
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::AgentEnd {
            model_provider: "opaque-provider".into(),
            model: "opaque-model".into(),
            duration: Duration::from_millis(1),
            tokens_used: None,
            cost_usd: None,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::ToolCallStart {
            tool: "opaque-tool".into(),
            tool_call_id: None,
            arguments: None,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            parent_agent_alias: None,
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::ToolCall {
            tool: "opaque-tool".into(),
            tool_call_id: None,
            duration: Duration::from_millis(1),
            success: true,
            arguments: None,
            result: None,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            parent_agent_alias: None,
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::MemoryRecall {
            query_summary: None,
            duration: Duration::from_millis(1),
            num_entries: 0,
            backend: "none".into(),
            success: true,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::MemoryStore {
            category: "conversation".into(),
            backend: "none".into(),
            duration: Duration::from_millis(1),
            success: true,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
        ObserverEvent::RagRetrieve {
            query_summary: None,
            duration: Duration::from_millis(1),
            num_chunks: 0,
            num_boards: 0,
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some(turn.into()),
            conversation_id: Some(conversation.into()),
        },
    ]
}

/// Collect the conversation ids carried by every turn-attributed event.
fn attributed_conversation_ids(events: &[ObserverEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| conversation_id_and_turn(e).0)
        .collect()
}

/// Collect the distinct turn ids carried by every turn-attributed event.
fn attributed_turn_ids(events: &[ObserverEvent]) -> Vec<String> {
    let mut ids: Vec<String> = events
        .iter()
        .filter_map(|e| conversation_id_and_turn(e).1.map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn extractor_reads_the_nine_attributed_variants_and_skips_the_rest() {
    for event in nine_attributed_events("conversation-opaque-1", "turn-1") {
        let (conv, turn) = conversation_id_and_turn(&event);
        assert_eq!(conv, Some("conversation-opaque-1"), "{event:?}");
        assert_eq!(turn, Some("turn-1"), "{event:?}");
    }

    // Events outside the nine attributed variants carry neither id.
    for event in [
        ObserverEvent::TurnComplete,
        ObserverEvent::HeartbeatTick,
        ObserverEvent::ChannelMessage {
            channel: "opaque-channel".into(),
            direction: "inbound".into(),
        },
        ObserverEvent::CacheHit {
            cache_type: "hot".into(),
            tokens_saved: 1,
        },
        ObserverEvent::MemoryAudit {
            action: "store".into(),
            backend: "none".into(),
            duration: Duration::from_millis(1),
            success: true,
        },
        ObserverEvent::HistoryTrimmed {
            dropped_messages: 1,
            kept_turns: 1,
            reason: "opaque-reason".into(),
            channel: Some("opaque-channel".into()),
            agent_alias: Some("opaque-agent".into()),
            turn_id: Some("turn-1".into()),
        },
    ] {
        let (conv, turn) = conversation_id_and_turn(&event);
        assert!(
            conv.is_none() && turn.is_none(),
            "non-attributed event must yield (None, None): {event:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cross-turn stability: the conversation id is stable while the turn id rolls
// ─────────────────────────────────────────────────────────────────────────────

/// One agent, two turns: the same caller-owned conversation id must be
/// stamped on every attributed event of BOTH turns, while the turn id
/// changes per turn. Turn one drives a tool call so the tool-call attributed
/// variants fire alongside the lifecycle, LLM, and memory variants.
#[tokio::test]
async fn conversation_id_is_stable_across_two_turns_while_turn_id_changes() {
    let capturing = Arc::new(CapturingObserver::default());
    let mut agent = build_capturing_agent(
        capturing.clone(),
        Some("conversation-opaque-stable"),
        None,
        vec![
            tool_response(vec![ToolCall {
                id: "tc-opaque-1".into(),
                name: "echo".into(),
                arguments: r#"{"message":"opaque-first"}"#.into(),
                extra_content: None,
            }]),
            text_response("opaque-first-done"),
            text_response("opaque-second-done"),
        ],
    );

    let _ = agent.turn("opaque-first").await.expect("first turn");
    let first_turn_event_count = capturing.events().len();
    let _ = agent.turn("opaque-second").await.expect("second turn");
    let all_events = capturing.events();
    let turn_one = &all_events[..first_turn_event_count];
    let turn_two = &all_events[first_turn_event_count..];

    // Every attributed event in either turn carries the same conversation id.
    for (events, label) in [(turn_one, "turn one"), (turn_two, "turn two")] {
        let convs = attributed_conversation_ids(events);
        assert!(!convs.is_empty(), "{label} must emit attributed events");
        assert!(
            convs.iter().all(|c| *c == "conversation-opaque-stable"),
            "{label} attributed events must all carry the stable conversation id, got {convs:?}"
        );
    }

    // Within each turn, every attributed event shares that turn's single
    // turn id; the two turns mint two distinct turn ids.
    let turn_one_ids = attributed_turn_ids(turn_one);
    let turn_two_ids = attributed_turn_ids(turn_two);
    assert_eq!(turn_one_ids.len(), 1, "turn one must share one turn id");
    assert_eq!(turn_two_ids.len(), 1, "turn two must share one turn id");
    assert_ne!(
        turn_one_ids[0], turn_two_ids[0],
        "the turn id must change between turns"
    );

    // The tool-call turn exercised the tool-call attributed variants.
    assert!(
        turn_one
            .iter()
            .any(|e| matches!(e, ObserverEvent::ToolCallStart { .. })),
        "tool-call turn must emit ToolCallStart"
    );
    assert!(
        turn_one
            .iter()
            .any(|e| matches!(e, ObserverEvent::ToolCall { .. })),
        "tool-call turn must emit ToolCall"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Isolation under concurrency: two conversations never cross
// ─────────────────────────────────────────────────────────────────────────────

/// Two agents with different conversation ids run concurrently: each
/// observer sees ONLY its own id. Guards against a task-local or
/// process-global current-conversation cache crossing the agents.
#[tokio::test]
async fn concurrent_agents_keep_their_conversation_ids_isolated() {
    let cap_a = Arc::new(CapturingObserver::default());
    let cap_b = Arc::new(CapturingObserver::default());
    let mut agent_a = build_capturing_agent(
        cap_a.clone(),
        Some("conversation-opaque-a"),
        None,
        vec![text_response("opaque-a")],
    );
    let mut agent_b = build_capturing_agent(
        cap_b.clone(),
        Some("conversation-opaque-b"),
        None,
        vec![text_response("opaque-b")],
    );

    let (a, b) = tokio::join!(agent_a.turn("opaque-a"), agent_b.turn("opaque-b"));
    let _ = a.expect("agent A turn");
    let _ = b.expect("agent B turn");

    for (cap, own, other) in [
        (
            cap_a.clone(),
            "conversation-opaque-a",
            "conversation-opaque-b",
        ),
        (
            cap_b.clone(),
            "conversation-opaque-b",
            "conversation-opaque-a",
        ),
    ] {
        let events = cap.events();
        let convs = attributed_conversation_ids(&events);
        assert!(!convs.is_empty(), "agent must emit attributed events");
        assert!(
            convs.iter().all(|c| *c == own),
            "agent must carry only its own conversation id {own}, got {convs:?}"
        );
        assert!(
            !convs.contains(&other),
            "agent must not see the other conversation id {other}, got {convs:?}"
        );
    }
}

/// Rotating one session's conversation id must not touch another session's
/// id. Exercises the durable backend's record-scoped atomicity directly.
#[test]
fn backend_rotating_one_key_does_not_rotate_another() {
    let (_tmp, backend) = temp_backend();
    let id_a = backend
        .resolve_or_create_conversation_id("session-key-opaque-a")
        .unwrap();
    let id_b = backend
        .resolve_or_create_conversation_id("session-key-opaque-b")
        .unwrap();
    assert_ne!(id_a, id_b, "two sessions mint distinct ids");

    let id_a_rotated = backend
        .clear_and_rotate_conversation("session-key-opaque-a")
        .unwrap();
    assert_ne!(id_a, id_a_rotated, "rotate must mint a fresh id");
    assert_eq!(
        backend
            .resolve_or_create_conversation_id("session-key-opaque-b")
            .unwrap(),
        id_b,
        "rotating a must not change b's id"
    );
}

/// Two independent backend connections resolving the same fresh key for the
/// first time must converge on one and the same id (the IMMEDIATE
/// transaction + busy_timeout serialize them). Guards against two
/// concurrent first-resolves minting two divergent ids.
#[test]
fn backend_concurrent_first_resolve_converges_on_one_uuid() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let a = Arc::new(SqliteSessionBackend::new(tmp.path()).expect("backend a"));
    let b = SqliteSessionBackend::new(tmp.path()).expect("backend b");
    let barrier = Arc::new(Barrier::new(2));
    let key = "session-key-opaque-concurrent";

    let bar = barrier.clone();
    let a_c = a.clone();
    let h1 = thread::spawn(move || -> String {
        bar.wait();
        a_c.resolve_or_create_conversation_id(key).unwrap()
    });
    let bar2 = barrier.clone();
    let h2 = thread::spawn(move || -> String {
        bar2.wait();
        b.resolve_or_create_conversation_id(key).unwrap()
    });
    let id1 = h1.join().expect("resolver a");
    let id2 = h2.join().expect("resolver b");

    assert!(!id1.is_empty() && !id2.is_empty());
    assert_eq!(
        id1, id2,
        "two concurrent first-access resolves must converge on one id"
    );

    // A third fresh instance reads the same committed value, not a new one.
    let c = SqliteSessionBackend::new(tmp.path()).expect("backend c");
    let id3 = c
        .resolve_or_create_conversation_id("session-key-opaque-concurrent")
        .unwrap();
    assert_eq!(id3, id1, "a later resolve must re-read the committed id");
}

// ─────────────────────────────────────────────────────────────────────────────
// Owner contract: embedded caller id round-trip + durable backend lifecycle
// ─────────────────────────────────────────────────────────────────────────────

/// The embedded caller owns the telemetry identity slot via the builder and
/// the mid-stream setter: the getter returns whatever was set, and the
/// attributed events on each turn carry exactly that value - re-attributing
/// mid-conversation swaps the id stamped on later turns.
#[tokio::test]
async fn embedded_caller_id_round_trips_and_propagates_across_setter() {
    let capturing = Arc::new(CapturingObserver::default());
    let mut agent = build_capturing_agent(
        capturing.clone(),
        Some("conversation-opaque-initial"),
        None,
        vec![
            text_response("opaque-initial"),
            text_response("opaque-after-set"),
        ],
    );
    assert_eq!(
        agent.conversation_id(),
        Some("conversation-opaque-initial"),
        "getter must return the builder-set id"
    );

    let _ = agent.turn("opaque-first").await.expect("first turn");
    let initial_count = capturing.events().len();

    // Re-attribute mid-conversation: the setter is the embedded caller's
    // contract for swapping the telemetry identity slot.
    agent.set_conversation_id(Some("conversation-opaque-rotated".into()));
    assert_eq!(
        agent.conversation_id(),
        Some("conversation-opaque-rotated"),
        "getter must reflect the setter's new id"
    );

    let _ = agent.turn("opaque-second").await.expect("second turn");
    let all_events = capturing.events();

    let initial_convs = attributed_conversation_ids(&all_events[..initial_count]);
    let rotated_convs = attributed_conversation_ids(&all_events[initial_count..]);
    assert!(
        !initial_convs.is_empty(),
        "first turn must emit attributed events"
    );
    assert!(
        !rotated_convs.is_empty(),
        "second turn must emit attributed events"
    );
    assert!(
        initial_convs
            .iter()
            .all(|c| *c == "conversation-opaque-initial"),
        "first turn must carry the initial id, got {initial_convs:?}"
    );
    assert!(
        rotated_convs
            .iter()
            .all(|c| *c == "conversation-opaque-rotated"),
        "second turn must carry the rotated id, got {rotated_convs:?}"
    );
}

/// The durable conversation id is a fact of record creation: re-opening the
/// backend re-reads the committed id rather than recomputing one.
#[test]
fn backend_durable_resolve_survives_reopen() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let id_before = {
        let backend = SqliteSessionBackend::new(tmp.path()).expect("backend");
        backend
            .resolve_or_create_conversation_id("session-key-opaque-durable")
            .unwrap()
    };
    let backend2 = SqliteSessionBackend::new(tmp.path()).expect("backend reopen");
    let id_after = backend2
        .resolve_or_create_conversation_id("session-key-opaque-durable")
        .unwrap();
    assert_eq!(
        id_before, id_after,
        "durable id must be re-read from the record, not recomputed"
    );
    assert!(
        looks_like_uuid(&id_before),
        "durable id should be a UUID: {id_before}"
    );
}

/// A legacy metadata row created before the conversation id column existed
/// (NULL id, no prior resolve) is backfilled on first resolve, and the
/// backfilled value is stable on re-resolve.
#[test]
fn backend_legacy_null_row_backfills_on_first_resolve() {
    let (_tmp, backend) = temp_backend();
    backend
        .append(
            "session-key-opaque-legacy",
            &ChatMessage::user("opaque-old"),
        )
        .unwrap();
    let id = backend
        .resolve_or_create_conversation_id("session-key-opaque-legacy")
        .unwrap();
    assert!(!id.is_empty(), "resolve must backfill a non-empty id");
    assert!(looks_like_uuid(&id), "backfilled id should be a UUID: {id}");
    assert_eq!(
        backend
            .resolve_or_create_conversation_id("session-key-opaque-legacy")
            .unwrap(),
        id,
        "re-resolve must return the same committed value"
    );
}

/// The reset path (`/clear`) clears history AND rotates the id in one atomic
/// operation: the fresh id differs from the prior one, the history is gone,
/// and a later resolve re-reads the rotated id (rotate is not repeated).
#[test]
fn backend_reset_clears_history_and_mints_a_fresh_id() {
    let (_tmp, backend) = temp_backend();
    backend
        .append("session-key-opaque-reset", &ChatMessage::user("opaque-a"))
        .unwrap();
    backend
        .append(
            "session-key-opaque-reset",
            &ChatMessage::assistant("opaque-b"),
        )
        .unwrap();
    let id1 = backend
        .resolve_or_create_conversation_id("session-key-opaque-reset")
        .unwrap();
    assert_eq!(
        backend.load("session-key-opaque-reset").len(),
        2,
        "history must be present before reset"
    );

    let id2 = backend
        .clear_and_rotate_conversation("session-key-opaque-reset")
        .unwrap();
    assert_ne!(id1, id2, "reset must mint a fresh id");
    assert!(
        backend.load("session-key-opaque-reset").is_empty(),
        "reset must clear history"
    );
    assert_eq!(
        backend
            .resolve_or_create_conversation_id("session-key-opaque-reset")
            .unwrap(),
        id2,
        "post-reset resolve must re-read the rotated id"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Data hygiene: the telemetry id never leaks routing/history/storage/path/
// sender/memory values, and is never a gateway/rpc session id or history key
// ─────────────────────────────────────────────────────────────────────────────

/// The memory session id selects the memory partition only; it must never
/// leak into the telemetry conversation id slot. Drive an agent whose
/// `memory_session_id` is a poison value and assert every attributed event
/// carries the clean caller-owned id instead, never any of the poison
/// routing/history/storage/path/sender/memory values - especially not the
/// `gw_<sid>` / `rpc_<sid>` / history-key / CLI-state-path / memory-scope
/// shapes that other owners' identifiers take.
#[tokio::test]
async fn conversation_id_does_not_leak_routing_history_storage_path_sender_or_memory() {
    let poison_memory_session = "mem-session-opaque-1";
    let clean_conversation = "conversation-opaque-clean";
    let poison_values = [
        "route-opaque-1",          // routing
        "history-key-opaque-1",    // history key
        "storage-opaque-1",        // storage
        "sender-opaque-1",         // sender
        "mem-session-opaque-1",    // memory scope (== memory_session_id)
        "/tmp/cli-state-opaque-1", // CLI state path
        "gw_session-opaque-1",     // gateway raw session id shape
        "rpc_session-opaque-1",    // rpc session id shape
    ];

    let capturing = Arc::new(CapturingObserver::default());
    let mut agent = build_capturing_agent(
        capturing.clone(),
        Some(clean_conversation),
        Some(poison_memory_session),
        vec![text_response("opaque-ok")],
    );
    let _ = agent.turn("opaque-poisoned").await.expect("turn");

    let events = capturing.events();
    let attributed: Vec<&ObserverEvent> = events
        .iter()
        .filter(|e| conversation_id_and_turn(e).0.is_some())
        .collect();
    assert!(!attributed.is_empty(), "turn must emit attributed events");
    // auto_save(true) fires a MemoryStore that uses the memory session for the
    // store call but the conversation id for telemetry - the guarantee is
    // that the two never cross.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ObserverEvent::MemoryStore { .. })),
        "auto_save must emit MemoryStore so the isolation is exercised"
    );

    for event in &attributed {
        let (conv, _) = conversation_id_and_turn(event);
        let conv = conv.expect("attributed event carries a conversation id");
        assert_eq!(
            conv, clean_conversation,
            "telemetry id must be the clean caller-owned value: {event:?}"
        );
        for poison in &poison_values {
            assert_ne!(
                conv, *poison,
                "conversation id must not equal poison {poison:?}: {event:?}"
            );
        }
        assert!(
            !conv.starts_with("gw_"),
            "conversation id must not be a gateway session id: {conv}"
        );
        assert!(
            !conv.starts_with("rpc_"),
            "conversation id must not be an rpc session id: {conv}"
        );
    }

    // Durable backend: the server-minted id is a fresh UUID, never the
    // history key / sender / routing value it was resolved for.
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let backend = SqliteSessionBackend::new(tmp.path()).expect("backend");
    let history_key = "history-key-opaque-1";
    let resolved = backend
        .resolve_or_create_conversation_id(history_key)
        .unwrap();
    assert_ne!(
        resolved, history_key,
        "durable id must not equal the history key it was resolved for"
    );
    for poison in &poison_values {
        assert_ne!(
            resolved, *poison,
            "durable id must not equal poison {poison:?}"
        );
    }
    assert!(
        !resolved.starts_with("gw_") && !resolved.starts_with("rpc_"),
        "durable id must not be a gateway/rpc session id: {resolved}"
    );
    assert!(
        looks_like_uuid(&resolved),
        "durable id should be a server-minted UUID: {resolved}"
    );
}
