use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use std::time::Instant;

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::json;
use tracing::warn;

use super::types::{SopRun, SopRunStatus, SopStepStatus};
use crate::memory::traits::{Memory, MemoryCategory};

/// Maximum recent runs kept in each ring buffer (global + per-SOP).
/// Covers ~90-day window at ~11 runs/day. If throughput exceeds this,
/// windowed metrics gracefully undercount rather than error.
const MAX_RECENT_RUNS: usize = 1000;

/// Stale pending-approval entries older than this are evicted.
const PENDING_EVICT_SECS: u64 = 3600;

// ── MetricCounters ────────────────────────────────────────────

/// Base counters shared between all-time and windowed aggregation.
/// Extracted to avoid field duplication across `SopCounters` and windowed
/// accumulators (fixes S1: WindowedCounters was a 1:1 copy of 9 fields).
#[derive(Debug, Default, Clone)]
struct MetricCounters {
    runs_completed: u64,
    runs_failed: u64,
    runs_cancelled: u64,
    steps_executed: u64,
    steps_defined: u64,
    steps_failed: u64,
    steps_skipped: u64,
    human_approvals: u64,
    timeout_auto_approvals: u64,
}

// ── RunSnapshot ────────────────────────────────────────────────

/// Lightweight snapshot of a terminal run for windowed metric computation.
///
/// Stores **event-level counts** (not booleans) so windowed and all-time
/// metrics are semantically consistent: both count approval events, not runs.
#[derive(Debug, Clone)]
struct RunSnapshot {
    completed_at: DateTime<Utc>,
    terminal_status: SopRunStatus,
    steps_executed: u64,
    steps_defined: u64,
    steps_failed: u64,
    steps_skipped: u64,
    human_approval_count: u64,
    timeout_approval_count: u64,
}

// ── SopCounters ────────────────────────────────────────────────

/// Accumulated counters for a single SOP (or global aggregate).
#[derive(Debug, Default)]
struct SopCounters {
    counters: MetricCounters,
    recent_runs: VecDeque<RunSnapshot>,
}

// ── CollectorState ─────────────────────────────────────────────

#[derive(Debug, Default)]
struct CollectorState {
    global: SopCounters,
    per_sop: HashMap<String, SopCounters>,
    /// Pending human approvals: run_id → (last_updated, event_count).
    pending_approvals: HashMap<String, (Instant, u64)>,
    /// Pending timeout auto-approvals: run_id → (last_updated, event_count).
    pending_timeout_approvals: HashMap<String, (Instant, u64)>,
}

// ── SopMetricsCollector ────────────────────────────────────────

/// Thread-safe SOP metrics aggregator.
///
/// Bridges raw SOP audit events into queryable metrics for gate evaluation,
/// health endpoints, and diagnostics.
pub struct SopMetricsCollector {
    inner: RwLock<CollectorState>,
}

