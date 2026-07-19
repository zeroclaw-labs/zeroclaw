//! Live SOP action executor.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use super::audit::SopAuditLogger;
use super::engine::SopEngine;
use super::types::{SopRun, SopRunAction, SopStepResult, StepToolCall};

use crate::agent::history::truncate_tool_result;
use crate::agent::turn::redact::{scrub_credentials, scrub_credentials_value};

const MAX_STEP_TOOL_CALLS: usize = 256;
const MAX_STEP_TOOL_OUTPUT_CHARS: usize = 4096;

/// Live SOP action captured by SOP tools while they run inside an agent turn.
#[derive(Clone)]
pub(crate) struct QueuedSopAction {
    pub engine: Arc<Mutex<SopEngine>>,
    pub audit: Option<Arc<SopAuditLogger>>,
    pub action: SopRunAction,
}

pub(crate) type LiveActionQueue = Arc<Mutex<VecDeque<QueuedSopAction>>>;

/// Ordered tool invocations captured while a live SOP step's nested tool
/// loop runs. Scoped per step so concurrent runs never interleave.
pub(crate) type StepCallSink = Arc<Mutex<Vec<StepToolCall>>>;

tokio::task_local! {
    static LIVE_SOP_ACTION_QUEUE: Option<LiveActionQueue>;
    static LIVE_STEP_CALL_SINK: Option<StepCallSink>;
}

pub(crate) fn new_live_action_queue() -> LiveActionQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

pub(crate) async fn scope_live_action_queue<T>(
    queue: LiveActionQueue,
    future: impl Future<Output = T>,
) -> T {
    LIVE_SOP_ACTION_QUEUE.scope(Some(queue), future).await
}

pub(crate) fn new_step_call_sink() -> StepCallSink {
    Arc::new(Mutex::new(Vec::new()))
}

pub(crate) async fn scope_step_call_sink<T>(
    sink: StepCallSink,
    future: impl Future<Output = T>,
) -> T {
    LIVE_STEP_CALL_SINK.scope(Some(sink), future).await
}

/// Record one executed tool call into the innermost active step sink.
/// No-op outside a live SOP step scope, so the turn loop can call this
/// unconditionally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_step_tool_call(
    tool: &str,
    args: &serde_json::Value,
    success: bool,
    output: String,
    output_data: Option<serde_json::Value>,
    error: Option<&str>,
    duration_ms: u64,
) {
    let _ = LIVE_STEP_CALL_SINK.try_with(|sink| {
        if let Some(sink) = sink
            && let Ok(mut calls) = sink.lock()
        {
            if calls.len() >= MAX_STEP_TOOL_CALLS {
                return;
            }
            let index = u32::try_from(calls.len()).unwrap_or(u32::MAX);
            let scrubbed_args = scrub_credentials(&args.to_string());
            let args = serde_json::from_str(&scrubbed_args).unwrap_or(serde_json::Value::Null);
            let output =
                truncate_tool_result(&scrub_credentials(&output), MAX_STEP_TOOL_OUTPUT_CHARS);
            let output_data = output_data.map(scrub_credentials_value);
            calls.push(StepToolCall {
                index,
                tool: tool.to_string(),
                args,
                success,
                output,
                output_data,
                error: error.map(scrub_credentials),
                duration_ms,
            });
        }
    });
}

/// True when a live SOP step scope is active on this task, so the turn loop
/// can skip the argument/output clones the capture would otherwise consume.
pub(crate) fn step_capture_active() -> bool {
    LIVE_STEP_CALL_SINK
        .try_with(|sink| sink.is_some())
        .unwrap_or(false)
}

pub(crate) fn drain_step_calls(sink: &StepCallSink) -> Vec<StepToolCall> {
    match sink.lock() {
        Ok(mut calls) => std::mem::take(&mut *calls),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    }
}

/// Queue a live action when the current tool call is running inside an agent
/// turn. Only `ExecuteStep` actions are queued; all other variants are already
/// terminal or blocked.
pub(crate) fn enqueue_live_action(
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    action: &SopRunAction,
) {
    if !matches!(action, SopRunAction::ExecuteStep { .. }) {
        return;
    }

    let queued = QueuedSopAction {
        engine,
        audit,
        action: action.clone(),
    };
    let _ = LIVE_SOP_ACTION_QUEUE.try_with(|queue| {
        if let Some(queue) = queue
            && let Ok(mut queue) = queue.lock()
        {
            queue.push_back(queued);
        }
    });
}

