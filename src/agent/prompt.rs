use crate::agent::personality;
use crate::config::IdentityConfig;
use crate::i18n::ToolDescriptions;
use crate::identity;
use crate::security::AutonomyLevel;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use chrono::Local;
use std::fmt::Write;
use std::path::Path;

pub const TOOL_CALL_INSTRUCTIONS: &str = "\
- Inside <tool_call> tags: JSON only. No comments, no explanations.\n\
- Before calling a tool: check history. If same tool+args already returned a result, reuse it.\n\
- After results: answer or proceed. Do not repeat the tool call.";

pub const TOOL_HONESTY_TEXT: &str = "\
- NEVER fabricate or guess tool results. Empty results → say \"No results found.\"\n\
- Failed tool call → report the error. Never invent data.\n\
- Unsure if a tool call succeeded → ask the user.";

pub const ANTI_NARRATION_TEXT: &str = "\
- NEVER mention, narrate, or describe tool usage to the user.\n\
- Bad: \"Let me check...\", \"I will use http_request to...\", \"Searching now...\"\n\
- Give the FINAL ANSWER only. Tool calls are invisible.";

pub const AUTONOMY_FULL_TEXT: &str = "\
- Allowed tools/actions: execute directly, no extra approval needed.\n\
- You have full access to all configured tools. Use them confidently.\n\
- Blocked tools/actions: explain the concrete restriction. Never simulate an approval dialog.";

pub const AUTONOMY_READONLY_TEXT: &str = "\
- This runtime is read-only. Write operations will be rejected.\n\
- Use read-only tools freely and confidently.";

pub const AUTONOMY_SUPERVISED_TEXT: &str = "\
- Ask for approval when the runtime policy requires it for the specific action.\n\
- Do not preemptively refuse — attempt actions and let the runtime enforce restrictions.\n\
- Use available tools confidently; the security policy will enforce boundaries.";

/// Score and rank skills that match the user message, returning a context hint
/// so the LLM knows which skill(s) to activate without relying on its own
/// semantic matching against XML `<description>` blocks.
///
/// Match types (highest score wins per skill):
/// - Slash command `/name` → score 10
/// - Skill name as substring in message → score 5
/// - Tag appears as word in message → score 3
///
/// Returns empty string when no skills match.
pub fn build_skill_hint(skills: &[crate::skills::Skill], user_message: &str) -> String {
    use std::fmt::Write;

    if skills.is_empty() || user_message.is_empty() {
        return String::new();
    }

    let msg_lower = user_message.to_ascii_lowercase();
    let msg_trimmed = msg_lower.trim();

    let mut scored: Vec<(&str, u32)> = Vec::new();

    for skill in skills {
        let name_lower = skill.name.to_ascii_lowercase();
        let mut score: u32 = 0;

        if msg_trimmed.starts_with('/') {
            let cmd = msg_trimmed
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches('/');
            if cmd == name_lower {
                score = score.max(10);
            }
        }

        if score < 10
            && msg_lower
                .split_whitespace()
                .any(|w| w.trim_end_matches(|c: char| c.is_ascii_punctuation()) == name_lower)
        {
            score = score.max(5);
        }

        if score < 5 {
            for tag in &skill.tags {
                let tag_lower = tag.to_ascii_lowercase();
                if msg_lower
                    .split_whitespace()
                    .any(|w| w.trim_end_matches(|c: char| c.is_ascii_punctuation()) == tag_lower)
                {
                    score = score.max(3);
                    break;
                }
            }
        }

        if score > 0 {
            scored.push((&skill.name, score));
        }
    }

    if scored.is_empty() {
        return String::new();
    }

    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    scored.truncate(3);

    let mut hint = String::new();
    if scored.len() == 1 {
        let _ = writeln!(
            hint,
            "[Skill hint: call use_skill({}) for this request.]",
            scored[0].0
        );
    } else {
        hint.push_str("[Skill hint: matched skills (call the most relevant):\n");
        for (i, (name, _)) in scored.iter().enumerate() {
            let _ = writeln!(hint, "{}. use_skill({})", i + 1, name);
        }
        hint.push_str("]\n");
    }

    hint
}