impl SopMetricsCollector {
    /// Create an empty collector (cold start).
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CollectorState::default()),
        }
    }

    // ── Push methods (sync, write lock) ────────────────────────

    /// Record a terminal run (Completed/Failed/Cancelled).
    ///
    /// Call after `audit.log_run_complete()`.
    pub fn record_run_complete(&self, run: &SopRun) {
        let Ok(mut state) = self.inner.write() else {
            warn!("SOP metrics collector lock poisoned in record_run_complete");
            return;
        };

        // Evict stale pending entries (>1h)
        let now = Instant::now();
        state
            .pending_approvals
            .retain(|_, (ts, _)| now.duration_since(*ts).as_secs() < PENDING_EVICT_SECS);
        state
            .pending_timeout_approvals
            .retain(|_, (ts, _)| now.duration_since(*ts).as_secs() < PENDING_EVICT_SECS);

        let human_count = state
            .pending_approvals
            .remove(&run.run_id)
            .map(|(_, c)| c)
            .unwrap_or(0);
        let timeout_count = state
            .pending_timeout_approvals
            .remove(&run.run_id)
            .map(|(_, c)| c)
            .unwrap_or(0);

        let snapshot = build_snapshot(run, human_count, timeout_count);
        apply_run(&mut state.global, &snapshot);
        let counters = state.per_sop.entry(run.sop_name.clone()).or_default();
        apply_run(counters, &snapshot);
    }

    /// Record a human approval event.
    ///
    /// Call after `audit.log_approval()`.
    pub fn record_approval(&self, sop_name: &str, run_id: &str) {
        let Ok(mut state) = self.inner.write() else {
            warn!("SOP metrics collector lock poisoned in record_approval");
            return;
        };
        state.global.counters.human_approvals += 1;
        state
            .per_sop
            .entry(sop_name.to_string())
            .or_default()
            .counters
            .human_approvals += 1;
        let entry = state
            .pending_approvals
            .entry(run_id.to_string())
            .or_insert((Instant::now(), 0));
        entry.0 = Instant::now();
        entry.1 += 1;
    }

    /// Record a timeout auto-approval event.
    ///
    /// Call after `audit.log_timeout_auto_approve()`.
    pub fn record_timeout_auto_approve(&self, sop_name: &str, run_id: &str) {
        let Ok(mut state) = self.inner.write() else {
            warn!("SOP metrics collector lock poisoned in record_timeout_auto_approve");
            return;
        };
        state.global.counters.timeout_auto_approvals += 1;
        state
            .per_sop
            .entry(sop_name.to_string())
            .or_default()
            .counters
            .timeout_auto_approvals += 1;
        let entry = state
            .pending_timeout_approvals
            .entry(run_id.to_string())
            .or_insert((Instant::now(), 0));
        entry.0 = Instant::now();
        entry.1 += 1;
    }

    // ── Warm-start (async) ─────────────────────────────────────

    /// Rebuild collector state from Memory backend (single-pass O(n)).
    ///
    /// Scans all entries in `MemoryCategory::Custom("sop")`.
    /// Falls back to empty collector on failure.
    ///
    /// For approval entries whose run_id does **not** match a terminal run,
    /// populates `pending_approvals` / `pending_timeout_approvals` so that
    /// if the run completes via live push after restart, approval flags are
    /// correctly propagated to the `RunSnapshot`.
    pub async fn rebuild_from_memory(memory: &dyn Memory) -> anyhow::Result<Self> {
        let category = MemoryCategory::Custom("sop".into());
        let entries = memory.list(Some(&category), None).await?;

        // Pass 1: collect terminal runs and count approvals per run_id
        let mut runs: HashMap<String, SopRun> = HashMap::new();
        let mut approval_counts: HashMap<String, u64> = HashMap::new();
        let mut timeout_counts: HashMap<String, u64> = HashMap::new();
        // Track sop_name per run_id for approval entries (needed for pending + per-SOP counters)
        let mut approval_sop_names: HashMap<String, String> = HashMap::new();

        for entry in &entries {
            if entry.key.starts_with("sop_run_") {
                if let Ok(run) = serde_json::from_str::<SopRun>(&entry.content) {
                    if matches!(
                        run.status,
                        SopRunStatus::Completed | SopRunStatus::Failed | SopRunStatus::Cancelled
                    ) {
                        runs.insert(run.run_id.clone(), run);
                    }
                }
            } else if entry.key.starts_with("sop_approval_") {
                if let Ok(run) = serde_json::from_str::<SopRun>(&entry.content) {
                    *approval_counts.entry(run.run_id.clone()).or_default() += 1;
                    approval_sop_names
                        .entry(run.run_id.clone())
                        .or_insert(run.sop_name);
                }
            } else if entry.key.starts_with("sop_timeout_approve_") {
                if let Ok(run) = serde_json::from_str::<SopRun>(&entry.content) {
                    *timeout_counts.entry(run.run_id.clone()).or_default() += 1;
                    approval_sop_names
                        .entry(run.run_id.clone())
                        .or_insert(run.sop_name);
                }
            }
        }

        // Build state from terminal runs
        let mut state = CollectorState::default();
        for (run_id, run) in &runs {
            let human_count = approval_counts.get(run_id).copied().unwrap_or(0);
            let timeout_count = timeout_counts.get(run_id).copied().unwrap_or(0);
            let snapshot = build_snapshot(run, human_count, timeout_count);
            apply_run(&mut state.global, &snapshot);
            let counters = state.per_sop.entry(run.sop_name.clone()).or_default();
            apply_run(counters, &snapshot);
        }

        // All-time approval counters: count every approval event
        for (run_id, count) in &approval_counts {
            state.global.counters.human_approvals += count;
            if let Some(sop_name) = approval_sop_names.get(run_id) {
                state
                    .per_sop
                    .entry(sop_name.clone())
                    .or_default()
                    .counters
                    .human_approvals += count;
            }
        }
        for (run_id, count) in &timeout_counts {
            state.global.counters.timeout_auto_approvals += count;
            if let Some(sop_name) = approval_sop_names.get(run_id) {
                state
                    .per_sop
                    .entry(sop_name.clone())
                    .or_default()
                    .counters
                    .timeout_auto_approvals += count;
            }
        }

        // Populate pending maps for non-terminal runs so that if the run
        // completes via live push after restart, approval flags are correct.
        for (run_id, count) in &approval_counts {
            if !runs.contains_key(run_id) {
                state
                    .pending_approvals
                    .insert(run_id.clone(), (Instant::now(), *count));
            }
        }
        for (run_id, count) in &timeout_counts {
            if !runs.contains_key(run_id) {
                state
                    .pending_timeout_approvals
                    .insert(run_id.clone(), (Instant::now(), *count));
            }
        }

        Ok(Self {
            inner: RwLock::new(state),
        })
    }

    // ── Internal metric API ────────────────────────────────────

    /// Resolve a metric name to its current value.
    ///
    /// Format: `sop.<metric>` (global) or `sop.<sop_name>.<metric>` (per-SOP).
    /// Per-SOP resolution uses longest-match-first to prevent shorter SOP
    /// names from shadowing longer ones.
    ///
    /// **Known edge case**: If a SOP name exactly matches a metric suffix
    /// (e.g., SOP named `"runs_completed"`), `sop.runs_completed` resolves
    /// to the **global** metric. Per-SOP metrics for such a SOP are only
    /// reachable via the full path `sop.runs_completed.runs_completed`.
    pub fn get_metric_value(&self, name: &str) -> Option<serde_json::Value> {
        let Ok(state) = self.inner.read() else {
            return None;
        };

        let rest = name.strip_prefix("sop.")?;

        // Try global first (no dot-separated SOP name prefix)
        if let Some(val) = resolve_metric(&state.global, rest) {
            return Some(val);
        }

        // Per-SOP: longest-match-first
        let mut best_key: Option<&str> = None;
        let mut best_len = 0;
        for key in state.per_sop.keys() {
            if rest.starts_with(key.as_str()) {
                let next_char_idx = key.len();
                // Must be followed by '.' to be a valid SOP name match
                if rest.len() > next_char_idx
                    && rest.as_bytes()[next_char_idx] == b'.'
                    && key.len() > best_len
                {
                    best_key = Some(key.as_str());
                    best_len = key.len();
                }
            }
        }

        if let Some(sop_key) = best_key {
            let suffix = &rest[sop_key.len() + 1..]; // skip "sop_name."
            if let Some(counters) = state.per_sop.get(sop_key) {
                return resolve_metric(counters, suffix);
            }
        }

        None
    }

    // ── Diagnostics ────────────────────────────────────────────

    /// Resolve a metric with an explicit time window (from `Criterion.window_seconds`).
    ///
    /// The `name` is the base metric name (e.g. `"sop.completion_rate"`).
    /// The `window` is the Duration from the evaluator.
    pub fn get_metric_value_windowed(
        &self,
        name: &str,
        window: &std::time::Duration,
    ) -> Option<serde_json::Value> {
        let state = self.inner.read().ok()?;
        let rest = name.strip_prefix("sop.")?;

        // Extract prefix (global vs per-sop) and base metric
        let (counters, metric_name) = if let Some(dot) = rest.find('.') {
            // Could be per-SOP: "sop.<sop_name>.<metric>"
            // Use longest-match-first for consistency with get_metric_value
            let mut best_key: Option<&str> = None;
            let mut best_len = 0;
            for key in state.per_sop.keys() {
                if rest.starts_with(key.as_str()) {
                    let next_char_idx = key.len();
                    if rest.len() > next_char_idx
                        && rest.as_bytes()[next_char_idx] == b'.'
                        && key.len() > best_len
                    {
                        best_key = Some(key.as_str());
                        best_len = key.len();
                    }
                }
            }
            if let Some(sop_key) = best_key {
                let suffix = &rest[sop_key.len() + 1..];
                match state.per_sop.get(sop_key) {
                    Some(c) => (c, suffix),
                    None => return None,
                }
            } else {
                // No matching SOP name prefix — treat as global metric
                // (handles case where metric name contains dots but isn't per-SOP)
                let _ = dot; // silence unused warning
                (&state.global, rest)
            }
        } else {
            // bare metric after "sop.": global
            (&state.global, rest)
        };

        let cutoff = Utc::now() - chrono::Duration::from_std(*window).ok()?;
        let wc = aggregate_windowed(&counters.recent_runs, cutoff);
        resolve_from_counters(&wc, metric_name)
    }

    /// Return a full snapshot of collector state for health/debug purposes.
    pub fn snapshot(&self) -> serde_json::Value {
        let Ok(state) = self.inner.read() else {
            return json!({"error": "lock poisoned"});
        };

        let per_sop: serde_json::Map<String, serde_json::Value> = state
            .per_sop
            .iter()
            .map(|(name, c)| (name.clone(), counters_to_json(c)))
            .collect();

        json!({
            "global": counters_to_json(&state.global),
            "per_sop": per_sop,
            "pending_approvals": state.pending_approvals.len(),
            "pending_timeout_approvals": state.pending_timeout_approvals.len(),
        })
    }
}

