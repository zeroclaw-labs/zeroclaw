use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;

#[cfg(not(feature = "embedded-brain"))]
use reqwest::Client;
#[cfg(feature = "embedded-brain")]
use std::sync::Arc;
#[cfg(feature = "embedded-brain")]
use tokio::sync::Mutex;
#[cfg(feature = "embedded-brain")]
use crate::brain::LearningBrain;

#[cfg(not(feature = "embedded-brain"))]
const DEFAULT_LIFEBOOK_URL: &str = "http://127.0.0.1:9090";

#[cfg(not(feature = "embedded-brain"))]
fn lifebook_url() -> String {
    std::env::var("LIFEBOOK_URL")
        .unwrap_or_else(|_| DEFAULT_LIFEBOOK_URL.to_string())
}

pub struct LearningReportTool {
    #[cfg(not(feature = "embedded-brain"))]
    client: Client,
    #[cfg(feature = "embedded-brain")]
    brain: Arc<Mutex<LearningBrain>>,
}

impl LearningReportTool {
    #[cfg(not(feature = "embedded-brain"))]
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest client"),
        }
    }

    #[cfg(feature = "embedded-brain")]
    pub fn new(brain: Arc<Mutex<LearningBrain>>) -> Self {
        Self { brain }
    }
}

#[cfg(not(feature = "embedded-brain"))]
impl Default for LearningReportTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for LearningReportTool {
    fn name(&self) -> &str {
        "learning_report"
    }

    fn description(&self) -> &str {
        "Inspect learning telemetry, cloud rescues, consolidated learn summaries, and promotion candidates for keeping task families local."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        #[cfg(feature = "embedded-brain")]
        {
            let brain = self.brain.lock().await;
            let mem_count = brain.rvf_memory.text_store.count().unwrap_or(0);
            let report = brain.learning_report_json(mem_count, 0);
            return Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&report)?,
                error: None,
            });
        }

        #[cfg(not(feature = "embedded-brain"))]
        {
            let url = format!("{}/learning_report", lifebook_url());
            let report: serde_json::Value = self.client
                .get(&url)
                .send()
                .await?
                .json()
                .await?;

            Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&report)?,
                error: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(feature = "embedded-brain"))]
    fn name_and_schema() {
        let tool = LearningReportTool::new();
        assert_eq!(tool.name(), "learning_report");
        assert_eq!(tool.parameters_schema()["type"], "object");
    }
}