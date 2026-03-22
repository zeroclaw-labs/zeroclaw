use super::traits::{Tool, ToolResult};
use crate::memory::{Memory, MemoryCategory};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write;
use std::sync::Arc;

/// Let the agent list memories by category and optional key prefix
pub struct MemoryListTool {
    memory: Arc<dyn Memory>,
}

impl MemoryListTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for MemoryListTool {
    fn name(&self) -> &str {
        "memory_list"
    }

    fn description(&self) -> &str {
        "List memories by category and optional key prefix. Unlike memory_recall (which searches by relevance), this returns all matching entries deterministically. Use when you need a complete inventory of stored data, e.g. all active TODOs or all health logs for a date."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Filter by category: 'core', 'daily', 'conversation', or a custom category name"
                },
                "prefix": {
                    "type": "string",
                    "description": "Filter to keys starting with this prefix (e.g. 'todo:active:', 'health:food:2026-03-21')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 100)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let category = args.get("category").and_then(|v| v.as_str()).map(|s| {
            match s {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                other => MemoryCategory::Custom(other.to_string()),
            }
        });

        let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("");

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(100, |v| v as usize);

        // Use list_by_prefix when a prefix is provided (pushes LIKE into SQL),
        // fall back to plain list otherwise.
        let result = if prefix.is_empty() {
            self.memory.list(category.as_ref(), None).await.map(|entries| {
                entries.into_iter().take(limit).collect::<Vec<_>>()
            })
        } else {
            self.memory.list_by_prefix(category.as_ref(), prefix, limit).await
        };

        match result {
            Ok(filtered) => {

                if filtered.is_empty() {
                    return Ok(ToolResult {
                        success: true,
                        output: "No memories found matching the criteria.".into(),
                        error: None,
                    });
                }

                let mut output = format!("Found {} memories:\n", filtered.len());
                for entry in &filtered {
                    let _ = writeln!(
                        output,
                        "- [{}] {}: {}",
                        entry.category, entry.key, entry.content
                    );
                }
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Memory list failed: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SqliteMemory;
    use tempfile::TempDir;

    fn seeded_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[tokio::test]
    async fn list_empty() {
        let (_tmp, mem) = seeded_mem();
        let tool = MemoryListTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No memories found"));
    }

    #[tokio::test]
    async fn list_all() {
        let (_tmp, mem) = seeded_mem();
        mem.store("a:1", "first", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("a:2", "second", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b:1", "third", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let tool = MemoryListTool::new(mem);
        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 3"));
    }

    #[tokio::test]
    async fn list_by_category() {
        let (_tmp, mem) = seeded_mem();
        mem.store("a:1", "core item", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("b:1", "daily item", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let tool = MemoryListTool::new(mem);
        let result = tool
            .execute(json!({"category": "core"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 1"));
        assert!(result.output.contains("core item"));
    }

    #[tokio::test]
    async fn list_by_prefix() {
        let (_tmp, mem) = seeded_mem();
        mem.store("todo:active:1", "task 1", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("todo:active:2", "task 2", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("todo:done:3", "done task", MemoryCategory::Daily, None)
            .await
            .unwrap();
        mem.store("health:food:1", "food log", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let tool = MemoryListTool::new(mem);
        let result = tool
            .execute(json!({"prefix": "todo:active:"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 2"));
        assert!(result.output.contains("task 1"));
        assert!(result.output.contains("task 2"));
    }

    #[tokio::test]
    async fn list_by_category_and_prefix() {
        let (_tmp, mem) = seeded_mem();
        mem.store("todo:active:1", "active task", MemoryCategory::Core, None)
            .await
            .unwrap();
        mem.store("todo:done:2", "done task", MemoryCategory::Daily, None)
            .await
            .unwrap();

        let tool = MemoryListTool::new(mem);
        let result = tool
            .execute(json!({"category": "core", "prefix": "todo:"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 1"));
        assert!(result.output.contains("active task"));
    }

    #[tokio::test]
    async fn list_respects_limit() {
        let (_tmp, mem) = seeded_mem();
        for i in 0..10 {
            mem.store(
                &format!("k:{i}"),
                &format!("item {i}"),
                MemoryCategory::Core,
                None,
            )
            .await
            .unwrap();
        }

        let tool = MemoryListTool::new(mem);
        let result = tool.execute(json!({"limit": 3})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Found 3"));
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = seeded_mem();
        let tool = MemoryListTool::new(mem);
        assert_eq!(tool.name(), "memory_list");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["category"].is_object());
        assert!(schema["properties"]["prefix"].is_object());
        assert!(schema["properties"]["limit"].is_object());
    }
}