impl Default for SopMetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

// ── Conditional MetricsProvider impl ───────────────────────────

#[cfg(feature = "ampersona-gates")]
impl ampersona_core::traits::MetricsProvider for SopMetricsCollector {
    fn get_metric(
        &self,
        query: &ampersona_core::traits::MetricQuery,
    ) -> Result<ampersona_core::traits::MetricSample, ampersona_core::errors::MetricError> {
        if self.inner.is_poisoned() {
            return Err(ampersona_core::errors::MetricError::ProviderUnavailable);
        }
        let value = if let Some(ref window) = query.window {
            // Window specified by evaluator (from Criterion.window_seconds)
            self.get_metric_value_windowed(&query.name, window)
        } else {
            // No window — use name as-is (may include _7d/_30d suffix or be all-time)
            self.get_metric_value(&query.name)
        };
        value
            .map(|v| ampersona_core::traits::MetricSample {
                name: query.name.clone(),
                value: v,
                sampled_at: Utc::now(),
            })
            .ok_or_else(|| ampersona_core::errors::MetricError::NotFound(query.name.clone()))
    }
}

// ── Helpers ────────────────────────────────────────────────────

fn build_snapshot(run: &SopRun, human_count: u64, timeout_count: u64) -> RunSnapshot {
    let completed_at = run
        .completed_at
        .as_deref()
        .and_then(parse_completed_at)
        .unwrap_or_else(Utc::now);

    let steps_executed = run.step_results.len() as u64;
    let steps_failed = run
        .step_results
        .iter()
        .filter(|s| s.status == SopStepStatus::Failed)
        .count() as u64;
    let steps_skipped = run
        .step_results
        .iter()
        .filter(|s| s.status == SopStepStatus::Skipped)
        .count() as u64;

    RunSnapshot {
        completed_at,
        terminal_status: run.status,
        steps_executed,
        steps_defined: u64::from(run.total_steps),
        steps_failed,
        steps_skipped,
        human_approval_count: human_count,
        timeout_approval_count: timeout_count,
    }
}

