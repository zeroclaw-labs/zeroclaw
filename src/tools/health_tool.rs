use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for querying stored Apple Health data.
///
/// Reads health metrics and workouts previously stored by the `/health/export`
/// gateway endpoint. Supports filtering by metric type and date range.
pub struct HealthQueryTool {
    memory: Arc<dyn Memory>,
}

impl HealthQueryTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for HealthQueryTool {
    fn name(&self) -> &str {
        "health_query"
    }

    fn description(&self) -> &str {
        "Query stored Apple Health data including heart rate, steps, sleep, workouts, \
         and other metrics synced from Health Auto Export. \
         Supports filtering by metric type and date range."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "metric": {
                    "type": "string",
                    "description": "Metric type to query (e.g. 'heart_rate', 'step_count', \
                        'sleep_analysis', 'weight_&_body_mass', 'active_energy', 'workout'). \
                        Omit to search all health data."
                },
                "since": {
                    "type": "string",
                    "description": "Start of date range (RFC 3339, e.g. '2026-03-20T00:00:00Z')"
                },
                "until": {
                    "type": "string",
                    "description": "End of date range (RFC 3339, e.g. '2026-03-23T23:59:59Z')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of entries to return. Default: 10"
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let metric = args.get("metric").and_then(|m| m.as_str());
        let since = args.get("since").and_then(|s| s.as_str());
        let until = args.get("until").and_then(|u| u.as_str());
        let limit =
            usize::try_from(args.get("limit").and_then(|l| l.as_u64()).unwrap_or(10)).unwrap_or(10);

        // Build search query based on metric filter
        let query = match metric {
            Some(m) => format!("health:{m}"),
            None => "health:".to_string(),
        };

        let entries = self
            .memory
            .recall(&query, limit, None, since, until)
            .await?;

        if entries.is_empty() {
            let msg = match metric {
                Some(m) => format!("No health data found for metric '{m}' in the specified range."),
                None => "No health data found in the specified range.".to_string(),
            };
            return Ok(ToolResult {
                success: true,
                output: msg,
                error: None,
            });
        }

        // Parse and format entries
        let mut results: Vec<Value> = Vec::with_capacity(entries.len());
        for entry in &entries {
            // Try to parse the stored content as JSON; fall back to raw string
            let content: Value = serde_json::from_str(&entry.content)
                .unwrap_or_else(|_| json!({"raw": entry.content}));
            results.push(json!({
                "key": entry.key,
                "data": content,
                "stored_at": entry.timestamp,
            }));
        }

        let output = serde_json::to_string_pretty(&json!({
            "count": results.len(),
            "entries": results,
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
    use crate::memory::traits::MemoryEntry;
    use crate::memory::MemoryCategory;

    struct MockHealthMemory {
        entries: Vec<MemoryEntry>,
    }

    #[async_trait]
    impl Memory for MockHealthMemory {
        fn name(&self) -> &str {
            "mock_health"
        }

        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
            _session_id: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn recall(
            &self,
            _query: &str,
            limit: usize,
            _session_id: Option<&str>,
            _since: Option<&str>,
            _until: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(self.entries.iter().take(limit).cloned().collect())
        }

        async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
            Ok(None)
        }

        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
            _session_id: Option<&str>,
        ) -> anyhow::Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }

        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn count(&self) -> anyhow::Result<usize> {
            Ok(self.entries.len())
        }

        async fn health_check(&self) -> bool {
            true
        }
    }

    #[test]
    fn spec_fields_are_populated() {
        let tool = HealthQueryTool::new(Arc::new(MockHealthMemory {
            entries: Vec::new(),
        }));
        let spec = tool.spec();
        assert_eq!(spec.name, "health_query");
        assert!(!spec.description.is_empty());
        assert_eq!(spec.parameters["type"], "object");
        assert!(spec.parameters["properties"]["metric"].is_object());
        assert!(spec.parameters["properties"]["since"].is_object());
        assert!(spec.parameters["properties"]["until"].is_object());
        assert!(spec.parameters["properties"]["limit"].is_object());
    }

    #[tokio::test]
    async fn execute_returns_empty_message_when_no_data() {
        let tool = HealthQueryTool::new(Arc::new(MockHealthMemory {
            entries: Vec::new(),
        }));
        let result = tool.execute(json!({"metric": "heart_rate"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No health data found"));
        assert!(result.output.contains("heart_rate"));
    }

    #[tokio::test]
    async fn execute_returns_entries_when_data_exists() {
        let entries = vec![MemoryEntry {
            id: "1".into(),
            key: "health:step_count:2026-03-23".into(),
            content: json!({
                "metric": "step_count",
                "units": "count",
                "date": "2026-03-23",
                "points": [{"qty": 8500, "date": "2026-03-23 12:00:00 -0500"}],
                "last_synced": "2026-03-23T18:00:00Z",
            })
            .to_string(),
            category: MemoryCategory::Custom("health".into()),
            timestamp: "2026-03-23T18:00:00Z".into(),
            session_id: None,
            score: None,
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        }];

        let tool = HealthQueryTool::new(Arc::new(MockHealthMemory { entries }));
        let result = tool.execute(json!({"metric": "step_count"})).await.unwrap();
        assert!(result.success);

        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["count"], 1);
        assert_eq!(parsed["entries"][0]["data"]["metric"], "step_count");
    }

    #[tokio::test]
    async fn execute_respects_limit() {
        let entries: Vec<MemoryEntry> = (0..5)
            .map(|i| MemoryEntry {
                id: i.to_string(),
                key: format!("health:step_count:2026-03-{:02}", 20 + i),
                content: json!({"metric": "step_count", "date": format!("2026-03-{:02}", 20 + i)})
                    .to_string(),
                category: MemoryCategory::Custom("health".into()),
                timestamp: "2026-03-23T18:00:00Z".into(),
                session_id: None,
                score: None,
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
            })
            .collect();

        let tool = HealthQueryTool::new(Arc::new(MockHealthMemory { entries }));
        let result = tool.execute(json!({"limit": 3})).await.unwrap();
        assert!(result.success);

        let parsed: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["count"], 3);
    }
}
