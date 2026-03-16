//! Built-in `read_skill` tool for on-demand skill file loading.
//!
//! In Compact mode the system prompt only includes skill names and locations,
//! telling the LLM to load full instructions on demand.  This tool provides a
//! reliable single-call mechanism: `read_skill(name: "weather")` returns the
//! full SKILL.md (or SKILL.toml) content without requiring the LLM to compose
//! shell commands or remember file paths.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;

use crate::tools::traits::{Tool, ToolResult};

/// Maps skill names to their on-disk file paths.
///
/// Built once from the already-loaded skill list and injected at tool
/// construction time.
#[derive(Debug, Clone)]
pub struct SkillIndex {
    entries: HashMap<String, PathBuf>,
}

impl SkillIndex {
    /// Build an index from loaded skills, resolving each skill's location.
    pub fn from_skills(skills: &[crate::skills::Skill], workspace_dir: &std::path::Path) -> Self {
        let mut entries = HashMap::with_capacity(skills.len());
        for skill in skills {
            let location = skill.location.clone().unwrap_or_else(|| {
                workspace_dir
                    .join("skills")
                    .join(&skill.name)
                    .join("SKILL.md")
            });
            entries.insert(skill.name.clone(), location);
        }
        Self { entries }
    }
}

/// Read-only tool that returns the full content of a skill file by name.
pub struct ReadSkillTool {
    index: SkillIndex,
}

impl ReadSkillTool {
    pub fn new(index: SkillIndex) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Read the full content of a skill file by name. Use this in Compact mode to load skill instructions on demand."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name to look up (as shown in the <name> tag of available_skills)"
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

        let path = match self.index.entries.get(name) {
            Some(p) => p,
            None => {
                let available: Vec<&str> = self.index.entries.keys().map(String::as_str).collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Skill '{}' not found. Available skills: {}",
                        name,
                        if available.is_empty() {
                            "(none)".to_string()
                        } else {
                            available.join(", ")
                        }
                    )),
                });
            }
        };

        match tokio::fs::read_to_string(path).await {
            Ok(content) => Ok(ToolResult {
                success: true,
                output: content,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to read skill file '{}': {}",
                    path.display(),
                    e
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Skill;

    fn make_skill(name: &str, location: Option<PathBuf>) -> Skill {
        Skill {
            name: name.to_string(),
            description: format!("{name} skill"),
            version: "1.0.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec![],
            location,
        }
    }

    #[test]
    fn tool_metadata() {
        let index = SkillIndex {
            entries: HashMap::new(),
        };
        let tool = ReadSkillTool::new(index);
        assert_eq!(tool.name(), "read_skill");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["name"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("name")));
    }

    #[test]
    fn skill_index_from_skills_uses_location() {
        let skills = vec![make_skill(
            "weather",
            Some(PathBuf::from("/workspace/skills/weather/SKILL.md")),
        )];
        let index = SkillIndex::from_skills(&skills, std::path::Path::new("/workspace"));
        assert_eq!(
            index.entries.get("weather").unwrap().to_str().unwrap(),
            "/workspace/skills/weather/SKILL.md"
        );
    }

    #[test]
    fn skill_index_from_skills_falls_back_to_default_path() {
        let skills = vec![make_skill("deploy", None)];
        let index = SkillIndex::from_skills(&skills, std::path::Path::new("/workspace"));
        assert_eq!(
            index.entries.get("deploy").unwrap().to_str().unwrap(),
            "/workspace/skills/deploy/SKILL.md"
        );
    }

    #[tokio::test]
    async fn missing_name_param_returns_error() {
        let index = SkillIndex {
            entries: HashMap::new(),
        };
        let tool = ReadSkillTool::new(index);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn unknown_skill_returns_not_found() {
        let mut entries = HashMap::new();
        entries.insert(
            "weather".to_string(),
            PathBuf::from("/tmp/skills/weather/SKILL.md"),
        );
        let index = SkillIndex { entries };
        let tool = ReadSkillTool::new(index);

        let result = tool.execute(json!({"name": "nonexistent"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not found"));
        assert!(result.error.as_ref().unwrap().contains("weather"));
    }

    #[tokio::test]
    async fn reads_existing_skill_file() {
        let dir = std::env::temp_dir().join("zeroclaw_test_read_skill");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(dir.join("skills/weather"))
            .await
            .unwrap();
        tokio::fs::write(
            dir.join("skills/weather/SKILL.md"),
            "# Weather\nFetch the weather forecast.\n",
        )
        .await
        .unwrap();

        let skills = vec![make_skill(
            "weather",
            Some(dir.join("skills/weather/SKILL.md")),
        )];
        let index = SkillIndex::from_skills(&skills, &dir);
        let tool = ReadSkillTool::new(index);

        let result = tool.execute(json!({"name": "weather"})).await.unwrap();
        assert!(result.success);
        assert!(result.output.contains("# Weather"));
        assert!(result.output.contains("Fetch the weather forecast."));
        assert!(result.error.is_none());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn file_read_failure_returns_error() {
        let mut entries = HashMap::new();
        entries.insert(
            "broken".to_string(),
            PathBuf::from("/tmp/zeroclaw_nonexistent_path/SKILL.md"),
        );
        let index = SkillIndex { entries };
        let tool = ReadSkillTool::new(index);

        let result = tool.execute(json!({"name": "broken"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("Failed to read"));
    }

    #[tokio::test]
    async fn empty_index_lists_no_skills() {
        let index = SkillIndex {
            entries: HashMap::new(),
        };
        let tool = ReadSkillTool::new(index);

        let result = tool.execute(json!({"name": "anything"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("(none)"));
    }

    #[test]
    fn skill_index_handles_multiple_skills() {
        let skills = vec![
            make_skill(
                "weather",
                Some(PathBuf::from("/workspace/skills/weather/SKILL.md")),
            ),
            make_skill("deploy", None),
            make_skill("backup", Some(PathBuf::from("/other/path/SKILL.toml"))),
        ];
        let index = SkillIndex::from_skills(&skills, std::path::Path::new("/workspace"));
        assert_eq!(index.entries.len(), 3);
        assert!(index.entries.contains_key("weather"));
        assert!(index.entries.contains_key("deploy"));
        assert!(index.entries.contains_key("backup"));
    }
}
