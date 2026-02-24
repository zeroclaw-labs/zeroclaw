use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use tracing::{info, warn};

use super::condition::evaluate_condition;
use super::load_sops;
use super::types::{
    Sop, SopEvent, SopPriority, SopRun, SopRunAction, SopRunStatus, SopStep, SopStepResult,
    SopStepStatus, SopTrigger, SopTriggerSource,
};
use crate::config::SopConfig;

/// Central SOP orchestrator: loads SOPs, matches triggers, manages run lifecycle.
pub struct SopEngine {
    sops: Vec<Sop>,
    active_runs: HashMap<String, SopRun>,
    /// Completed/failed/cancelled runs (kept for status queries).
    finished_runs: Vec<SopRun>,
    config: SopConfig,
    run_counter: u64,
}

impl SopEngine {
    /// Create a new engine with the given config. Call `reload()` to load SOPs.
    pub fn new(config: SopConfig) -> Self {
        Self {
            sops: Vec::new(),
            active_runs: HashMap::new(),
            finished_runs: Vec::new(),
            config,
            run_counter: 0,
        }
    }

    /// Load/reload SOPs from the configured directory.
    pub fn reload(&mut self, workspace_dir: &Path) {
        self.sops = load_sops(
            workspace_dir,
            self.config.sops_dir.as_deref(),
            self.config.default_execution_mode,
        );
        info!("SOP engine loaded {} SOPs", self.sops.len());
    }

    /// Return all loaded SOP definitions.
    pub fn sops(&self) -> &[Sop] {
        &self.sops
    }

    /// Return all active (in-flight) runs.
    pub fn active_runs(&self) -> &HashMap<String, SopRun> {
        &self.active_runs
    }

    /// Look up a run by ID (active or finished).
    pub fn get_run(&self, run_id: &str) -> Option<&SopRun> {
        self.active_runs
            .get(run_id)
            .or_else(|| self.finished_runs.iter().find(|r| r.run_id == run_id))
    }

    /// Look up an SOP by name.
    pub fn get_sop(&self, name: &str) -> Option<&Sop> {
        self.sops.iter().find(|s| s.name == name)
    }

    // ── Trigger matching ────────────────────────────────────────

    /// Match an incoming event against all loaded SOPs and return the names of
    /// SOPs whose triggers match.
    pub fn match_trigger(&self, event: &SopEvent) -> Vec<&Sop> {
        self.sops
            .iter()
            .filter(|sop| sop.triggers.iter().any(|t| trigger_matches(t, event)))
            .collect()
    }

    // ── Run lifecycle ───────────────────────────────────────────

    /// Check whether a new run can be started for the given SOP
    /// (respects cooldown and concurrency limits).
    pub fn can_start(&self, sop_name: &str) -> bool {
        let sop = match self.get_sop(sop_name) {
            Some(s) => s,
            None => return false,
        };

        // Per-SOP concurrency limit
        let active_for_sop = self
            .active_runs
            .values()
            .filter(|r| r.sop_name == sop_name)
            .count();
        if active_for_sop >= sop.max_concurrent as usize {
            return false;
        }

        // Global concurrency limit
        if self.active_runs.len() >= self.config.max_concurrent_total {
            return false;
        }

        // Cooldown: check most recent finished run for this SOP
        if sop.cooldown_secs > 0 {
            if let Some(last) = self.last_finished_run(sop_name) {
                if let Some(ref completed_at) = last.completed_at {
                    if !cooldown_elapsed(completed_at, sop.cooldown_secs) {
                        return false;
                    }
                }
            }
        }

        true
    }

    /// Start a new SOP run. Returns the first action to take.
    pub fn start_run(&mut self, sop_name: &str, event: SopEvent) -> Result<SopRunAction> {
        let sop = self
            .get_sop(sop_name)
            .ok_or_else(|| anyhow::anyhow!("SOP not found: {sop_name}"))?
            .clone();

        if !self.can_start(sop_name) {
            bail!(
                "Cannot start SOP '{}': cooldown or concurrency limit reached",
                sop_name
            );
        }

        if sop.steps.is_empty() {
            bail!("SOP '{}' has no steps defined", sop_name);
        }

        self.run_counter += 1;
        let dur = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let epoch_ms = dur.as_secs() * 1000 + u64::from(dur.subsec_millis());
        let run_id = format!("run-{epoch_ms}-{:04}", self.run_counter);
        let now = now_iso8601();

        let run = SopRun {
            run_id: run_id.clone(),
            sop_name: sop_name.to_string(),
            trigger_event: event,
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: u32::try_from(sop.steps.len()).unwrap_or(u32::MAX),
            started_at: now,
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
        };

        self.active_runs.insert(run_id.clone(), run);

        info!("SOP run {} started for '{}'", run_id, sop_name);

        // Determine first action based on execution mode
        let step = sop.steps[0].clone();
        let context = format_step_context(&sop, &self.active_runs[&run_id], &step);
        let action = resolve_step_action(&sop, &step, run_id.clone(), context);

        // If the action is WaitApproval, update run status and record timestamp
        if matches!(action, SopRunAction::WaitApproval { .. }) {
            if let Some(run) = self.active_runs.get_mut(&run_id) {
                run.status = SopRunStatus::WaitingApproval;
                run.waiting_since = Some(now_iso8601());
            }
        }

        Ok(action)
    }

