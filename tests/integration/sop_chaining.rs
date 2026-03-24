//! Integration tests for cross-SOP workflow chaining via SopCompletion trigger.

use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use zeroclaw::config::{MemoryConfig, SopConfig};
use zeroclaw::memory::{create_memory, traits::Memory};
use zeroclaw::sop::{
    audit::SopAuditLogger,
    dispatch::{dispatch_completion, dispatch_sop_event},
    engine::SopEngine,
    types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopStep, SopStepKind, SopStepResult,
        SopStepStatus, SopTrigger, SopTriggerSource,
    },
};

fn test_sop(name: &str, triggers: Vec<SopTrigger>) -> Sop {
    Sop {
        name: name.into(),
        description: format!("Test SOP: {name}"),
        version: "1.0.0".into(),
        priority: SopPriority::Normal,
        execution_mode: SopExecutionMode::Auto,
        triggers,
        steps: vec![SopStep {
            number: 1,
            title: "Step one".into(),
            body: "Do step one".into(),
            suggested_tools: vec![],
            requires_confirmation: false,
            kind: SopStepKind::Execute,
            schema: None,
        }],
        cooldown_secs: 0,
        max_concurrent: 2,
        location: None,
        deterministic: false,
    }
}

fn test_engine(sops: Vec<Sop>) -> Arc<Mutex<SopEngine>> {
    let mut engine = SopEngine::new(SopConfig::default());
    engine.set_sops_for_test(sops);
    Arc::new(Mutex::new(engine))
}

fn test_audit() -> SopAuditLogger {
    let mem_cfg = MemoryConfig {
        backend: "sqlite".into(),
        ..MemoryConfig::default()
    };
    let tmp = TempDir::new().unwrap();
    let memory: Arc<dyn Memory> = Arc::from(create_memory(&mem_cfg, tmp.path(), None).unwrap());
    // Leak the tempdir so it lives for the test
    std::mem::forget(tmp);
    SopAuditLogger::new(memory)
}

fn now_iso8601() -> String {
    zeroclaw::sop::engine::now_iso8601()
}

#[tokio::test]
async fn sop_completion_triggers_downstream_sop() {
    // SOP A: manual trigger
    let sop_a = test_sop("sop_a", vec![SopTrigger::Manual]);

    // SOP B: triggered when sop_a completes
    let sop_b = test_sop(
        "sop_b",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_a".into(),
            on_status: None,
        }],
    );

    let engine = test_engine(vec![sop_a, sop_b]);
    let audit = test_audit();

    // Start SOP A manually
    let manual_event = SopEvent {
        source: SopTriggerSource::Manual,
        topic: None,
        payload: None,
        timestamp: now_iso8601(),
    };

    let results = dispatch_sop_event(&engine, &audit, manual_event).await;
    assert_eq!(results.len(), 1, "SOP A should start");

    // Get run_id for SOP A
    let run_id_a = {
        let eng = engine.lock().unwrap();
        eng.active_runs().keys().next().unwrap().clone()
    };

    // Complete SOP A's step
    {
        let mut eng = engine.lock().unwrap();
        eng.advance_step(
            &run_id_a,
            SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "done".into(),
                started_at: now_iso8601(),
                completed_at: Some(now_iso8601()),
            },
        )
        .unwrap();
    }

    // Now dispatch the completion event manually (simulating what dispatch.rs should do)
    let completion_event = SopEvent {
        source: SopTriggerSource::SopCompletion,
        topic: Some("sop_a".into()),
        payload: Some(
            serde_json::json!({
                "status": "completed",
                "run_id": run_id_a,
                "sop_name": "sop_a",
            })
            .to_string(),
        ),
        timestamp: now_iso8601(),
    };

    let results = dispatch_sop_event(&engine, &audit, completion_event).await;
    assert_eq!(results.len(), 1, "SOP B should be triggered");

    // Verify SOP B was started
    let eng = engine.lock().unwrap();
    let active_runs: Vec<_> = eng.active_runs().values().collect();
    assert_eq!(
        active_runs.len(),
        1,
        "SOP B should be the only active run (SOP A finished)"
    );
    assert_eq!(active_runs[0].sop_name, "sop_b");
}

#[tokio::test]
async fn sop_completion_respects_cooldown() {
    // SOP A: manual trigger
    let sop_a = test_sop("sop_a", vec![SopTrigger::Manual]);

    // SOP B: triggered by sop_a completion, with cooldown
    let mut sop_b = test_sop(
        "sop_b",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_a".into(),
            on_status: None,
        }],
    );
    sop_b.cooldown_secs = 3600; // 1 hour

    let engine = test_engine(vec![sop_a, sop_b]);
    let audit = test_audit();

    // First completion event
    let completion_event = SopEvent {
        source: SopTriggerSource::SopCompletion,
        topic: Some("sop_a".into()),
        payload: Some(
            serde_json::json!({
                "status": "completed",
                "run_id": "run-001",
                "sop_name": "sop_a",
            })
            .to_string(),
        ),
        timestamp: now_iso8601(),
    };

    let results = dispatch_sop_event(&engine, &audit, completion_event.clone()).await;
    assert_eq!(results.len(), 1, "First completion should trigger SOP B");

    // Complete SOP B
    {
        let mut eng = engine.lock().unwrap();
        let run_id = eng.active_runs().keys().next().unwrap().clone();
        eng.advance_step(
            &run_id,
            SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "done".into(),
                started_at: now_iso8601(),
                completed_at: Some(now_iso8601()),
            },
        )
        .unwrap();
    }

    // Second completion event (immediate)
    let _results = dispatch_sop_event(&engine, &audit, completion_event).await;
    // Should be skipped due to cooldown
    let eng = engine.lock().unwrap();
    assert_eq!(
        eng.active_runs().len(),
        0,
        "Second completion should not start SOP B (cooldown)"
    );
}

