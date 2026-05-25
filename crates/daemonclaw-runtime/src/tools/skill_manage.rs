use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use daemonclaw_api::tool::{Tool, ToolResult};

use crate::hooks::builtin::background_llm::BackgroundLlmConfig;
use crate::observability::Observer;
use crate::skills::store::SkillStore;
use crate::skills::types::{AgentSkillFrontmatter, AgentSkillMeta, SkillSource};

/// Agent-callable tool for managing skills (create, update, archive, restore, list, review).
///
/// Only operates on agent-category skills. Bundled and imported skills are
/// read-only and cannot be modified through this tool.
pub struct SkillManageTool {
    store: Arc<SkillStore>,
    llm_config: Option<BackgroundLlmConfig>,
    observer: Option<Arc<dyn Observer>>,
    curator_min_grade: u8,
}

impl SkillManageTool {
    pub fn new(store: Arc<SkillStore>) -> Self {
        Self {
            store,
            llm_config: None,
            observer: None,
            curator_min_grade: 2,
        }
    }

    pub fn with_curator(
        mut self,
        llm_config: BackgroundLlmConfig,
        observer: Arc<dyn Observer>,
        min_grade: u8,
    ) -> Self {
        self.llm_config = Some(llm_config);
        self.observer = Some(observer);
        self.curator_min_grade = min_grade;
        self
    }
}

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        "Manage agent skills: create, update, archive, restore, or list. Only agent-created skills can be modified."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "update", "archive", "restore", "list", "list_archived", "review"],
                    "description": "The management action to perform. 'review' runs the automated curator to grade, archive low-quality, and consolidate duplicate skills."
                },
                "name": {
                    "type": "string",
                    "description": "Skill name (required for create, update, archive, restore)."
                },
                "description": {
                    "type": "string",
                    "description": "Skill description (required for create, optional for update)."
                },
                "body": {
                    "type": "string",
                    "description": "Skill markdown body (required for create, optional for update)."
                }
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
            "create" => self.do_create(&args),
            "update" => self.do_update(&args),
            "archive" => self.do_archive(&args),
            "restore" => self.do_restore(&args),
            "list" => self.do_list(),
            "list_archived" => self.do_list_archived(),
            "review" => self.do_review().await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: create, update, archive, restore, list, list_archived, review"
                )),
            }),
        }
    }
}