fn apply_run(sop: &mut SopCounters, snap: &RunSnapshot) {
    let c = &mut sop.counters;
    match snap.terminal_status {
        SopRunStatus::Completed => c.runs_completed += 1,
        SopRunStatus::Failed => c.runs_failed += 1,
        SopRunStatus::Cancelled => c.runs_cancelled += 1,
        _ => {}
    }
    c.steps_executed += snap.steps_executed;
    c.steps_defined += snap.steps_defined;
    c.steps_failed += snap.steps_failed;
    c.steps_skipped += snap.steps_skipped;

    sop.recent_runs.push_back(snap.clone());
    if sop.recent_runs.len() > MAX_RECENT_RUNS {
        sop.recent_runs.pop_front();
    }
}

fn parse_completed_at(ts: &str) -> Option<DateTime<Utc>> {
    // Primary: RFC 3339
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        return Some(dt.with_timezone(&Utc));
    }
    // Fallback: naive without timezone suffix
    if let Ok(n) = NaiveDateTime::parse_from_str(ts.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S") {
        return Some(n.and_utc());
    }
    // Last resort
    warn!("SOP metrics: could not parse completed_at timestamp: {ts}");
    None
}

/// Aggregate run snapshots newer than `cutoff` into metric counters.
fn aggregate_windowed(
    recent_runs: &VecDeque<RunSnapshot>,
    cutoff: DateTime<Utc>,
) -> MetricCounters {
    let mut wc = MetricCounters::default();
    for snap in recent_runs {
        if snap.completed_at >= cutoff {
            match snap.terminal_status {
                SopRunStatus::Completed => wc.runs_completed += 1,
                SopRunStatus::Failed => wc.runs_failed += 1,
                SopRunStatus::Cancelled => wc.runs_cancelled += 1,
                _ => {}
            }
            wc.steps_executed += snap.steps_executed;
            wc.steps_defined += snap.steps_defined;
            wc.steps_failed += snap.steps_failed;
            wc.steps_skipped += snap.steps_skipped;
            wc.human_approvals += snap.human_approval_count;
            wc.timeout_auto_approvals += snap.timeout_approval_count;
        }
    }
    wc
}

/// Resolve a metric suffix against a `SopCounters` struct.
fn resolve_metric(sop: &SopCounters, suffix: &str) -> Option<serde_json::Value> {
    // Check for windowed variant
    let (base, window_days) = if let Some(base) = suffix.strip_suffix("_7d") {
        (base, Some(7i64))
    } else if let Some(base) = suffix.strip_suffix("_30d") {
        (base, Some(30i64))
    } else if let Some(base) = suffix.strip_suffix("_90d") {
        (base, Some(90i64))
    } else {
        (suffix, None)
    };

    if let Some(days) = window_days {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let wc = aggregate_windowed(&sop.recent_runs, cutoff);
        resolve_from_counters(&wc, base)
    } else {
        resolve_from_counters(&sop.counters, base)
    }
}

/// Core metric resolution against a `MetricCounters` instance.
/// Used by both all-time and windowed metric paths, eliminating the
/// ~100-line duplication between the former `resolve_alltime`/`resolve_windowed`.
fn resolve_from_counters(c: &MetricCounters, metric: &str) -> Option<serde_json::Value> {
    match metric {
        "runs_completed" => Some(json!(c.runs_completed)),
        "runs_failed" => Some(json!(c.runs_failed)),
        "runs_cancelled" => Some(json!(c.runs_cancelled)),
        "deviation_rate" => {
            if c.steps_executed == 0 {
                Some(json!(0.0))
            } else {
                Some(json!(
                    (c.steps_failed + c.steps_skipped) as f64 / c.steps_executed as f64
                ))
            }
        }
        "protocol_adherence_rate" => {
            if c.steps_defined == 0 {
                Some(json!(0.0))
            } else {
                let good = c
                    .steps_executed
                    .saturating_sub(c.steps_failed)
                    .saturating_sub(c.steps_skipped);
                Some(json!(good as f64 / c.steps_defined as f64))
            }
        }
        "human_intervention_count" => Some(json!(c.human_approvals)),
        "human_intervention_rate" => Some(json!(
            c.human_approvals as f64 / c.runs_completed.max(1) as f64
        )),
        "timeout_auto_approvals" => Some(json!(c.timeout_auto_approvals)),
        "timeout_approval_rate" => Some(json!(
            c.timeout_auto_approvals as f64 / c.runs_completed.max(1) as f64
        )),
        "completion_rate" => {
            let total = c.runs_completed + c.runs_failed + c.runs_cancelled;
            Some(json!(c.runs_completed as f64 / total.max(1) as f64))
        }
        _ => None,
    }
}

