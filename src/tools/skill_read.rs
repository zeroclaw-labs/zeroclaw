use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

/// On-demand skill loader — reads full skill instructions by name.
pub struct SkillReadTool {
    workspace_dir: PathBuf,
}

impl SkillReadTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for SkillReadTool {
    fn name(&self) -> &str {
        "skill_read"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a skill by name. Use this before executing a skill's workflow."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name (e.g. 'exa-ai-search', 'get-crypto-price'). Use the skill names from the skill catalog in the system prompt."
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

        match crate::skills::read_skill_content(name, &self.workspace_dir) {
            Some(content) => Ok(ToolResult {
                success: true,
                output: content,
                error: None,
            }),
            None => {
                let available = crate::skills::list_skill_names(&self.workspace_dir);
                let hint = if available.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\nAvailable skills: {}",
                        available.join(", ")
                    )
                };
                Ok(ToolResult {
                    success: false,
                    output: format!("Skill '{name}' not found.{hint}"),
                    error: Some(format!("Skill '{name}' not found")),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn reads_workspace_skill() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Test Skill\nDo the thing.",
        )
        .unwrap();

        let tool = SkillReadTool::new(tmp.path().to_path_buf());
        let result = tool
            .execute(json!({"name": "test-skill"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("Do the thing"));
    }

    #[tokio::test]
    async fn returns_error_for_missing_skill() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("skills")).unwrap();

        let tool = SkillReadTool::new(tmp.path().to_path_buf());
        let result = tool
            .execute(json!({"name": "nonexistent"}))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.output.contains("not found"));
    }
}
