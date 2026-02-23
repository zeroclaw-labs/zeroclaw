use crate::config::{IdentityConfig, SoulConfig};
use crate::identity;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use chrono::Local;
use std::fmt::Write;
use std::path::Path;

const BOOTSTRAP_MAX_CHARS: usize = 20_000;

pub struct PromptContext<'a> {
    pub workspace_dir: &'a Path,
    pub model_name: &'a str,
    pub tools: &'a [Box<dyn Tool>],
    pub skills: &'a [Skill],
    pub identity_config: Option<&'a IdentityConfig>,
    pub soul_config: Option<&'a SoulConfig>,
    pub dispatcher_instructions: &'a str,
}

pub trait PromptSection: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, ctx: &PromptContext<'_>) -> Result<String>;
}

#[derive(Default)]
pub struct SystemPromptBuilder {
    sections: Vec<Box<dyn PromptSection>>,
}

impl SystemPromptBuilder {
    pub fn with_defaults() -> Self {
        Self {
            sections: vec![
                Box::new(IdentitySection),
                Box::new(SoulSection),
                Box::new(ToolsSection),
                Box::new(SafetySection),
                Box::new(SkillsSection),
                Box::new(WorkspaceSection),
                Box::new(DateTimeSection),
                Box::new(RuntimeSection),
            ],
        }
    }

    pub fn add_section(mut self, section: Box<dyn PromptSection>) -> Self {
        self.sections.push(section);
        self
    }

    pub fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut output = String::new();
        for section in &self.sections {
            let part = section.build(ctx)?;
            if part.trim().is_empty() {
                continue;
            }
            output.push_str(part.trim_end());
            output.push_str("\n\n");
        }
        Ok(output)
    }
}

pub struct IdentitySection;
pub struct SoulSection;
pub struct ToolsSection;
pub struct SafetySection;
pub struct SkillsSection;
pub struct WorkspaceSection;
pub struct RuntimeSection;
pub struct DateTimeSection;

impl PromptSection for IdentitySection {
    fn name(&self) -> &str {
        "identity"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut prompt = String::from("## Project Context\n\n");
        if let Some(config) = ctx.identity_config {
            if identity::is_aieos_configured(config) {
                if let Ok(Some(aieos)) = identity::load_aieos_identity(config, ctx.workspace_dir) {
                    let rendered = identity::aieos_to_system_prompt(&aieos);
                    if !rendered.is_empty() {
                        prompt.push_str(&rendered);
                        return Ok(prompt);
                    }
                }
            }
        }

        prompt.push_str(
            "The following workspace files define your identity, behavior, and context.\n\n",
        );
        for file in [
            "AGENTS.md",
            "SOUL.md",
            "TOOLS.md",
            "IDENTITY.md",
            "USER.md",
            "HEARTBEAT.md",
            "BOOTSTRAP.md",
            "MEMORY.md",
        ] {
            inject_workspace_file(&mut prompt, ctx.workspace_dir, file);
        }

        Ok(prompt)
    }
}

impl PromptSection for SoulSection {
    fn name(&self) -> &str {
        "soul"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let config = match ctx.soul_config {
            Some(c) if c.enabled => c,
            _ => return Ok(String::new()),
        };

        let soul_path = if std::path::Path::new(&config.soul_path).is_absolute() {
            std::path::PathBuf::from(&config.soul_path)
        } else {
            ctx.workspace_dir.join(&config.soul_path)
        };

        let soul = match crate::soul::parse_soul_file(&soul_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to load soul file: {e}");
                return Ok(String::new());
            }
        };

        let mut prompt = String::from("## Soul\n\n");

        // Render soul model
        let rendered = soul.to_prompt_section();
        if !rendered.is_empty() {
            prompt.push_str(&rendered);
            prompt.push_str("\n\n");
        }

