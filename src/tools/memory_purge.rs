use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent bulk-delete memories by namespace or session
pub struct MemoryPurgeTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
}

impl MemoryPurgeTool {
    pub fn new(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self { memory, security }
    }
}

#[async_trait]
impl Tool for MemoryPurgeTool {
    fn name(&self) -> &str {
        "memory_purge"
    }

    fn description(&self) -> &str {
        "Remove all memories in a namespace (category) or session. Use to bulk-delete conversation context or category-scoped data. Returns the number of deleted entries. WARNING: This operation cannot be undone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "The namespace (category) to purge. Deletes all memories in this category."
                },
                "session_id": {
                    "type": "string",
                    "description": "The session ID to purge. Deletes all memories in this session."
                }
            },
            "minProperties": 1
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let namespace = args.get("namespace").and_then(|v| v.as_str());
        let session_id = args.get("session_id").and_then(|v| v.as_str());

        if namespace.is_none() && session_id.is_none() {
            return Err(anyhow::anyhow!(
                "Must provide either 'namespace' or 'session_id' parameter"
            ));
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "memory_purge")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let mut total_purged = 0;
        let mut output_parts = Vec::new();

        if let Some(ns) = namespace {
            match self.memory.purge_namespace(ns).await {
                Ok(count) => {
                    total_purged += count;
                    output_parts.push(format!("Purged {count} memories from namespace '{ns}'"));
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to purge namespace: {e}")),
                    });
                }
            }
        }

        if let Some(sid) = session_id {
            match self.memory.purge_session(sid).await {
                Ok(count) => {
                    total_purged += count;
                    output_parts.push(format!("Purged {count} memories from session '{sid}'"));
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to purge session: {e}")),
                    });
                }
            }
        }

        Ok(ToolResult {
            success: true,
            output: if output_parts.is_empty() {
                format!("Purged {total_purged} memories")
            } else {
                output_parts.join("; ")
            },
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemoryCategory, SqliteMemory};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryPurgeTool::new(mem, test_security());
        assert_eq!(tool.name(), "memory_purge");
        assert!(tool.parameters_schema()["properties"]["namespace"].is_object());
        assert!(tool.parameters_schema()["properties"]["session_id"].is_object());
    }

    #[tokio::test]
    async fn purge_namespace_removes_all_memories() {
        let (_tmp, mem) = test_mem();
        mem.store(
            "a1",
            "data1",
            MemoryCategory::Custom("test_ns".into()),
            None,
        )
        .await
        .unwrap();
        mem.store(
            "a2",
            "data2",
            MemoryCategory::Custom("test_ns".into()),
            None,
        )
        .await
        .unwrap();
        mem.store("b1", "data3", MemoryCategory::Core, None)
            .await
            .unwrap();

        let tool = MemoryPurgeTool::new(mem.clone(), test_security());
        let result = tool.execute(json!({"namespace": "test_ns"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("2 memories"));

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn purge_session_removes_all_memories() {
        let (_tmp, mem) = test_mem();
        mem.store("a1", "data1", MemoryCategory::Core, Some("sess-x"))
            .await
            .unwrap();
        mem.store("a2", "data2", MemoryCategory::Core, Some("sess-x"))
            .await
            .unwrap();
        mem.store("b1", "data3", MemoryCategory::Core, Some("sess-y"))
            .await
            .unwrap();

        let tool = MemoryPurgeTool::new(mem.clone(), test_security());
        let result = tool.execute(json!({"session_id": "sess-x"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("2 memories"));

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn purge_namespace_nonexistent_is_noop() {
        let (_tmp, mem) = test_mem();
        mem.store("a", "data", MemoryCategory::Core, None)
            .await
            .unwrap();

        let tool = MemoryPurgeTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"namespace": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 memories"));

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn purge_session_nonexistent_is_noop() {
        let (_tmp, mem) = test_mem();
        mem.store("a", "data", MemoryCategory::Core, Some("sess"))
            .await
            .unwrap();

        let tool = MemoryPurgeTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"session_id": "nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("0 memories"));

        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn purge_missing_parameter() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryPurgeTool::new(mem, test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn purge_blocked_in_readonly_mode() {
        let (_tmp, mem) = test_mem();
        mem.store("a", "data", MemoryCategory::Custom("test".into()), None)
            .await
            .unwrap();
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryPurgeTool::new(mem.clone(), readonly);
        let result = tool.execute(json!({"namespace": "test"})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("read-only mode")
        );
        assert_eq!(mem.count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn purge_blocked_when_rate_limited() {
        let (_tmp, mem) = test_mem();
        mem.store("a", "data", MemoryCategory::Custom("test".into()), None)
            .await
            .unwrap();
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MemoryPurgeTool::new(mem.clone(), limited);
        let result = tool.execute(json!({"namespace": "test"})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Rate limit exceeded")
        );
        assert_eq!(mem.count().await.unwrap(), 1);
    }
}
