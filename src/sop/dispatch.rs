//! Unified SOP event dispatch helpers.
//!
//! All event sources (MQTT, webhook, cron, peripheral) route through
//! `dispatch_sop_event` so that locking, audit, and health bookkeeping
//! happen in exactly one place.

use std::sync::{Arc, Mutex};

use tracing::{debug, info, warn};

use super::audit::SopAuditLogger;
use super::engine::{now_iso8601, SopEngine};
use super::types::{SopEvent, SopRun, SopRunAction, SopTriggerSource};

// ── Dispatch result ─────────────────────────────────────────────

/// Outcome of attempting to dispatch an event to the SOP engine.
#[derive(Debug, Clone)]
pub enum DispatchResult {
    /// A new SOP run was started. `action` carries the next step the runtime
    /// must execute (or wait for approval on). Callers that cannot act on the
    /// action (e.g. headless fan-in) must still audit/log it — never silently
    /// drop.
    Started {
        run_id: String,
        sop_name: String,
        action: SopRunAction,
    },
    /// A matching SOP was found but could not start (cooldown / concurrency).
    Skipped { sop_name: String, reason: String },
    /// No loaded SOP matched the event.
    NoMatch,
}

// ── Action helpers ──────────────────────────────────────────────

/// Extract the `run_id` from any `SopRunAction` variant.
fn extract_run_id_from_action(action: &SopRunAction) -> &str {
    match action {
        SopRunAction::ExecuteStep { run_id, .. }
        | SopRunAction::WaitApproval { run_id, .. }
        | SopRunAction::Completed { run_id, .. }
        | SopRunAction::Failed { run_id, .. } => run_id,
    }
}

/// Short label for logging which action was returned.
fn action_label(action: &SopRunAction) -> &'static str {
    match action {
        SopRunAction::ExecuteStep { .. } => "ExecuteStep",
        SopRunAction::WaitApproval { .. } => "WaitApproval",
        SopRunAction::Completed { .. } => "Completed",
        SopRunAction::Failed { .. } => "Failed",
    }
}

// ── Core dispatch ───────────────────────────────────────────────

/// Dispatch an incoming event to the SOP engine.
///
/// Pattern (batch lock — exactly 2 acquisitions):
/// 1. Lock → `match_trigger` → collect SOP names → drop lock
/// 2. Lock → for each name: `start_run` → collect results → drop lock
/// 3. Async (no lock): audit each started run
#[tracing::instrument(skip(engine, audit), fields(source = %event.source, topic = ?event.topic))]
pub async fn dispatch_sop_event(
    engine: &Arc<Mutex<SopEngine>>,
    audit: &SopAuditLogger,
    event: SopEvent,
) -> Vec<DispatchResult> {
    // Phase 1: match
    let matched_names: Vec<String> = match engine.lock() {
        Ok(eng) => eng
            .match_trigger(&event)
            .iter()
            .map(|s| s.name.clone())
            .collect(),
        Err(e) => {
            crate::health::mark_component_error("sop_dispatch", format!("lock poisoned: {e}"));
            warn!("SOP dispatch: engine lock poisoned during match phase: {e}");
            return vec![];
        }
    };

    if matched_names.is_empty() {
        debug!("SOP dispatch: no match for event");
        return vec![DispatchResult::NoMatch];
    }

    info!(
        "SOP dispatch: {} SOP(s) matched: {:?}",
        matched_names.len(),
        matched_names
    );

    // Phase 2: start runs
    let mut results = Vec::new();
    let mut started_runs: Vec<SopRun> = Vec::new();

    {
        let mut eng = match engine.lock() {
            Ok(e) => e,
            Err(e) => {
                crate::health::mark_component_error("sop_dispatch", format!("lock poisoned: {e}"));
                warn!("SOP dispatch: engine lock poisoned during start phase: {e}");
                return vec![];
            }
        };

        for sop_name in &matched_names {
            match eng.start_run(sop_name, event.clone()) {
                Ok(action) => {
                    // Extract run_id from the action (authoritative source)
                    let run_id = extract_run_id_from_action(&action).to_string();
                    // Snapshot the run for audit (must be done under lock)
                    if let Some(run) = eng.active_runs().get(&run_id) {
                        started_runs.push(run.clone());
                    }
                    info!(
                        "SOP dispatch: started '{}' run {run_id} (action: {})",
                        sop_name,
                        action_label(&action),
                    );
                    results.push(DispatchResult::Started {
                        run_id,
                        sop_name: sop_name.clone(),
                        action,
                    });
                }
                Err(e) => {
                    info!("SOP dispatch: skipped '{}': {e}", sop_name);
                    results.push(DispatchResult::Skipped {
                        sop_name: sop_name.clone(),
                        reason: e.to_string(),
                    });
                }
            }
        }
    } // lock dropped

    // Phase 3: audit (async, no lock)
    for run in &started_runs {
        if let Err(e) = audit.log_run_start(run).await {
            warn!("SOP dispatch: audit log failed for run {}: {e}", run.run_id);
        }
    }

    crate::health::mark_component_ok("sop_dispatch");
    results
}