    /// Report the result of the current step and advance the run.
    /// Returns the next action to take.
    pub fn advance_step(&mut self, run_id: &str, result: SopStepResult) -> Result<SopRunAction> {
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::anyhow!("Active run not found: {run_id}"))?;

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == run.sop_name)
            .ok_or_else(|| anyhow::anyhow!("SOP '{}' no longer loaded", run.sop_name))?
            .clone();

        // Record step result
        run.step_results.push(result.clone());

        // Check if step failed
        if result.status == SopStepStatus::Failed {
            let reason = format!("Step {} failed: {}", result.step_number, result.output);
            warn!("SOP run {run_id}: {reason}");
            return Ok(self.finish_run(run_id, SopRunStatus::Failed, Some(reason)));
        }

        // Advance to next step
        let next_step_num = run.current_step + 1;
        if next_step_num > run.total_steps {
            // All steps completed
            info!("SOP run {run_id} completed successfully");
            return Ok(self.finish_run(run_id, SopRunStatus::Completed, None));
        }

        // Update run state
        let run = self.active_runs.get_mut(run_id).unwrap();
        run.current_step = next_step_num;

        let step_idx = (next_step_num - 1) as usize;
        let step = sop.steps[step_idx].clone();
        let context = format_step_context(&sop, run, &step);
        let run_id_str = run_id.to_string();
        let action = resolve_step_action(&sop, &step, run_id_str.clone(), context);

        // If the action is WaitApproval, update run status and record timestamp
        if matches!(action, SopRunAction::WaitApproval { .. }) {
            if let Some(run) = self.active_runs.get_mut(&run_id_str) {
                run.status = SopRunStatus::WaitingApproval;
                run.waiting_since = Some(now_iso8601());
            }
        }

        Ok(action)
    }

    /// Cancel an active run.
    pub fn cancel_run(&mut self, run_id: &str) -> Result<()> {
        if !self.active_runs.contains_key(run_id) {
            bail!("Active run not found: {run_id}");
        }
        self.finish_run(run_id, SopRunStatus::Cancelled, None);
        info!("SOP run {run_id} cancelled");
        Ok(())
    }

    /// Approve a step that is waiting for approval, transitioning back to Running.
    pub fn approve_step(&mut self, run_id: &str) -> Result<SopRunAction> {
        let run = self
            .active_runs
            .get_mut(run_id)
            .ok_or_else(|| anyhow::anyhow!("Active run not found: {run_id}"))?;

        if run.status != SopRunStatus::WaitingApproval {
            bail!(
                "Run {run_id} is not waiting for approval (status: {})",
                run.status
            );
        }

        run.status = SopRunStatus::Running;
        run.waiting_since = None;

        let sop = self
            .sops
            .iter()
            .find(|s| s.name == run.sop_name)
            .ok_or_else(|| anyhow::anyhow!("SOP '{}' no longer loaded", run.sop_name))?
            .clone();

        let step_idx = (run.current_step - 1) as usize;
        let step = sop.steps[step_idx].clone();
        let context = format_step_context(&sop, run, &step);

        Ok(SopRunAction::ExecuteStep {
            run_id: run_id.to_string(),
            step,
            context,
        })
    }

    /// List finished runs, optionally filtered by SOP name.
    pub fn finished_runs(&self, sop_name: Option<&str>) -> Vec<&SopRun> {
        self.finished_runs
            .iter()
            .filter(|r| sop_name.map_or(true, |name| r.sop_name == name))
            .collect()
    }

    // ── Approval timeout ──────────────────────────────────────────

    /// Check all WaitingApproval runs for timeout. For Critical/High-priority SOPs,
    /// auto-approve and return the resulting actions. Non-critical SOPs stay
    /// in WaitingApproval indefinitely (or until explicitly approved/cancelled).
    pub fn check_approval_timeouts(&mut self) -> Vec<SopRunAction> {
        let timeout_secs = self.config.approval_timeout_secs;
        if timeout_secs == 0 {
            return Vec::new();
        }

        // Collect timed-out runs with their priority classification
        // cooldown_elapsed(ts, secs) returns true when (now - ts) >= secs
        let timed_out: Vec<(String, bool)> = self
            .active_runs
            .values()
            .filter(|r| r.status == SopRunStatus::WaitingApproval)
            .filter(|r| {
                r.waiting_since
                    .as_deref()
                    .map_or(false, |ts| cooldown_elapsed(ts, timeout_secs))
            })
            .map(|r| {
                let is_critical = self
                    .sops
                    .iter()
                    .find(|s| s.name == r.sop_name)
                    .map_or(false, |s| {
                        matches!(s.priority, SopPriority::Critical | SopPriority::High)
                    });
                (r.run_id.clone(), is_critical)
            })
            .collect();

        let mut actions = Vec::new();
        for (run_id, is_critical) in timed_out {
            if is_critical {
                // Auto-approve: Critical/High priority SOPs fall back to Auto on timeout
                info!(
                    "SOP run {run_id}: approval timeout — auto-approving (critical/high priority)"
                );
                match self.approve_step(&run_id) {
                    Ok(action) => actions.push(action),
                    Err(e) => warn!("SOP run {run_id}: auto-approve failed: {e}"),
                }
            } else {
                info!("SOP run {run_id}: approval timeout — waiting indefinitely (non-critical)");
            }
        }

        actions
    }

    // ── Test helpers ──────────────────────────────────────────────

    /// Replace loaded SOPs (for testing from other modules).
    #[cfg(test)]
    pub(crate) fn set_sops_for_test(&mut self, sops: Vec<Sop>) {
        self.sops = sops;
    }

    // ── Internal helpers ────────────────────────────────────────

    fn last_finished_run(&self, sop_name: &str) -> Option<&SopRun> {
        self.finished_runs
            .iter()
            .rev()
            .find(|r| r.sop_name == sop_name)
    }

    fn finish_run(
        &mut self,
        run_id: &str,
        status: SopRunStatus,
        reason: Option<String>,
    ) -> SopRunAction {
        let mut run = self.active_runs.remove(run_id).unwrap();
        run.status = status;
        run.completed_at = Some(now_iso8601());
        let sop_name = run.sop_name.clone();
        let run_id_owned = run.run_id.clone();
        self.finished_runs.push(run);

        // Evict oldest finished runs when over capacity
        let max = self.config.max_finished_runs;
        if max > 0 && self.finished_runs.len() > max {
            let excess = self.finished_runs.len() - max;
            self.finished_runs.drain(..excess);
        }

        match status {
            SopRunStatus::Failed => SopRunAction::Failed {
                run_id: run_id_owned,
                sop_name,
                reason: reason.unwrap_or_default(),
            },
            _ => SopRunAction::Completed {
                run_id: run_id_owned,
                sop_name,
            },
        }
    }
}

