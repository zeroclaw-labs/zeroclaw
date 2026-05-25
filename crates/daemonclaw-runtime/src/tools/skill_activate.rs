use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use daemonclaw_api::tool::{Tool, ToolResult};

use crate::skills::store::SkillStore;

/// Agent-callable tool for activating a skill from the SkillStore.
///
/// Progressive disclosure: the system prompt contains only the skill catalog
/// (name + description). The agent calls `skill_activate` to load the full
/// SKILL.md body into context when it decides a skill is relevant.
pub struct SkillActivateTool {
    store: Arc<SkillStore>,
}

impl SkillActivateTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SkillActivateTool {
    fn name(&self) -> &str {
        "skill_activate"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a skill by name. Use this when you decide a cataloged skill is relevant to the current task. Returns the complete SKILL.md body."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name exactly as listed in the skill catalog."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

        match self.store.get(name) {
            Ok(Some(skill)) => {
                let mut output = String::new();
                output.push_str(&format!("# Skill: {}\n\n", skill.name()));
                output.push_str(&format!(
                    "**Category**: {} | **Source**: {} | **Version**: {}\n\n",
                    skill.category,
                    skill.meta().source,
                    skill.meta().version,
                ));
                output.push_str("---\n\n");
                output.push_str(&skill.body);
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Ok(None) => {
                let all = self.store.list_all();
                let mut names: Vec<&str> = all.iter().map(|s| s.name()).collect();
                names.sort_unstable();
                let available = if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                };
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown skill '{name}'. Available skills: {available}"
                    )),
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to load skill '{name}': {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::store::SkillStore;
    use crate::skills::types::{AgentSkillFrontmatter, AgentSkillMeta, SkillSource};
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<SkillStore>) {
        let tmp = TempDir::new().unwrap();
        let store = SkillStore::new(tmp.path());
        store.ensure_dirs().unwrap();
        (tmp, Arc::new(store))
    }

    fn seed_skill(store: &SkillStore, name: &str, body: &str) {
        let fm = AgentSkillFrontmatter {
            name: name.to_string(),
            description: format!("Test skill {name}"),
            license: None,
            metadata: AgentSkillMeta {
                source: SkillSource::Manual,
                ..AgentSkillMeta::default()
            },
        };
        store.create(&fm, body).unwrap();
    }

    #[tokio::test]
    async fn activate_existing_skill() {
        let (_tmp, store) = setup();
        seed_skill(&store, "deploy-nginx", "## Procedure\n1. Edit config\n2. Reload");

        let tool = SkillActivateTool::new(store);
        let result = tool.execute(json!({"name": "deploy-nginx"})).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("# Skill: deploy-nginx"));
        assert!(result.output.contains("## Procedure"));
    }

    #[tokio::test]
    async fn activate_missing_skill_lists_available() {
        let (_tmp, store) = setup();
        seed_skill(&store, "deploy-nginx", "body");

        let tool = SkillActivateTool::new(store);
        let result = tool.execute(json!({"name": "unknown"})).await.unwrap();

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("Unknown skill 'unknown'"));
        assert!(err.contains("deploy-nginx"));
    }

    #[tokio::test]
    async fn activate_missing_name_param() {
        let (_tmp, store) = setup();
        let tool = SkillActivateTool::new(store);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }
}