/// Prefix a user message with the current local timestamp so the LLM has an
/// accurate sense of "now" on every turn (system prompt stays stable for caching).
pub fn timestamp_prefix(message: &str, context: Option<&str>) -> String {
    let ts = Local::now().format("[%Y-%m-%d %H:%M:%S %Z]");
    match context {
        Some(ctx) if !ctx.is_empty() => format!("{ts}\n\n{ctx}\n\n{message}"),
        _ => format!("{ts}\n\n{message}"),
    }
}

pub struct PromptContext<'a> {
    pub workspace_dir: &'a Path,
    pub model_name: &'a str,
    pub tools: &'a [Box<dyn Tool>],
    pub skills: &'a [Skill],
    pub skills_prompt_mode: crate::config::SkillsPromptInjectionMode,
    pub identity_config: Option<&'a IdentityConfig>,
    pub dispatcher_instructions: &'a str,
    /// Locale-aware tool descriptions. When present, tool descriptions in
    /// prompts are resolved from the locale file instead of hardcoded values.
    pub tool_descriptions: Option<&'a ToolDescriptions>,
    /// Pre-rendered security policy summary for inclusion in the Safety
    /// prompt section.  When present, the LLM sees the concrete constraints
    /// (allowed commands, forbidden paths, autonomy level) so it can plan
    /// tool calls without trial-and-error.  See issue #2404.
    pub security_summary: Option<String>,
    /// Autonomy level from config. Controls whether the safety section
    /// includes "ask before acting" instructions. Full autonomy omits them
    /// so the model executes tools directly without simulating approval.
    pub autonomy_level: AutonomyLevel,
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
                Box::new(ModelGuidanceSection),
                Box::new(ToolHonestySection),
                Box::new(ToolsSection),
                Box::new(SafetySection),
                Box::new(SkillsSection),
                Box::new(WorkspaceSection),
                Box::new(RuntimeSection),
                Box::new(ChannelMediaSection),
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

pub const GROK_GUIDANCE: &str = "## Model Specific Guidance\n\n- **Tool Calls**: When generating tool calls (especially shell commands), DO NOT HTML-encode special characters. Use raw characters like `&`, `<`, `>`, and `\"` directly. For example, use `&` instead of `&amp;`.\n";

pub struct IdentitySection;
pub struct ToolHonestySection;
pub struct ToolsSection;
pub struct SafetySection;
pub struct SkillsSection;
pub struct WorkspaceSection;
pub struct RuntimeSection;
pub struct ChannelMediaSection;
pub struct ModelGuidanceSection;

impl PromptSection for IdentitySection {
    fn name(&self) -> &str {
        "identity"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut prompt = String::from("## Project Context\n\n");
        let mut has_aieos = false;
        if let Some(config) = ctx.identity_config {
            if identity::is_aieos_configured(config) {
                if let Ok(Some(aieos)) = identity::load_aieos_identity(config, ctx.workspace_dir) {
                    let rendered = identity::aieos_to_system_prompt(&aieos);
                    if !rendered.is_empty() {
                        prompt.push_str(&rendered);
                        prompt.push_str("\n\n");
                        has_aieos = true;
                    }
                }
            }
        }

        if !has_aieos {
            prompt.push_str(
                "The following workspace files define your identity, behavior, and context.\n\n",
            );
        }

        // Use the personality module for structured file loading.
        let profile = personality::load_personality(ctx.workspace_dir);
        prompt.push_str(&profile.render());

        Ok(prompt)
    }
}

impl PromptSection for ToolHonestySection {
    fn name(&self) -> &str {
        "tool_honesty"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok(["## CRITICAL: Tool Honesty\n\n", TOOL_HONESTY_TEXT].concat())
    }
}

