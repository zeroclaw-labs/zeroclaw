use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;

/// Tool for autonomously creating and managing skills.
pub struct SkillDeveloperTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl SkillDeveloperTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    fn autonomous_skills_dir(&self) -> PathBuf {
        self.workspace_dir.join("skills").join("autonomous")
    }

    async fn create_skill_dir(&self, name: &str) -> anyhow::Result<String> {
        let path = self.autonomous_skills_dir().join(name);
        fs::create_dir_all(&path).await?;
        fs::create_dir_all(path.join("scripts")).await?;
        fs::create_dir_all(path.join("resources")).await?;
        Ok(format!("Created skill directory structure at {}", path.display()))
    }

    async fn write_skill_file(&self, skill_name: &str, filename: &str, content: &str) -> anyhow::Result<String> {
        // Path traversal protection
        if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
            anyhow::bail!("Invalid filename: {}", filename);
        }

        let path = self.autonomous_skills_dir().join(skill_name).join(filename);
        
        // Ensure parent exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&path, content).await?;
        Ok(format!("Wrote file to {}", path.display()))
    }

    async fn audit_skill(&self, name: &str) -> anyhow::Result<String> {
        let path = self.autonomous_skills_dir().join(name);
        if !path.exists() {
            anyhow::bail!("Skill directory not found: {}", path.display());
        }

        let report = crate::skills::audit_skill_directory(&path)?;
        if report.is_clean() {
            Ok(format!("✓ Skill audit passed for {} ({} files scanned).", name, report.files_scanned))
        } else {
            Ok(format!("✗ Skill audit failed for {}: {}", name, report.summary()))
        }
    }
}

#[async_trait]
impl Tool for SkillDeveloperTool {
    fn name(&self) -> &str {
        "skill_developer"
    }

    fn description(&self) -> &str {
        "Develop and manage autonomous agent skills. Actions: create_skill_dir, write_skill_file, audit_skill."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create_skill_dir", "write_skill_file", "audit_skill"],
                    "description": "The development action to perform"
                },
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill (lowercase-kebab-case)"
                },
                "filename": {
                    "type": "string",
                    "description": "Filename (e.g., SKILL.md, scripts/my_script.py)"
                },
                "content": {
                    "type": "string",
                    "description": "Content of the file"
                }
            },
            "required": ["action", "skill_name"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let skill_name = args.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");

        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        match action {
            "create_skill_dir" => {
                let output = self.create_skill_dir(skill_name).await?;
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            "write_skill_file" => {
                let filename = args.get("filename").and_then(|v| v.as_str()).unwrap_or("SKILL.md");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let output = self.write_skill_file(skill_name, filename, content).await?;
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            "audit_skill" => {
                let output = self.audit_skill(skill_name).await?;
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {}", action)),
            }),
        }
    }
}
