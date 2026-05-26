//! Soul status tool — read-only introspection into agent soul and survival state.

use super::traits::{Tool, ToolResult};
use crate::soul::survival::SurvivalMonitor;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

/// Read-only tool that returns the agent's current survival tier, credit balance,
/// and soul identity summary.
pub struct SoulStatusTool {
    monitor: Arc<Mutex<SurvivalMonitor>>,
}

impl SoulStatusTool {
    pub fn new(monitor: Arc<Mutex<SurvivalMonitor>>) -> Self {
        Self { monitor }
    }
}

#[async_trait]
impl Tool for SoulStatusTool {
    fn name(&self) -> &str {
        "soul_status"
    }

    fn description(&self) -> &str {
        "Returns the agent's current survival tier, credit balance, and operational status. Read-only."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let status = {
            let monitor = self.monitor.lock();
            monitor.status_summary()
        };

        let output = serde_json::to_string_pretty(&json!({
            "tier": status.tier.label(),
            "balance_cents": status.balance_cents,
            "balance_usd": format!("${:.2}", status.balance_cents as f64 / 100.0),
            "is_alive": status.is_alive,
            "is_degraded": status.is_degraded,
        }))?;

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
    use crate::soul::survival::SurvivalThresholds;

    fn test_monitor(balance: i64) -> Arc<Mutex<SurvivalMonitor>> {
        Arc::new(Mutex::new(SurvivalMonitor::new(
            balance,
            SurvivalThresholds::default(),
        )))
    }

    #[test]
    fn tool_metadata() {
        let tool = SoulStatusTool::new(test_monitor(100));
        assert_eq!(tool.name(), "soul_status");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["type"] == "object");
    }

    #[tokio::test]
    async fn tool_returns_normal_status() {
        let tool = SoulStatusTool::new(test_monitor(200));
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("NORMAL"));
        assert!(result.output.contains("200"));
    }

    #[tokio::test]
    async fn tool_returns_critical_status() {
        let tool = SoulStatusTool::new(test_monitor(5));
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("CRITICAL"));
        assert!(result.output.contains("\"is_degraded\": true"));
    }

    #[tokio::test]
    async fn tool_returns_dead_status() {
        let tool = SoulStatusTool::new(test_monitor(-10));
        let result = tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("DEAD"));
        assert!(result.output.contains("\"is_alive\": false"));
    }
}
