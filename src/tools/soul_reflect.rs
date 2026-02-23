//! Soul reflect tool — triggers soul reflection to update capabilities,
//! relationships, and personality based on conversation history.

use super::traits::{Tool, ToolResult};
use crate::soul::model::SoulModel;
use crate::soul::reflection::{apply_insights, write_soul_file, ReflectionInsights};
use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Tool that updates the agent's soul model with new insights.
///
/// Accepts explicit capabilities, relationships, personality, and financial
/// character updates via parameters. The agent calls this tool when it
/// discovers new things about itself through conversation.
pub struct SoulReflectTool {
    soul_path: PathBuf,
    soul: Arc<Mutex<SoulModel>>,
}

impl SoulReflectTool {
    pub fn new(soul_path: PathBuf, soul: Arc<Mutex<SoulModel>>) -> Self {
        Self { soul_path, soul }
    }
}

#[async_trait]
impl Tool for SoulReflectTool {
    fn name(&self) -> &str {
        "soul_reflect"
    }

    fn description(&self) -> &str {
        "Updates the agent's soul identity with new insights discovered during conversation. \
         Pass arrays of new capabilities, and objects of new relationships, personality traits, \
         or financial character traits. Changes are persisted to SOUL.md."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "capabilities": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "New capabilities discovered (e.g. ['web_scraping', 'data_analysis'])"
                },
                "relationships": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "New or updated relationships (e.g. {'collaborator': 'agent_b'})"
                },
                "personality": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Updated personality traits (e.g. {'patience': 'high'})"
                },
                "financial_character": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Updated financial traits (e.g. {'risk_tolerance': 'moderate'})"
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let capabilities: Vec<String> = args
            .get("capabilities")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let relationships: HashMap<String, String> = args
            .get("relationships")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let personality: HashMap<String, String> = args
            .get("personality")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let financial: HashMap<String, String> = args
            .get("financial_character")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let insights = ReflectionInsights {
            new_capabilities: capabilities,
            new_relationships: relationships,
            personality_updates: personality,
            financial_updates: financial,
        };

        if insights.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No insights provided — soul unchanged.".into(),
                error: None,
            });
        }

        // Apply to in-memory soul model
        {
            let mut soul = self.soul.lock();
            apply_insights(&mut soul, &insights);

            // Persist to disk
            write_soul_file(&self.soul_path, &soul)?;
        }

        let output = serde_json::to_string_pretty(&json!({
            "updated": true,
            "insights_applied": insights.count(),
            "soul_path": self.soul_path.to_string_lossy(),
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
    use crate::soul::parser::parse_soul_file;

    fn test_soul() -> Arc<Mutex<SoulModel>> {
        Arc::new(Mutex::new(SoulModel {
            name: "TestAgent".into(),
            capabilities: vec!["coding".into()],
            ..Default::default()
        }))
    }

    #[test]
    fn tool_metadata() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SoulReflectTool::new(tmp.path().join("SOUL.md"), test_soul());
        assert_eq!(tool.name(), "soul_reflect");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["type"] == "object");
    }

    #[tokio::test]
    async fn reflect_adds_capabilities() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul_path = tmp.path().join("SOUL.md");
        let soul = test_soul();
        let tool = SoulReflectTool::new(soul_path.clone(), soul.clone());

        let result = tool
            .execute(json!({
                "capabilities": ["research", "devops"]
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("\"updated\": true"));

        let locked = soul.lock();
        assert_eq!(locked.capabilities.len(), 3);
        assert!(locked.capabilities.contains(&"research".into()));

        // Verify persisted to disk
        let loaded = parse_soul_file(&soul_path).unwrap();
        assert!(loaded.capabilities.contains(&"devops".into()));
    }

    #[tokio::test]
    async fn reflect_empty_insights_noop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tool = SoulReflectTool::new(tmp.path().join("SOUL.md"), test_soul());

        let result = tool.execute(json!({})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("soul unchanged"));
    }

    #[tokio::test]
    async fn reflect_updates_personality() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul = test_soul();
        let tool = SoulReflectTool::new(tmp.path().join("SOUL.md"), soul.clone());

        let result = tool
            .execute(json!({
                "personality": { "patience": "high" }
            }))
            .await
            .unwrap();

        assert!(result.success);
        let locked = soul.lock();
        assert_eq!(locked.personality.get("patience").unwrap(), "high");
    }

    #[tokio::test]
    async fn reflect_updates_financial_character() {
        let tmp = tempfile::TempDir::new().unwrap();
        let soul = test_soul();
        let tool = SoulReflectTool::new(tmp.path().join("SOUL.md"), soul.clone());

        let result = tool
            .execute(json!({
                "financial_character": { "risk_tolerance": "moderate" }
            }))
            .await
            .unwrap();

        assert!(result.success);
        let locked = soul.lock();
        assert_eq!(
            locked.financial_character.get("risk_tolerance").unwrap(),
            "moderate"
        );
    }
}
