//! Gate evaluation state for ampersona trust-phase transitions.
//!
//! This module is only compiled when the `ampersona-gates` feature is active
//! (module declaration in `mod.rs` is behind `#[cfg]`).
//!
//! Gate decisions do NOT change SOP execution behavior — this is purely
//! observation + phase state tracking + audit logging.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ampersona_core::spec::gates::Gate;
use ampersona_core::state::{PendingTransition, PhaseState, TransitionRecord};
use ampersona_core::traits::MetricsProvider;
use ampersona_engine::gates::decision::GateDecisionRecord;
use ampersona_engine::gates::evaluator::DefaultGateEvaluator;
use anyhow::Result;
use chrono::Utc;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::memory::traits::{Memory, MemoryCategory};

const PHASE_STATE_KEY: &str = "sop_phase_state";

fn sop_category() -> MemoryCategory {
    MemoryCategory::Custom("sop".into())
}

// ── Inner state ────────────────────────────────────────────────

struct GateEvalInner {
    phase_state: PhaseState,
    last_tick: Instant,
}

// ── GateEvalState ──────────────────────────────────────────────

/// Manages trust-phase gate evaluation state.
///
/// Single `Mutex<GateEvalInner>` ensures atomic interval-check + evaluate + apply.
/// `DefaultGateEvaluator` is a unit struct — called inline, not stored.
pub struct GateEvalState {
    inner: Mutex<GateEvalInner>,
    memory: Arc<dyn Memory>,
    gates: Vec<Gate>,
    tick_interval: Duration,
}

impl GateEvalState {
    /// Create with fresh (default) phase state.
    pub fn new(
        agent_name: &str,
        gates: Vec<Gate>,
        interval_secs: u64,
        memory: Arc<dyn Memory>,
    ) -> Self {
        Self {
            inner: Mutex::new(GateEvalInner {
                phase_state: PhaseState::new(agent_name.to_string()),
                last_tick: Instant::now(),
            }),
            memory,
            gates,
            tick_interval: Duration::from_secs(interval_secs),
        }
    }

    /// Create with a known phase state (warm-start).
    pub fn with_state(
        state: PhaseState,
        gates: Vec<Gate>,
        interval_secs: u64,
        memory: Arc<dyn Memory>,
    ) -> Self {
        Self {
            inner: Mutex::new(GateEvalInner {
                phase_state: state,
                last_tick: Instant::now(),
            }),
            memory,
            gates,
            tick_interval: Duration::from_secs(interval_secs),
        }
    }

    /// Load gate definitions from a persona JSON file.
    ///
    /// Expects `{"gates": [...]}` at the top level. Missing file → empty Vec.
    /// Parse error → warn log + empty Vec.
    pub fn load_gates_from_file(path: &Path) -> Vec<Gate> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        #[derive(serde::Deserialize)]
        struct PersonaGates {
            #[serde(default)]
            gates: Vec<Gate>,
        }

        match serde_json::from_str::<PersonaGates>(&content) {
            Ok(parsed) => parsed.gates,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to parse gates from persona file");
                Vec::new()
            }
        }
    }

    /// Rebuild from Memory backend (warm-start).
    ///
    /// Loads `PhaseState` from Memory key `sop_phase_state`, loads gates from
    /// file, falls back to fresh state on parse error.
    pub async fn rebuild_from_memory(
        memory: Arc<dyn Memory>,
        agent_name: &str,
        gates_file: Option<&Path>,
        interval_secs: u64,
    ) -> Result<Self> {
        let gates = gates_file
            .map(Self::load_gates_from_file)
            .unwrap_or_default();

        let phase_state = match memory.get(PHASE_STATE_KEY).await? {
            Some(entry) => match serde_json::from_str::<PhaseState>(&entry.content) {
                Ok(state) => {
                    info!(
                        phase = ?state.current_phase,
                        rev = state.state_rev,
                        "gate eval warm-started from memory"
                    );
                    state
                }
                Err(e) => {
                    warn!(error = %e, "failed to parse phase state from memory, using fresh state");
                    PhaseState::new(agent_name.to_string())
                }
            },
            None => PhaseState::new(agent_name.to_string()),
        };

        Ok(Self::with_state(phase_state, gates, interval_secs, memory))
    }

    /// Atomic tick: interval check + evaluate + apply under single lock.
    ///
    /// Returns `Some(record)` if a gate fired, `None` otherwise.
    pub fn tick(&self, metrics: &dyn MetricsProvider) -> Option<GateDecisionRecord> {
        let _span = tracing::info_span!("gate_eval_tick", gates = self.gates.len()).entered();

        // interval_secs=0 means disabled
        if self.tick_interval.is_zero() {
            return None;
        }

        if self.inner.is_poisoned() {
            error!("gate eval mutex poisoned — loss of gate evaluation until restart");
            return None;
        }

        let mut inner = self.inner.lock().ok()?;

        // Check interval
        if inner.last_tick.elapsed() < self.tick_interval {
            return None;
        }
        inner.last_tick = Instant::now();

        // Evaluate
        let record = DefaultGateEvaluator.evaluate(&self.gates, &inner.phase_state, metrics);

        match record {
            Some(ref record) => {
                // Apply decision in-place under the same lock
                apply_decision(&mut inner.phase_state, record);
                info!(
                    gate_id = %record.gate_id,
                    decision = %record.decision,
                    from = ?record.from_phase,
                    to = %record.to_phase,
                    "gate decision"
                );
            }
            None => {
                debug!("no gate fired");
            }
        }

        record
    }

    /// Persist current phase state to Memory.
    pub async fn persist(&self) -> Result<()> {
        let content = {
            let inner = self
                .inner
                .lock()
                .map_err(|e| anyhow::anyhow!("gate eval lock poisoned: {e}"))?;
            serde_json::to_string_pretty(&inner.phase_state)?
        };
        self.memory
            .store(PHASE_STATE_KEY, &content, sop_category(), None)
            .await?;
        Ok(())
    }

    /// Snapshot of current phase state (for diagnostics / sop_status).
    pub fn phase_state_snapshot(&self) -> Option<PhaseState> {
        self.inner.lock().ok().map(|g| g.phase_state.clone())
    }

    /// Number of loaded gate definitions.
    pub fn gate_count(&self) -> usize {
        self.gates.len()
    }
}