// ── Headless result processing ──────────────────────────────────

/// Process dispatch results in headless (non-agent-loop) callers.
///
/// This handles audit and logging for fan-in callers (MQTT, webhook, cron)
/// that cannot execute SOP steps interactively. For `WaitApproval` actions,
/// approval timeout polling in the scheduler handles progression.
/// For `ExecuteStep` actions, the run is started in the engine but steps
/// cannot be executed without an agent loop — this is logged as a warning.
pub async fn process_headless_results(results: &[DispatchResult]) {
    for result in results {
        match result {
            DispatchResult::Started {
                run_id,
                sop_name,
                action,
            } => match action {
                SopRunAction::ExecuteStep { step, .. } => {
                    warn!(
                        "SOP headless dispatch: run {run_id} ('{sop_name}') ready for step {} \
                         '{}' but no agent loop available to execute",
                        step.number, step.title,
                    );
                }
                SopRunAction::WaitApproval { step, .. } => {
                    info!(
                        "SOP headless dispatch: run {run_id} ('{sop_name}') waiting for approval \
                         on step {} '{}'. Timeout polling will handle progression",
                        step.number, step.title,
                    );
                }
                SopRunAction::Completed { .. } => {
                    info!(
                        "SOP headless dispatch: run {run_id} ('{sop_name}') completed immediately"
                    );
                }
                SopRunAction::Failed { reason, .. } => {
                    warn!("SOP headless dispatch: run {run_id} ('{sop_name}') failed: {reason}");
                }
            },
            DispatchResult::Skipped { sop_name, reason } => {
                info!("SOP headless dispatch: skipped '{sop_name}': {reason}");
            }
            DispatchResult::NoMatch => {}
        }
    }
}

// ── Peripheral signal helper ────────────────────────────────────

/// Convenience wrapper for peripheral hardware callbacks.
///
/// Builds a `SopEvent` with source `Peripheral` and topic `"{board}/{signal}"`
/// then dispatches it through the standard path.
pub async fn dispatch_peripheral_signal(
    engine: &Arc<Mutex<SopEngine>>,
    audit: &SopAuditLogger,
    board: &str,
    signal: &str,
    payload: Option<&str>,
) -> Vec<DispatchResult> {
    let event = SopEvent {
        source: SopTriggerSource::Peripheral,
        topic: Some(format!("{board}/{signal}")),
        payload: payload.map(String::from),
        timestamp: now_iso8601(),
    };
    dispatch_sop_event(engine, audit, event).await
}

// ── Cron SOP cache + check ──────────────────────────────────────

/// Pre-parsed cron schedules for SOP triggers.
///
/// Built once at daemon startup to avoid re-parsing cron expressions
/// on every scheduler tick.
#[derive(Clone)]
pub struct SopCronCache {
    /// (sop_name, raw_expression, parsed_schedule)
    schedules: Vec<(String, String, cron::Schedule)>,
}

