use super::traits::{Tool, ToolResult};
use crate::memory::{Memory, MemoryCategory};
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Store observational memory entries in a dedicated category.
///
/// This gives agents an explicit path for Mastra-style observation memory
/// without mixing those entries into durable "core" facts by default.
pub struct MemoryObserveTool {
    memory: Arc<dyn Memory>,
    security: Arc<SecurityPolicy>,
}

impl MemoryObserveTool {
    pub fn new(memory: Arc<dyn Memory>, security: Arc<SecurityPolicy>) -> Self {
        Self { memory, security }
    }

    fn generate_key() -> String {
        format!("observation_{}", uuid::Uuid::new_v4())
    }
}

#[async_trait]
impl Tool for MemoryObserveTool {
    fn name(&self) -> &str {
        "memory_observe"
    }

    fn description(&self) -> &str {
        "Store an observation entry in observation memory for long-horizon context continuity."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "observation": {
                    "type": "string",
                    "description": "Observation to capture (fact, pattern, or running context signal)"
                },
                "key": {
                    "type": "string",
                    "description": "Optional custom key. Auto-generated when omitted."
                },
                "source": {
                    "type": "string",
                    "description": "Optional source label for traceability (e.g. 'chat', 'tool_result')."
                },
                "confidence": {
                    "type": "number",
                    "description": "Optional confidence score in [0.0, 1.0]."
                },
                "category": {
                    "type": "string",
                    "description": "Optional category override. Defaults to 'observation'."
                }
            },
            "required": ["observation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let observation = args
            .get("observation")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'observation' parameter"))?;

        if let Some(confidence) = args.get("confidence").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&confidence) {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'confidence' must be within [0.0, 1.0]".to_string()),
                });
            }
        }

        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(Self::generate_key);

        let source = args
            .get("source")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let confidence = args.get("confidence").and_then(|v| v.as_f64());

        let category = match args.get("category").and_then(|v| v.as_str()) {
            Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "core" => MemoryCategory::Core,
                "daily" => MemoryCategory::Daily,
                "conversation" => MemoryCategory::Conversation,
                "observation" | "" => MemoryCategory::Custom("observation".to_string()),
                other => MemoryCategory::Custom(other.to_string()),
            },
            None => MemoryCategory::Custom("observation".to_string()),
        };

        let mut content = observation.to_string();
        if source.is_some() || confidence.is_some() {
            let mut metadata = Vec::new();
            if let Some(source) = source {
                metadata.push(format!("source={source}"));
            }
            if let Some(confidence) = confidence {
                metadata.push(format!("confidence={confidence:.3}"));
            }
            content.push_str(&format!("\n\n[metadata] {}", metadata.join(", ")));
        }

        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "memory_store")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        match self.memory.store(&key, &content, category, None).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Stored observation memory: {key}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to store observation memory: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    fn test_mem() -> (TempDir, Arc<dyn Memory>) {
        let tmp = TempDir::new().unwrap();
        let mem = crate::memory::SqliteMemory::new(tmp.path()).unwrap();
        (tmp, Arc::new(mem))
    }

    #[test]
    fn name_and_schema() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryObserveTool::new(mem, test_security());
        assert_eq!(tool.name(), "memory_observe");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["observation"].is_object());
    }

    #[tokio::test]
    async fn stores_default_observation_category() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryObserveTool::new(mem.clone(), test_security());

        let result = tool
            .execute(json!({"observation": "User prefers concise deployment summaries"}))
            .await
            .unwrap();

        assert!(result.success);

        let entries = mem
            .list(Some(&MemoryCategory::Custom("observation".into())), None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0]
            .content
            .contains("User prefers concise deployment summaries"));
    }

    #[tokio::test]
    async fn stores_metadata_when_provided() {
        let (_tmp, mem) = test_mem();
        let tool = MemoryObserveTool::new(mem.clone(), test_security());

        let result = tool
            .execute(json!({
                "key": "obs_custom",
                "observation": "Compaction starts near long transcript threshold",
                "source": "agent_loop",
                "confidence": 0.92
            }))
            .await
            .unwrap();
        assert!(result.success);

        let entry = mem.get("obs_custom").await.unwrap().unwrap();
        assert!(entry.content.contains("[metadata]"));
        assert!(entry.content.contains("source=agent_loop"));
        assert!(entry.content.contains("confidence=0.920"));
        assert_eq!(entry.category, MemoryCategory::Custom("observation".into()));
    }

    #[tokio::test]
    async fn blocked_in_readonly_mode() {
        let (_tmp, mem) = test_mem();
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = MemoryObserveTool::new(mem.clone(), readonly);
        let result = tool
            .execute(json!({"observation": "Should not persist"}))
            .await
            .unwrap();

        assert!(!result.success);
        let count = mem.count().await.unwrap();
        assert_eq!(count, 0);
    }
}
