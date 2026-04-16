//! skill_manage tool — create, patch, or delete procedural skills.
//!
//! The agent uses this tool to save complex workflows as reusable skills
//! and to patch them when errors or user corrections occur during use.

use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use crate::skills::procedural::{
    auto_create::{maybe_create_skill, SkillWorthinessVerdict},
    self_improve::{improve_after_execution, ExecutionResult},
    SkillStore,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct SkillManageTool {
    store: Arc<SkillStore>,
    #[allow(dead_code)]
    security: Arc<SecurityPolicy>,
}

impl SkillManageTool {
    pub fn new(store: Arc<SkillStore>, security: Arc<SecurityPolicy>) -> Self {
        Self { store, security }
    }
}

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        "Manage procedural skills — create, patch (update pitfalls/procedure), or delete learned skills. \
         Use 'create' after completing a complex multi-step task worth preserving. \
         Use 'patch_pitfall' when an error occurs using a skill. \
         Use 'patch_procedure' when the user corrects a skill's output."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "patch_pitfall", "patch_procedure", "delete", "record_usage"],
                    "description": "What to do"
                },
                "name": { "type": "string", "description": "Skill name (kebab-case)" },
                "category": { "type": "string", "description": "Category (coding, document, daily, etc.)" },
                "description": { "type": "string", "description": "One-line description" },
                "content_md": { "type": "string", "description": "Full SKILL.md content (for create)" },
                "skill_id": { "type": "string", "description": "Skill ID (for patch/delete)" },
                "patch_content": { "type": "string", "description": "Content to patch in" },
                "succeeded": { "type": "boolean", "description": "Whether usage succeeded (for record_usage)" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        match action {
            "create" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'name' for create"))?;
                let description = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'description' for create"))?;
                let content_md = args
                    .get("content_md")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'content_md' for create"))?;
                let category = args.get("category").and_then(|v| v.as_str());

                let verdict = SkillWorthinessVerdict {
                    worth_saving: true,
                    skill_name: name.to_string(),
                    description: description.to_string(),
                    category: category.map(str::to_string),
                };

                match maybe_create_skill(&self.store, &verdict, content_md)? {
                    Some(r) => Ok(ToolResult {
                        success: true,
                        output: format!("Created skill '{}' (id: {})", r.skill_name, r.skill_id),
                        error: None,
                    }),
                    None => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("skill not saved (worth_saving=false)".into()),
                    }),
                }
            }
            "patch_pitfall" => {
                let skill_id = args
                    .get("skill_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'skill_id' for patch_pitfall"))?;
                let patch_content = args
                    .get("patch_content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'patch_content' for patch_pitfall"))?;

                let result = ExecutionResult {
                    had_errors: true,
                    user_edited_output: false,
                    succeeded: false,
                    error_context: Some(patch_content.to_string()),
                    user_edits: None,
                    skill_id: skill_id.to_string(),
                };
                improve_after_execution(&self.store, &result, Some(patch_content), None)?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Added pitfall to skill {}", skill_id),
                    error: None,
                })
            }
            "patch_procedure" => {
                let skill_id = args
                    .get("skill_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'skill_id' for patch_procedure"))?;
                let patch_content = args
                    .get("patch_content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'patch_content' for patch_procedure"))?;

                let result = ExecutionResult {
                    had_errors: false,
                    user_edited_output: true,
                    succeeded: true,
                    error_context: None,
                    user_edits: Some(patch_content.to_string()),
                    skill_id: skill_id.to_string(),
                };
                improve_after_execution(&self.store, &result, None, Some(patch_content))?;

                Ok(ToolResult {
                    success: true,
                    output: format!("Revised procedure of skill {}", skill_id),
                    error: None,
                })
            }
            "delete" => {
                let skill_id = args
                    .get("skill_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'skill_id' for delete"))?;
                self.store.delete(skill_id)?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Deleted skill {}", skill_id),
                    error: None,
                })
            }
            "record_usage" => {
                let skill_id = args
                    .get("skill_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'skill_id' for record_usage"))?;
                let succeeded = args
                    .get("succeeded")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                self.store.record_usage(skill_id, succeeded)?;
                Ok(ToolResult {
                    success: true,
                    output: format!("Recorded usage for skill {}", skill_id),
                    error: None,
                })
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("unknown action: {}", other)),
            }),
        }
    }
}
