use std::fmt::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};
use crate::sop::{SopEngine, SopMetricsCollector};

/// Query SOP execution status — active runs, finished runs, or a specific run by ID.
pub struct SopStatusTool {
    engine: Arc<Mutex<SopEngine>>,
    collector: Option<Arc<SopMetricsCollector>>,
    #[cfg(feature = "ampersona-gates")]
    gate_eval: Option<Arc<crate::sop::GateEvalState>>,
}

impl SopStatusTool {
    pub fn new(engine: Arc<Mutex<SopEngine>>) -> Self {
        Self {
            engine,
            collector: None,
            #[cfg(feature = "ampersona-gates")]
            gate_eval: None,
        }
    }

    pub fn with_collector(mut self, collector: Arc<SopMetricsCollector>) -> Self {
        self.collector = Some(collector);
        self
    }

    #[cfg(feature = "ampersona-gates")]
    pub fn with_gate_eval(mut self, gate_eval: Arc<crate::sop::GateEvalState>) -> Self {
        self.gate_eval = Some(gate_eval);
        self
    }

    fn append_gate_status(&self, output: &mut String, include_gate_status: bool) {
        #[cfg(feature = "ampersona-gates")]
        if include_gate_status {
            if let Some(ref ge) = self.gate_eval {
                if let Some(snap) = ge.phase_state_snapshot() {
                    let _ = writeln!(output, "\nGate Status:");
                    let _ = writeln!(
                        output,
                        "  current_phase: {}",
                        snap.current_phase.as_deref().unwrap_or("(none)")
                    );
                    let _ = writeln!(output, "  state_rev: {}", snap.state_rev);
                    let _ = writeln!(output, "  gates_loaded: {}", ge.gate_count());
                    if let Some(ref tr) = snap.last_transition {
                        let _ = writeln!(
                            output,
                            "  last_transition: {} ({} → {})",
                            tr.at.to_rfc3339(),
                            tr.from_phase.as_deref().unwrap_or("(none)"),
                            tr.to_phase,
                        );
                    } else {
                        let _ = writeln!(output, "  last_transition: none");
                    }
                    if let Some(ref pt) = snap.pending_transition {
                        let _ = writeln!(
                            output,
                            "  pending_transition: {} → {} ({})",
                            pt.from_phase.as_deref().unwrap_or("(none)"),
                            pt.to_phase,
                            pt.decision,
                        );
                    } else {
                        let _ = writeln!(output, "  pending_transition: none");
                    }
                }
            } else {
                let _ = writeln!(
                    output,
                    "\nGate Status: not available (gate eval not configured)"
                );
            }
        }

        #[cfg(not(feature = "ampersona-gates"))]
        if include_gate_status {
            let _ = writeln!(
                output,
                "\nGate Status: not available (ampersona-gates feature not enabled)"
            );
        }
    }
}

#[async_trait]
impl Tool for SopStatusTool {
    fn name(&self) -> &str {
        "sop_status"
    }

