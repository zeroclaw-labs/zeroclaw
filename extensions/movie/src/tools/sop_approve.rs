use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tracing::warn;

use super::traits::{Tool, ToolResult};
use crate::sop::types::SopRunAction;
use crate::sop::{SopAuditLogger, SopEngine, SopMetricsCollector};

/// Approve a pending SOP step that is waiting for operator approval.
pub struct SopApproveTool {
    engine: Arc<Mutex<SopEngine>>,
    audit: Option<Arc<SopAuditLogger>>,
    collector: Option<Arc<SopMetricsCollector>>,
}

impl SopApproveTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>) -> Self {
        Self {
            engine,
            audit: None,
            collector: None,
        }
    }

    pub fn with_audit(mut self, audit: Arc<SopAuditLogger>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn with_collector(mut self, collector: Arc<SopMetricsCollector>) -> Self {
        self.collector = Some(collector);
        self
    }
}

#[async_trait]
impl Tool for SopApproveTool {
    fn name(&self) -> &str {
        "sop_approve"
    }

    fn description(&self) -> &str {
        "Approve a pending SOP step that is waiting for operator approval. Returns the step instruction to execute. Use sop_status to see which runs are waiting."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The run ID to approve"
                }
            },
            "required": ["run_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let run_id = args
            .get("run_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'run_id' parameter"))?;

        // Lock engine, approve, snapshot run for audit, then drop lock
        let (result, run_snapshot) = {
            let mut engine = self
                .engine
                .lock()
                .map_err(|e| anyhow::anyhow!("Engine lock poisoned: {e}"))?;

            match engine.approve_step(run_id) {
                Ok(action) => {
                    let snapshot = engine.get_run(run_id).cloned();
                    (Ok(action), snapshot)
                }
                Err(e) => (Err(e), None),
            }
        };

        // Audit logging (engine lock dropped, safe to await)
        if let Some(ref audit) = self.audit {
            if let Some(ref run) = run_snapshot {
                if let Err(e) = audit.log_approval(run, run.current_step).await {
                    warn!("SOP audit log after approve failed: {e}");
                }
            }
        }

        // Metrics collector (independent of audit)
        if let Some(ref collector) = self.collector {
            if let Some(ref run) = run_snapshot {
                collector.record_approval(&run.sop_name, &run.run_id);
            }
        }

        match result {
            Ok(action) => {
                let output = match action {
                    SopRunAction::ExecuteStep {
                        run_id, context, ..
                    } => {
                        format!("Approved. Proceeding with run {run_id}.\n\n{context}")
                    }
                    other => format!("Approved. Action: {other:?}"),
                };
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Approval failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SopConfig;
    use crate::memory::Memory;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;

    fn test_sop() -> Sop {
        Sop {
            name: "test-sop".into(),
            description: "Test SOP".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Step one".into(),
                body: "Do it".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
        }
    }

    fn engine_with_run() -> (Arc<Mutex<SopEngine>>, String) {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![test_sop()]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        };
        // Start run — Supervised mode → WaitApproval
        engine.start_run("test-sop", event).unwrap();
        let run_id = engine
            .active_runs()
            .keys()
            .next()
            .expect("expected active run")
            .clone();
        (Arc::new(Mutex::new(engine)), run_id)
    }

    #[tokio::test]
    async fn approve_waiting_run() {
        let (engine, run_id) = engine_with_run();
        let tool = SopApproveTool::new(engine);
        let result = tool.execute(json!({"run_id": run_id})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Approved"));
        assert!(result.output.contains("Step one"));
    }

    #[tokio::test]
    async fn approve_nonexistent_run() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        let result = tool
            .execute(json!({"run_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Approval failed"));
    }

    #[tokio::test]
    async fn approve_missing_run_id() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn name_and_schema() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tool = SopApproveTool::new(engine);
        assert_eq!(tool.name(), "sop_approve");
        assert!(tool.parameters_schema()["required"].is_array());
    }

    #[tokio::test]
    async fn approve_writes_audit() {
        let (engine, run_id) = engine_with_run();
        let tmp = tempfile::tempdir().unwrap();
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(SopAuditLogger::new(memory.clone()));

        let tool = SopApproveTool::new(engine).with_audit(audit.clone());
        let result = tool.execute(json!({"run_id": &run_id})).await.unwrap();
        assert!(result.success);

        // Verify approval audit entry was written (stored under sop_approval_ key)
        let entries = memory
            .list(
                Some(&crate::memory::traits::MemoryCategory::Custom("sop".into())),
                None,
            )
            .await
            .unwrap();
        let approval_keys: Vec<_> = entries
            .iter()
            .filter(|e| e.key.starts_with("sop_approval_"))
            .collect();
        assert!(
            !approval_keys.is_empty(),
            "approval audit should be written on approve"
        );
    }

    #[tokio::test]
    async fn approve_failure_does_not_write_audit() {
        let engine = Arc::new(Mutex::new(SopEngine::new(SopConfig::default())));
        let tmp = tempfile::tempdir().unwrap();
        let mem_cfg = crate::config::MemoryConfig {
            backend: "sqlite".into(),
            ..crate::config::MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let audit = Arc::new(SopAuditLogger::new(memory.clone()));

        let tool = SopApproveTool::new(engine).with_audit(audit.clone());
        let result = tool
            .execute(json!({"run_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);

        // No audit entry for failed approval
        let stored = audit.get_run("nonexistent").await.unwrap();
        assert!(stored.is_none(), "failed approve should not write audit");
    }
}
