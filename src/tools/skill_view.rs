//! skill_view tool — load the full SKILL.md content for a named skill.
//!
//! Part of Progressive Disclosure (L0 → L1). The L0 skill index is injected
//! into the system prompt; when the agent identifies a relevant skill it
//! calls skill_view(name) to fetch the full content.

use super::traits::{Tool, ToolResult};
use crate::skills::procedural::{progressive, SkillStore};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct SkillViewTool {
    store: Arc<SkillStore>,
}

impl SkillViewTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn description(&self) -> &str {
        "Load the full content of a learned procedural skill by name. \
         Use this when the L0 skill index mentions a skill relevant to the current task."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name (kebab-case identifier from the L0 index)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Optional L2 reference file path inside the skill"
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

        let file_path = args.get("file_path").and_then(|v| v.as_str());

        let skill = match self.store.get_by_name(name)? {
            Some(s) => s,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("skill '{}' not found", name)),
                });
            }
        };

        // L2 reference file requested
        if let Some(path) = file_path {
            let refs = self.store.get_references(&skill.id)?;
            if let Some(r) = refs.iter().find(|r| r.file_path == path) {
                return Ok(ToolResult {
                    success: true,
                    output: format!("# {} / {}\n\n{}", skill.name, r.file_path, r.content),
                    error: None,
                });
            }
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "reference '{}' not found for skill '{}'",
                    path, name
                )),
            });
        }

        // L1 full skill
        Ok(ToolResult {
            success: true,
            output: progressive::format_skill_full(&skill),
            error: None,
        })
    }
}
