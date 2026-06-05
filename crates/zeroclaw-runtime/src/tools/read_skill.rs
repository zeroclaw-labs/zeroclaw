use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::schema::Config;

/// Compact-mode helper for loading a skill's source file on demand.
/// Supports workspace skills, open-skills, agent-bound skill bundles, and plugin skills.
pub struct ReadSkillTool {
    config: Arc<Config>,
    workspace_dir: PathBuf,
    agent_alias: String,
}

impl ReadSkillTool {
    pub fn new(config: Arc<Config>, workspace_dir: PathBuf, agent_alias: String) -> Self {
        Self {
            config,
            workspace_dir,
            agent_alias,
        }
    }
}

#[async_trait]
impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Read the full source file for an available skill by name. Use this in compact skills mode when you need the complete skill instructions without remembering file paths."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name exactly as listed in <available_skills>."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let requested = args
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "name"})),
                    "tool argument validation failed"
                );

                anyhow::Error::msg("Missing 'name' parameter")
            })?;

        // Load all skills for this agent (workspace + open-skills + bundles + plugins)
        let skills = crate::skills::load_skills_for_agent(
            &self.workspace_dir,
            &self.config,
            &self.agent_alias,
        );

        let Some(skill) = skills
            .iter()
            .find(|skill| skill.name.eq_ignore_ascii_case(requested))
        else {
            let mut names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
            names.sort_unstable();
            let available = if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            };

            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown skill '{requested}'. Available skills: {available}"
                )),
            });
        };

        let Some(location) = skill.location.as_ref() else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Skill '{}' has no readable source location.",
                    skill.name
                )),
            });
        };

        match tokio::fs::read_to_string(location).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(err) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to read skill '{}' from {}: {err}",
                    skill.name,
                    location.display()
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use zeroclaw_config::schema::Config;

    fn make_tool(tmp: &TempDir) -> ReadSkillTool {
        let mut config = Config::default();
        config.skills.open_skills_enabled = false;
        config.skills.allow_scripts = false;
        ReadSkillTool::new(
            Arc::new(config),
            tmp.path().join("workspace"),
            "default".to_string(),
        )
    }

    #[tokio::test]
    async fn reads_markdown_skill_by_name() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/weather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Weather\n\nUse this skill for forecast lookups.\n",
        )
        .unwrap();

        let result = make_tool(&tmp)
            .execute(json!({ "name": "weather" }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("# Weather"));
        assert!(result.output.contains("forecast lookups"));
    }

    #[tokio::test]
    async fn reads_toml_skill_manifest_by_name() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/deploy");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"[skill]
name = "deploy"
description = "Ship safely"
"#,
        )
        .unwrap();

        let result = make_tool(&tmp)
            .execute(json!({ "name": "deploy" }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("[skill]"));
        assert!(result.output.contains("Ship safely"));
    }

    #[tokio::test]
    async fn unknown_skill_lists_available_names() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/weather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Weather\n").unwrap();

        let result = make_tool(&tmp)
            .execute(json!({ "name": "calendar" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("Unknown skill 'calendar'. Available skills: weather")
        );
    }

    #[tokio::test]
    async fn script_skill_is_returned_when_allow_scripts_true() {
        // Regression pin for #5697: a skill directory containing a script
        // file (.sh) must be returned by read_skill when the tool was
        // constructed with allow_scripts=true. Prior to the fix,
        // ReadSkillTool forwarded a hardcoded None to
        // load_skills_with_open_skills_settings, which unwrap_or(false)
        // resolved to false, silently blocking the skill.
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/setup");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Setup\n\nRuns ./configure and logs.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("configure.sh"), "#!/bin/sh\necho ok\n").unwrap();

        // Construct with allow_scripts=true. Pre-fix this resolved to false
        // inside the loader and the skill was skipped.
        let mut config = Config::default();
        config.skills.allow_scripts = true;
        let tool = ReadSkillTool::new(
            Arc::new(config),
            tmp.path().join("workspace"),
            "default".to_string(),
        );
        let result = tool.execute(json!({ "name": "setup" })).await.unwrap();

        assert!(
            result.success,
            "script-bearing skill must be returned when allow_scripts=true; got error={:?}",
            result.error
        );
        assert!(result.output.contains("# Setup"));
    }

    #[tokio::test]
    async fn reads_skill_from_agent_bundle() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::{AliasedAgentConfig, SkillBundleConfig};

        let tmp = TempDir::new().unwrap();

        // Setup config with skill bundle and agent
        let mut config = Config::default();
        config.skill_bundles.insert(
            "default".to_string(),
            SkillBundleConfig {
                directory: Some(tmp.path().join("bundles/default").display().to_string()),
                ..Default::default()
            },
        );
        // Ensure the "default" agent exists
        config
            .agents
            .entry("default".to_string())
            .or_insert_with(|| AliasedAgentConfig {
                skill_bundles: vec!["default".to_string()],
                ..Default::default()
            });

        // Create bundle skill
        let bundle_dir = tmp.path().join("bundles/default/my-bundle-skill");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(
            bundle_dir.join("SKILL.md"),
            "# Bundle Skill\n\nThis skill comes from a bundle.\n",
        )
        .unwrap();

        let tool = ReadSkillTool::new(
            Arc::new(config),
            tmp.path().join("workspace"),
            "default".to_string(),
        );

        let result = tool
            .execute(json!({ "name": "my-bundle-skill" }))
            .await
            .unwrap();

        assert!(
            result.success,
            "bundle skill should be found. error={:?}",
            result.error
        );
        assert!(result.output.contains("# Bundle Skill"));
    }

    #[tokio::test]
    async fn workspace_skill_overrides_bundle_skill() {
        use tempfile::TempDir;
        use zeroclaw_config::schema::{AliasedAgentConfig, SkillBundleConfig};

        let tmp = TempDir::new().unwrap();

        // Setup config with skill bundle and agent
        let mut config = Config::default();
        config.skill_bundles.insert(
            "default".to_string(),
            SkillBundleConfig {
                directory: Some(tmp.path().join("bundles/default").display().to_string()),
                ..Default::default()
            },
        );
        config
            .agents
            .entry("default".to_string())
            .or_insert_with(|| AliasedAgentConfig {
                skill_bundles: vec!["default".to_string()],
                ..Default::default()
            });

        // Create workspace skill
        let workspace_skill_dir = tmp.path().join("workspace/skills/weather");
        std::fs::create_dir_all(&workspace_skill_dir).unwrap();
        std::fs::write(
            workspace_skill_dir.join("SKILL.md"),
            "# Weather\n\nWorkspace version.\n",
        )
        .unwrap();

        // Create bundle skill with same name
        let bundle_dir = tmp.path().join("bundles/default/weather");
        std::fs::create_dir_all(&bundle_dir).unwrap();
        std::fs::write(
            bundle_dir.join("SKILL.md"),
            "# Weather\n\nBundle version.\n",
        )
        .unwrap();

        let tool = ReadSkillTool::new(
            Arc::new(config),
            tmp.path().join("workspace"),
            "default".to_string(),
        );

        let result = tool.execute(json!({ "name": "weather" })).await.unwrap();

        assert!(result.success);
        // Workspace skill takes precedence
        assert!(result.output.contains("Workspace version"));
        assert!(!result.output.contains("Bundle version"));
    }
}
