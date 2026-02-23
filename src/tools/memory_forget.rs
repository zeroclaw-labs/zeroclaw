use super::traits::{Tool, ToolResult};
use crate::memory::Memory;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Let the agent forget/delete a memory entry
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
        "Remove a memory by key. Use to delete outdated facts or sensitive data. Returns whether the memory was found and removed."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The key of the memory to forget"
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'key' parameter"))?;

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

        match self.memory.forget(key).await {
            Ok(true) => Ok(ToolResult {
                success: true,
                output: format!("Forgot memory: {key}"),
                error: None,
            }),
            Ok(false) => Ok(ToolResult {
                success: true,
                output: format!("No memory found with key: {key}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to forget memory: {e}")),
            }),
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
        assert!(tool.parameters_schema()["properties"]["key"].is_object());
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
    async fn forget_missing_key() {
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

    #[test]
    fn schema_has_required_key_field() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        let required = schema["required"].as_array().expect("required should be array");
        assert!(required.contains(&json!("key")));
        assert!(schema["properties"]["key"]["type"] == "string");
    }

    #[test]
    fn description_mentions_remove() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        let desc = tool.description();
        assert!(
            desc.contains("Remove") || desc.contains("remove") || desc.contains("delete"),
            "Description should mention removal: {desc}"
        );
    }

    #[test]
    fn spec_returns_consistent_metadata() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        let spec = tool.spec();
        assert_eq!(spec.name, "memory_forget");
        assert!(!spec.description.is_empty());
        assert!(spec.parameters["properties"]["key"].is_object());
    }

    #[tokio::test]
    async fn forget_key_with_special_characters() {
        let (_tmp, mem) = test_mem();
        let special_key = "user::pref/lang#1";
        mem.store(special_key, "value", MemoryCategory::Core, None)
            .await
            .unwrap();
        let tool = MemoryForgetTool::new(mem.clone(), test_security());
        let result = tool
            .execute(json!({"key": special_key}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Forgot"));
        assert!(mem.get(special_key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn forget_empty_string_key() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        // Empty string is a valid string value, so execute should succeed
        // but no memory exists for empty key
        let result = tool.execute(json!({"key": ""})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("No memory found"));
    }

    #[tokio::test]
    async fn forget_key_as_non_string_returns_error() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryForgetTool::new(mem, test_security());
        // Numeric value for key should fail since as_str() returns None
        let result = tool.execute(json!({"key": 42})).await;
        assert!(result.is_err());
    }
}