pub(crate) fn drain_live_actions(queue: &LiveActionQueue) -> Vec<QueuedSopAction> {
    match queue.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(poisoned) => poisoned.into_inner().drain(..).collect(),
    }
}

/// Upper bound on steps a single headless drive may execute, so a routing
/// cycle can never pin a background task forever.
const MAX_HEADLESS_DRIVE_STEPS: usize = 128;

/// Spawn a background task that drives a resumed SOP action to its next
/// blocking or terminal state. Gate-clearing surfaces without an ambient agent
/// turn (HTTP decide, WS approvals, manual dashboard runs) land here:
/// `ExecuteStep` runs through a fresh agent loop under the step's resolved
/// agent, `DeterministicStep` routes through the engine's headless
/// deterministic driver, and every other action is already parked or terminal.
pub fn spawn_headless_run_driver(
    config: zeroclaw_config::schema::Config,
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    first_action: SopRunAction,
) {
    zeroclaw_spawn::spawn!(async move {
        drive_headless_run(config, engine, audit, first_action).await;
    });
}

async fn drive_headless_run(
    config: zeroclaw_config::schema::Config,
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    first_action: SopRunAction,
) {
    use crate::sop::types::SopStepStatus;

    let mut action = first_action;
    for _ in 0..MAX_HEADLESS_DRIVE_STEPS {
        match action {
            SopRunAction::ExecuteStep {
                run_id,
                step,
                context,
            } => {
                let agent_alias = step
                    .agent
                    .clone()
                    .or_else(|| config.agents.keys().min().cloned())
                    .unwrap_or_default();
                let started_at = crate::sop::engine::now_iso8601();
                let session_path =
                    std::path::PathBuf::from(format!("sop-{run_id}-step-{}", step.number));
                let run_result = Box::pin(crate::agent::run(
                    config.clone(),
                    &agent_alias,
                    Some(context),
                    None,
                    None,
                    config
                        .model_provider_for_agent(&agent_alias)
                        .and_then(|e| e.temperature),
                    vec![],
                    false,
                    Some(session_path),
                    None,
                    zeroclaw_api::ingress::TurnOrigin::Daemon,
                    crate::agent::loop_::AgentRunOverrides::default(),
                ))
                .await;
                let completed_at = crate::sop::engine::now_iso8601();
                let step_result = match run_result {
                    Ok(output) => SopStepResult {
                        step_number: step.number,
                        status: SopStepStatus::Completed,
                        output,
                        started_at,
                        completed_at: Some(completed_at),
                        tool_calls: Vec::new(),
                    },
                    Err(e) => SopStepResult {
                        step_number: step.number,
                        status: SopStepStatus::Failed,
                        output: e.to_string(),
                        started_at,
                        completed_at: Some(completed_at),
                        tool_calls: Vec::new(),
                    },
                };
                match advance_sop_step(&engine, &run_id, step_result.clone()) {
                    Ok((next, finished_run)) => {
                        audit_sop_step(
                            audit.as_deref(),
                            &run_id,
                            &step_result,
                            finished_run.as_ref(),
                        )
                        .await;
                        action = next;
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "error": e.to_string(),
                            })),
                            "SOP headless driver: failed to advance run"
                        );
                        return;
                    }
                }
            }
            SopRunAction::DeterministicStep { ref run_id, .. } => {
                let run_id = run_id.clone();
                let next = {
                    let mut guard = match engine.lock() {
                        Ok(g) => g,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    guard.drive_headless_deterministic(&run_id, action)
                };
                match next {
                    Ok(SopRunAction::DeterministicStep { .. }) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"run_id": run_id})),
                            "SOP headless driver: deterministic drive made no progress"
                        );
                        return;
                    }
                    Ok(next) => action = next,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "error": e.to_string(),
                            })),
                            "SOP headless driver: deterministic drive failed"
                        );
                        return;
                    }
                }
            }
            SopRunAction::WaitApproval { run_id, step, .. }
            | SopRunAction::CheckpointWait { run_id, step, .. } => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "step": step.number,
                        })),
                    "SOP headless driver: run parked at a gate"
                );
                return;
            }
            SopRunAction::Pending {
                run_id,
                step,
                reason,
                ..
            } => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "step": step,
                            "reason": reason,
                        })),
                    "SOP headless driver: run pending on dependencies"
                );
                return;
            }
            SopRunAction::Completed { run_id, sop_name } => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "sop_name": sop_name,
                        })),
                    "SOP headless driver: run completed"
                );
                return;
            }
            SopRunAction::Failed {
                run_id,
                sop_name,
                reason,
            } => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "run_id": run_id,
                            "sop_name": sop_name,
                            "reason": reason,
                        })),
                    "SOP headless driver: run failed"
                );
                return;
            }
        }
    }
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
        "SOP headless driver: step budget exhausted; leaving run in place"
    );
}

