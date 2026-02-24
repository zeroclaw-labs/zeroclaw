use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use super::types::{SopRun, SopStepResult};
use crate::memory::traits::{Memory, MemoryCategory};

const SOP_CATEGORY: &str = "sop";

/// Persists SOP execution runs and step results to the Memory backend.
///
/// Storage keys:
/// - `sop_run_{run_id}` — full `SopRun` JSON (created on start, updated on complete)
/// - `sop_step_{run_id}_{step_number}` — `SopStepResult` JSON (one per step)
pub struct SopAuditLogger {
    memory: Arc<dyn Memory>,
}

impl SopAuditLogger {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }

    /// Log the start of a new SOP run.
    pub async fn log_run_start(&self, run: &SopRun) -> Result<()> {
        let key = run_key(&run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            "SOP audit: run {} started for '{}'",
            run.run_id, run.sop_name
        );
        Ok(())
    }

    /// Log a step result.
    pub async fn log_step_result(&self, run_id: &str, result: &SopStepResult) -> Result<()> {
        let key = step_key(run_id, result.step_number);
        let content = serde_json::to_string_pretty(result)?;
        self.memory.store(&key, &content, category(), None).await?;
        Ok(())
    }

    /// Log run completion (updates the run record with final state).
    pub async fn log_run_complete(&self, run: &SopRun) -> Result<()> {
        let key = run_key(&run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            "SOP audit: run {} finished with status {}",
            run.run_id, run.status
        );
        Ok(())
    }

    /// Log an operator approval event for a specific step.
    pub async fn log_approval(&self, run: &SopRun, step_number: u32) -> Result<()> {
        let key = format!("sop_approval_{}_{step_number}", run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            "SOP audit: run {} step {step_number} approved by operator",
            run.run_id
        );
        Ok(())
    }

    /// Log a timeout-based auto-approval event for a specific step.
    pub async fn log_timeout_auto_approve(&self, run: &SopRun, step_number: u32) -> Result<()> {
        let key = format!("sop_timeout_approve_{}_{step_number}", run.run_id);
        let content = serde_json::to_string_pretty(run)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            "SOP audit: run {} step {step_number} auto-approved after timeout",
            run.run_id
        );
        Ok(())
    }

    /// Log a gate evaluation decision record.
    #[cfg(feature = "ampersona-gates")]
    pub async fn log_gate_decision(
        &self,
        record: &ampersona_engine::gates::decision::GateDecisionRecord,
    ) -> Result<()> {
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let key = format!("sop_gate_decision_{}_{timestamp_ms}", record.gate_id);
        let content = serde_json::to_string_pretty(record)?;
        self.memory.store(&key, &content, category(), None).await?;
        info!(
            gate_id = %record.gate_id,
            decision = %record.decision,
            "SOP audit: gate decision logged"
        );
        Ok(())
    }

    /// Persist (upsert) the current gate phase state.
    #[cfg(feature = "ampersona-gates")]
    pub async fn log_phase_state(&self, state: &ampersona_core::state::PhaseState) -> Result<()> {
        let key = "sop_phase_state";
        let content = serde_json::to_string_pretty(state)?;
        self.memory.store(key, &content, category(), None).await?;
        Ok(())
    }

    /// Retrieve a stored run by ID (if it exists in memory).
    pub async fn get_run(&self, run_id: &str) -> Result<Option<SopRun>> {
        let key = run_key(run_id);
        match self.memory.get(&key).await? {
            Some(entry) => {
                let run: SopRun = serde_json::from_str(&entry.content).map_err(|e| {
                    warn!("SOP audit: failed to parse run {run_id}: {e}");
                    e
                })?;
                Ok(Some(run))
            }
            None => Ok(None),
        }
    }

    /// List all stored SOP run keys.
    pub async fn list_runs(&self) -> Result<Vec<String>> {
        let entries = self.memory.list(Some(&category()), None).await?;
        let run_keys: Vec<String> = entries
            .into_iter()
            .filter(|e| e.key.starts_with("sop_run_"))
            .map(|e| e.key)
            .collect();
        Ok(run_keys)
    }
}

fn run_key(run_id: &str) -> String {
    format!("sop_run_{run_id}")
}

fn step_key(run_id: &str, step_number: u32) -> String {
    format!("sop_step_{run_id}_{step_number}")
}

fn category() -> MemoryCategory {
    MemoryCategory::Custom(SOP_CATEGORY.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::types::{SopEvent, SopRunStatus, SopStepStatus, SopTriggerSource};

    fn test_run() -> SopRun {
        SopRun {
            run_id: "run-test-001".into(),
            sop_name: "test-sop".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-02-19T12:00:00Z".into(),
            },
            status: SopRunStatus::Running,
            current_step: 1,
            total_steps: 3,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: None,
            step_results: Vec::new(),
            waiting_since: None,
        }
    }

    fn test_step_result(n: u32) -> SopStepResult {
        SopStepResult {
            step_number: n,
            status: SopStepStatus::Completed,
            output: format!("Step {n} completed"),
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:00:05Z".into()),
        }
    }

    #[tokio::test]
    async fn audit_roundtrip() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory);

        // Log run start
        let run = test_run();
        logger.log_run_start(&run).await.unwrap();

        // Log step result
        let step = test_step_result(1);
        logger.log_step_result(&run.run_id, &step).await.unwrap();

        // Log run complete
        let mut completed_run = run.clone();
        completed_run.status = SopRunStatus::Completed;
        completed_run.completed_at = Some("2026-02-19T12:05:00Z".into());
        completed_run.step_results = vec![step];
        logger.log_run_complete(&completed_run).await.unwrap();

        // Retrieve
        let retrieved = logger.get_run("run-test-001").await.unwrap().unwrap();
        assert_eq!(retrieved.run_id, "run-test-001");
        assert_eq!(retrieved.status, SopRunStatus::Completed);
        assert_eq!(retrieved.step_results.len(), 1);

        // List runs
        let keys = logger.list_runs().await.unwrap();
        assert!(keys.contains(&"sop_run_run-test-001".to_string()));
    }

    #[tokio::test]
    async fn log_approval_persists_entry() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory.clone());
        let run = test_run();
        logger.log_approval(&run, 1).await.unwrap();

        let entries = memory.list(Some(&category()), None).await.unwrap();
        let approval_keys: Vec<_> = entries
            .iter()
            .filter(|e| e.key.starts_with("sop_approval_"))
            .collect();
        assert_eq!(approval_keys.len(), 1);
        assert!(approval_keys[0].key.contains("run-test-001"));
    }

    #[tokio::test]
    async fn log_timeout_auto_approve_persists_entry() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory.clone());
        let run = test_run();
        logger.log_timeout_auto_approve(&run, 1).await.unwrap();

        let entries = memory.list(Some(&category()), None).await.unwrap();
        let timeout_keys: Vec<_> = entries
            .iter()
            .filter(|e| e.key.starts_with("sop_timeout_approve_"))
            .collect();
        assert_eq!(timeout_keys.len(), 1);
        assert!(timeout_keys[0].key.contains("run-test-001"));
    }

    #[tokio::test]
    async fn get_nonexistent_run_returns_none() {
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let logger = SopAuditLogger::new(memory);
        let result = logger.get_run("nonexistent").await.unwrap();
        assert!(result.is_none());
    }
}
