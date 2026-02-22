use crate::clawhub::client::ClawHubClient;
use crate::clawhub::downloader::SkillDownloader;
use crate::skills::audit;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

/// Tool for installing ClawHub skills
pub struct ClawhubInstallTool {
    workspace_dir: PathBuf,
    config_dir: PathBuf,
}

impl ClawhubInstallTool {
    pub fn new(workspace_dir: PathBuf) -> Self {
        let config_dir = workspace_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            workspace_dir,
            config_dir,
        }
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
                    "description": "ClawHub skill slug to install (e.g., 'agent-memory', 'weather-tool')"
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
                let readme_url = match &skill.readme_url {
                    Some(url) => url,
                    None => {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Skill has no SKILL.md - cannot install".to_string()),
                        });
                    }
                };

                let skills_path = self.workspace_dir.join("skills");
                std::fs::create_dir_all(&skills_path)?;

                let skill_dir = skills_path.join(slug);

                // Check if already installed
                if skill_dir.exists() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Skill '{}' is already installed. Use 'zeroclaw clawhub update' to update.",
                            slug
                        )),
                    });
                }

                // Download to temp location first for audit
                let temp_dir = std::env::temp_dir().join(format!("clawhub_install_{}", slug));
                let _ = std::fs::remove_dir_all(&temp_dir);
                std::fs::create_dir_all(&temp_dir)?;

                // Download SKILL.md
                let downloader = SkillDownloader::new();
                match downloader.download_file(readme_url).await {
                    Ok(content) => {
                        let skill_md = temp_dir.join("SKILL.md");
                        if let Err(e) = std::fs::write(&skill_md, &content) {
                            let _ = std::fs::remove_dir_all(&temp_dir);
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some(format!("Failed to write skill file: {}", e)),
                            });
                        }

                        // Run security audit
                        match audit::audit_skill_directory(&temp_dir) {
                            Ok(report) => {
                                if !report.is_clean() {
                                    let _ = std::fs::remove_dir_all(&temp_dir);
                                    return Ok(ToolResult {
                                        success: false,
                                        output: String::new(),
                                        error: Some(format!(
                                            "Security audit failed: {}",
                                            report.summary()
                                        )),
                                    });
                                }

                                // Copy to final location
                                if let Err(e) = std::fs::create_dir_all(&skill_dir) {
                                    let _ = std::fs::remove_dir_all(&temp_dir);
                                    return Ok(ToolResult {
                                        success: false,
                                        output: String::new(),
                                        error: Some(format!(
                                            "Failed to create skill directory: {}",
                                            e
                                        )),
                                    });
                                }
                                if let Err(e) = std::fs::copy(&skill_md, skill_dir.join("SKILL.md"))
                                {
                                    let _ = std::fs::remove_dir_all(&temp_dir);
                                    return Ok(ToolResult {
                                        success: false,
                                        output: String::new(),
                                        error: Some(format!("Failed to copy skill file: {}", e)),
                                    });
                                }
                            }
                            Err(e) => {
                                let _ = std::fs::remove_dir_all(&temp_dir);
                                return Ok(ToolResult {
                                    success: false,
                                    output: String::new(),
                                    error: Some(format!("Security audit error: {}", e)),
                                });
                            }
                        }

                        // Clean up temp
                        let _ = std::fs::remove_dir_all(&temp_dir);

                        Ok(ToolResult {
                            success: true,
                            output: format!(
                                "Successfully installed '{}' v{}\n\
                                Description: {}\n\
                                Author: {}\n\
                                \n\
                                The skill is now available. Use 'zeroclaw clawhub list' to see all installed skills.",
                                skill.name,
                                skill.version,
                                skill.description,
                                skill.author
                            ),
                            error: None,
                        })
                    }
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&temp_dir);
                        Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Failed to download skill: {}", e)),
                        })
                    }
                }
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to get skill: {}", e)),
            }),
        }
    }
}