    fn description(&self) -> &str {
        "Query SOP execution status. Provide run_id for a specific run, or sop_name to list runs for that SOP. With no arguments, shows all active runs."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Specific run ID to query"
                },
                "sop_name": {
                    "type": "string",
                    "description": "SOP name to list runs for"
                },
                "include_metrics": {
                    "type": "boolean",
                    "description": "Include aggregated SOP metrics (completion rate, deviation rate, intervention counts, windowed variants)"
                },
                "include_gate_status": {
                    "type": "boolean",
                    "description": "Include trust phase and gate evaluation status"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let run_id = args.get("run_id").and_then(|v| v.as_str());
        let sop_name = args.get("sop_name").and_then(|v| v.as_str());
        let include_metrics = args
            .get("include_metrics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_gate_status = args
            .get("include_gate_status")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Engine lock poisoned: {e}"))?;

        // Query specific run
        if let Some(run_id) = run_id {
            return match engine.get_run(run_id) {
                Some(run) => {
                    let mut output = format!(
                        "Run: {}\nSOP: {}\nStatus: {}\nStep: {} of {}\nStarted: {}\n",
                        run.run_id,
                        run.sop_name,
                        run.status,
                        run.current_step,
                        run.total_steps,
                        run.started_at,
                    );
                    if let Some(ref completed) = run.completed_at {
                        let _ = writeln!(output, "Completed: {completed}");
                    }
                    if !run.step_results.is_empty() {
                        let _ = writeln!(output, "\nStep results:");
                        for step in &run.step_results {
                            let _ = writeln!(
                                output,
                                "  Step {}: {} — {}",
                                step.step_number, step.status, step.output
                            );
                        }
                    }
                    self.append_gate_status(&mut output, include_gate_status);
                    Ok(ToolResult {
                        success: true,
                        output,
                        error: None,
                    })
                }
                None => Ok(ToolResult {
                    success: true,
                    output: format!("No run found with ID '{run_id}'."),
                    error: None,
                }),
            };
        }

        // List runs for a specific SOP or all active runs
        let mut output = String::new();

        // Active runs
        let active: Vec<_> = engine
            .active_runs()
            .values()
            .filter(|r| sop_name.map_or(true, |name| r.sop_name == name))
            .collect();

        if active.is_empty() {
            let scope = sop_name.map_or(String::new(), |n| format!(" for '{n}'"));
            let _ = writeln!(output, "No active runs{scope}.");
        } else {
            let _ = writeln!(output, "Active runs ({}):", active.len());
            for run in &active {
                let _ = writeln!(
                    output,
                    "  {} — {} [{}] step {}/{}",
                    run.run_id, run.sop_name, run.status, run.current_step, run.total_steps
                );
            }
        }

        // Finished runs
        let finished = engine.finished_runs(sop_name);
        if !finished.is_empty() {
            let _ = writeln!(output, "\nFinished runs ({}):", finished.len());
            for run in finished.iter().rev().take(10) {
                let _ = writeln!(
                    output,
                    "  {} — {} [{}] ({})",
                    run.run_id,
                    run.sop_name,
                    run.status,
                    run.completed_at.as_deref().unwrap_or("?")
                );
            }
        }

        // Metrics summary (when requested and collector is available)
        if include_metrics {
            if let Some(ref collector) = self.collector {
                let prefix = sop_name.map_or("sop".to_string(), |n| format!("sop.{n}"));
                let _ = writeln!(output, "\nMetrics ({prefix}):");
                for suffix in METRIC_SUFFIXES {
                    let key = format!("{prefix}.{suffix}");
                    if let Some(val) = collector.get_metric_value(&key) {
                        let _ = writeln!(output, "  {suffix}: {}", format_metric_value(&val));
                    }
                }
            } else {
                let _ = writeln!(
                    output,
                    "\nMetrics: not available (collector not configured)"
                );
            }
        }

        self.append_gate_status(&mut output, include_gate_status);

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// Metric suffixes rendered in status output.
const METRIC_SUFFIXES: &[&str] = &[
    "runs_completed",
    "runs_failed",
    "runs_cancelled",
    "completion_rate",
    "deviation_rate",
    "protocol_adherence_rate",
    "human_intervention_count",
    "human_intervention_rate",
    "timeout_auto_approvals",
    "timeout_approval_rate",
    "completion_rate_7d",
    "deviation_rate_7d",
    "completion_rate_30d",
    "deviation_rate_30d",
];

fn format_metric_value(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                format!("{u}")
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 {
                    format!("{f:.0}")
                } else {
                    format!("{f:.4}")
                }
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SopConfig;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;

    fn test_sop(name: &str) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Test SOP: {name}"),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "Step one".into(),
                body: "Do it".into(),
                suggested_tools: vec![],
                requires_confirmation: false,
            }],
            cooldown_secs: 0,
            max_concurrent: 2,
            location: None,
        }
    }

    fn engine_with_sops(sops: Vec<Sop>) -> Arc<Mutex<SopEngine>> {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(sops);
        Arc::new(Mutex::new(engine))
    }

    fn manual_event() -> SopEvent {
        SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-02-19T12:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn status_no_runs() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let tool = SopStatusTool::new(engine);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No active runs"));
    }

    #[tokio::test]
    async fn status_with_active_run() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let run_id = {
            let mut e = engine.lock().unwrap();
            e.start_run("s1", manual_event()).unwrap();
            e.active_runs().keys().next().unwrap().clone()
        };
        let tool = SopStatusTool::new(engine);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Active runs (1)"));
        assert!(result.output.contains(&run_id));
    }

    #[tokio::test]
    async fn status_specific_run() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let run_id = {
            let mut e = engine.lock().unwrap();
            e.start_run("s1", manual_event()).unwrap();
            e.active_runs().keys().next().unwrap().clone()
        };
        let tool = SopStatusTool::new(engine);
        let result = tool.execute(json!({"run_id": run_id})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains(&format!("Run: {run_id}")));
        assert!(result.output.contains("Status: running"));
    }

    #[tokio::test]
    async fn status_unknown_run() {
        let engine = engine_with_sops(vec![]);
        let tool = SopStatusTool::new(engine);
        let result = tool
            .execute(json!({"run_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No run found"));
    }

    #[tokio::test]
    async fn status_filter_by_sop_name() {
        let engine = engine_with_sops(vec![test_sop("s1"), test_sop("s2")]);
        {
            let mut e = engine.lock().unwrap();
            e.start_run("s1", manual_event()).unwrap();
            e.start_run("s2", manual_event()).unwrap();
        }
        let tool = SopStatusTool::new(engine);
        let result = tool.execute(json!({"sop_name": "s1"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("s1"));
        // s2's run shouldn't show
        assert!(!result.output.contains(" s2 "));
    }

    #[test]
    fn name_and_schema() {
        let engine = engine_with_sops(vec![]);
        let tool = SopStatusTool::new(engine);
        assert_eq!(tool.name(), "sop_status");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["run_id"].is_object());
        assert!(schema["properties"]["sop_name"].is_object());
        assert!(schema["properties"]["include_metrics"].is_object());
    }

    #[tokio::test]
    async fn status_with_metrics_global() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let collector = Arc::new(SopMetricsCollector::new());
        // Record a completed run in the collector
        let run = SopRun {
            run_id: "r1".into(),
            sop_name: "s1".into(),
            trigger_event: manual_event(),
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

        let tool = SopStatusTool::new(engine).with_collector(collector);
        let result = tool
            .execute(json!({"include_metrics": true}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Metrics (sop):"));
        assert!(result.output.contains("runs_completed: 1"));
        assert!(result.output.contains("completion_rate: 1"));
    }

    #[tokio::test]
    async fn status_with_metrics_per_sop() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let collector = Arc::new(SopMetricsCollector::new());
        let run = SopRun {
            run_id: "r1".into(),
            sop_name: "s1".into(),
            trigger_event: manual_event(),
            status: SopRunStatus::Failed,
            current_step: 1,
            total_steps: 2,
            started_at: "2026-02-19T12:00:00Z".into(),
            completed_at: Some("2026-02-19T12:05:00Z".into()),
            step_results: vec![SopStepResult {
                step_number: 1,
                status: SopStepStatus::Failed,
                output: "fail".into(),
                started_at: "2026-02-19T12:00:00Z".into(),
                completed_at: Some("2026-02-19T12:01:00Z".into()),
            }],
            waiting_since: None,
        };
        collector.record_run_complete(&run);

        let tool = SopStatusTool::new(engine).with_collector(collector);
        let result = tool
            .execute(json!({"sop_name": "s1", "include_metrics": true}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Metrics (sop.s1):"));
        assert!(result.output.contains("runs_failed: 1"));
        assert!(result.output.contains("completion_rate: 0"));
    }

    #[tokio::test]
    async fn status_metrics_without_collector() {
        let engine = engine_with_sops(vec![]);
        let tool = SopStatusTool::new(engine);
        let result = tool
            .execute(json!({"include_metrics": true}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("not available"));
    }

    #[tokio::test]
    async fn status_metrics_not_shown_by_default() {
        let engine = engine_with_sops(vec![test_sop("s1")]);
        let collector = Arc::new(SopMetricsCollector::new());
        let tool = SopStatusTool::new(engine).with_collector(collector);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(!result.output.contains("Metrics"));
    }
}
