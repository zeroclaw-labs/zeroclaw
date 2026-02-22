use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

/// Tool for uninstalling ClawHub skills
pub struct ClawhubUninstallTool {
    workspace_dir: PathBuf,
}

impl ClawhubUninstallTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for ClawhubUninstallTool {
    fn name(&self) -> &str {
        "clawhub_uninstall"
    }

    fn description(&self) -> &str {
        "Uninstall a ClawHub skill by slug"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "ClawHub skill slug to uninstall (e.g., 'agent-memory', 'weather-tool')"
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let slug = args["slug"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing slug parameter"))?;

        let skills_path = self.workspace_dir.join("skills");
        let skill_dir = skills_path.join(slug);

        if !skill_dir.exists() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Skill '{}' is not installed. Use 'zeroclaw clawhub list' to see installed skills.",
                    slug
                )),
            });
        }

        // Remove the skill directory
        match std::fs::remove_dir_all(&skill_dir) {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Successfully uninstalled skill '{}'.\n\
                        \n\
                        The skill files have been removed from {}.\n\
                        Use 'zeroclaw clawhub search' to find and reinstall skills.",
                    slug,
                    skill_dir.display()
                ),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to remove skill directory: {}", e)),
            }),
        }
    }
}