// ── Decision application ───────────────────────────────────────

fn apply_decision(state: &mut PhaseState, record: &GateDecisionRecord) {
    match record.decision.as_str() {
        "transition" => {
            state.current_phase = Some(record.to_phase.clone());
            state.state_rev += 1;
            state.last_transition = Some(TransitionRecord {
                gate_id: record.gate_id.clone(),
                from_phase: record.from_phase.clone(),
                to_phase: record.to_phase.clone(),
                at: Utc::now(),
                decision_id: format!(
                    "{}-{}-{}",
                    record.gate_id, record.state_rev, record.metrics_hash
                ),
                metrics_hash: Some(record.metrics_hash.clone()),
                state_rev: state.state_rev,
            });
            state.pending_transition = None;
            state.updated_at = Utc::now();
        }
        "observed" => {
            debug!(
                gate_id = %record.gate_id,
                "observed gate — no state change"
            );
        }
        "pending_human" => {
            state.pending_transition = Some(PendingTransition {
                gate_id: record.gate_id.clone(),
                from_phase: record.from_phase.clone(),
                to_phase: record.to_phase.clone(),
                decision: record.decision.clone(),
                metrics_hash: record.metrics_hash.clone(),
                state_rev: record.state_rev,
                created_at: Utc::now(),
            });
            state.updated_at = Utc::now();
        }
        other => {
            warn!(decision = %other, gate_id = %record.gate_id, "unknown gate decision — skipping");
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ampersona_core::errors::MetricError;
    use ampersona_core::spec::gates::Gate;
    use ampersona_core::traits::{MetricQuery, MetricSample};
    use ampersona_core::types::{CriterionOp, GateApproval, GateDirection, GateEnforcement};
    use serde_json::json;
    use std::collections::HashMap;

    // ── Mock MetricsProvider ──────────────────────────────────

    struct MockMetrics {
        values: HashMap<String, serde_json::Value>,
    }

    impl MockMetrics {
        fn new(values: Vec<(&str, serde_json::Value)>) -> Self {
            Self {
                values: values
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            }
        }
    }

    impl MetricsProvider for MockMetrics {
        fn get_metric(&self, query: &MetricQuery) -> Result<MetricSample, MetricError> {
            self.values
                .get(&query.name)
                .cloned()
                .map(|value| MetricSample {
                    name: query.name.clone(),
                    value,
                    sampled_at: Utc::now(),
                })
                .ok_or_else(|| MetricError::NotFound(query.name.clone()))
        }
    }

    // ── Helpers ───────────────────────────────────────────────

    fn make_promote_gate(
        id: &str,
        metric: &str,
        op: CriterionOp,
        value: serde_json::Value,
        to_phase: &str,
    ) -> Gate {
        Gate {
            id: id.into(),
            direction: GateDirection::Promote,
            enforcement: GateEnforcement::Enforce,
            priority: 0,
            cooldown_seconds: 0,
            from_phase: None,
            to_phase: to_phase.into(),
            criteria: vec![ampersona_core::spec::gates::Criterion {
                metric: metric.into(),
                op,
                value,
                window_seconds: None,
            }],
            metrics_schema: None,
            approval: GateApproval::Auto,
            on_pass: None,
        }
    }

    fn test_memory() -> Arc<dyn Memory> {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap())
    }

    // ── Tests ─────────────────────────────────────────────────

    #[test]
    fn tick_no_gates_returns_none() {
        let mem = test_memory();
        let ge = GateEvalState::new("test-agent", vec![], 1, mem);
        let metrics = MockMetrics::new(vec![]);
        // Force past interval
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        assert!(ge.tick(&metrics).is_none());
    }

    #[test]
    fn tick_with_passing_gate_returns_decision() {
        let mem = test_memory();
        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.9))]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let record = ge.tick(&metrics);
        assert!(record.is_some());
        let record = record.unwrap();
        assert_eq!(record.gate_id, "g1");
        assert_eq!(record.to_phase, "active");
    }

    #[test]
    fn tick_transition_advances_phase() {
        let mem = test_memory();
        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        ge.tick(&metrics);

        let snap = ge.phase_state_snapshot().unwrap();
        assert_eq!(snap.current_phase, Some("active".into()));
        assert!(snap.state_rev > 0);
        assert!(snap.last_transition.is_some());
    }

    #[test]
    fn tick_observed_no_state_change() {
        let mem = test_memory();
        let mut gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        gate.enforcement = GateEnforcement::Observe;
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let record = ge.tick(&metrics);
        assert!(record.is_some());
        assert_eq!(record.unwrap().decision, "observed");

        let snap = ge.phase_state_snapshot().unwrap();
        assert!(snap.current_phase.is_none()); // no change
        assert_eq!(snap.state_rev, 0);
    }

    #[test]
    fn tick_pending_human_sets_pending() {
        let mem = test_memory();
        let mut gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        gate.approval = GateApproval::Human;
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let record = ge.tick(&metrics);
        assert!(record.is_some());
        assert_eq!(record.unwrap().decision, "pending_human");

        let snap = ge.phase_state_snapshot().unwrap();
        assert!(snap.pending_transition.is_some());
        assert_eq!(snap.pending_transition.unwrap().to_phase, "active");
    }

    #[test]
    fn load_gates_missing_file_returns_empty() {
        let gates = GateEvalState::load_gates_from_file(Path::new("/nonexistent/persona.json"));
        assert!(gates.is_empty());
    }

    #[test]
    fn load_gates_valid_persona() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persona.json");
        std::fs::write(
            &path,
            r#"{
                "gates": [{
                    "id": "g1",
                    "direction": "promote",
                    "to_phase": "active",
                    "criteria": [{"metric": "sop.completion_rate", "op": "gte", "value": 0.8}]
                }]
            }"#,
        )
        .unwrap();
        let gates = GateEvalState::load_gates_from_file(&path);
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].id, "g1");
    }

    #[test]
    fn load_gates_no_gates_key_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persona.json");
        std::fs::write(&path, r#"{"name": "test"}"#).unwrap();
        let gates = GateEvalState::load_gates_from_file(&path);
        assert!(gates.is_empty());
    }

    #[test]
    fn load_gates_invalid_json_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persona.json");
        std::fs::write(&path, "not json at all {{{").unwrap();
        let gates = GateEvalState::load_gates_from_file(&path);
        assert!(gates.is_empty());
    }

    #[tokio::test]
    async fn warm_start_roundtrip() {
        let mem = test_memory();
        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );

        // Create, tick to advance state, persist
        let ge = GateEvalState::new("test-agent", vec![gate.clone()], 1, Arc::clone(&mem));
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        ge.tick(&metrics);
        ge.persist().await.unwrap();

        // Write gates file for rebuild
        let dir = tempfile::tempdir().unwrap();
        let gates_path = dir.path().join("persona.json");
        std::fs::write(
            &gates_path,
            serde_json::to_string(&serde_json::json!({"gates": [gate]})).unwrap(),
        )
        .unwrap();

        // Rebuild
        let ge2 = GateEvalState::rebuild_from_memory(
            Arc::clone(&mem),
            "test-agent",
            Some(gates_path.as_path()),
            1,
        )
        .await
        .unwrap();

        let snap = ge2.phase_state_snapshot().unwrap();
        assert_eq!(snap.current_phase, Some("active".into()));
        assert!(snap.state_rev > 0);
        assert_eq!(ge2.gate_count(), 1);
    }

    #[tokio::test]
    async fn warm_start_empty_memory() {
        let mem = test_memory();
        let ge = GateEvalState::rebuild_from_memory(Arc::clone(&mem), "test-agent", None, 60)
            .await
            .unwrap();
        let snap = ge.phase_state_snapshot().unwrap();
        assert!(snap.current_phase.is_none());
        assert_eq!(snap.state_rev, 0);
        assert_eq!(ge.gate_count(), 0);
    }

    #[test]
    fn demote_priority_over_promote() {
        let mem = test_memory();
        let promote = make_promote_gate(
            "promote-g",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        let mut demote = make_promote_gate(
            "demote-g",
            "sop.deviation_rate",
            CriterionOp::Gte,
            json!(0.3),
            "restricted",
        );
        demote.direction = GateDirection::Demote;
        demote.from_phase = Some("active".into());

        let state = PhaseState {
            current_phase: Some("active".into()),
            ..PhaseState::new("test-agent".into())
        };
        let ge = GateEvalState::with_state(state, vec![promote, demote], 1, mem);
        let metrics = MockMetrics::new(vec![
            ("sop.completion_rate", json!(0.95)),
            ("sop.deviation_rate", json!(0.5)),
        ]);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let record = ge.tick(&metrics).unwrap();
        // Demote should fire first (evaluator sorts demote before promote)
        assert_eq!(record.gate_id, "demote-g");
        assert_eq!(record.to_phase, "restricted");
    }

    #[test]
    fn idempotent_tick_after_apply() {
        let mem = test_memory();
        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);

        // First tick — fires
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let first = ge.tick(&metrics);
        assert!(first.is_some());

        // Second tick with same metrics + updated state_rev — should not fire again
        // (evaluator idempotency via metrics_hash + state_rev)
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let second = ge.tick(&metrics);
        assert!(second.is_none());
    }

    #[test]
    fn gate_tick_with_real_collector() {
        use crate::sop::metrics::SopMetricsCollector;
        use crate::sop::types::{
            SopEvent, SopRun, SopRunStatus, SopStepResult, SopStepStatus, SopTriggerSource,
        };

        let mem = test_memory();
        let collector = SopMetricsCollector::new();

        // Record a completed run
        let run = SopRun {
            run_id: "r1".into(),
            sop_name: "test-sop".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-02-19T12:00:00Z".into(),
            },
            status: SopRunStatus::Completed,
            current_step: 1,
            total_steps: 1,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:05:00Z".into()),
            step_results: vec![SopStepResult {
                step_number: 1,
                status: SopStepStatus::Completed,
                output: "done".into(),
                started_at: "2026-02-19T12:00:00Z".into(),
                completed_at: Some("2026-02-19T12:01:00Z".into()),
            }],
            waiting_since: None,
        };
        collector.record_run_complete(&run);

        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );
        let ge = GateEvalState::new("test-agent", vec![gate], 1, mem);
        {
            let mut inner = ge.inner.lock().unwrap();
            inner.last_tick = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        }
        let record = ge.tick(&collector);
        assert!(record.is_some());
        assert_eq!(record.unwrap().to_phase, "active");
    }

    #[test]
    fn tick_respects_interval() {
        let mem = test_memory();
        let gate = make_promote_gate(
            "g1",
            "sop.completion_rate",
            CriterionOp::Gte,
            json!(0.8),
            "active",
        );

        // Long interval
        let ge = GateEvalState::new("test-agent", vec![gate.clone()], 3600, mem.clone());
        let metrics = MockMetrics::new(vec![("sop.completion_rate", json!(0.95))]);
        // last_tick is Instant::now() — not enough elapsed
        assert!(ge.tick(&metrics).is_none());

        // Zero interval = disabled
        let ge_disabled = GateEvalState::new("test-agent", vec![gate], 0, mem);
        assert!(ge_disabled.tick(&metrics).is_none());
    }

    #[test]
    fn ampersona_decision_strings_stable() {
        // Canary test: verifies that DefaultGateEvaluator produces the decision
        // strings we expect. If ampersona changes them, this test fails.
        let state = PhaseState::new("test".into());

        // Enforce promote → "transition"
        let enforce_gate =
            make_promote_gate("g-enforce", "m", CriterionOp::Gte, json!(1), "phase-b");
        let metrics = MockMetrics::new(vec![("m", json!(1))]);
        let record = DefaultGateEvaluator.evaluate(&[enforce_gate], &state, &metrics);
        assert_eq!(
            record.as_ref().map(|r| r.decision.as_str()),
            Some("transition")
        );

        // Observe promote → "observed"
        let mut observe_gate =
            make_promote_gate("g-observe", "m", CriterionOp::Gte, json!(1), "phase-b");
        observe_gate.enforcement = GateEnforcement::Observe;
        let record = DefaultGateEvaluator.evaluate(&[observe_gate], &state, &metrics);
        assert_eq!(
            record.as_ref().map(|r| r.decision.as_str()),
            Some("observed")
        );

        // RequireApproval promote → "pending_human"
        let mut approval_gate =
            make_promote_gate("g-approval", "m", CriterionOp::Gte, json!(1), "phase-b");
        approval_gate.approval = GateApproval::Human;
        let record = DefaultGateEvaluator.evaluate(&[approval_gate], &state, &metrics);
        assert_eq!(
            record.as_ref().map(|r| r.decision.as_str()),
            Some("pending_human")
        );
    }
}
