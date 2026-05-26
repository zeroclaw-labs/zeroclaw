//! Soul replicate tool — spawns child agents with constitution propagation.

use super::traits::{Tool, ToolResult};
use crate::soul::replication::ReplicationManager;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;

pub struct SoulReplicateTool {
    manager: Arc<Mutex<ReplicationManager>>,
}

impl SoulReplicateTool {
    pub fn new(manager: Arc<Mutex<ReplicationManager>>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SoulReplicateTool {
    fn name(&self) -> &str {
        "soul_replicate"
    }

    fn description(&self) -> &str {
        "Spawn a child agent with inherited constitution. Requires full autonomy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "child_id": {
                    "type": "string",
                    "description": "Unique identifier for the child agent"
                },
                "constitution_hash": {
                    "type": "string",
                    "description": "SHA-256 hash of the constitution to propagate (must match parent)"
                }
            },
            "required": ["child_id", "constitution_hash"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let child_id = args["child_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("child_id must be a string"))?;

        let constitution_hash = args["constitution_hash"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("constitution_hash must be a string"))?;

        let mut mgr = self.manager.lock();

        if !mgr.verify_constitution(constitution_hash) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "constitution hash mismatch — child must inherit parent constitution".into(),
                ),
            });
        }

        match mgr.request_spawn(child_id) {
            Ok(record) => {
                let output = serde_json::to_string_pretty(&json!({
                    "spawned": true,
                    "child_id": record.id,
                    "workspace": record.workspace.display().to_string(),
                    "phase": format!("{:?}", record.phase),
                    "active_children": mgr.active_children(),
                }))?;

                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReplicationConfig;
    use crate::soul::constitution::Constitution;

    fn test_manager() -> Arc<Mutex<ReplicationManager>> {
        let config = ReplicationConfig {
            enabled: true,
            max_children: 2,
            child_workspace_dir: "children".into(),
        };
        let mut mgr = ReplicationManager::new(&config, std::path::Path::new("/tmp/zeroclaw"));
        mgr.set_constitution(Constitution::default());
        Arc::new(Mutex::new(mgr))
    }

    fn constitution_hash() -> String {
        Constitution::default().hash().to_string()
    }

    #[test]
    fn tool_metadata() {
        let tool = SoulReplicateTool::new(test_manager());
        assert_eq!(tool.name(), "soul_replicate");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["required"].as_array().unwrap().len() == 2);
    }

    #[tokio::test]
    async fn spawn_child_succeeds() {
        let tool = SoulReplicateTool::new(test_manager());
        let result = tool
            .execute(json!({
                "child_id": "agent_alpha",
                "constitution_hash": constitution_hash()
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("agent_alpha"));
        assert!(result.output.contains("\"spawned\": true"));
    }

    #[tokio::test]
    async fn wrong_constitution_hash_rejected() {
        let tool = SoulReplicateTool::new(test_manager());
        let result = tool
            .execute(json!({
                "child_id": "agent_beta",
                "constitution_hash": "deadbeef"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("constitution hash mismatch"));
    }

    #[tokio::test]
    async fn max_children_enforced() {
        let mgr = test_manager();
        let tool = SoulReplicateTool::new(mgr);
        let hash = constitution_hash();

        tool.execute(json!({"child_id": "c1", "constitution_hash": hash}))
            .await
            .unwrap();
        tool.execute(json!({"child_id": "c2", "constitution_hash": hash}))
            .await
            .unwrap();

        let result = tool
            .execute(json!({"child_id": "c3", "constitution_hash": hash}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("max children reached"));
    }
}
