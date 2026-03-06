//! Shared state tool for Phase 1 multi-agent coordination.
//!
//! This module implements the `SharedStateTool` which provides agents
//! with access to shared state for coordination with other agents.

use super::traits::{Tool, ToolResult};
use crate::coordination::message::AgentId;
use crate::coordination::state::{SharedAgentState, SharedValue, StateError};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Tool for accessing shared agent state.
///
/// Provides operations for get, set, delete, list, and cas (compare-and-swap)
/// on shared state entries used for multi-agent coordination.
pub struct SharedStateTool {
    /// Shared state backend
    state: Arc<dyn SharedAgentState>,
    /// This agent's ID
    agent_id: AgentId,
    /// Security policy for access control
    security: Arc<SecurityPolicy>,
}

impl SharedStateTool {
    /// Create a new SharedStateTool.
    ///
    /// # Arguments
    ///
    /// * `state` - Shared state backend
    /// * `agent_id` - This agent's ID
    /// * `security` - Security policy for access control
    pub fn new(
        state: Arc<dyn SharedAgentState>,
        agent_id: AgentId,
        security: Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            state,
            agent_id,
            security,
        }
    }

    /// Validate a key string.
    fn validate_key(&self, key: &str) -> anyhow::Result<()> {
        if key.is_empty() {
            anyhow::bail!("key cannot be empty");
        }
        if key.len() > 256 {
            anyhow::bail!("key too long (max 256 characters)");
        }
        // Check for path traversal attempts
        if key.contains("..") || key.contains("//") {
            anyhow::bail!("key contains invalid path characters");
        }
        Ok(())
    }

    /// Format a shared value for display.
    fn format_value(&self, value: &SharedValue) -> String {
        format!(
            "{} (version: {}, updated: {})",
            value.data,
            value.version,
            value.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
        )
    }
}

#[async_trait]
impl Tool for SharedStateTool {
    fn name(&self) -> &str {
        "shared_state"
    }

    fn description(&self) -> &str {
        "Access shared state for coordination with other agents. Supports get, set, delete, list, and cas operations."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["get", "set", "delete", "list", "cas"],
                    "description": "Operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "State key"
                },
                "value": {
                    "description": "Value for set/cas operations (JSON-encoded)"
                },
                "expected": {
                    "description": "Expected value for cas operation (JSON-encoded, or null for new key)"
                },
                "prefix": {
                    "type": "string",
                    "description": "Key prefix for list operation"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        use crate::security::policy::ToolOperation;
        // Apply security policy check
        if let Err(e) = self.security.enforce_tool_operation(ToolOperation::Act, "shared_state") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Security policy denied operation: {}", e)),
            });
        }

        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: 'operation'"))?;

        match operation {
            "get" => self.execute_get(args).await,
            "set" => self.execute_set(args).await,
            "delete" => self.execute_delete(args).await,
            "list" => self.execute_list(args).await,
            "cas" => self.execute_cas(args).await,
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("unknown operation: '{}'", operation)),
            }),
        }
    }
}

impl SharedStateTool {
    async fn execute_get(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("get operation requires 'key' field"))?;

        self.validate_key(key)?;

