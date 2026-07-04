use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;
use zeroclaw_memory::Memory;

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
        "Remove all memories in a namespace or session. Use to bulk-delete per-tenant or per-conversation data. Returns the number of deleted entries. WARNING: This operation cannot be undone."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "The namespace to purge. Deletes all memories whose namespace field equals this value."
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
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"missing": "namespace_or_session_id"})),
                "memory_purge: must provide namespace or session_id"
            );
            return Err(anyhow::Error::msg(
                "Must provide either 'namespace' or 'session_id' parameter",
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
    use tempfile::TempDir;
    use zeroclaw_config::autonomy::AutonomyLevel;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_memory::{MemoryCategory, MemoryEntry, SqliteMemory};

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
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
    async fn purge_namespace_removes_only_all_matching_memories() {
        let (_tmp, mem) = test_mem();

        mem.store_with_metadata("a", "data", MemoryCategory::Core, None, Some("ns1"), None)
            .await
            .unwrap();
        mem.store_with_metadata("b", "data", MemoryCategory::Core, None, Some("ns2"), None)
            .await
            .unwrap();

        let in_ns1 =
            |entries: &[MemoryEntry]| entries.iter().filter(|e| e.namespace == "ns1").count();

        let before = mem.list(None, None).await.unwrap();
        let tool = MemoryPurgeTool::new(mem.clone(), test_security());
        let result = tool.execute(json!({"namespace": "ns1"})).await.unwrap();
        let after = mem.list(None, None).await.unwrap();

        assert!(result.success);
        assert_eq!(in_ns1(&after), 0);
        assert_eq!(after.len() - in_ns1(&after), before.len() - in_ns1(&before));
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
        mem.store_with_metadata(
            "a",
            "data",
            MemoryCategory::Core,
            None,
            Some("test-ns"),
            None,
        )
        .await
        .unwrap();
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryPurgeTool::new(mem.clone(), readonly);
        let result = tool.execute(json!({"namespace": "test-ns"})).await.unwrap();
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
        mem.store_with_metadata(
            "a",
            "data",
            MemoryCategory::Core,
            None,
            Some("test-ns"),
            None,
        )
        .await
        .unwrap();
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MemoryPurgeTool::new(mem.clone(), limited);
        let result = tool.execute(json!({"namespace": "test-ns"})).await.unwrap();
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