// ── Trigger matching ────────────────────────────────────────────

/// Check whether a single trigger definition matches an incoming event.
fn trigger_matches(trigger: &SopTrigger, event: &SopEvent) -> bool {
    match (trigger, event.source) {
        (SopTrigger::Mqtt { topic, condition }, SopTriggerSource::Mqtt) => {
            let topic_match = event
                .topic
                .as_deref()
                .map_or(false, |t| mqtt_topic_matches(topic, t));
            if !topic_match {
                return false;
            }
            // Evaluate condition against payload (None condition = unconditional)
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (SopTrigger::Webhook { path }, SopTriggerSource::Webhook) => {
            event.topic.as_deref().map_or(false, |t| t == path)
        }

        (
            SopTrigger::Peripheral {
                board,
                signal,
                condition,
            },
            SopTriggerSource::Peripheral,
        ) => {
            let topic_match = event.topic.as_deref().map_or(false, |t| {
                let expected = format!("{board}/{signal}");
                t == expected
            });
            if !topic_match {
                return false;
            }
            // Evaluate condition against payload (None condition = unconditional)
            match condition {
                Some(cond) => evaluate_condition(cond, event.payload.as_deref()),
                None => true,
            }
        }

        (SopTrigger::Cron { expression }, SopTriggerSource::Cron) => {
            event.topic.as_deref().map_or(false, |t| t == expression)
        }

        (SopTrigger::Manual, SopTriggerSource::Manual) => true,

        _ => false,
    }
}

/// Simple MQTT topic matching with `+` (single-level) and `#` (multi-level) wildcards.
fn mqtt_topic_matches(pattern: &str, topic: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let top_parts: Vec<&str> = topic.split('/').collect();

    let mut pi = 0;
    let mut ti = 0;

    while pi < pat_parts.len() && ti < top_parts.len() {
        match pat_parts[pi] {
            "#" => return true, // multi-level wildcard matches everything remaining
            "+" => {
                // single-level wildcard matches one segment
                pi += 1;
                ti += 1;
            }
            seg => {
                if seg != top_parts[ti] {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }

    // Both must be fully consumed (unless pattern ended with #)
    pi == pat_parts.len() && ti == top_parts.len()
}

// ── Execution mode resolution ───────────────────────────────────

/// Determine the action for a step based on SOP execution mode.
fn resolve_step_action(sop: &Sop, step: &SopStep, run_id: String, context: String) -> SopRunAction {
    // Steps with requires_confirmation always need approval
    if step.requires_confirmation {
        return SopRunAction::WaitApproval {
            run_id,
            step: step.clone(),
            context,
        };
    }

    let needs_approval = match sop.execution_mode {
        crate::sop::SopExecutionMode::Auto => false,
        crate::sop::SopExecutionMode::Supervised => {
            // Supervised: approval only before the first step
            step.number == 1
        }
        crate::sop::SopExecutionMode::StepByStep => true,
        crate::sop::SopExecutionMode::PriorityBased => {
            match sop.priority {
                SopPriority::Critical | SopPriority::High => false,
                SopPriority::Normal | SopPriority::Low => {
                    // Supervised behavior for normal/low
                    step.number == 1
                }
            }
        }
    };

    if needs_approval {
        SopRunAction::WaitApproval {
            run_id,
            step: step.clone(),
            context,
        }
    } else {
        SopRunAction::ExecuteStep {
            run_id,
            step: step.clone(),
            context,
        }
    }
}

// ── Step context formatting ─────────────────────────────────────

/// Build the structured context message that gets injected into the agent.
fn format_step_context(sop: &Sop, run: &SopRun, step: &SopStep) -> String {
    let mut ctx = format!(
        "[SOP: {} (run {}) — Step {} of {}]\n\n",
        sop.name, run.run_id, step.number, run.total_steps
    );

    let _ = writeln!(
        ctx,
        "Trigger: {} {}",
        run.trigger_event.source,
        run.trigger_event.topic.as_deref().unwrap_or("(no topic)")
    );

    if let Some(ref payload) = run.trigger_event.payload {
        let _ = writeln!(ctx, "Payload: {payload}");
    }

    // Previous step summary
    if let Some(prev) = run.step_results.last() {
        let _ = writeln!(
            ctx,
            "Previous: Step {} {} — {}",
            prev.step_number, prev.status, prev.output
        );
    }

    let _ = write!(ctx, "\nCurrent step: **{}**\n{}\n", step.title, step.body);

    if !step.suggested_tools.is_empty() {
        let _ = write!(
            ctx,
            "\nSuggested tools: {}\n",
            step.suggested_tools.join(", ")
        );
    }

    ctx.push_str("\nWhen done, report your result.\n");

    ctx
}

// ── Utilities ───────────────────────────────────────────────────

pub(crate) fn now_iso8601() -> String {
    // Use chrono if available, otherwise fallback to SystemTime
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    // Simple UTC timestamp without chrono dependency
    let secs = now.as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to Y-M-D (simplified — good enough for run IDs)
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719_468;
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Check if enough time has elapsed since a timestamp string.
fn cooldown_elapsed(completed_at: &str, cooldown_secs: u64) -> bool {
    // Parse the ISO-8601 timestamp we generate
    let completed = parse_iso8601_secs(completed_at);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match completed {
        Some(ts) => now.saturating_sub(ts) >= cooldown_secs,
        None => true, // Can't parse timestamp; allow start
    }
}

/// Minimal ISO-8601 parser returning seconds since epoch.
fn parse_iso8601_secs(input: &str) -> Option<u64> {
    // Expected format: YYYY-MM-DDTHH:MM:SSZ
    let input = input.trim_end_matches('Z');
    let parts: Vec<&str> = input.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let time_parts: Vec<u64> = parts[1].split(':').filter_map(|p| p.parse().ok()).collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }
    let (year, month, day) = (date_parts[0], date_parts[1], date_parts[2]);
    let (hour, min, sec) = (time_parts[0], time_parts[1], time_parts[2]);

    // Reverse of days_to_ymd: compute days since epoch
    let year_adj = if month <= 2 { year - 1 } else { year };
    let month_adj = if month > 2 { month - 3 } else { month + 9 };
    let era = year_adj / 400;
    let yoe = year_adj - era * 400;
    let doy = (153 * month_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::SopExecutionMode;

    fn manual_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        }
    }

    fn mqtt_event(topic: &str, payload: &str) -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some(topic.into()),
            payload: Some(payload.into()),
            timestamp: now_iso8601(),
        }
    }

    fn test_sop(name: &str, mode: SopExecutionMode, priority: SopPriority) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Test SOP: {name}"),
            version: "1.0.0".into(),
            priority,
            execution_mode: mode,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "Step one".into(),
                    body: "Do step one".into(),
                    suggested_tools: vec!["shell".into()],
                    requires_confirmation: false,
                },
                SopStep {
                    number: 2,
                    title: "Step two".into(),
                    body: "Do step two".into(),
                    suggested_tools: vec![],
                    requires_confirmation: false,
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
        }
    }

    fn engine_with_sops(sops: Vec<Sop>) -> SopEngine {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.sops = sops;
        engine
    }

    /// Extract run_id from any SopRunAction variant.
    fn extract_run_id(action: &SopRunAction) -> &str {
        match action {
            SopRunAction::ExecuteStep { run_id, .. }
            | SopRunAction::WaitApproval { run_id, .. }
            | SopRunAction::Completed { run_id, .. }
            | SopRunAction::Failed { run_id, .. } => run_id,
        }
    }

    /// Get the first active run_id from the engine (for tests with a single run).
    fn first_active_run_id(engine: &SopEngine) -> String {
        engine
            .active_runs()
            .keys()
            .next()
            .expect("expected at least one active run")
            .clone()
    }

    // ── Trigger matching ────────────────────────────────

    #[test]
    fn match_manual_trigger() {
        let engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let matches = engine.match_trigger(&manual_event());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "s1");
    }

    #[test]
    fn no_match_for_wrong_source() {
        let engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let event = mqtt_event("sensors/temp", "{}");
        let matches = engine.match_trigger(&event);
        assert!(matches.is_empty());
    }

    #[test]
    fn match_mqtt_trigger_exact() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/pump/pressure".into(),
                condition: None,
            }],
            ..test_sop(
                "pressure-sop",
                SopExecutionMode::Auto,
                SopPriority::Critical,
            )
        };
        let engine = engine_with_sops(vec![sop]);
        let matches = engine.match_trigger(&mqtt_event("plant/pump/pressure", "87.3"));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn match_mqtt_wildcard_plus() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/+/pressure".into(),
                condition: None,
            }],
            ..test_sop("wildcard-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/pump_3/pressure", "87"))
                .len(),
            1
        );
        assert!(engine
            .match_trigger(&mqtt_event("plant/pump_3/temperature", "50"))
            .is_empty());
    }

    #[test]
    fn match_mqtt_wildcard_hash() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "plant/#".into(),
                condition: None,
            }],
            ..test_sop("hash-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/pump/pressure", "87"))
                .len(),
            1
        );
        assert_eq!(
            engine
                .match_trigger(&mqtt_event("plant/a/b/c/d", "x"))
                .len(),
            1
        );
    }

    #[test]
    fn mqtt_topic_matching_edge_cases() {
        assert!(mqtt_topic_matches("a/b/c", "a/b/c"));
        assert!(!mqtt_topic_matches("a/b/c", "a/b/d"));
        assert!(!mqtt_topic_matches("a/b/c", "a/b"));
        assert!(!mqtt_topic_matches("a/b", "a/b/c"));
        assert!(mqtt_topic_matches("+/+/+", "a/b/c"));
        assert!(!mqtt_topic_matches("+/+", "a/b/c"));
        assert!(mqtt_topic_matches("#", "a/b/c"));
        assert!(mqtt_topic_matches("a/#", "a/b/c"));
        assert!(!mqtt_topic_matches("b/#", "a/b/c"));
    }

    // ── Webhook trigger matching ─────────────────────

    #[test]
    fn webhook_trigger_matches_exact_path() {
        let sop = Sop {
            triggers: vec![SopTrigger::Webhook {
                path: "/webhook".into(),
            }],
            ..test_sop("webhook-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Exact match — should match
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/webhook".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    #[test]
    fn webhook_trigger_rejects_different_path() {
        let sop = Sop {
            triggers: vec![SopTrigger::Webhook {
                path: "/sop/deploy".into(),
            }],
            ..test_sop("deploy-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Path /webhook does NOT match /sop/deploy
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/webhook".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());

        // But /sop/deploy matches /sop/deploy
        let event = SopEvent {
            source: SopTriggerSource::Webhook,
            topic: Some("/sop/deploy".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    // ── Cron trigger matching ─────────────────────────

    #[test]
    fn cron_trigger_matches_only_matching_expression() {
        let sop = Sop {
            triggers: vec![SopTrigger::Cron {
                expression: "0 */5 * * *".into(),
            }],
            ..test_sop("cron-sop", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Matching expression
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: Some("0 */5 * * *".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);

        // Different expression — should NOT match
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: Some("0 */10 * * *".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());

        // No topic — should NOT match
        let event = SopEvent {
            source: SopTriggerSource::Cron,
            topic: None,
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    // ── Condition-based trigger matching ────────────────

    #[test]
    fn mqtt_condition_filters_by_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/pressure".into(),
                condition: Some("$.value > 85".into()),
            }],
            ..test_sop("cond-sop", SopExecutionMode::Auto, SopPriority::Critical)
        };
        let engine = engine_with_sops(vec![sop]);

        // Payload meets condition
        let matches = engine.match_trigger(&mqtt_event("sensors/pressure", r#"{"value": 90}"#));
        assert_eq!(matches.len(), 1);

        // Payload does not meet condition
        let matches = engine.match_trigger(&mqtt_event("sensors/pressure", r#"{"value": 50}"#));
        assert!(matches.is_empty());
    }

    #[test]
    fn mqtt_no_condition_matches_any_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/temp".into(),
                condition: None,
            }],
            ..test_sop("no-cond", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let matches = engine.match_trigger(&mqtt_event("sensors/temp", "anything"));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn mqtt_condition_no_payload_fails_closed() {
        let sop = Sop {
            triggers: vec![SopTrigger::Mqtt {
                topic: "sensors/temp".into(),
                condition: Some("$.value > 0".into()),
            }],
            ..test_sop("no-payload", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        // Event with no payload
        let event = SopEvent {
            source: SopTriggerSource::Mqtt,
            topic: Some("sensors/temp".into()),
            payload: None,
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    #[test]
    fn peripheral_condition_filters_by_payload() {
        let sop = Sop {
            triggers: vec![SopTrigger::Peripheral {
                board: "nucleo".into(),
                signal: "pin_3".into(),
                condition: Some("> 0".into()),
            }],
            ..test_sop("periph-cond", SopExecutionMode::Auto, SopPriority::High)
        };
        let engine = engine_with_sops(vec![sop]);

        // Positive signal
        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("nucleo/pin_3".into()),
            payload: Some("1".into()),
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);

        // Zero signal — does not meet condition
        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("nucleo/pin_3".into()),
            payload: Some("0".into()),
            timestamp: now_iso8601(),
        };
        assert!(engine.match_trigger(&event).is_empty());
    }

    #[test]
    fn peripheral_no_condition_matches_any() {
        let sop = Sop {
            triggers: vec![SopTrigger::Peripheral {
                board: "rpi".into(),
                signal: "gpio_5".into(),
                condition: None,
            }],
            ..test_sop("periph-nocond", SopExecutionMode::Auto, SopPriority::Normal)
        };
        let engine = engine_with_sops(vec![sop]);

        let event = SopEvent {
            source: SopTriggerSource::Peripheral,
            topic: Some("rpi/gpio_5".into()),
            payload: Some("0".into()),
            timestamp: now_iso8601(),
        };
        assert_eq!(engine.match_trigger(&event).len(), 1);
    }

    // ── Run lifecycle ───────────────────────────────────

    #[test]
    fn start_run_returns_first_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action);
        assert!(run_id.starts_with("run-"));
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
        assert_eq!(engine.active_runs().len(), 1);
    }

    #[test]
    fn start_run_unknown_sop_fails() {
        let mut engine = engine_with_sops(vec![]);
        assert!(engine.start_run("nonexistent", manual_event()).is_err());
    }

    #[test]
    fn advance_step_to_completion() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Complete step 1
        let action = engine
            .advance_step(
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

        // Should get step 2
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        // Complete step 2
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "done".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();

        assert!(matches!(action, SopRunAction::Completed { .. }));
        assert!(engine.active_runs().is_empty());
        assert_eq!(engine.finished_runs(None).len(), 1);
    }

    #[test]
    fn step_failure_ends_run() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Failed,
                    output: "valve stuck".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();

        assert!(
            matches!(action, SopRunAction::Failed { ref reason, .. } if reason.contains("valve stuck"))
        );
        assert!(engine.active_runs().is_empty());
    }

    #[test]
    fn cancel_run() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.cancel_run(&run_id).unwrap();
        assert!(engine.active_runs().is_empty());
        let finished = engine.finished_runs(None);
        assert_eq!(finished[0].status, SopRunStatus::Cancelled);
    }

    #[test]
    fn cancel_unknown_run_fails() {
        let mut engine = engine_with_sops(vec![]);
        assert!(engine.cancel_run("nonexistent").is_err());
    }

    // ── Concurrency ─────────────────────────────────────

    #[test]
    fn per_sop_concurrency_limit() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        // max_concurrent = 1 by default
        engine.start_run("s1", manual_event()).unwrap();
        assert!(!engine.can_start("s1"));
        assert!(engine.start_run("s1", manual_event()).is_err());
    }

    #[test]
    fn global_concurrency_limit() {
        let sops = vec![
            test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal),
            test_sop("s2", SopExecutionMode::Auto, SopPriority::Normal),
        ];
        let mut engine = SopEngine::new(SopConfig {
            max_concurrent_total: 1,
            ..SopConfig::default()
        });
        engine.sops = sops;

        engine.start_run("s1", manual_event()).unwrap();
        assert!(!engine.can_start("s2"));
    }

    // ── Cooldown ────────────────────────────────────────

    #[test]
    fn cooldown_blocks_immediate_restart() {
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.cooldown_secs = 3600; // 1 hour
        let mut engine = engine_with_sops(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        // Complete both steps
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();

        // Cooldown not elapsed — should block
        assert!(!engine.can_start("s1"));
    }

    // ── Execution modes ─────────────────────────────────

    #[test]
    fn auto_mode_executes_immediately() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn supervised_mode_waits_on_first_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn step_by_step_waits_on_every_step() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::StepByStep,
            SopPriority::Normal,
        )]);

        // Step 1: WaitApproval
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));

        // Approve step 1
        let action = engine.approve_step(&run_id).unwrap();
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        // Complete step 1, step 2 should also WaitApproval
        let action = engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn priority_based_critical_auto() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::PriorityBased,
            SopPriority::Critical,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn priority_based_normal_supervised() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::PriorityBased,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        // Normal + PriorityBased → Supervised → WaitApproval on step 1
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    #[test]
    fn requires_confirmation_overrides_auto() {
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Critical);
        sop.steps[0].requires_confirmation = true;
        let mut engine = engine_with_sops(vec![sop]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        // Even in Auto mode, requires_confirmation forces WaitApproval
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));
    }

    // ── Approve ─────────────────────────────────────────

    #[test]
    fn approve_transitions_to_execute() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Run should be WaitingApproval
        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);

        // Approve
        let action = engine.approve_step(&run_id).unwrap();
        assert!(matches!(action, SopRunAction::ExecuteStep { .. }));

        let run = engine.active_runs().get(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::Running);
    }

    #[test]
    fn approve_non_waiting_fails() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(engine.approve_step(&run_id).is_err());
    }

    // ── Context formatting ──────────────────────────────

    #[test]
    fn step_context_includes_sop_name_and_step() {
        let sop = test_sop(
            "pump-shutdown",
            SopExecutionMode::Auto,
            SopPriority::Critical,
        );
        let run = SopRun {
            run_id: "run-001".into(),
            sop_name: "pump-shutdown".into(),
            trigger_event: manual_event(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 2,
            started_at: now_iso8601(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
        };
        let ctx = format_step_context(&sop, &run, &sop.steps[0]);
        assert!(ctx.contains("pump-shutdown"));
        assert!(ctx.contains("Step 1 of 2"));
        assert!(ctx.contains("Step one"));
    }

    // ── Get run (active + finished) ─────────────────────

    #[test]
    fn get_run_finds_active_and_finished() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Auto,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Active
        assert!(engine.get_run(&run_id).is_some());
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Running
        );

        // Complete
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 1,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();
        engine
            .advance_step(
                &run_id,
                SopStepResult {
                    step_number: 2,
                    status: SopStepStatus::Completed,
                    output: "ok".into(),
                    started_at: now_iso8601(),
                    completed_at: Some(now_iso8601()),
                },
            )
            .unwrap();

        // Now finished — still findable
        assert!(engine.get_run(&run_id).is_some());
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::Completed
        );

        // Unknown
        assert!(engine.get_run("nonexistent").is_none());
    }

    // ── ISO-8601 helpers ────────────────────────────────

    #[test]
    fn iso8601_roundtrip() {
        let ts = now_iso8601();
        let secs = parse_iso8601_secs(&ts);
        assert!(secs.is_some());
        // Should be close to current time
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now.abs_diff(secs.unwrap()) < 2);
    }

    #[test]
    fn parse_known_timestamp() {
        // 2026-01-01T00:00:00Z
        let secs = parse_iso8601_secs("2026-01-01T00:00:00Z").unwrap();
        // Jan 1 2026 = 20454 days since epoch * 86400
        assert_eq!(secs, 20454 * 86400);
    }

    // ── Approval timeout ─────────────────────────────────

    #[test]
    fn timeout_auto_approves_critical() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1, // 1 second for test
            ..SopConfig::default()
        });
        let mut sop = test_sop("s1", SopExecutionMode::Supervised, SopPriority::Critical);
        // PriorityBased would auto-execute critical, so use Supervised to force WaitApproval
        sop.execution_mode = SopExecutionMode::Supervised;
        engine.set_sops_for_test(vec![sop]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        assert!(matches!(action, SopRunAction::WaitApproval { .. }));

        // Manually backdate waiting_since to simulate timeout
        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], SopRunAction::ExecuteStep { .. }));
    }

    #[test]
    fn timeout_does_not_auto_approve_normal() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 1,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);

        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        // Backdate waiting_since
        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        // Normal priority → no auto-approve
        let actions = engine.check_approval_timeouts();
        assert!(actions.is_empty());
        // Run should still be WaitingApproval
        assert_eq!(
            engine.get_run(&run_id).unwrap().status,
            SopRunStatus::WaitingApproval
        );
    }

    #[test]
    fn timeout_zero_disables_check() {
        let mut engine = SopEngine::new(SopConfig {
            approval_timeout_secs: 0,
            ..SopConfig::default()
        });
        engine.set_sops_for_test(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Critical,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let run = engine.active_runs.get_mut(&run_id).unwrap();
        run.waiting_since = Some("2020-01-01T00:00:00Z".into());

        let actions = engine.check_approval_timeouts();
        assert!(actions.is_empty());
    }

    #[test]
    fn waiting_since_set_on_wait_approval() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();

        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::WaitingApproval);
        assert!(run.waiting_since.is_some());
    }

    // ── Eviction ──────────────────────────────────────

    #[test]
    fn max_finished_runs_evicts_oldest() {
        let mut engine = SopEngine::new(SopConfig {
            max_finished_runs: 2,
            ..SopConfig::default()
        });
        // SOP with 1 step so each run completes in one advance
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps = vec![sop.steps[0].clone()];
        sop.max_concurrent = 10;
        engine.sops = vec![sop];

        // Complete 3 runs
        let mut finished_ids = Vec::new();
        for _ in 0..3 {
            let action = engine.start_run("s1", manual_event()).unwrap();
            let rid = extract_run_id(&action).to_string();
            engine
                .advance_step(
                    &rid,
                    SopStepResult {
                        step_number: 1,
                        status: SopStepStatus::Completed,
                        output: "ok".into(),
                        started_at: now_iso8601(),
                        completed_at: Some(now_iso8601()),
                    },
                )
                .unwrap();
            finished_ids.push(rid);
        }

        // Only 2 should be kept (max_finished_runs=2)
        let finished = engine.finished_runs(None);
        assert_eq!(
            finished.len(),
            2,
            "eviction should cap at max_finished_runs"
        );
        // Oldest (first) run should be evicted, newest two remain
        assert_eq!(finished[0].run_id, finished_ids[1]);
        assert_eq!(finished[1].run_id, finished_ids[2]);
    }

    #[test]
    fn max_finished_runs_zero_means_unlimited() {
        let mut engine = SopEngine::new(SopConfig {
            max_finished_runs: 0,
            ..SopConfig::default()
        });
        let mut sop = test_sop("s1", SopExecutionMode::Auto, SopPriority::Normal);
        sop.steps = vec![sop.steps[0].clone()];
        sop.max_concurrent = 10;
        engine.sops = vec![sop];

        for _ in 0..5 {
            let action = engine.start_run("s1", manual_event()).unwrap();
            let rid = extract_run_id(&action).to_string();
            engine
                .advance_step(
                    &rid,
                    SopStepResult {
                        step_number: 1,
                        status: SopStepStatus::Completed,
                        output: "ok".into(),
                        started_at: now_iso8601(),
                        completed_at: Some(now_iso8601()),
                    },
                )
                .unwrap();
        }

        assert_eq!(engine.finished_runs(None).len(), 5, "zero means unlimited");
    }

    #[test]
    fn waiting_since_cleared_on_approve() {
        let mut engine = engine_with_sops(vec![test_sop(
            "s1",
            SopExecutionMode::Supervised,
            SopPriority::Normal,
        )]);
        let action = engine.start_run("s1", manual_event()).unwrap();
        let run_id = extract_run_id(&action).to_string();
        engine.approve_step(&run_id).unwrap();

        let run = engine.get_run(&run_id).unwrap();
        assert_eq!(run.status, SopRunStatus::Running);
        assert!(run.waiting_since.is_none());
    }
}