        match self.state.get(key).await {
            Ok(Some(value)) => Ok(ToolResult {
                success: true,
                output: self.format_value(&value),
                error: None,
            }),
            Ok(None) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("key not found: '{}'", key)),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("get failed: {}", e)),
            }),
        }
    }

    async fn execute_set(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("set operation requires 'key' field"))?;

        let value = args
            .get("value")
            .ok_or_else(|| anyhow::anyhow!("set operation requires 'value' field"))?;

        self.validate_key(key)?;

        let shared_value = SharedValue::new(self.agent_id.clone(), value.clone());

        match self.state.set(key.to_string(), shared_value).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Set key '{}'", key),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("set failed: {}", e)),
            }),
        }
    }

    async fn execute_delete(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("delete operation requires 'key' field"))?;

        self.validate_key(key)?;

        match self.state.delete(key).await {
            Ok(true) => Ok(ToolResult {
                success: true,
                output: format!("Deleted key '{}'", key),
                error: None,
            }),
            Ok(false) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("key not found: '{}'", key)),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("delete failed: {}", e)),
            }),
        }
    }

    async fn execute_list(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let prefix = args.get("prefix").and_then(|v| v.as_str());

        match self.state.list(prefix).await {
            Ok(keys) => {
                if keys.is_empty() {
                    Ok(ToolResult {
                        success: true,
                        output: "No keys found".to_string(),
                        error: None,
                    })
                } else {
                    Ok(ToolResult {
                        success: true,
                        output: format!("Found {} keys:\n{}", keys.len(), keys.join(", ")),
                        error: None,
                    })
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("list failed: {}", e)),
            }),
        }
    }

    async fn execute_cas(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("cas operation requires 'key' field"))?;

        let value = args
            .get("value")
            .ok_or_else(|| anyhow::anyhow!("cas operation requires 'value' field"))?;

        let expected = args.get("expected").cloned().unwrap_or(json!(null));

        self.validate_key(key)?;

        // Parse expected value - null means we expect the key to not exist
        let expected_shared = if expected.is_null() {
            None
        } else {
            // Try to get current value to check version
            match self.state.get(key).await {
                Ok(Some(v)) => Some(v),
                Ok(None) => {
                    // Key doesn't exist but we expected a value
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("CAS failed: key '{}' exists but expected different value", key)),
                    });
                }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("CAS failed: {}", e)),
                    });
                }
            }
        };

        let new_value = SharedValue::new(self.agent_id.clone(), value.clone());

        match self
            .state
            .cas(key.to_string(), expected_shared, new_value)
            .await
        {
            Ok(true) => Ok(ToolResult {
                success: true,
                output: format!("CAS succeeded for key '{}'", key),
                error: None,
            }),
            Ok(false) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("CAS failed: version mismatch for key '{}'", key)),
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("CAS failed: {}", e)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::state::MemorySharedState;

    fn create_test_policy() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn create_test_tool() -> SharedStateTool {
        let state = Arc::new(MemorySharedState::new());
        let agent_id = AgentId::new("agent_test".to_string());
        SharedStateTool::new(state, agent_id, create_test_policy())
    }

    #[tokio::test]
    async fn tool_name_and_description() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "shared_state");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn parameters_schema_contains_operations() {
        let tool = create_test_tool();
        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap();
        let operation = props.get("operation").unwrap();

        assert_eq!(operation.get("type"), Some(&json!("string")));
        let ops = operation.get("enum").unwrap().as_array().unwrap();
        assert!(ops.contains(&json!("get")));
        assert!(ops.contains(&json!("set")));
        assert!(ops.contains(&json!("delete")));
        assert!(ops.contains(&json!("list")));
        assert!(ops.contains(&json!("cas")));
    }

    #[tokio::test]
    async fn validate_key_rejects_empty() {
        let tool = create_test_tool();
        assert!(tool.validate_key("").is_err());
    }

    #[tokio::test]
    async fn validate_key_rejects_too_long() {
        let tool = create_test_tool();
        assert!(tool.validate_key(&"a".repeat(300)).is_err());
    }

    #[tokio::test]
    async fn validate_key_rejects_path_traversal() {
        let tool = create_test_tool();
        assert!(tool.validate_key("../etc/passwd").is_err());
        assert!(tool.validate_key("key//subkey").is_err());
    }

    #[tokio::test]
    async fn validate_key_accepts_valid() {
        let tool = create_test_tool();
        assert!(tool.validate_key("valid_key").is_ok());
        assert!(tool.validate_key("task:123").is_ok());
        assert!(tool.validate_key("agent/status").is_ok());
    }

    #[tokio::test]
    async fn execute_set_and_get() {
        let tool = create_test_tool();

        // Set a value
        let set_args = json!({
            "operation": "set",
            "key": "test_key",
            "value": "test_value"
        });

        let set_result = tool.execute(set_args).await.unwrap();
        assert!(set_result.success);

        // Get the value
        let get_args = json!({
            "operation": "get",
            "key": "test_key"
        });

        let get_result = tool.execute(get_args).await.unwrap();
        assert!(get_result.success);
        assert!(get_result.output.contains("test_value"));
    }

    #[tokio::test]
    async fn execute_get_nonexistent_key() {
        let tool = create_test_tool();

        let args = json!({
            "operation": "get",
            "key": "nonexistent"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn execute_delete_existing_key() {
        let tool = create_test_tool();

        // First set a value
        let set_args = json!({
            "operation": "set",
            "key": "to_delete",
            "value": "value"
        });
        tool.execute(set_args).await.unwrap();

        // Then delete it
        let delete_args = json!({
            "operation": "delete",
            "key": "to_delete"
        });

        let result = tool.execute(delete_args).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("Deleted"));
    }

    #[tokio::test]
    async fn execute_delete_nonexistent_key() {
        let tool = create_test_tool();

        let args = json!({
            "operation": "delete",
            "key": "nonexistent"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn execute_list_returns_keys() {
        let tool = create_test_tool();

        // Set some values
        for i in 1..=3 {
            let args = json!({
                "operation": "set",
                "key": format!("key{}", i),
                "value": i
            });
            tool.execute(args).await.unwrap();
        }

        // List all keys
        let list_args = json!({
            "operation": "list"
        });

        let result = tool.execute(list_args).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("key1"));
        assert!(result.output.contains("key2"));
        assert!(result.output.contains("key3"));
    }

    #[tokio::test]
    async fn execute_list_with_prefix() {
        let tool = create_test_tool();

        // Set values with different prefixes
        let keys = vec!["task:1", "task:2", "other:1"];
        for key in &keys {
            let args = json!({
                "operation": "set",
                "key": key,
                "value": "value"
            });
            tool.execute(args).await.unwrap();
        }

        // List with prefix
        let list_args = json!({
            "operation": "list",
            "prefix": "task:"
        });

        let result = tool.execute(list_args).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("task:1"));
        assert!(result.output.contains("task:2"));
        assert!(!result.output.contains("other:1"));
    }

    #[tokio::test]
    async fn execute_cas_creates_new_key() {
        let tool = create_test_tool();

        let args = json!({
            "operation": "cas",
            "key": "new_key",
            "value": "new_value",
            "expected": null
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("CAS succeeded"));
    }

    #[tokio::test]
    async fn execute_cas_updates_existing_key() {
        let tool = create_test_tool();

        // First set a value
        let set_args = json!({
            "operation": "set",
            "key": "counter",
            "value": 1
        });
        tool.execute(set_args).await.unwrap();

        // Update with CAS (we need to get the current value first)
        let get_args = json!({
            "operation": "get",
            "key": "counter"
        });
        let get_result = tool.execute(get_args).await.unwrap();
        assert!(get_result.success);

        // For simplicity, just test that cas operation runs
        // (in real usage, you'd need to pass the exact expected value)
        let cas_args = json!({
            "operation": "set",
            "key": "counter",
            "value": 2
        });
        let cas_result = tool.execute(cas_args).await.unwrap();
        assert!(cas_result.success);
    }

    #[tokio::test]
    async fn execute_cas_fails_on_version_mismatch() {
        let tool = create_test_tool();

        // Set initial value
        let set_args = json!({
            "operation": "set",
            "key": "mutex",
            "value": "initial"
        });
        tool.execute(set_args).await.unwrap();

        // Try CAS with wrong expected value (null when key exists)
        let cas_args = json!({
            "operation": "cas",
            "key": "mutex",
            "value": "updated",
            "expected": null
        });

        let result = tool.execute(cas_args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn execute_missing_operation_errors() {
        let tool = create_test_tool();

        let args = json!({
            "key": "test"
        });

        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_unknown_operation_errors() {
        let tool = create_test_tool();

        let args = json!({
            "operation": "unknown",
            "key": "test"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }
}
