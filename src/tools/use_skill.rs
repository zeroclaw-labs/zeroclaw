use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;

/// Format the activation output following the Agent Skills standard's
/// structured wrapping recommendation: include skill directory, resolved
/// instructions, path resolution note, and bundled resource listing.
fn format_activation_output(
    skill_name: &str,
    skill_dir: &std::path::Path,
    body: &str,
    paths_were_resolved: bool,
) -> String {
    use std::fmt::Write;

    let mut out = format!(
        "[Skill '{}' activated. Skill directory: {}]\n",
        skill_name,
        skill_dir.display()
    );

    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }

    let resources = crate::skills::list_skill_resources(skill_dir);
    if !resources.is_empty() {
        let _ = write!(out, "\n\n<skill_resources>");
        for r in &resources {
            let _ = write!(out, "\n  <file>{}</file>", r);
        }
        let _ = write!(out, "\n</skill_resources>");
    }

    if paths_were_resolved {
        let _ = write!(
            out,
            "\n\nScript paths above have been resolved to absolute paths. \
             Use them directly in shell commands."
        );
    }

    out
}

/// First-class tool for invoking a skill by name.
///
/// Unlike `ReadSkillTool` (which only reads raw file content in compact mode),
/// `UseSkillTool` acts as an explicit invocation signal — the LLM calls this to
/// commit to following a skill's instructions. Works in both Full and Compact
/// prompt injection modes.
pub struct UseSkillTool {
    workspace_dir: PathBuf,
    open_skills_enabled: bool,
    open_skills_dir: Option<String>,
    mode: crate::config::SkillsPromptInjectionMode,
}

impl UseSkillTool {
    pub fn new(
        workspace_dir: PathBuf,
        open_skills_enabled: bool,
        open_skills_dir: Option<String>,
        mode: crate::config::SkillsPromptInjectionMode,
    ) -> Self {
        Self {
            workspace_dir,
            open_skills_enabled,
            open_skills_dir,
            mode,
        }
    }
}

#[async_trait]
impl Tool for UseSkillTool {
    fn name(&self) -> &str {
        "use_skill"
    }

