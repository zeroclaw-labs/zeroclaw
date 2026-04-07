use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent forget/delete memory entries — by exact key or by pattern.
pub struct MemoryForgetTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
}

impl MemoryForgetTool {
    pub fn new(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self { memory, security }
    }
}

#[async_trait]
impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn description(&self) -> &str {
        "Remove memories by exact key or by keyword pattern. \
         Supports bulk deletion for '망각 요청' — e.g. delete all memories about a topic/person. \
         ALWAYS confirm with the user before pattern-based bulk deletion."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Exact key of a single memory to forget"
                },
                "pattern": {
                    "type": "string",
                    "description": "Keyword pattern to bulk-delete all matching memories \
                     (e.g. '전남편', 'old_project'). Matches against both key and content. \
                     ALWAYS confirm with user before using this."
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args.get("key").and_then(|v| v.as_str());
        let pattern = args.get("pattern").and_then(|v| v.as_str());

        if key.is_none() && pattern.is_none() {
            anyhow::bail!("Missing 'key' or 'pattern' parameter — provide at least one");
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "memory_forget")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        // Pattern-based bulk deletion (망각 요청)
        if let Some(pat) = pattern {
            if !pat.is_empty() {
                match self.memory.forget_matching(pat).await {
                    Ok(count) if count > 0 => {
                        return Ok(ToolResult {
                            success: true,
                            output: format!(
                                "'{pat}' 패턴과 일치하는 기억 {count}건을 삭제했습니다."
                            ),
                            error: None,
                        });
                    }
                    Ok(_) => {
                        return Ok(ToolResult {
                            success: true,
                            output: format!("'{pat}' 패턴과 일치하는 기억이 없습니다."),
                            error: None,
                        });
                    }
                    Err(e) => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("패턴 삭제 실패: {e}")),
                        });
                    }
                }
            }
        }

        // Exact key deletion
        if let Some(k) = key {
            match self.memory.forget(k).await {
                Ok(true) => Ok(ToolResult {
                    success: true,
                    output: format!("Forgot memory: {k}"),
                    error: None,
                }),
                Ok(false) => Ok(ToolResult {
                    success: true,
                    output: format!("No memory found with key: {k}"),
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to forget memory: {e}")),
                }),
            }
        } else {
            Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("No key or pattern provided".into()),
            })
        }
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
        let tool = MemoryForgetTool::new(mem, test_security());
        assert_eq!(tool.name(), "memory_forget");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["key"].is_object());
        assert!(schema["properties"]["pattern"].is_object());
    }

    #[tokio::test]
    async fn forget_existing() {
        let (_tmp, mem) = test_mem();
        mem.store("temp", "temporary", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        let tool = MemoryForgetTool::new(mem.clone(), test_security());
        let result = tool.execute(json!({"key": "temp"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Forgot"));

        assert!(mem.get("temp").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn forget_nonexistent() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        let result = tool.execute(json!({"key": "nope"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No memory found"));
    }

    #[tokio::test]
    async fn forget_missing_both_params() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn forget_blocked_in_readonly_mode() {
        let (_tmp, mem) = test_mem();
        mem.store("temp", "temporary", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryForgetTool::new(mem.clone(), readonly);
        let result = tool.execute(json!({"key": "temp"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("read-only mode"));
        assert!(mem.get("temp").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn forget_blocked_when_rate_limited() {
        let (_tmp, mem) = test_mem();
        mem.store("temp", "temporary", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        let limited = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });
        let tool = MemoryForgetTool::new(mem.clone(), limited);
        let result = tool.execute(json!({"key": "temp"})).await.unwrap();
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("Rate limit exceeded"));
        assert!(mem.get("temp").await.unwrap().is_some());
    }
}
