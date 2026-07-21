//! Replays captured `card.action.trigger` fixtures (collected from a live
//! Lark/Feishu tenant via `RUST_LOG=info,zeroclaw_log_event=debug`) through
//! the exact JSON-pointer logic used by `LarkChannel::handle_card_action_event`,
//! and asserts that `approval_id` + `decision` extract via the production

use serde_json::Value;

const APPROVE_FIXTURE: &str = include_str!("fixtures/lark/card_action_approve.json");
const DENY_FIXTURE: &str = include_str!("fixtures/lark/card_action_deny.json");
const ALWAYS_FIXTURE: &str = include_str!("fixtures/lark/card_action_always.json");

fn extract_decision(payload: &Value) -> (String, String) {
    let value = payload
        .pointer("/action/value")
        .or_else(|| payload.pointer("/action/behaviors/0/value"))
        .expect(
            "card.action.trigger payload must expose /action/value or \
             /action/behaviors/0/value — drift here means production parser \
             will WARN-and-fail on real clicks",
        );

    let approval_id = value
        .get("approval_id")
        .and_then(|v| v.as_str())
        .expect("approval_id must be a string under the click-value object")
        .to_owned();

    let decision = value
        .get("decision")
        .and_then(|v| v.as_str())
        .expect("decision must be a string under the click-value object")
        .to_owned();

    (approval_id, decision)
}

#[test]
fn approve_fixture_round_trips_through_production_pointer_logic() {
    let payload: Value =
        serde_json::from_str(APPROVE_FIXTURE).expect("approve fixture must be valid JSON");
    let (approval_id, decision) = extract_decision(&payload);
    assert!(!approval_id.is_empty(), "approval_id must be non-empty");
    assert_eq!(decision, "approve");
}

#[test]
fn deny_fixture_round_trips_through_production_pointer_logic() {
    let payload: Value =
        serde_json::from_str(DENY_FIXTURE).expect("deny fixture must be valid JSON");
    let (approval_id, decision) = extract_decision(&payload);
    assert!(!approval_id.is_empty(), "approval_id must be non-empty");
    assert_eq!(decision, "deny");
}

#[test]
fn always_fixture_round_trips_through_production_pointer_logic() {
    let payload: Value =
        serde_json::from_str(ALWAYS_FIXTURE).expect("always fixture must be valid JSON");
    let (approval_id, decision) = extract_decision(&payload);
    assert!(!approval_id.is_empty(), "approval_id must be non-empty");
    assert_eq!(decision, "always");
}