pub(crate) fn advance_sop_step(
    engine: &Arc<Mutex<SopEngine>>,
    run_id: &str,
    result: SopStepResult,
) -> Result<(SopRunAction, Option<SopRun>)> {
    let mut engine = engine
        .lock()
        .map_err(|e| anyhow::Error::msg(format!("SOP engine lock poisoned: {e}")))?;
    let action = engine
        .advance_step(run_id, result)
        .with_context(|| format!("failed to advance SOP run {run_id}"))?;
    let finished_run = match &action {
        SopRunAction::Completed { run_id, .. } | SopRunAction::Failed { run_id, .. } => {
            engine.get_run(run_id).cloned()
        }
        _ => None,
    };
    Ok((action, finished_run))
}

pub(crate) async fn audit_sop_step(
    audit: Option<&SopAuditLogger>,
    run_id: &str,
    result: &SopStepResult,
    finished_run: Option<&SopRun>,
) {
    let Some(audit) = audit else {
        return;
    };
    if let Err(e) = audit.log_step_result(run_id, result).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": e.to_string()})),
            "SOP executor: audit log_step_result failed"
        );
    }
    if let Some(run) = finished_run
        && let Err(e) = audit.log_run_complete(run).await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"error": e.to_string()})),
            "SOP executor: audit log_run_complete failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::metrics::SopMetricsCollector;
    use crate::sop::types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopStep, SopStepResult, SopStepStatus,
        SopTrigger, SopTriggerSource,
    };
    use serde_json::json;
    use zeroclaw_config::schema::SopConfig;

    fn test_sop(name: &str) -> Sop {
        Sop {
            name: name.to_string(),
            description: "Test SOP".to_string(),
            version: "0.1.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Step one".to_string(),
                body: "Complete the step".to_string(),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: crate::sop::types::SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }
    }

    fn manual_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-06-28T00:00:00Z".to_string(),
        }
    }

    fn extract_run_id(action: &SopRunAction) -> String {
        match action {
            SopRunAction::ExecuteStep { run_id, .. } => run_id.clone(),
            other => panic!("expected ExecuteStep, got {other:?}"),
        }
    }

    #[test]
    fn live_executor_records_terminal_metrics_once() {
        let collector = SopMetricsCollector::shared();
        collector.reset_for_test();

        let mut engine = SopEngine::new(SopConfig::default()).with_metrics(collector.clone());
        engine.set_sops_for_test(vec![test_sop("live-once")]);
        let action = engine.start_run("live-once", manual_event()).unwrap();
        let run_id = extract_run_id(&action);
        let engine = Arc::new(Mutex::new(engine));

        let (action, finished_run) = advance_sop_step(
            &engine,
            &run_id,
            SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "ok".to_string(),
                started_at: "2026-06-28T00:00:00Z".to_string(),
                completed_at: Some("2026-06-28T00:00:01Z".to_string()),
                tool_calls: Vec::new(),
            },
        )
        .unwrap();

        assert!(matches!(action, SopRunAction::Completed { .. }));
        assert!(finished_run.is_some());
        assert_eq!(
            collector.get_metric_value("sop.runs_completed"),
            Some(json!(1u64))
        );
        assert_eq!(
            collector.get_metric_value("sop.live-once.runs_completed"),
            Some(json!(1u64))
        );
    }

    #[tokio::test]
    async fn step_call_sink_captures_in_order_and_only_inside_scope() {
        // Outside any scope: silently dropped.
        record_step_tool_call(
            "shell",
            &json!({"command": "ls"}),
            true,
            "x".into(),
            None,
            None,
            1,
        );

        let sink = new_step_call_sink();
        scope_step_call_sink(sink.clone(), async {
            record_step_tool_call(
                "http_request",
                &json!({"url": "https://example.com"}),
                true,
                "200 OK".into(),
                Some(json!({"status": 200})),
                None,
                42,
            );
            record_step_tool_call(
                "calculator",
                &json!({"function": "add", "values": [1, 2]}),
                false,
                "bad args".into(),
                None,
                Some("bad args"),
                3,
            );
        })
        .await;

        let calls = drain_step_calls(&sink);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].index, 0);
        assert_eq!(calls[0].tool, "http_request");
        assert_eq!(calls[0].output_data, Some(json!({"status": 200})));
        assert_eq!(calls[1].index, 1);
        assert!(!calls[1].success);
        assert_eq!(calls[1].error.as_deref(), Some("bad args"));
        assert_eq!(calls[1].duration_ms, 3);
        // Drain empties the sink.
        assert!(drain_step_calls(&sink).is_empty());
    }

    #[tokio::test]
    async fn record_step_tool_call_scrubs_output_data_secrets() {
        let sink = new_step_call_sink();
        scope_step_call_sink(sink.clone(), async {
            record_step_tool_call(
                "http_request",
                &json!({"url": "https://example.com/token"}),
                true,
                "200 OK".into(),
                Some(json!({"body": {"access_token": "sk-live-abcdef0123456789"}})),
                None,
                7,
            );
        })
        .await;

        let calls = drain_step_calls(&sink);
        assert_eq!(calls.len(), 1);
        let data = calls[0].output_data.as_ref().expect("output_data present");
        let token = data
            .get("body")
            .and_then(|b| b.get("access_token"))
            .and_then(|t| t.as_str())
            .expect("access_token present");
        assert!(
            token.contains("[REDACTED]"),
            "output_data secret was not scrubbed: {token}"
        );
        assert!(!token.contains("abcdef0123456789"));
    }

    #[tokio::test]
    async fn record_step_tool_call_scrubs_authorization_and_cookie_output_data() {
        let sink = new_step_call_sink();
        scope_step_call_sink(sink.clone(), async {
            record_step_tool_call(
                "http_request",
                &json!({"url": "https://example.com/login"}),
                true,
                "200 OK".into(),
                Some(json!({"body": {
                    "authorization": "Bearer sk-live-abcdef0123456789",
                    "cookie": "session=deadbeefcafebabe0123",
                    "set-cookie": "sid=9f8e7d6c5b4a3210feed"
                }})),
                None,
                7,
            );
        })
        .await;

        let calls = drain_step_calls(&sink);
        assert_eq!(calls.len(), 1);
        let body = calls[0]
            .output_data
            .as_ref()
            .and_then(|d| d.get("body"))
            .expect("output_data body present");
        for (key, leaked) in [
            ("authorization", "sk-live-abcdef0123456789"),
            ("cookie", "deadbeefcafebabe0123"),
            ("set-cookie", "9f8e7d6c5b4a3210feed"),
        ] {
            let value = body
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("{key} present"));
            assert!(
                value.contains("[REDACTED]"),
                "output_data {key} was not scrubbed: {value}"
            );
            assert!(!value.contains(leaked), "output_data {key} leaked secret");
        }
    }

    #[tokio::test]
    async fn nested_step_call_scopes_do_not_leak_into_outer_sink() {
        let outer = new_step_call_sink();
        let inner = new_step_call_sink();
        scope_step_call_sink(outer.clone(), async {
            record_step_tool_call("shell", &json!({}), true, "outer".into(), None, None, 1);
            scope_step_call_sink(inner.clone(), async {
                record_step_tool_call("shell", &json!({}), true, "inner".into(), None, None, 1);
            })
            .await;
        })
        .await;

        let outer_calls = drain_step_calls(&outer);
        let inner_calls = drain_step_calls(&inner);
        assert_eq!(outer_calls.len(), 1);
        assert_eq!(outer_calls[0].output, "outer");
        assert_eq!(inner_calls.len(), 1);
        assert_eq!(inner_calls[0].output, "inner");
    }
}
