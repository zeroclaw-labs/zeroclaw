use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use zeroclaw_api::tool::{Tool, ToolResult};

/// Compact-mode helper for loading a skill's source file on demand.
///
/// Honors the same `skills.enabled` master switch and `skills.disabled`
/// blocklist as the system-prompt loader, so a skill hidden from the prompt
/// catalog cannot be re-fetched here. Without this enforcement, compact
/// mode would let `read_skill("disabled-name")` succeed even after the user
/// disabled the skill.
pub struct ReadSkillTool {
    workspace_dir: PathBuf,
    open_skills_enabled: bool,
    open_skills_dir: Option<String>,
    skills_enabled: bool,
    skills_disabled: Vec<String>,
}

impl ReadSkillTool {
    pub fn new(
        workspace_dir: PathBuf,
        open_skills_enabled: bool,
        open_skills_dir: Option<String>,
        skills_enabled: bool,
        skills_disabled: Vec<String>,
    ) -> Self {
        Self {
            workspace_dir,
            open_skills_enabled,
            open_skills_dir,
            skills_enabled,
            skills_disabled,
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
        // Master kill switch — refuse to read any skill body when the loader
        // is disabled. Mirrors the short-circuit in load_skills_with_config.
        if !self.skills_enabled {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Skills are disabled (skills.enabled = false). Set skills.enabled = true in config to use read_skill."
                        .to_string(),
                ),
            });
        }

        let requested = args
            .get("name")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

        // Reject explicitly-disabled names with a clear error before touching
        // the filesystem. Case-insensitive match mirrors the lookup below.
        if self
            .skills_disabled
            .iter()
            .any(|d| d.eq_ignore_ascii_case(requested))
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Skill '{requested}' is in skills.disabled. Remove it from the blocklist to use this skill."
                )),
            });
        }

        let skills = crate::skills::load_skills_with_open_skills_settings(
            &self.workspace_dir,
            self.open_skills_enabled,
            self.open_skills_dir.as_deref(),
        );

        // Belt-and-suspenders: filter the loaded list by `skills.disabled` so
        // any skill that loads through a path the early-reject didn't catch
        // (e.g. a name normalisation difference between request and manifest)
        // still gets blocked here.
        let blocklist: HashSet<String> = self
            .skills_disabled
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();

        let skills: Vec<_> = skills
            .into_iter()
            .filter(|skill| !blocklist.contains(&skill.name.to_ascii_lowercase()))
            .collect();

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
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir) -> ReadSkillTool {
        ReadSkillTool::new(tmp.path().join("workspace"), false, None, true, Vec::new())
    }

    fn make_tool_with_disabled(tmp: &TempDir, disabled: Vec<String>) -> ReadSkillTool {
        ReadSkillTool::new(tmp.path().join("workspace"), false, None, true, disabled)
    }

    fn make_tool_globally_disabled(tmp: &TempDir) -> ReadSkillTool {
        ReadSkillTool::new(tmp.path().join("workspace"), false, None, false, Vec::new())
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
    async fn skills_globally_disabled_returns_error() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/weather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Weather\n").unwrap();

        let result = make_tool_globally_disabled(&tmp)
            .execute(json!({ "name": "weather" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("Skills are disabled"),
            "expected disabled error, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn explicitly_disabled_skill_returns_error() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/chatty");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Chatty\n").unwrap();

        let tool = make_tool_with_disabled(&tmp, vec!["chatty".to_string()]);

        let result = tool.execute(json!({ "name": "chatty" })).await.unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(
            err.contains("skills.disabled"),
            "expected blocklist error, got: {err}"
        );

        // Case-insensitive match: requesting 'CHATTY' must also be blocked.
        let result = tool.execute(json!({ "name": "CHATTY" })).await.unwrap();
        assert!(!result.success);
        assert!(
            result.error.as_deref().unwrap().contains("skills.disabled"),
            "case-insensitive disabled match failed"
        );
    }

    #[tokio::test]
    async fn disabled_filter_hides_skill_from_listing_on_unknown_lookup() {
        let tmp = TempDir::new().unwrap();
        for name in &["weather", "blocked"] {
            let dir = tmp.path().join("workspace/skills").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("# {name}\n")).unwrap();
        }
        let tool = make_tool_with_disabled(&tmp, vec!["blocked".to_string()]);

        let result = tool
            .execute(json!({ "name": "nonexistent" }))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(
            err.contains("weather"),
            "expected weather in available list, got: {err}"
        );
        assert!(
            !err.contains("blocked"),
            "disabled skill should not appear in available list, got: {err}"
        );
    }
}