impl SopCronCache {
    /// Build cache from the current engine state.
    ///
    /// Locks the engine once, iterates SOPs, parses Cron trigger expressions.
    /// Invalid expressions are logged and skipped (fail-closed).
    pub fn from_engine(engine: &Arc<Mutex<SopEngine>>) -> Self {
        let mut schedules = Vec::new();
        let eng = match engine.lock() {
            Ok(e) => e,
            Err(e) => {
                warn!("SopCronCache: engine lock poisoned: {e}");
                return Self { schedules };
            }
        };

        for sop in eng.sops() {
            for trigger in &sop.triggers {
                if let super::types::SopTrigger::Cron { expression } = trigger {
                    // Normalize 5-field crontab to 6-field (prepend seconds)
                    let normalized = match crate::cron::schedule::normalize_expression(expression) {
                        Ok(n) => n,
                        Err(e) => {
                            warn!(
                                "SopCronCache: invalid cron expression '{}' in SOP '{}': {e}",
                                expression, sop.name
                            );
                            continue;
                        }
                    };
                    match normalized.parse::<cron::Schedule>() {
                        Ok(schedule) => {
                            schedules.push((sop.name.clone(), expression.clone(), schedule));
                        }
                        Err(e) => {
                            warn!(
                                "SopCronCache: failed to parse cron schedule '{}' for SOP '{}': {e}",
                                normalized, sop.name
                            );
                        }
                    }
                }
            }
        }

        info!("SopCronCache: cached {} cron schedule(s)", schedules.len());
        Self { schedules }
    }

    /// Return the cached schedules (for testing).
    #[cfg(test)]
    pub fn schedules(&self) -> &[(String, String, cron::Schedule)] {
        &self.schedules
    }
}

