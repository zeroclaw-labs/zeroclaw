use crate::clawhub::client::ClawHubClient;
use crate::clawhub::downloader::SkillDownloader;
use crate::config::ClawHubConfig;
use crate::skills::audit;
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use std::path::PathBuf;

/// Tool for installing ClawHub skills
pub struct ClawhubInstallTool {
    workspace_dir: PathBuf,
    config_dir: PathBuf,
    clawhub_config: Option<ClawHubConfig>,
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
            clawhub_config: None,
        }
    }

    pub fn with_config(mut self, config: ClawHubConfig) -> Self {
        self.clawhub_config = Some(config);
        self
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
                if skill.readme_url.is_none() && skill.readme_url_master.is_none() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Skill has no SKILL.md - cannot install".to_string()),
                    });
                }

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

                // Build fallback URL if configured
                let fallback_url = self
                    .clawhub_config
                    .as_ref()
                    .and_then(|c| c.download_fallback.as_ref())
                    .map(|pattern| pattern.replace("{slug}", slug));

                // Download SKILL.md with fallback support
                let downloader = SkillDownloader::new();
                let readme_url_master = skill.readme_url_master.as_deref();
                match downloader
                    .download_skill_with_zip_fallback(
                        skill.readme_url.as_deref(),
                        readme_url_master,
                        fallback_url.as_deref(),
                        &temp_dir,
                    )
                    .await
                {
                    Ok(()) => {
                        // File downloaded to temp_dir, verify it exists
                        let skill_md = temp_dir.join("SKILL.md");
                        if !skill_md.exists() {
                            let _ = std::fs::remove_dir_all(&temp_dir);
                            return Ok(ToolResult {
                                success: false,
                                output: String::new(),
                                error: Some("Downloaded skill file not found".to_string()),
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
                            error: Some(format!(
                                "Failed to download SKILL.md: {}\n\n\
                                 This skill may be hosted on ClawHub's backend instead of GitHub.",
                                e
                            )),
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