        // Render constitution
        let constitution = if let Some(ref expected_hash) = config.constitution_hash {
            // Load from soul's boundaries as custom laws, verify hash
            if soul.boundaries.len() >= 3 {
                let laws = [
                    soul.boundaries[0].clone(),
                    soul.boundaries[1].clone(),
                    soul.boundaries[2].clone(),
                ];
                match crate::soul::Constitution::from_parts(laws, expected_hash) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("Constitution integrity check failed: {e}");
                        crate::soul::Constitution::default()
                    }
                }
            } else {
                crate::soul::Constitution::default()
            }
        } else {
            crate::soul::Constitution::default()
        };

        prompt.push_str(&constitution.to_prompt_section());
        prompt.push('\n');

        // Alignment tracking
        if config.enable_alignment_tracking {
            if let Some(ref genesis) = soul.genesis_prompt {
                let current_text = soul.to_prompt_section();
                let score = crate::soul::AlignmentScore::compute(genesis, &current_text);
                let _ = write!(
                    prompt,
                    "\n**Alignment:** jaccard={:.2}, recall={:.2}, combined={:.2}",
                    score.jaccard, score.recall, score.combined
                );
                if !score.is_aligned(0.5) {
                    prompt.push_str(" [WARNING: significant genesis drift detected]");
                }
                prompt.push('\n');
            }
        }

        Ok(prompt)
    }
}

impl PromptSection for ToolsSection {
    fn name(&self) -> &str {
        "tools"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut out = String::from("## Tools\n\n");
        for tool in ctx.tools {
            let _ = writeln!(
                out,
                "- **{}**: {}\n  Parameters: `{}`",
                tool.name(),
                tool.description(),
                tool.parameters_schema()
            );
        }
        if !ctx.dispatcher_instructions.is_empty() {
            out.push('\n');
            out.push_str(ctx.dispatcher_instructions);
        }
        Ok(out)
    }
}

impl PromptSection for SafetySection {
    fn name(&self) -> &str {
        "safety"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok("## Safety\n\n- Do not exfiltrate private data.\n- Do not run destructive commands without asking.\n- Do not bypass oversight or approval mechanisms.\n- Prefer `trash` over `rm`.\n- When in doubt, ask before acting externally.".into())
    }
}

impl PromptSection for SkillsSection {
    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        if ctx.skills.is_empty() {
            return Ok(String::new());
        }

        let mut prompt = String::from("## Available Skills\n\n");
        for skill in ctx.skills {
            let _ = writeln!(prompt, "### {} (v{})", skill.name, skill.version);
            let _ = writeln!(prompt, "{}\n", skill.description);

            // Include full skill instructions so the LLM knows how to use the skill
            for p in &skill.prompts {
                prompt.push_str(p);
                prompt.push('\n');
            }
            prompt.push('\n');
        }
        Ok(prompt)
    }
}

impl PromptSection for WorkspaceSection {
    fn name(&self) -> &str {
        "workspace"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        Ok(format!(
            "## Workspace\n\nWorking directory: `{}`",
            ctx.workspace_dir.display()
        ))
    }
}

impl PromptSection for RuntimeSection {
    fn name(&self) -> &str {
        "runtime"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let host =
            hostname::get().map_or_else(|_| "unknown".into(), |h| h.to_string_lossy().to_string());
        Ok(format!(
            "## Runtime\n\nHost: {host} | OS: {} | Model: {}",
            std::env::consts::OS,
            ctx.model_name
        ))
    }
}

impl PromptSection for DateTimeSection {
    fn name(&self) -> &str {
        "datetime"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        let now = Local::now();
        Ok(format!(
            "## Current Date & Time\n\nTimezone: {}",
            now.format("%Z")
        ))
    }
}

fn inject_workspace_file(prompt: &mut String, workspace_dir: &Path, filename: &str) {
    let path = workspace_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                return;
            }
            let _ = writeln!(prompt, "### {filename}\n");
            let truncated = if trimmed.chars().count() > BOOTSTRAP_MAX_CHARS {
                trimmed
                    .char_indices()
                    .nth(BOOTSTRAP_MAX_CHARS)
                    .map(|(idx, _)| &trimmed[..idx])
                    .unwrap_or(trimmed)
            } else {
                trimmed
            };
            prompt.push_str(truncated);
            if truncated.len() < trimmed.len() {
                let _ = writeln!(
                    prompt,
                    "\n\n[... truncated at {BOOTSTRAP_MAX_CHARS} chars — use `read` for full file]\n"
                );
            } else {
                prompt.push_str("\n\n");
            }
        }
        Err(_) => {
            let _ = writeln!(prompt, "### {filename}\n\n[File not found: {filename}]\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::traits::Tool;
    use async_trait::async_trait;

    struct TestTool;

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            "test_tool"
        }

        fn description(&self) -> &str {
            "tool desc"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn prompt_builder_assembles_sections() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(TestTool)];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            identity_config: None,
            soul_config: None,
            dispatcher_instructions: "instr",
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("test_tool"));
        assert!(prompt.contains("instr"));
    }
}
