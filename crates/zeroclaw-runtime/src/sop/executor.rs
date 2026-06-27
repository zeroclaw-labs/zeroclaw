//! Live SOP action executor.
//!
//! The SOP engine is intentionally synchronous: it decides the next
//! [`SopRunAction`] and owns run state, while callers perform side effects.
//! This module is the bridge for live agent execution. It drives
//! `ExecuteStep` actions through an async step runner, feeds the result back to
//! the engine, and repeats until the run blocks or terminates.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use super::audit::SopAuditLogger;
use super::engine::SopEngine;
use super::metrics::SopMetricsCollector;
use super::types::{SopRun, SopRunAction, SopStepResult};

/// Live SOP action captured by SOP tools while they run inside an agent turn.
#[derive(Clone)]
pub(crate) struct QueuedSopAction {
    pub engine: Arc<Mutex<SopEngine>>,
    pub audit: Option<Arc<SopAuditLogger>>,
    pub action: SopRunAction,
}

pub(crate) type LiveActionQueue = Arc<Mutex<VecDeque<QueuedSopAction>>>;

tokio::task_local! {
    static LIVE_SOP_ACTION_QUEUE: Option<LiveActionQueue>;
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
    if let Some(ref run) = finished_run {
        SopMetricsCollector::shared().record_run_complete(run);
    }
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
