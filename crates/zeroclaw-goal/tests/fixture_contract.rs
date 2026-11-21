//! Self-checks for the VT86 registry fixture; these do not prove SQLite behavior.

mod support;

use support::{IntentState, NoModelAccounting, Registry};
use zeroclaw_api::task::TaskStatus;
use zeroclaw_goal::{
    GoalPauseReason, GoalPauseState, GoalTaskRegistry, GoalTransitionResult,
    task::{GoalControlIntent, GoalControlIntentKind, GoalControlIntentSettlement},
};

fn intent() -> GoalControlIntent {
    GoalControlIntent {
        intent_id: uuid::Uuid::new_v4().to_string(),
        session_id: "fixture-session".into(),
        task_id: Some("fixture-goal".into()),
        expected_epoch: Some(1),
        predecessor_task_id: None,
        kind: GoalControlIntentKind::Pause,
        owner_pid: 1,
        owner_boot_id: "fixture-boot".into(),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn operator_pause() -> GoalPauseState {
    GoalPauseState {
        reason: GoalPauseReason::OperatorPaused,
        description: None,
        blockers: vec![],
    }
}

#[tokio::test]
async fn audit_claim_is_boot_bound_once_and_settlement_is_immutable() {
    let registry = Registry::default();
    registry.seed("fixture-session", TaskStatus::Running);
    let intent = intent();
    registry.record_control_intent(&intent).await.unwrap();
    assert_eq!(
        registry
            .claim_control_intent(&intent.intent_id, "another-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        registry
            .settle_control_intent(&intent.intent_id, GoalControlIntentSettlement::Applied)
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        registry
            .claim_control_intent(&intent.intent_id, "fixture-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        registry
            .claim_control_intent(&intent.intent_id, "fixture-boot")
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    assert_eq!(
        registry
            .settle_control_intent(&intent.intent_id, GoalControlIntentSettlement::Applied)
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        registry
            .settle_control_intent(
                &intent.intent_id,
                GoalControlIntentSettlement::Abandoned {
                    reason: "late_drop".into()
                }
            )
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    let state = registry.0.lock().unwrap();
    assert_eq!(
        state.intents[0].1,
        IntentState::Settled(GoalControlIntentSettlement::Applied)
    );
    assert_eq!(state.task.as_ref().unwrap().status, TaskStatus::Running);
    assert_eq!(state.task.as_ref().unwrap().execution_epoch, 1);
    assert!(
        state.transitions.is_empty(),
        "audit writes are not lifecycle authority"
    );
    let _boundary_only_accountant = NoModelAccounting;
}

#[tokio::test]
async fn guarded_pause_rejects_wrong_identity_and_applies_once() {
    let registry = Registry::default();
    registry.seed("fixture-session", TaskStatus::Running);
    for (id, session, epoch) in [
        ("another-task", "fixture-session", 1),
        ("fixture-goal", "another-session", 1),
        ("fixture-goal", "fixture-session", 2),
    ] {
        assert_eq!(
            registry
                .pause_session_goal(id, session, epoch, operator_pause())
                .await
                .unwrap(),
            GoalTransitionResult::Stale
        );
    }
    assert!(registry.0.lock().unwrap().transitions.is_empty());
    assert_eq!(
        registry
            .pause_session_goal("fixture-goal", "fixture-session", 1, operator_pause())
            .await
            .unwrap(),
        GoalTransitionResult::Applied
    );
    assert_eq!(
        registry
            .pause_session_goal("fixture-goal", "fixture-session", 1, operator_pause())
            .await
            .unwrap(),
        GoalTransitionResult::Stale
    );
    let state = registry.0.lock().unwrap();
    assert_eq!(state.transitions, ["pause"]);
    assert_eq!(state.task.as_ref().unwrap().execution_epoch, 2);
    assert_eq!(
        state.goal.as_ref().unwrap().pause_reason,
        Some(GoalPauseReason::OperatorPaused)
    );
}