    fn description(&self) -> &str {
        "Invoke a skill by name. Use when: the user's request matches a skill \
         description in <available_skills>, the user mentions a skill by name, \
         or the user types a slash command (e.g. /commit). Don't use when: no \
         available skill matches the user's intent."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name exactly as listed in <available_skills>. Leading '/' is stripped automatically."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let raw_name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' parameter"))?;

        // Strip leading '/' so `/commit` resolves to `commit`.
        let requested = raw_name.strip_prefix('/').unwrap_or(raw_name);

        let skills = crate::skills::load_skills_with_open_skills_settings(
            &self.workspace_dir,
            self.open_skills_enabled,
            self.open_skills_dir.as_deref(),
        );

        let Some(skill) = skills
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(requested))
        else {
            let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
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

        let skill_dir = crate::skills::resolve_skill_dir(skill, &self.workspace_dir);

        let output = match self.mode {
            crate::config::SkillsPromptInjectionMode::Full => {
                // In Full mode the instructions are already in the system prompt.
                // Return them here as an activation signal so the LLM has them
                // front-of-mind in the tool result.
                let (body, resolved) = if skill.prompts.is_empty() {
                    (String::new(), false)
                } else {
                    let joined = skill.prompts.join("\n\n");
                    let out = crate::skills::resolve_relative_paths(&joined, &skill_dir);
                    let changed = out != joined;
                    (out, changed)
                };
                format_activation_output(&skill.name, &skill_dir, &body, resolved)
            }
            crate::config::SkillsPromptInjectionMode::Compact => {
                // In Compact mode, load the full skill file and strip frontmatter.
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

                let raw = tokio::fs::read_to_string(location).await.map_err(|err| {
                    anyhow::anyhow!(
                        "Failed to read skill '{}' from {}: {err}",
                        skill.name,
                        location.display()
                    )
                })?;

                let body = if location.extension().is_some_and(|e| e == "md") {
                    crate::skills::split_skill_frontmatter(&raw)
                        .map(|(_fm, body)| body)
                        .unwrap_or(raw)
                } else {
                    raw
                };
                let resolved = crate::skills::resolve_relative_paths(&body, &skill_dir);
                let changed = resolved != body;
                format_activation_output(&skill.name, &skill_dir, &resolved, changed)
            }
        };

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
    use tempfile::TempDir;

    fn make_tool(tmp: &TempDir, mode: crate::config::SkillsPromptInjectionMode) -> UseSkillTool {
        UseSkillTool::new(tmp.path().join("workspace"), false, None, mode)
    }

    #[tokio::test]
    async fn invokes_skill_full_mode() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/deploy");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\ndescription: Ship code\n---\n\n# Deploy\n\nRun the deploy pipeline.\n",
        )
        .unwrap();

        let tool = make_tool(&tmp, crate::config::SkillsPromptInjectionMode::Full);
        let result = tool.execute(json!({ "name": "deploy" })).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("[Skill 'deploy' activated"));
        assert!(result.output.contains("Skill directory:"));
        assert!(result.output.contains("skills/deploy"));
        assert!(result.output.contains("# Deploy"));
        assert!(result.output.contains("Run the deploy pipeline."));
    }

    #[tokio::test]
    async fn invokes_skill_compact_mode_strips_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/lint");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: lint\ndescription: Lint code\nversion: 1.0.0\n---\n\n# Lint\n\nRun the linter.\n",
        )
        .unwrap();

        let tool = make_tool(&tmp, crate::config::SkillsPromptInjectionMode::Compact);
        let result = tool.execute(json!({ "name": "lint" })).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("[Skill 'lint' activated"));
        assert!(result.output.contains("Skill directory:"));
        assert!(result.output.contains("skills/lint"));
        assert!(result.output.contains("# Lint"));
        assert!(result.output.contains("Run the linter."));
        // Frontmatter must not leak into output.
        assert!(!result.output.contains("---"));
        assert!(!result.output.contains("version: 1.0.0"));
    }

    #[tokio::test]
    async fn strips_leading_slash() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/commit");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Commit\n\nMake a commit.\n").unwrap();

        let tool = make_tool(&tmp, crate::config::SkillsPromptInjectionMode::Full);
        let result = tool.execute(json!({ "name": "/commit" })).await.unwrap();

        assert!(result.success);
        assert!(result.output.contains("[Skill 'commit' activated"));
        assert!(result.output.contains("Skill directory:"));
    }

    #[tokio::test]
    async fn output_includes_resolved_paths_and_resources() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/myskill");
        let scripts_dir = skill_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        std::fs::write(scripts_dir.join("cli.py"), "# script").unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: myskill\ndescription: test\n---\n\nRun `uv run scripts/cli.py search`\n",
        )
        .unwrap();

        let tool = make_tool(&tmp, crate::config::SkillsPromptInjectionMode::Full);
        let result = tool.execute(json!({ "name": "myskill" })).await.unwrap();

        assert!(result.success);
        // Path should be resolved to absolute
        let abs_path = scripts_dir.join("cli.py").display().to_string();
        assert!(
            result.output.contains(&abs_path),
            "Expected absolute path in output, got: {}",
            result.output
        );
        // Resource listing should be present
        assert!(result.output.contains("<skill_resources>"));
        assert!(result.output.contains("scripts/cli.py"));
        // Path resolution note
        assert!(result.output.contains("resolved to absolute paths"));
    }

    #[tokio::test]
    async fn unknown_skill_lists_available() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("workspace/skills/weather");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Weather\n").unwrap();

        let tool = make_tool(&tmp, crate::config::SkillsPromptInjectionMode::Full);
        let result = tool.execute(json!({ "name": "calendar" })).await.unwrap();

        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("Unknown skill 'calendar'. Available skills: weather")
        );
    }
}