impl PromptSection for ToolsSection {
    fn name(&self) -> &str {
        "tools"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut out = String::from("## Tools\n\n");
        for tool in ctx.tools {
            let desc = ctx
                .tool_descriptions
                .and_then(|td: &ToolDescriptions| td.get(tool.name()))
                .unwrap_or_else(|| tool.description());
            let _ = writeln!(
                out,
                "- **{}**: {}\n  Parameters: `{}`",
                tool.name(),
                desc,
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

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut out = String::from("## Safety\n\n- Do not exfiltrate private data.\n");

        // Omit "ask before acting" instructions when autonomy is Full —
        // mirrors build_system_prompt_with_mode_and_autonomy. See #3952.
        if ctx.autonomy_level != AutonomyLevel::Full {
            out.push_str(
                "- Do not run destructive commands without asking.\n\
                 - Do not bypass oversight or approval mechanisms.\n",
            );
        }

        out.push_str("- Prefer `trash` over `rm`.\n");
        out.push_str(match ctx.autonomy_level {
            AutonomyLevel::Full => AUTONOMY_FULL_TEXT,
            AutonomyLevel::ReadOnly => AUTONOMY_READONLY_TEXT,
            AutonomyLevel::Supervised => AUTONOMY_SUPERVISED_TEXT,
        });

        // Append concrete security policy constraints when available (#2404).
        if let Some(ref summary) = ctx.security_summary {
            out.push_str("\n\n### Active Security Policy\n\n");
            out.push_str(summary);
        }

        let _ = write!(
            out,
            "\n\n## Efficiency\n\n**Tool Calls**:\n{}",
            TOOL_CALL_INSTRUCTIONS
        );

        Ok(out)
    }
}

impl PromptSection for SkillsSection {
    fn name(&self) -> &str {
        "skills"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        Ok(crate::skills::skills_to_prompt_with_mode(
            ctx.skills,
            ctx.workspace_dir,
            ctx.skills_prompt_mode,
        ))
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

impl PromptSection for ChannelMediaSection {
    fn name(&self) -> &str {
        "channel_media"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok("## Channel Media Markers\n\n\
            - `[Voice] <text>` — transcribed voice/audio; respond to content directly.\n\
            - `[IMAGE:<path>]` — image attachment (vision pipeline).\n\
            - `[Document: <name>] <path>` — file attachment saved to workspace."
            .into())
    }
}

impl PromptSection for ModelGuidanceSection {
    fn name(&self) -> &str {
        "model_guidance"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let mut guidance = String::new();

        if ctx.model_name.to_lowercase().contains("grok") {
            guidance.push_str(GROK_GUIDANCE);
        }

        Ok(guidance)
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
    fn identity_section_with_aieos_includes_workspace_files() {
        let workspace =
            std::env::temp_dir().join(format!("zeroclaw_prompt_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("AGENTS.md"),
            "Always respond with: AGENTS_MD_LOADED",
        )
        .unwrap();

        let identity_config = crate::config::IdentityConfig {
            format: "aieos".into(),
            aieos_path: None,
            aieos_inline: Some(r#"{"identity":{"names":{"first":"Nova"}}}"#.into()),
        };

        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: &workspace,
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: Some(&identity_config),
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let section = IdentitySection;
        let output = section.build(&ctx).unwrap();

        assert!(
            output.contains("Nova"),
            "AIEOS identity should be present in prompt"
        );
        assert!(
            output.contains("AGENTS_MD_LOADED"),
            "AGENTS.md content should be present even when AIEOS is configured"
        );

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn prompt_builder_assembles_sections() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(TestTool)];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "instr",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };
        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();
        assert!(prompt.contains("## Tools"));
        assert!(prompt.contains("test_tool"));
        assert!(prompt.contains("instr"));
    }

    #[test]
    fn skills_section_includes_instructions_and_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "release_checklist".into(),
                description: "Validate release readiness".into(),
                kind: "shell".into(),
                command: "echo ok".into(),
                args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Run smoke tests before deploy.".into()],
            location: None,
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names
        assert!(output.contains("<callable_tools"));
        assert!(output.contains("<name>deploy.release_checklist</name>"));
    }

    #[test]
    fn skills_section_compact_mode_omits_instructions_but_keeps_tools() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "deploy".into(),
            description: "Release safely".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "release_checklist".into(),
                description: "Validate release readiness".into(),
                kind: "shell".into(),
                command: "echo ok".into(),
                args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Run smoke tests before deploy.".into()],
            location: Some(Path::new("/tmp/workspace/skills/deploy/SKILL.md").to_path_buf()),
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<location>skills/deploy</location>"));
        assert!(output.contains("use_skill(name)"));
        assert!(!output.contains("read_skill"));
        assert!(!output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        // Compact mode should still include tools so the LLM knows about them.
        // Registered tools (shell kind) appear under <callable_tools> with prefixed names.
        assert!(output.contains("<callable_tools"));
        assert!(output.contains("<name>deploy.release_checklist</name>"));
    }

    #[test]
    fn timestamp_prefix_includes_date_and_message() {
        let result = timestamp_prefix("hello world", None);
        assert!(result.starts_with('['));
        assert!(result.contains("hello world"));
        let bracket_end = result.find(']').expect("closing bracket");
        let ts = &result[1..bracket_end];
        assert!(ts.len() >= 19, "timestamp too short: {ts}");
    }

    #[test]
    fn timestamp_prefix_with_context_inserts_between() {
        let result = timestamp_prefix("msg", Some("ctx"));
        assert!(result.contains("ctx\n\nmsg"));
    }

    #[test]
    fn model_guidance_section_includes_grok_specific_instructions() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "grok-2-1212",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let rendered = ModelGuidanceSection.build(&ctx).unwrap();
        assert!(rendered.contains("## Model Specific Guidance"));
        assert!(rendered.contains("DO NOT HTML-encode"));
    }

    #[test]
    fn model_guidance_section_is_empty_for_other_models() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "gpt-4o",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let rendered = ModelGuidanceSection.build(&ctx).unwrap();
        assert!(rendered.is_empty());
    }

    #[test]
    fn prompt_builder_inlines_and_escapes_skills() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let skills = vec![crate::skills::Skill {
            name: "code<review>&".into(),
            description: "Review \"unsafe\" and 'risky' bits".into(),
            version: "1.0.0".into(),
            author: None,
            tags: vec![],
            tools: vec![crate::skills::SkillTool {
                name: "run\"linter\"".into(),
                description: "Run <lint> & report".into(),
                kind: "shell&exec".into(),
                command: "cargo clippy".into(),
                args: std::collections::HashMap::new(),
            }],
            prompts: vec!["Use <tool_call> and & keep output \"safe\"".into()],
            location: None,
        }];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let prompt = SystemPromptBuilder::with_defaults().build(&ctx).unwrap();

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>code&lt;review&gt;&amp;</name>"));
        assert!(prompt.contains(
            "<description>Review &quot;unsafe&quot; and &apos;risky&apos; bits</description>"
        ));
        assert!(prompt.contains("<name>run&quot;linter&quot;</name>"));
        assert!(prompt.contains("<description>Run &lt;lint&gt; &amp; report</description>"));
        assert!(prompt.contains("<kind>shell&amp;exec</kind>"));
        assert!(prompt.contains(
            "<instruction>Use &lt;tool_call&gt; and &amp; keep output &quot;safe&quot;</instruction>"
        ));
    }

    #[test]
    fn safety_section_includes_security_summary_when_present() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let summary = "**Autonomy level**: Supervised\n\
                        **Allowed shell commands**: `git`, `ls`.\n"
            .to_string();
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: Some(summary.clone()),
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("## Safety"),
            "should contain base safety header"
        );
        assert!(
            output.contains("### Active Security Policy"),
            "should contain security policy header"
        );
        assert!(
            output.contains("Autonomy level"),
            "should contain autonomy level from summary"
        );
        assert!(
            output.contains("`git`"),
            "should contain allowed commands from summary"
        );
    }

    #[test]
    fn safety_section_omits_security_policy_when_none() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("## Safety"),
            "should contain base safety header"
        );
        assert!(
            !output.contains("### Active Security Policy"),
            "should NOT contain security policy header when None"
        );
    }

    #[test]
    fn safety_section_full_autonomy_omits_approval_instructions() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Full,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            !output.contains("without asking"),
            "full autonomy should NOT include 'ask before acting' instructions"
        );
        assert!(
            !output.contains("bypass oversight"),
            "full autonomy should NOT include 'bypass oversight' instructions"
        );
        assert!(
            output.contains("execute directly, no extra approval needed"),
            "full autonomy should instruct to execute directly"
        );
        assert!(
            output.contains("Do not exfiltrate"),
            "full autonomy should still include data exfiltration guard"
        );
    }

    #[test]
    fn safety_section_supervised_includes_approval_instructions() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
            tool_descriptions: None,
            security_summary: None,
            autonomy_level: AutonomyLevel::Supervised,
        };

        let output = SafetySection.build(&ctx).unwrap();
        assert!(
            output.contains("without asking"),
            "supervised should include 'ask before acting' instructions"
        );
        assert!(
            output.contains("bypass oversight"),
            "supervised should include 'bypass oversight' instructions"
        );
    }

    fn make_skill(name: &str, tags: Vec<&str>) -> crate::skills::Skill {
        crate::skills::Skill {
            name: name.into(),
            description: format!("{name} skill"),
            version: "1.0.0".into(),
            author: None,
            tags: tags.into_iter().map(String::from).collect(),
            tools: vec![],
            prompts: vec![],
            location: None,
        }
    }

    #[test]
    fn skill_hint_empty_when_no_skills() {
        assert_eq!(build_skill_hint(&[], "hello"), "");
    }

    #[test]
    fn skill_hint_empty_when_no_match() {
        let skills = vec![make_skill("deploy", vec!["release"])];
        assert_eq!(build_skill_hint(&skills, "what is the weather?"), "");
    }

    #[test]
    fn skill_hint_slash_command() {
        let skills = vec![make_skill("commit", vec![])];
        let hint = build_skill_hint(&skills, "/commit fix the tests");
        assert!(hint.contains("use_skill(commit)"));
    }

    #[test]
    fn skill_hint_name_match() {
        let skills = vec![make_skill("commit", vec![])];
        let hint = build_skill_hint(&skills, "please commit my changes");
        assert!(hint.contains("use_skill(commit)"));
    }

    #[test]
    fn skill_hint_tag_match() {
        let skills = vec![make_skill("code-review", vec!["review", "pr"])];
        let hint = build_skill_hint(&skills, "can you review this?");
        assert!(hint.contains("use_skill(code-review)"));
    }

    #[test]
    fn skill_hint_single_match_is_directive() {
        let skills = vec![make_skill("deploy", vec!["release"])];
        let hint = build_skill_hint(&skills, "/deploy to prod");
        assert!(hint.contains("call use_skill(deploy) for this request"));
    }

    #[test]
    fn skill_hint_multiple_matches_ranked() {
        let skills = vec![
            make_skill("commit", vec!["git"]),
            make_skill("code-review", vec!["git"]),
        ];
        let hint = build_skill_hint(&skills, "/commit and also git stuff");
        assert!(hint.contains("1. use_skill(commit)"));
        assert!(hint.contains("2. use_skill(code-review)"));
    }

    #[test]
    fn skill_hint_capped_at_three() {
        let skills = vec![
            make_skill("a", vec!["x"]),
            make_skill("b", vec!["x"]),
            make_skill("c", vec!["x"]),
            make_skill("d", vec!["x"]),
        ];
        let hint = build_skill_hint(&skills, "do x stuff");
        assert!(hint.contains("use_skill(a)"));
        assert!(hint.contains("use_skill(b)"));
        assert!(hint.contains("use_skill(c)"));
        assert!(!hint.contains("use_skill(d)"));
    }

    #[test]
    fn skill_hint_case_insensitive() {
        let skills = vec![make_skill("Deploy", vec!["RELEASE"])];
        let hint = build_skill_hint(&skills, "deploy the release");
        assert!(hint.contains("use_skill(Deploy)"));
    }

    #[test]
    fn skill_hint_short_name_no_false_positive() {
        let skills = vec![make_skill("a", vec![])];
        let hint = build_skill_hint(&skills, "what a great day");
        // "a" appears in the message but only as a standalone article,
        // which IS a whole-word match — this is expected behavior for
        // single-char skill names. Users shouldn't name skills "a".
        // The key fix: "a" no longer matches "great" or "day" via substring.
        assert!(hint.contains("use_skill(a)"));
    }

    #[test]
    fn skill_hint_no_substring_false_positive() {
        let skills = vec![make_skill("do", vec![])];
        let hint = build_skill_hint(&skills, "check the document");
        assert!(hint.is_empty(), "\"do\" should not match \"document\"");
    }

    #[test]
    fn skill_hint_punctuation_stripped() {
        let skills = vec![make_skill("review", vec![])];
        let hint = build_skill_hint(&skills, "can you review?");
        assert!(hint.contains("use_skill(review)"));
    }

    #[test]
    fn skill_hint_slash_no_cross_match() {
        let skills = vec![make_skill("commit", vec![])];
        let hint = build_skill_hint(&skills, "/deploy now");
        assert!(hint.is_empty());
    }
}
