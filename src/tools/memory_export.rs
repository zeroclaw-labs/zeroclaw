use super::traits::{Tool, ToolResult};
use crate::memory::traits::ExportFilter;
use crate::memory::{Memory, MemoryCategory};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Bulk-export memories as a JSON array for GDPR Art. 20 data portability.
pub struct MemoryExportTool {
    memory: Arc<dyn Memory>,
}

impl MemoryExportTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryExportTool {
    fn name(&self) -> &str {
        "memory_export"
    }

    fn description(&self) -> &str {
        "Export memories as a JSON array for GDPR Art. 20 data portability. \
         Supports filtering by namespace, session, category, and time range. \
         Returns a structured, machine-readable JSON array of memory entries."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Filter by namespace (agent/context isolation boundary)."
                },
                "session_id": {
                    "type": "string",
                    "description": "Filter by session ID."
                },
                "category": {
                    "type": "string",
                    "description": "Filter by category: core, daily, conversation, or a custom name."
                },
                "since": {
                    "type": "string",
                    "description": "RFC 3339 lower bound (inclusive) on created_at. Example: 2025-01-01T00:00:00Z"
                },
                "until": {
                    "type": "string",
                    "description": "RFC 3339 upper bound (inclusive) on created_at. Example: 2025-12-31T23:59:59Z"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let namespace = args
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from);
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .map(|s| match s {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                other => MemoryCategory::Custom(other.to_string()),
            });
        let since = args.get("since").and_then(|v| v.as_str()).map(String::from);
        let until = args.get("until").and_then(|v| v.as_str()).map(String::from);

        let filter = ExportFilter {
            namespace,
            session_id,
            category,
            since,
            until,
        };

        match self.memory.export(&filter).await {
            Ok(entries) => {
                let json_output = serde_json::to_string(&entries)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
                Ok(ToolResult {
                    success: true,
                    output: json_output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Export failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SqliteMemory;
    use tempfile::TempDir;

    fn test_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryExportTool::new(mem);
        assert_eq!(tool.name(), "memory_export");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["namespace"].is_object());
        assert!(schema["properties"]["session_id"].is_object());
        assert!(schema["properties"]["category"].is_object());
        assert!(schema["properties"]["since"].is_object());
        assert!(schema["properties"]["until"].is_object());
    }

    #[tokio::test]
    async fn export_produces_valid_json_output() {
        let (_tmp, mem) = test_mem();
        mem.store("k1", "test data", MemoryCategory::Core, None)
            .await
            .unwrap();

        let tool = MemoryExportTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn export_empty_database_returns_empty_array() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryExportTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(parsed.is_array());
        assert!(parsed.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn export_with_category_filter() {
        let (_tmp, mem) = test_mem();
        mem.store("k1", "core data", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("k2", "daily data", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let tool = MemoryExportTool::new(mem);
        let result = tool.execute(json!({"category": "core"})).await.unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["category"], "core");
    }

    #[tokio::test]
    async fn export_with_session_filter() {
        let (_tmp, mem) = test_mem();
        mem.store("k1", "sess-a data", MemoryCategory::Core, Some("sess-a"))
            .await
            .unwrap();
        mem.store("k2", "sess-b data", MemoryCategory::Core, Some("sess-b"))
            .await
            .unwrap();

        let tool = MemoryExportTool::new(mem);
        let result = tool.execute(json!({"session_id": "sess-a"})).await.unwrap();
        assert!(result.success);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["key"], "k1");
    }
}
