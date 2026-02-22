use crate::clawhub::client::ClawHubClient;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

/// Tool for installing ClawHub skills
pub struct ClawhubInstallTool {
    workspace_dir: PathBuf,
}

impl ClawhubInstallTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        Self { workspace_dir }
    }
}

#[async_trait]
impl Tool for ClawhubInstallTool {
    fn name(&self) -> &str {
        "clawhub_install"
    }

    fn description(&self) -> &str {
        "Install a skill from ClawHub by slug"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "slug": {
                    "type": "string",
                    "description": "ClawHub skill slug to install"
                },
                "version": {
                    "type": "string",
                    "description": "Specific version to install (optional, default: latest)"
                }
            },
            "required": ["slug"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let slug = args["slug"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing slug parameter"))?;

        let client = ClawHubClient::default();

        match client.get_skill(slug).await {
            Ok(skill) => {
                // TODO: Full implementation - download, audit, install
                Ok(ToolResult {
                    success: true,
                    output: format!(
                        "Skill '{}' ({}) v{} would be installed.\n\
                        Description: {}\n\
                        Author: {}",
                        skill.name,
                        skill.slug,
                        skill.version,
                        skill.description,
                        skill.author
                    ),
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to get skill: {}", e)),
            }),
        }
    }
}