impl SkillManageTool {
    fn require_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolResult> {
        args.get(key)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Missing required parameter '{key}'")),
            })
    }

    fn do_create(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match Self::require_str(args, "name") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
        let description = match Self::require_str(args, "description") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };
        let body = match Self::require_str(args, "body") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };

        let frontmatter = AgentSkillFrontmatter {
            name: name.to_string(),
            description: description.to_string(),
            license: None,
            metadata: AgentSkillMeta {
                source: SkillSource::Autonomous,
                created: Some(chrono::Utc::now().to_rfc3339()),
                updated: Some(chrono::Utc::now().to_rfc3339()),
                ..AgentSkillMeta::default()
            },
        };

        match self.store.create(&frontmatter, body) {
            Ok(path) => Ok(ToolResult {
                success: true,
                output: format!("Created skill '{name}' at {}", path.display()),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to create skill '{name}': {e}")),
            }),
        }
    }

    fn do_update(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match Self::require_str(args, "name") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };

        let mut skill = match self.store.get_agent(name) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Agent skill '{name}' not found. Only agent-created skills can be updated."
                    )),
                });
            }
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read skill '{name}': {e}")),
                });
            }
        };

        let mut changed = Vec::new();

        if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
            if !desc.trim().is_empty() {
                skill.frontmatter.description = desc.to_string();
                changed.push("description");
            }
        }

        if let Some(body) = args.get("body").and_then(|v| v.as_str()) {
            skill.body = body.to_string();
            changed.push("body");
        }

        if changed.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("No fields to update. Provide 'description' and/or 'body'.".into()),
            });
        }

        skill.meta_mut().version = (skill
            .meta()
            .version
            .parse::<u64>()
            .unwrap_or(1)
            + 1)
        .to_string();
        skill.meta_mut().updated = Some(chrono::Utc::now().to_rfc3339());

        match self.store.write_agent(name, &skill.frontmatter, &skill.body) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Updated skill '{}' (changed: {})",
                    name,
                    changed.join(", ")
                ),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to write skill '{name}': {e}")),
            }),
        }
    }

    fn do_archive(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match Self::require_str(args, "name") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };

        match self.store.archive(name) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Archived skill '{name}'"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to archive skill '{name}': {e}")),
            }),
        }
    }

    fn do_restore(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let name = match Self::require_str(args, "name") {
            Ok(v) => v,
            Err(r) => return Ok(r),
        };

        match self.store.restore(name) {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Restored skill '{name}' from archive"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to restore skill '{name}': {e}")),
            }),
        }
    }

    fn do_list(&self) -> anyhow::Result<ToolResult> {
        let skills = self.store.list_all();
        if skills.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No skills found.".into(),
                error: None,
            });
        }

        let mut output = String::new();
        for skill in &skills {
            output.push_str(&format!(
                "- **{}** [{}] — {}\n",
                skill.name(),
                skill.category,
                skill.description()
            ));
        }
        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn do_review(&self) -> anyhow::Result<ToolResult> {
        let (llm_config, observer) = match (&self.llm_config, &self.observer) {
            (Some(llm), Some(obs)) => (llm, obs),
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Curator review is not configured. Enable skills.curator in config."
                            .into(),
                    ),
                });
            }
        };

        let summary = crate::skills::curator::run_curator(
            &self.store,
            llm_config,
            observer,
            self.curator_min_grade,
        )
        .await;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Curator review complete: {} reviewed, {} archived, {} consolidated",
                summary.skills_reviewed,
                summary.skills_archived,
                summary.skills_consolidated,
            ),
            error: None,
        })
    }

    fn do_list_archived(&self) -> anyhow::Result<ToolResult> {
        let skills = self.store.list_archived();
        if skills.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No archived skills.".into(),
                error: None,
            });
        }

        let mut output = String::new();
        for skill in &skills {
            output.push_str(&format!("- **{}** — {}\n", skill.name(), skill.description()));
        }
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
    use crate::skills::store::SkillStore;
    use tempfile::TempDir;

    fn setup() -> (TempDir, Arc<SkillStore>) {
        let tmp = TempDir::new().unwrap();
        let store = SkillStore::new(tmp.path());
        store.ensure_dirs().unwrap();
        (tmp, Arc::new(store))
    }

    #[tokio::test]
    async fn create_and_list() {
        let (_tmp, store) = setup();
        let tool = SkillManageTool::new(store);

        let result = tool
            .execute(json!({
                "action": "create",
                "name": "deploy-app",
                "description": "Deploy the application",
                "body": "## Steps\n1. Build\n2. Deploy"
            }))
            .await
            .unwrap();
        assert!(result.success, "create failed: {:?}", result.error);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("deploy-app"));
    }

    #[tokio::test]
    async fn update_agent_skill() {
        let (_tmp, store) = setup();
        let tool = SkillManageTool::new(store);

        tool.execute(json!({
            "action": "create",
            "name": "my-skill",
            "description": "Original",
            "body": "Original body"
        }))
        .await
        .unwrap();

        let result = tool
            .execute(json!({
                "action": "update",
                "name": "my-skill",
                "description": "Updated desc",
                "body": "Updated body"
            }))
            .await
            .unwrap();
        assert!(result.success, "update failed: {:?}", result.error);
        assert!(result.output.contains("description"));
        assert!(result.output.contains("body"));
    }

    #[tokio::test]
    async fn archive_and_restore() {
        let (_tmp, store) = setup();
        let tool = SkillManageTool::new(store);

        tool.execute(json!({
            "action": "create",
            "name": "temp-skill",
            "description": "Temporary",
            "body": "temp"
        }))
        .await
        .unwrap();

        let result = tool
            .execute(json!({"action": "archive", "name": "temp-skill"}))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(!result.output.contains("temp-skill"));

        let result = tool
            .execute(json!({"action": "list_archived"}))
            .await
            .unwrap();
        assert!(result.output.contains("temp-skill"));

        let result = tool
            .execute(json!({"action": "restore", "name": "temp-skill"}))
            .await
            .unwrap();
        assert!(result.success);

        let result = tool.execute(json!({"action": "list"})).await.unwrap();
        assert!(result.output.contains("temp-skill"));
    }

    #[tokio::test]
    async fn update_nonexistent_fails() {
        let (_tmp, store) = setup();
        let tool = SkillManageTool::new(store);

        let result = tool
            .execute(json!({
                "action": "update",
                "name": "ghost",
                "body": "new body"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn invalid_action() {
        let (_tmp, store) = setup();
        let tool = SkillManageTool::new(store);

        let result = tool
            .execute(json!({"action": "destroy"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown action"));
    }
}