#[tokio::test]
async fn sop_completion_status_filter_completed_only() {
    let sop_a = test_sop("sop_a", vec![SopTrigger::Manual]);

    // SOP B: only triggers on completed status
    let sop_b = test_sop(
        "sop_b",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_a".into(),
            on_status: Some("completed".into()),
        }],
    );

    let engine = test_engine(vec![sop_a, sop_b]);
    let audit = test_audit();

    // Failed completion event
    let failed_event = SopEvent {
        source: SopTriggerSource::SopCompletion,
        topic: Some("sop_a".into()),
        payload: Some(
            serde_json::json!({
                "status": "failed",
                "run_id": "run-001",
                "sop_name": "sop_a",
            })
            .to_string(),
        ),
        timestamp: now_iso8601(),
    };

    let results = dispatch_sop_event(&engine, &audit, failed_event).await;
    assert_eq!(
        results.len(),
        1,
        "Should return NoMatch or similar, not trigger SOP B"
    );
    {
        let eng = engine.lock().unwrap();
        assert_eq!(
            eng.active_runs().len(),
            0,
            "SOP B should NOT be triggered by failed status"
        );
    }

    // Completed event
    let completed_event = SopEvent {
        source: SopTriggerSource::SopCompletion,
        topic: Some("sop_a".into()),
        payload: Some(
            serde_json::json!({
                "status": "completed",
                "run_id": "run-002",
                "sop_name": "sop_a",
            })
            .to_string(),
        ),
        timestamp: now_iso8601(),
    };

    let results = dispatch_sop_event(&engine, &audit, completed_event).await;
    assert_eq!(results.len(), 1, "Should trigger SOP B");
    let eng = engine.lock().unwrap();
    assert_eq!(
        eng.active_runs().len(),
        1,
        "SOP B should be triggered by completed status"
    );
}

#[tokio::test]
async fn sop_completion_no_deadlock_under_lock() {
    // Verify that dispatch_completion does NOT hold the engine lock when calling
    // dispatch_sop_event. If it did, dispatch_sop_event's lock acquisition would
    // deadlock. A timeout proves no deadlock occurred.
    let sop_a = test_sop("sop_a", vec![SopTrigger::Manual]);
    let sop_b = test_sop(
        "sop_b",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_a".into(),
            on_status: None,
        }],
    );

    let engine = test_engine(vec![sop_a, sop_b]);
    let audit = test_audit();

    // dispatch_completion internally calls dispatch_sop_event (which acquires the lock).
    // If the lock were held across this call, we'd deadlock.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        dispatch_completion(&engine, &audit, "sop_a", "run-001", "completed"),
    )
    .await;

    assert!(
        result.is_ok(),
        "dispatch_completion should not deadlock (timed out)"
    );

    // Verify SOP B was triggered by the completion event
    let eng = engine.lock().unwrap();
    assert_eq!(eng.active_runs().len(), 1, "SOP B should have started");
    assert_eq!(eng.active_runs().values().next().unwrap().sop_name, "sop_b");
}

#[tokio::test]
async fn sop_completion_concurrent_no_deadlock() {
    // Verify that concurrent completion dispatches don't deadlock.
    // Uses two separate audit loggers since SopAuditLogger is not Clone.
    let sop_a = test_sop("sop_a", vec![SopTrigger::Manual]);
    let sop_b = test_sop("sop_b", vec![SopTrigger::Manual]);
    let mut sop_c = test_sop(
        "sop_c",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_a".into(),
            on_status: None,
        }],
    );
    sop_c.max_concurrent = 10;
    let mut sop_d = test_sop(
        "sop_d",
        vec![SopTrigger::SopCompletion {
            sop_name: "sop_b".into(),
            on_status: None,
        }],
    );
    sop_d.max_concurrent = 10;

    let engine = test_engine(vec![sop_a, sop_b, sop_c, sop_d]);
    let audit_1 = test_audit();
    let audit_2 = test_audit();

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            dispatch_completion(&engine, &audit_1, "sop_a", "run-001", "completed"),
            dispatch_completion(&engine, &audit_2, "sop_b", "run-002", "completed"),
        )
    })
    .await;

    assert!(
        result.is_ok(),
        "Concurrent completion dispatches should not deadlock"
    );

    // Both SOP C and SOP D should have been triggered
    let eng = engine.lock().unwrap();
    assert_eq!(
        eng.active_runs().len(),
        2,
        "Both SOP C and SOP D should be active"
    );
}