/// Check all cached cron SOP triggers for firings in the window
/// `(last_check, now]` and dispatch events for each.
///
/// Uses window-based evaluation so ticks between polls are never missed.
pub async fn check_sop_cron_triggers(
    engine: &Arc<Mutex<SopEngine>>,
    audit: &SopAuditLogger,
    cache: &SopCronCache,
    last_check: &mut chrono::DateTime<chrono::Utc>,
) -> Vec<DispatchResult> {
    let now = chrono::Utc::now();
    let mut all_results = Vec::new();

    for (_sop_name, expression, schedule) in &cache.schedules {
        // Check if any occurrence fell in the window (last_check, now].
        // At-most-once semantics: even if multiple ticks of the same expression
        // fell in the window (e.g., scheduler delayed), we fire only once.
        // This is intentional — SOP triggers should not retroactively batch-fire.
        let mut upcoming = schedule.after(last_check);
        if let Some(next) = upcoming.next() {
            if next <= now {
                // This expression fired in the window
                let event = SopEvent {
                    source: SopTriggerSource::Cron,
                    topic: Some(expression.clone()),
                    payload: None,
                    timestamp: now_iso8601(),
                };
                let results = dispatch_sop_event(engine, audit, event).await;
                all_results.extend(results);
            }
        }
    }

    *last_check = now;
    all_results
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryConfig, SopConfig};
    use crate::memory::traits::Memory;
    use crate::sop::types::{
        Sop, SopExecutionMode, SopPriority, SopRunAction, SopStep, SopTrigger, SopTriggerSource,
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
            }],
            cooldown_secs: 0,
            max_concurrent: 2,
            location: None,
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
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        // Leak the tempdir so it lives for the test
        std::mem::forget(tmp);
        SopAuditLogger::new(memory)
    }

    #[tokio::test]
    async fn dispatch_starts_matching_sop() {
        let engine = test_engine(vec![test_sop(
            "mqtt-sop",
            vec![SopTrigger::Mqtt {
                topic: "sensors/temp".into(),
                condition: None,
            }],
        )]);
        let audit = test_audit();

        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("sensors/temp".into()),
            payload: Some(r#"{"value": 42}"#.into()),
            timestamp: now_iso8601(),
        };

        let results = dispatch_sop_event(&engine, &audit, event).await;
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], DispatchResult::Started { sop_name, action, .. } if sop_name == "mqtt-sop" && matches!(action, SopRunAction::ExecuteStep { .. }))
        );
    }

    #[tokio::test]
    async fn dispatch_skips_when_cooldown_active() {
        let mut sop = test_sop("cooldown-sop", vec![SopTrigger::Manual]);
        sop.cooldown_secs = 3600;
        sop.max_concurrent = 1;
        let engine = test_engine(vec![sop]);
        let audit = test_audit();

        // Start a run manually so that completing it will trigger cooldown
        {
            let mut eng = engine.lock().unwrap();
            let _action = eng
                .start_run(
                    "cooldown-sop",
                    SopEvent {
                        source: SopTriggerSource::Manual,
                        topic: None,
                        payload: None,
                        timestamp: now_iso8601(),
                    },
                )
                .unwrap();
            // Complete the run
            let run_id = eng.active_runs().keys().next().unwrap().clone();
            eng.advance_step(
                &run_id,
                crate::sop::types::SopStepResult {
                    step_number: 1,
                    status: crate::sop::types::SopStepStatus::Completed,
                    output: "done".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();
        }

        // Now dispatch — should skip due to cooldown
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        };
        let results = dispatch_sop_event(&engine, &audit, event).await;
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], DispatchResult::Skipped { sop_name, .. } if sop_name == "cooldown-sop")
        );
    }

    #[tokio::test]
    async fn dispatch_returns_no_match_for_unknown_event() {
        let engine = test_engine(vec![test_sop("manual-sop", vec![SopTrigger::Manual])]);
        let audit = test_audit();

        // Send an MQTT event — the SOP only has a Manual trigger
        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("some/topic".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        let results = dispatch_sop_event(&engine, &audit, event).await;
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], DispatchResult::NoMatch));
    }

    #[tokio::test]
    async fn dispatch_batch_lock_starts_multiple_sops() {
        let sop1 = test_sop(
            "webhook-sop-1",
            vec![SopTrigger::Webhook {
                path: "/api/deploy".into(),
            }],
        );
        let sop2 = test_sop(
            "webhook-sop-2",
            vec![SopTrigger::Webhook {
                path: "/api/deploy".into(),
            }],
        );
        let engine = test_engine(vec![sop1, sop2]);
        let audit = test_audit();

        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/api/deploy".into()),
            payload: None,
            timestamp: now_iso8601(),
        };

        let results = dispatch_sop_event(&engine, &audit, event).await;
        let started_count = results
            .iter()
            .filter(|r| matches!(r, DispatchResult::Started { .. }))
            .count();
        assert_eq!(started_count, 2);
    }

    /// B1 DoD: prove that the action returned by `start_run` is captured in
    /// `DispatchResult::Started` — not silently dropped.
    #[tokio::test]
    async fn dispatch_captures_action_for_wait_approval() {
        // Supervised mode → WaitApproval on step 1
        let mut sop = test_sop(
            "supervised-sop",
            vec![SopTrigger::Mqtt {
                topic: "alert".into(),
                condition: None,
            }],
        );
        sop.execution_mode = SopExecutionMode::Supervised;
        let engine = test_engine(vec![sop]);
        let audit = test_audit();

        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("alert".into()),
            payload: None,
            timestamp: now_iso8601(),
        };

        let results = dispatch_sop_event(&engine, &audit, event).await;
        assert_eq!(results.len(), 1);
        match &results[0] {
            DispatchResult::Started {
                run_id,
                sop_name,
                action,
            } => {
                assert_eq!(sop_name, "supervised-sop");
                assert!(!run_id.is_empty());
                assert!(
                    matches!(action, SopRunAction::WaitApproval { .. }),
                    "Supervised SOP must return WaitApproval, got {:?}",
                    action
                );
            }
            other => panic!("Expected Started, got {other:?}"),
        }
    }

    /// B1 DoD: Auto-mode SOP returns ExecuteStep action in dispatch result.
    #[tokio::test]
    async fn dispatch_captures_action_for_execute_step() {
        let engine = test_engine(vec![test_sop("auto-sop", vec![SopTrigger::Manual])]);
        let audit = test_audit();

        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        };

        let results = dispatch_sop_event(&engine, &audit, event).await;
        assert_eq!(results.len(), 1);
        match &results[0] {
            DispatchResult::Started { action, .. } => {
                assert!(
                    matches!(action, SopRunAction::ExecuteStep { .. }),
                    "Auto SOP must return ExecuteStep, got {:?}",
                    action
                );
            }
            other => panic!("Expected Started, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peripheral_signal_dispatches_to_matching_sop() {
        let engine = test_engine(vec![test_sop(
            "gpio-sop",
            vec![SopTrigger::Peripheral {
                board: "nucleo".into(),
                signal: "pin_3".into(),
                condition: None,
            }],
        )]);
        let audit = test_audit();

        let results =
            dispatch_peripheral_signal(&engine, &audit, "nucleo", "pin_3", Some("1")).await;
        assert_eq!(results.len(), 1);
        assert!(
            matches!(&results[0], DispatchResult::Started { sop_name, .. } if sop_name == "gpio-sop" )
        );
    }

    #[tokio::test]
    async fn peripheral_signal_no_match_returns_empty() {
        let engine = test_engine(vec![test_sop(
            "gpio-sop",
            vec![SopTrigger::Peripheral {
                board: "nucleo".into(),
                signal: "pin_3".into(),
                condition: None,
            }],
        )]);
        let audit = test_audit();

        let results = dispatch_peripheral_signal(&engine, &audit, "rpi", "gpio_5", None).await;
        assert_eq!(results.len(), 1);
        assert!(matches!(&results[0], DispatchResult::NoMatch));
    }

    #[test]
    fn cron_cache_skips_invalid_expression() {
        let sop = test_sop(
            "bad-cron",
            vec![SopTrigger::Cron {
                expression: "not a valid cron".into(),
            }],
        );
        let engine = test_engine(vec![sop]);
        let cache = SopCronCache::from_engine(&engine);
        assert!(cache.schedules().is_empty());
    }

    #[test]
    fn cron_cache_parses_valid_expression() {
        let sop = test_sop(
            "valid-cron",
            vec![SopTrigger::Cron {
                expression: "0 */5 * * *".into(),
            }],
        );
        let engine = test_engine(vec![sop]);
        let cache = SopCronCache::from_engine(&engine);
        assert_eq!(cache.schedules().len(), 1);
        assert_eq!(cache.schedules()[0].0, "valid-cron");
        assert_eq!(cache.schedules()[0].1, "0 */5 * * *");
    }

    #[tokio::test]
    async fn cron_sop_trigger_fires_on_schedule() {
        let sop = test_sop(
            "cron-sop",
            vec![SopTrigger::Cron {
                expression: "* * * * *".into(),
            }],
        );
        let engine = test_engine(vec![sop]);
        let audit = test_audit();
        let cache = SopCronCache::from_engine(&engine);

        // Set last_check to 2 minutes ago so the window contains a tick
        let mut last_check = chrono::Utc::now() - chrono::Duration::minutes(2);
        let results = check_sop_cron_triggers(&engine, &audit, &cache, &mut last_check).await;

        let started = results
            .iter()
            .filter(|r| matches!(r, DispatchResult::Started { .. }))
            .count();
        assert!(started >= 1, "Expected at least 1 started SOP from cron");
    }

    #[tokio::test]
    async fn cron_sop_only_matching_expression_fires() {
        let sop1 = test_sop(
            "every-min",
            vec![SopTrigger::Cron {
                expression: "* * * * *".into(),
            }],
        );
        // An expression that won't fire in a 2-minute window from now:
        // "0 0 1 1 *" = midnight Jan 1
        let sop2 = test_sop(
            "yearly",
            vec![SopTrigger::Cron {
                expression: "0 0 1 1 *".into(),
            }],
        );
        let engine = test_engine(vec![sop1, sop2]);
        let audit = test_audit();
        let cache = SopCronCache::from_engine(&engine);

        let mut last_check = chrono::Utc::now() - chrono::Duration::minutes(2);
        let results = check_sop_cron_triggers(&engine, &audit, &cache, &mut last_check).await;

        // Only "every-min" should have fired
        let started_names: Vec<&str> = results
            .iter()
            .filter_map(|r| match r {
                DispatchResult::Started { sop_name, .. } => Some(sop_name.as_str()),
                _ => None,
            })
            .collect();
        assert!(started_names.contains(&"every-min"));
        assert!(!started_names.contains(&"yearly"));
    }

    #[tokio::test]
    async fn cron_sop_window_check_does_not_miss_tick() {
        let sop = test_sop(
            "every-min",
            vec![SopTrigger::Cron {
                expression: "* * * * *".into(),
            }],
        );
        let engine = test_engine(vec![sop]);
        let audit = test_audit();
        let cache = SopCronCache::from_engine(&engine);

        // Simulate: last_check was 5 minutes ago, poll just now
        let mut last_check = chrono::Utc::now() - chrono::Duration::minutes(5);
        let results = check_sop_cron_triggers(&engine, &audit, &cache, &mut last_check).await;

        // At least one tick should have been caught
        let started = results
            .iter()
            .filter(|r| matches!(r, DispatchResult::Started { .. }))
            .count();
        assert!(
            started >= 1,
            "Window-based check should catch ticks from 5 minutes ago"
        );

        // last_check should be updated to approximately now
        let now = chrono::Utc::now();
        assert!(
            (now - last_check).num_seconds() < 2,
            "last_check should be updated to now"
        );
    }
}