fn counters_to_json(sop: &SopCounters) -> serde_json::Value {
    let c = &sop.counters;
    json!({
        "runs_completed": c.runs_completed,
        "runs_failed": c.runs_failed,
        "runs_cancelled": c.runs_cancelled,
        "steps_executed": c.steps_executed,
        "steps_defined": c.steps_defined,
        "steps_failed": c.steps_failed,
        "steps_skipped": c.steps_skipped,
        "human_approvals": c.human_approvals,
        "timeout_auto_approvals": c.timeout_auto_approvals,
        "recent_runs_depth": sop.recent_runs.len(),
    })
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{SopEvent, SopStepResult, SopTriggerSource};

    fn make_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        }
    }

    fn make_run(
        run_id: &str,
        sop_name: &str,
        status: SopRunStatus,
        total_steps: u32,
        step_results: Vec<SopStepResult>,
    ) -> SopRun {
        SopRun {
            run_id: run_id.into(),
            sop_name: sop_name.into(),
            trigger_event: make_event(),
            status,
            current_step: total_steps,
            total_steps,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:05:00Z".into()),
            step_results,
            waiting_since: None,
        }
    }

    fn make_step(number: u32, status: SopStepStatus) -> SopStepResult {
        SopStepResult {
            step_number: number,
            status,
            output: format!("Step {number}"),
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:01:00Z".into()),
        }
    }

    #[test]
    fn zero_state_baseline() {
        let c = SopMetricsCollector::new();
        assert_eq!(c.get_metric_value("sop.runs_completed"), Some(json!(0u64)));
        assert_eq!(c.get_metric_value("sop.runs_failed"), Some(json!(0u64)));
        assert_eq!(c.get_metric_value("sop.runs_cancelled"), Some(json!(0u64)));
        assert_eq!(c.get_metric_value("sop.deviation_rate"), Some(json!(0.0)));
        assert_eq!(c.get_metric_value("sop.completion_rate"), Some(json!(0.0)));
    }

    #[test]
    fn counter_arithmetic() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            3,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
                make_step(3, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        assert_eq!(c.get_metric_value("sop.runs_completed"), Some(json!(1u64)));
        assert_eq!(c.get_metric_value("sop.runs_failed"), Some(json!(0u64)));
        assert_eq!(c.get_metric_value("sop.deviation_rate"), Some(json!(0.0)));
        assert_eq!(c.get_metric_value("sop.completion_rate"), Some(json!(1.0)));
    }

    #[test]
    fn windowed_filtering() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        assert_eq!(
            c.get_metric_value("sop.runs_completed_7d"),
            Some(json!(1u64))
        );
        assert_eq!(
            c.get_metric_value("sop.runs_completed_30d"),
            Some(json!(1u64))
        );
        assert_eq!(
            c.get_metric_value("sop.runs_completed_90d"),
            Some(json!(1u64))
        );
    }

    #[test]
    fn deviation_rate_zero_steps() {
        let c = SopMetricsCollector::new();
        let run = make_run("r1", "test-sop", SopRunStatus::Completed, 0, vec![]);
        c.record_run_complete(&run);
        assert_eq!(c.get_metric_value("sop.deviation_rate"), Some(json!(0.0)));
    }

    #[test]
    fn protocol_adherence_rate_partial_run() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Failed,
            3,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Failed),
            ],
        );
        c.record_run_complete(&run);

        // adherence = (2 - 1 - 0) / 3 = 1/3
        let val = c
            .get_metric_value("sop.protocol_adherence_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((val - 1.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn protocol_adherence_rate_full_run() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        let val = c
            .get_metric_value("sop.protocol_adherence_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((val - 1.0).abs() < 1e-10);
    }

    #[test]
    fn protocol_adherence_rate_failed_run() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Failed,
            3,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Failed),
                make_step(3, SopStepStatus::Skipped),
            ],
        );
        c.record_run_complete(&run);

        // adherence = (3 - 1 - 1) / 3 = 1/3
        let val = c
            .get_metric_value("sop.protocol_adherence_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((val - 1.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn derived_rate_metrics() {
        let c = SopMetricsCollector::new();
        c.record_approval("test-sop", "r1");
        c.record_timeout_auto_approve("test-sop", "r2");

        let run1 = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        let run2 = make_run(
            "r2",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run1);
        c.record_run_complete(&run2);

        // human_intervention_rate = 1 / 2 = 0.5
        let hir = c
            .get_metric_value("sop.human_intervention_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((hir - 0.5).abs() < 1e-10);

        // timeout_approval_rate = 1 / 2 = 0.5
        let tar = c
            .get_metric_value("sop.timeout_approval_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((tar - 0.5).abs() < 1e-10);

        assert_eq!(c.get_metric_value("sop.completion_rate"), Some(json!(1.0)));
    }

    #[test]
    fn per_sop_lookup() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "valve-shutdown",
            SopRunStatus::Completed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        assert_eq!(
            c.get_metric_value("sop.valve-shutdown.runs_completed"),
            Some(json!(1u64))
        );
        assert_eq!(
            c.get_metric_value("sop.valve-shutdown.completion_rate"),
            Some(json!(1.0))
        );
    }

    #[test]
    fn longest_match_disambiguation() {
        let c = SopMetricsCollector::new();
        let r1 = make_run(
            "r1",
            "valve",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        let r2 = make_run(
            "r2",
            "valve-shutdown",
            SopRunStatus::Failed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Failed),
            ],
        );
        c.record_run_complete(&r1);
        c.record_run_complete(&r2);

        assert_eq!(
            c.get_metric_value("sop.valve-shutdown.runs_failed"),
            Some(json!(1u64))
        );
        assert_eq!(
            c.get_metric_value("sop.valve.runs_completed"),
            Some(json!(1u64))
        );
    }

    #[test]
    fn not_found_for_unknown_metric() {
        let c = SopMetricsCollector::new();
        assert_eq!(c.get_metric_value("sop.nonexistent"), None);
        assert_eq!(c.get_metric_value("other.runs_completed"), None);
        assert_eq!(c.get_metric_value("sop.no-sop.nonexistent"), None);
    }

    #[test]
    fn approval_flag_propagation() {
        let c = SopMetricsCollector::new();
        c.record_approval("test-sop", "r1");

        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        let snap = c.snapshot();
        let global = &snap["global"];
        assert_eq!(global["human_approvals"], json!(1u64));
        assert_eq!(global["runs_completed"], json!(1u64));

        let hic = c
            .get_metric_value("sop.human_intervention_count_7d")
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(hic, 1);
    }

    #[test]
    fn pending_approval_stale_eviction() {
        let c = SopMetricsCollector::new();
        c.record_approval("test-sop", "orphan-run");

        {
            let state = c.inner.read().unwrap();
            assert_eq!(state.pending_approvals.len(), 1);
        }

        let run = make_run(
            "r2",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        // Orphan entry still present (not stale yet — less than 1h old)
        {
            let state = c.inner.read().unwrap();
            assert_eq!(state.pending_approvals.len(), 1);
        }
    }

    #[test]
    fn snapshot_diagnostic_output() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        let snap = c.snapshot();
        assert!(snap["global"].is_object());
        assert!(snap["per_sop"].is_object());
        assert_eq!(snap["global"]["runs_completed"], json!(1u64));
        assert_eq!(snap["global"]["recent_runs_depth"], json!(1));
        assert!(snap["per_sop"]["test-sop"].is_object());
    }

    #[test]
    fn runs_cancelled_tracking() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Cancelled,
            2,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        assert_eq!(c.get_metric_value("sop.runs_cancelled"), Some(json!(1u64)));
        let cr = c
            .get_metric_value("sop.completion_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((cr - 0.0).abs() < 1e-10);
    }

    // ── BUG 1 regression: multiple approvals per run ──────────

    #[test]
    fn multiple_approvals_per_run_consistent() {
        let c = SopMetricsCollector::new();
        // 3 approval events on the same run
        c.record_approval("test-sop", "r1");
        c.record_approval("test-sop", "r1");
        c.record_approval("test-sop", "r1");

        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            3,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
                make_step(3, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        // All-time: 3 events
        assert_eq!(
            c.get_metric_value("sop.human_intervention_count"),
            Some(json!(3u64))
        );
        // Windowed: also 3 events (not 1 run — consistent with all-time)
        assert_eq!(
            c.get_metric_value("sop.human_intervention_count_7d"),
            Some(json!(3u64))
        );
        // Rate: 3 / 1 = 3.0 (3 approval events per 1 completed run)
        let rate = c
            .get_metric_value("sop.human_intervention_rate")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((rate - 3.0).abs() < 1e-10);
    }

    // ── Ring buffer overflow ──────────────────────────────────

    #[test]
    fn ring_buffer_overflow_cap() {
        let c = SopMetricsCollector::new();
        for i in 0..1001u64 {
            let run = make_run(
                &format!("r{i}"),
                "test-sop",
                SopRunStatus::Completed,
                1,
                vec![make_step(1, SopStepStatus::Completed)],
            );
            c.record_run_complete(&run);
        }

        // All-time counts all 1001
        assert_eq!(
            c.get_metric_value("sop.runs_completed"),
            Some(json!(1001u64))
        );
        // Ring buffer capped at MAX_RECENT_RUNS
        let snap = c.snapshot();
        assert_eq!(snap["global"]["recent_runs_depth"], json!(MAX_RECENT_RUNS));
        // Windowed returns up to cap (all recent, all within 7d)
        let w = c
            .get_metric_value("sop.runs_completed_7d")
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(w, MAX_RECENT_RUNS as u64);
    }

    // ── Windowed old-run exclusion ───────────────────────────

    #[test]
    fn windowed_excludes_old_runs() {
        let c = SopMetricsCollector::new();
        // Inject an old run snapshot directly (10 days ago)
        {
            let mut state = c.inner.write().unwrap();
            let old_snap = RunSnapshot {
                completed_at: Utc::now() - chrono::Duration::days(10),
                terminal_status: SopRunStatus::Completed,
                steps_executed: 1,
                steps_defined: 1,
                steps_failed: 0,
                steps_skipped: 0,
                human_approval_count: 0,
                timeout_approval_count: 0,
            };
            state.global.counters.runs_completed += 1;
            state.global.counters.steps_executed += 1;
            state.global.counters.steps_defined += 1;
            state.global.recent_runs.push_back(old_snap);
        }

        // All-time: 1
        assert_eq!(c.get_metric_value("sop.runs_completed"), Some(json!(1u64)));
        // 7d window: 0 (run is 10 days old)
        assert_eq!(
            c.get_metric_value("sop.runs_completed_7d"),
            Some(json!(0u64))
        );
        // 30d window: 1 (run is 10 days old, within 30d)
        assert_eq!(
            c.get_metric_value("sop.runs_completed_30d"),
            Some(json!(1u64))
        );
    }

    // ── SOP name matching metric suffix (S3 edge case) ───────

    #[test]
    fn sop_name_matching_metric_suffix_resolves_global() {
        let c = SopMetricsCollector::new();
        // SOP named "runs_completed" — an edge case
        let run = make_run(
            "r1",
            "runs_completed",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        // "sop.runs_completed" resolves to global (1), not per-SOP
        assert_eq!(c.get_metric_value("sop.runs_completed"), Some(json!(1u64)));
        // Per-SOP accessible via full path
        assert_eq!(
            c.get_metric_value("sop.runs_completed.runs_completed"),
            Some(json!(1u64))
        );
    }

    // ── MetricsProvider impl (ampersona-gates feature) ───────

    #[cfg(feature = "ampersona-gates")]
    #[test]
    fn metrics_provider_get_metric() {
        use ampersona_core::traits::{MetricQuery, MetricsProvider};

        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        let query = MetricQuery {
            name: "sop.runs_completed".into(),
            window: None,
        };
        let sample = c.get_metric(&query).unwrap();
        assert_eq!(sample.value, json!(1u64));
        assert_eq!(sample.name, "sop.runs_completed");

        // NotFound for unknown metric
        let bad_query = MetricQuery {
            name: "sop.nonexistent".into(),
            window: None,
        };
        let err = c.get_metric(&bad_query).unwrap_err();
        assert!(matches!(
            err,
            ampersona_core::errors::MetricError::NotFound(_)
        ));
    }

    // ── Warm-start tests ─────────────────────────────────────

    #[tokio::test]
    async fn warm_start_roundtrip() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let audit = crate::sop::SopAuditLogger::new(memory.clone());
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
            ],
        );
        audit.log_run_start(&run).await.unwrap();
        audit.log_run_complete(&run).await.unwrap();
        audit.log_approval(&run, 1).await.unwrap();

        let collector = SopMetricsCollector::rebuild_from_memory(memory.as_ref())
            .await
            .unwrap();

        assert_eq!(
            collector.get_metric_value("sop.runs_completed"),
            Some(json!(1u64))
        );
        assert_eq!(
            collector.get_metric_value("sop.human_intervention_count"),
            Some(json!(1u64))
        );
        assert_eq!(
            collector.get_metric_value("sop.test-sop.runs_completed"),
            Some(json!(1u64))
        );
    }

    #[tokio::test]
    async fn warm_start_skips_running_runs() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let audit = crate::sop::SopAuditLogger::new(memory.clone());
        let run = SopRun {
            run_id: "r1".into(),
            sop_name: "test-sop".into(),
            trigger_event: make_event(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 3,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            step_results: vec![],
            waiting_since: None,
        };
        audit.log_run_start(&run).await.unwrap();

        let collector = SopMetricsCollector::rebuild_from_memory(memory.as_ref())
            .await
            .unwrap();

        assert_eq!(
            collector.get_metric_value("sop.runs_completed"),
            Some(json!(0u64))
        );
    }

    #[tokio::test]
    async fn warm_start_empty_memory() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let collector = SopMetricsCollector::rebuild_from_memory(memory.as_ref())
            .await
            .unwrap();

        assert_eq!(
            collector.get_metric_value("sop.runs_completed"),
            Some(json!(0u64))
        );
    }

    #[tokio::test]
    async fn warm_start_approval_matching() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let audit = crate::sop::SopAuditLogger::new(memory.clone());
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        audit.log_run_start(&run).await.unwrap();
        audit.log_timeout_auto_approve(&run, 1).await.unwrap();
        audit.log_run_complete(&run).await.unwrap();

        let collector = SopMetricsCollector::rebuild_from_memory(memory.as_ref())
            .await
            .unwrap();

        assert_eq!(
            collector.get_metric_value("sop.timeout_auto_approvals"),
            Some(json!(1u64))
        );
        let ta_7d = collector
            .get_metric_value("sop.timeout_auto_approvals_7d")
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(ta_7d, 1);
    }

    // ── BUG 2 regression: warm-start pending for non-terminal runs ──

    #[tokio::test]
    async fn warm_start_preserves_pending_for_nonterminal_runs() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: std::sync::Arc<dyn Memory> =
            std::sync::Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let audit = crate::sop::SopAuditLogger::new(memory.clone());

        // Store a Running (non-terminal) run with an approval
        let running_run = SopRun {
            run_id: "r1".into(),
            sop_name: "test-sop".into(),
            trigger_event: make_event(),
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 3,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            step_results: vec![],
            waiting_since: None,
        };
        audit.log_run_start(&running_run).await.unwrap();
        audit.log_approval(&running_run, 1).await.unwrap();

        // Warm-start: run is non-terminal, approval should go into pending
        let collector = SopMetricsCollector::rebuild_from_memory(memory.as_ref())
            .await
            .unwrap();

        // All-time approval counted
        assert_eq!(
            collector.get_metric_value("sop.human_intervention_count"),
            Some(json!(1u64))
        );
        // No completed runs yet
        assert_eq!(
            collector.get_metric_value("sop.runs_completed"),
            Some(json!(0u64))
        );

        // Now complete the run via live push (simulating post-restart completion)
        let completed_run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            3,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
                make_step(3, SopStepStatus::Completed),
            ],
        );
        collector.record_run_complete(&completed_run);

        // Windowed should reflect the approval from before the restart
        let hic_7d = collector
            .get_metric_value("sop.human_intervention_count_7d")
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(hic_7d, 1);
    }

    // ── Windowed MetricsProvider tests (ampersona-gates feature) ──

    #[test]
    fn get_metric_windowed_7d_matches_suffix() {
        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            2,
            vec![
                make_step(1, SopStepStatus::Completed),
                make_step(2, SopStepStatus::Completed),
            ],
        );
        c.record_run_complete(&run);

        let suffix_val = c.get_metric_value("sop.completion_rate_7d");
        let windowed_val = c.get_metric_value_windowed(
            "sop.completion_rate",
            &std::time::Duration::from_secs(7 * 86400),
        );
        assert_eq!(suffix_val, windowed_val);
    }

    #[test]
    fn get_metric_windowed_custom_duration() {
        let c = SopMetricsCollector::new();
        // Record one recent run
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        // Inject an old run (20 days ago)
        {
            let mut state = c.inner.write().unwrap();
            let old_snap = RunSnapshot {
                completed_at: Utc::now() - chrono::Duration::days(20),
                terminal_status: SopRunStatus::Completed,
                steps_executed: 1,
                steps_defined: 1,
                steps_failed: 0,
                steps_skipped: 0,
                human_approval_count: 0,
                timeout_approval_count: 0,
            };
            state.global.recent_runs.push_back(old_snap);
        }

        // 14-day window: only the recent run
        let val = c
            .get_metric_value_windowed(
                "sop.runs_completed",
                &std::time::Duration::from_secs(14 * 86400),
            )
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(val, 1);

        // 30-day window: both runs
        let val = c
            .get_metric_value_windowed(
                "sop.runs_completed",
                &std::time::Duration::from_secs(30 * 86400),
            )
            .unwrap()
            .as_u64()
            .unwrap();
        assert_eq!(val, 2);
    }

    #[cfg(feature = "ampersona-gates")]
    #[test]
    fn get_metric_provider_window_propagation() {
        use ampersona_core::traits::{MetricQuery, MetricsProvider};

        let c = SopMetricsCollector::new();
        let run = make_run(
            "r1",
            "test-sop",
            SopRunStatus::Completed,
            1,
            vec![make_step(1, SopStepStatus::Completed)],
        );
        c.record_run_complete(&run);

        // Query with window via MetricsProvider trait
        let query = MetricQuery {
            name: "sop.runs_completed".into(),
            window: Some(std::time::Duration::from_secs(7 * 86400)),
        };
        let sample = c.get_metric(&query).unwrap();
        assert_eq!(sample.value, json!(1u64));

        // Same result as suffix-based query
        let suffix_val = c.get_metric_value("sop.runs_completed_7d");
        assert_eq!(Some(sample.value), suffix_val);
    }
}
