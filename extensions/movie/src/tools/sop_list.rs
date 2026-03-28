use std::fmt::Write;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::json;

use super::traits::{Tool, ToolResult};
use crate::sop::SopEngine;

/// Lists all loaded SOPs with their triggers, priority, step count, and active runs.
pub struct SopListTool {
    engine: std::sync::Arc<Mutex<SopEngine>>,
}

impl SopListTool {
    pub fn new(engine: std::sync::Arc<Mutex<SopEngine>>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for SopListTool {
    fn name(&self) -> &str {
        "sop_list"
    }

    fn description(&self) -> &str {
        "List all loaded Standard Operating Procedures (SOPs) with their triggers, priority, step count, and active run count. Optionally filter by name or priority."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filter": {
                    "type": "string",
                    "description": "Filter SOPs by name substring or priority (low/normal/high/critical)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let filter = args.get("filter").and_then(|v| v.as_str()).unwrap_or("");
        let filter_lower = filter.to_lowercase();

        let engine = self
            .engine
            .lock()
            .map_err(|e| anyhow::anyhow!("Engine lock poisoned: {e}"))?;
        let sops = engine.sops();

        if sops.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No SOPs loaded.".into(),
                error: None,
            });
        }

        let filtered: Vec<_> = if filter_lower.is_empty() {
            sops.iter().collect()
        } else {
            sops.iter()
                .filter(|s| {
                    s.name.to_lowercase().contains(&filter_lower)
                        || s.priority.to_string() == filter_lower
                })
                .collect()
        };

        if filtered.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: format!("No SOPs match filter '{filter}'."),
                error: None,
            });
        }

        let active_runs = engine.active_runs();
        let mut output = format!(
            "Loaded SOPs ({} total, {} shown):\n\n",
            sops.len(),
            filtered.len()
        );

        for sop in &filtered {
            let active_count = active_runs
                .values()
                .filter(|r| r.sop_name == sop.name)
                .count();
            let triggers: Vec<String> = sop.triggers.iter().map(|t| t.to_string()).collect();

            let _ = writeln!(
                output,
                "- **{}** [{}] — {} steps, {} trigger(s): {}{}",
                sop.name,
                sop.priority,
                sop.steps.len(),
                sop.triggers.len(),
                triggers.join(", "),
                if active_count > 0 {
                    format!(" (active runs: {active_count})")
                } else {
                    String::new()
                }
            );
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SopConfig;
    use crate::sop::engine::SopEngine;
    use crate::sop::types::*;
    use std::sync::Arc;

    fn test_sop(name: &str, priority: SopPriority) -> Sop {
        Sop {
            name: name.into(),
            description: format!("Test SOP: {name}"),
            version: "1.0.0".into(),
            priority,
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
            max_concurrent: 1,
            location: None,
        }
    }

    fn engine_with_sops(sops: Vec<Sop>) -> Arc<Mutex<SopEngine>> {
        let mut engine = SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(sops);
        Arc::new(Mutex::new(engine))
    }

    #[tokio::test]
    async fn list_all_sops() {
        let engine = engine_with_sops(vec![
            test_sop("pump-shutdown", SopPriority::Critical),
            test_sop("daily-check", SopPriority::Normal),
        ]);
        let tool = SopListTool::new(engine);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("pump-shutdown"));
        assert!(result.output.contains("daily-check"));
        assert!(result.output.contains("2 total"));
    }

    #[tokio::test]
    async fn list_empty() {
        let engine = engine_with_sops(vec![]);
        let tool = SopListTool::new(engine);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No SOPs loaded"));
    }

    #[tokio::test]
    async fn filter_by_name() {
        let engine = engine_with_sops(vec![
            test_sop("pump-shutdown", SopPriority::Critical),
            test_sop("daily-check", SopPriority::Normal),
        ]);
        let tool = SopListTool::new(engine);
        let result = tool.execute(json!({"filter": "pump"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("pump-shutdown"));
        assert!(!result.output.contains("daily-check"));
    }

    #[tokio::test]
    async fn filter_by_priority() {
        let engine = engine_with_sops(vec![
            test_sop("pump-shutdown", SopPriority::Critical),
            test_sop("daily-check", SopPriority::Normal),
        ]);
        let tool = SopListTool::new(engine);
        let result = tool.execute(json!({"filter": "critical"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("pump-shutdown"));
        assert!(!result.output.contains("daily-check"));
    }

    #[tokio::test]
    async fn filter_no_match() {
        let engine = engine_with_sops(vec![test_sop("pump-shutdown", SopPriority::Critical)]);
        let tool = SopListTool::new(engine);
        let result = tool
            .execute(json!({"filter": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No SOPs match"));
    }

    #[test]
    fn name_and_schema() {
        let engine = engine_with_sops(vec![]);
        let tool = SopListTool::new(engine);
        assert_eq!(tool.name(), "sop_list");
        assert!(tool.parameters_schema()["properties"]["filter"].is_object());
    }
}
