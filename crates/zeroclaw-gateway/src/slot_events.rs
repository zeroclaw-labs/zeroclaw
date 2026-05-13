//! Slot-scoped event payload helpers for the dashboard multiplex.
//!
//! M2 introduces a second class of event on `AppState.event_tx`: events that
//! carry a `slot_id` so a dashboard client subscribing in read-only mode can
//! filter by slot. Legacy `/ws/chat` events (no `slot_id`) flow unchanged.
//!
//! Helpers here produce the JSON shapes the dashboard WebSocket protocol
//! expects (see `multi-session-dashboard.md §4.3`). Each helper returns a
//! `serde_json::Value` ready to publish onto the broadcast channel.

use serde_json::{Value, json};

use crate::slot::SlotResponse;

/// Event channel name used by the subscribe-mode filter.
///
/// The dashboard subscribes to symbolic channel names; the filter converts
/// those to event-type prefixes (`"slots"`, `"slot"`, `"chat:<id>"`, etc.).
pub const CHANNEL_SLOTS: &str = "slots";
pub const CHANNEL_DASHBOARD: &str = "dashboard";

/// Derived channel name for per-slot chat stream subscriptions.
///
/// Dashboard clients subscribe to `chat:<slot_id>` to receive chat deltas
/// for a specific slot. Returns the channel identifier string.
pub fn chat_channel(slot_id: &str) -> String {
    format!("chat:{slot_id}")
}

/// Build a `slots` full-list event payload.
///
/// Emitted on initial subscribe (dashboard mode) so the client can hydrate
/// its sidebar in one round-trip. `slots` is the full authoritative list.
pub fn slots_full(slots: &[SlotResponse]) -> Value {
    json!({
        "type": "slots",
        "data": slots,
    })
}

/// Build a single-slot update event payload.
///
/// Emitted whenever a slot transitions state (Idle → Running, Running →
/// Idle, etc.) or its metadata changes. Carries `slot_id` at the event
/// root to keep filter logic trivial.
pub fn slot_updated(slot: &SlotResponse) -> Value {
    json!({
        "type": "slot",
        "slot_id": slot.id,
        "data": slot,
    })
}

/// Build a chat-delta event payload for a slot's streaming turn.
///
/// `role` is one of `"user"`, `"assistant"`, `"tool"` (mirrors the existing
/// WS chat protocol at `ws.rs`). `content` is the delta text; `done`
/// signals the terminal event.
pub fn chat_delta(slot_id: &str, role: &str, content: &str, done: bool) -> Value {
    json!({
        "type": "chat",
        "slot_id": slot_id,
        "data": {
            "role": role,
            "content": content,
            "done": done,
        },
    })
}

/// Build a tool-approval request event tagged with the originating slot.
///
/// Companion to the connection-scoped `WsApprovalChannel` from #6387: when
/// a slot-spawned agent fires `TurnEvent::ApprovalRequest`, the slot agent
/// wraps it with `slot_id` before publishing so the dashboard sidebar can
/// badge the correct slot.
pub fn permission_request(
    slot_id: &str,
    request_id: &str,
    tool_name: &str,
    arguments_summary: &str,
    timeout_secs: u64,
) -> Value {
    json!({
        "type": "permission_request",
        "slot_id": slot_id,
        "data": {
            "request_id": request_id,
            "tool_name": tool_name,
            "arguments_summary": arguments_summary,
            "timeout_secs": timeout_secs,
        },
    })
}

/// Inspect an event payload and return the channel name a dashboard
/// subscriber would use to receive it, if any.
///
/// Returns `None` for legacy /ws/chat events (no `slot_id`, no recognized
/// dashboard `type`), which callers treat as "not intended for dashboard
/// subscribers".
pub fn event_channel(event: &Value) -> Option<String> {
    let ty = event.get("type").and_then(|v| v.as_str())?;
    match ty {
        "slots" => Some(CHANNEL_SLOTS.to_string()),
        "slot" => Some(CHANNEL_SLOTS.to_string()),
        "dashboard" => Some(CHANNEL_DASHBOARD.to_string()),
        "chat" | "permission_request" => {
            let slot_id = event.get("slot_id").and_then(|v| v.as_str())?;
            Some(chat_channel(slot_id))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slot::SlotResponse;
    use zeroclaw_infra::slot::{Slot, SlotState};

    fn sample_slot_response() -> SlotResponse {
        SlotResponse::from(Slot {
            id: "slot-1".into(),
            session_id: "gw_abc".into(),
            title: "T".into(),
            agent_config: Default::default(),
            state: SlotState::Running,
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            dirty: false,
            workspace: None,
        })
    }

    #[test]
    fn slot_updated_carries_slot_id_at_root() {
        let ev = slot_updated(&sample_slot_response());
        assert_eq!(ev["type"], "slot");
        assert_eq!(ev["slot_id"], "slot-1");
        assert_eq!(ev["data"]["id"], "slot-1");
        assert_eq!(ev["data"]["state"], "running");
    }

    #[test]
    fn chat_delta_shape_matches_ws_protocol() {
        let ev = chat_delta("slot-1", "assistant", "hello", false);
        assert_eq!(ev["type"], "chat");
        assert_eq!(ev["slot_id"], "slot-1");
        assert_eq!(ev["data"]["role"], "assistant");
        assert_eq!(ev["data"]["content"], "hello");
        assert_eq!(ev["data"]["done"], false);
    }

    #[test]
    fn permission_request_includes_request_id() {
        let ev = permission_request("slot-1", "req-abc", "shell", "ls -la", 120);
        assert_eq!(ev["type"], "permission_request");
        assert_eq!(ev["slot_id"], "slot-1");
        assert_eq!(ev["data"]["request_id"], "req-abc");
        assert_eq!(ev["data"]["tool_name"], "shell");
    }

    #[test]
    fn event_channel_routes_slot_events_to_slots_channel() {
        let ev = slot_updated(&sample_slot_response());
        assert_eq!(event_channel(&ev).as_deref(), Some("slots"));
    }

    #[test]
    fn event_channel_routes_chat_to_per_slot_channel() {
        let ev = chat_delta("slot-42", "assistant", "hi", true);
        assert_eq!(event_channel(&ev).as_deref(), Some("chat:slot-42"));
    }

    #[test]
    fn event_channel_routes_permission_request_to_per_slot_channel() {
        let ev = permission_request("slot-42", "r", "t", "a", 30);
        assert_eq!(event_channel(&ev).as_deref(), Some("chat:slot-42"));
    }

    #[test]
    fn event_channel_returns_none_for_legacy_events() {
        let legacy = json!({"type": "turn_start", "session_id": "gw_xyz"});
        assert_eq!(event_channel(&legacy), None);
    }

    #[test]
    fn slots_full_wraps_list() {
        let s = sample_slot_response();
        let ev = slots_full(&[s]);
        assert_eq!(ev["type"], "slots");
        let arr = ev["data"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["id"], "slot-1");
    }
}
