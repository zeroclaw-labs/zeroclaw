use crate::config::IdentityConfig;
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
    pub skills_prompt_mode: crate::config::SkillsPromptInjectionMode,
    pub identity_config: Option<&'a IdentityConfig>,
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
                Box::new(ToolsSection),
                Box::new(OntologySection),
                Box::new(SafetySection),
                Box::new(SchedulingSection),
                Box::new(SkillsSection),
                Box::new(WorkspaceSection),
                Box::new(DateTimeSection),
                Box::new(RuntimeSection),
                Box::new(ChannelMediaSection),
                Box::new(ToolUsageStrategySection),
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
pub struct ToolsSection;
pub struct SafetySection;
pub struct SchedulingSection;
pub struct SkillsSection;
pub struct WorkspaceSection;
pub struct RuntimeSection;
pub struct DateTimeSection;
pub struct OntologySection;
pub struct ChannelMediaSection;
pub struct ToolUsageStrategySection;

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
        for file in [
            "AGENTS.md",
            "SOUL.md",
            "TOOLS.md",
            "IDENTITY.md",
            "USER.md",
            "HEARTBEAT.md",
            "BOOTSTRAP.md",
        ] {
            inject_workspace_file(&mut prompt, ctx.workspace_dir, file);
        }
        let memory_path = ctx.workspace_dir.join("MEMORY.md");
        if memory_path.exists() {
            inject_workspace_file(&mut prompt, ctx.workspace_dir, "MEMORY.md");
        }

        let extra_files = ctx
            .identity_config
            .map_or(&[][..], |cfg| cfg.extra_files.as_slice());
        for file in extra_files {
            if let Some(safe_relative) = normalize_openclaw_identity_extra_file(file) {
                inject_workspace_file(&mut prompt, ctx.workspace_dir, safe_relative);
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

impl PromptSection for SchedulingSection {
    fn name(&self) -> &str {
        "scheduling"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let has_cron = ctx.tools.iter().any(|t| t.name() == "cron_add");
        if !has_cron {
            return Ok(String::new());
        }

        let has_browser = ctx.tools.iter().any(|t| t.name() == "browser");

        let mut out = String::from(
            "## Scheduling\n\n\
             For periodic/recurring tasks, use `cron_add` with `job_type: \"agent\"`.\n\
             Schedule formats: `{\"kind\":\"cron\",\"expr\":\"0 9 * * *\"}`, \
             `{\"kind\":\"every\",\"every_ms\":3600000}`, `{\"kind\":\"at\",\"at\":\"...\"}`\n\
             Optional: `name`, `delivery` (`{\"mode\":\"announce\",\"channel\":\"telegram\",\"to\":\"...\"}`), \
             `session_target`, `model`.\n",
        );

        if has_browser {
            out.push_str("Agent jobs can use `browser` for web scraping.\n");
        }

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

impl PromptSection for DateTimeSection {
    fn name(&self) -> &str {
        "datetime"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        let now = Local::now();
        Ok(format!(
            "## Current Date & Time\n\n{} ({})",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.format("%Z")
        ))
    }
}

impl PromptSection for OntologySection {
    fn name(&self) -> &str {
        "ontology"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        let has_ontology = ctx.tools.iter().any(|t| t.name() == "ontology_get_context");
        if !has_ontology {
            return Ok(String::new());
        }

        // Load user preferences from ontology to inject into the prompt.
        let mut out = String::from(
            "## Ontology\n\n\
             A structured knowledge graph models the user's world as Objects, Links, and Actions.\n\
             Types: User, Contact, Device, Channel, Task, Project, Document, Meeting, Context, Preference\n\n\
             Tools: `ontology_get_context` (world state), `ontology_search_objects` (find), \
             `ontology_execute_action` (act — auto-logs + updates graph).\n\
             Preferences persist across sessions; check before decisions.\n",
        );

        // Attempt to load preferences from workspace ontology DB.
        let db_path = ctx.workspace_dir.join("memory").join("brain.db");
        if db_path.exists() {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                let _ = conn.execute_batch("PRAGMA busy_timeout = 5000;");
                // Derive owner_user_id from identity config (same logic as tools/mod.rs).
                let owner_user_id = ctx
                    .identity_config
                    .and_then(|ic| ic.aieos_inline.as_deref())
                    .and_then(|json_str| {
                        serde_json::from_str::<serde_json::Value>(json_str)
                            .ok()
                            .and_then(|v| {
                                v.pointer("/identity/names/first")
                                    .or_else(|| v.pointer("/identity/name"))
                                    .and_then(|n| n.as_str().map(|s| s.to_string()))
                            })
                    })
                    .unwrap_or_else(|| "default_user".to_string());

                let mut prefs_text = String::new();
                let result: Result<Vec<(String, String)>, _> = (|| {
                    let mut stmt = conn.prepare_cached(
                        "SELECT o.title, o.properties FROM ontology_objects o
                         JOIN ontology_object_types t ON o.type_id = t.id
                         WHERE t.name = 'Preference' AND o.owner_user_id = ?1
                         ORDER BY o.updated_at DESC LIMIT 20"
                    )?;
                    let rows = stmt.query_map(rusqlite::params![owner_user_id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                        ))
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()
                })();

                if let Ok(prefs) = result {
                    if !prefs.is_empty() {
                        prefs_text.push_str("\n### Active Preferences\n");
                        for (title, props) in &prefs {
                            let value = serde_json::from_str::<serde_json::Value>(props)
                                .ok()
                                .and_then(|v| v.get("value").cloned())
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| props.clone());
                            let _ = writeln!(prefs_text, "- **{}**: {}", title, value);
                        }
                    }
                }
                if !prefs_text.is_empty() {
                    out.push_str(&prefs_text);
                }
            }
        }

        Ok(out)
    }
}

impl PromptSection for ChannelMediaSection {
    fn name(&self) -> &str {
        "channel_media"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Ok("## Channel Media Markers\n\n\
            Messages from channels may contain media markers:\n\
            - `[Voice] <text>` — The user sent a voice/audio message that has already been transcribed to text. Respond to the transcribed content directly.\n\
            - `[IMAGE:<path>]` — An image attachment, processed by the vision pipeline.\n\
            - `[Document: <name>] <path>` — A file attachment saved to the workspace."
            .into())
    }
}

impl PromptSection for ToolUsageStrategySection {
    fn name(&self) -> &str {
        "tool_usage_strategy"
    }

    fn build(&self, ctx: &PromptContext<'_>) -> Result<String> {
        if ctx.tools.is_empty() {
            return Ok(String::new());
        }

        // Classify tools into free and paid categories based on known paid providers.
        let paid_tool_info: &[(&str, &str, &str)] = &[
            ("brave", "Brave Search", "https://brave.com/search/api/"),
            ("firecrawl", "Firecrawl", "https://firecrawl.dev/"),
            ("tavily", "Tavily", "https://tavily.com/"),
            ("perplexity", "Perplexity", "https://www.perplexity.ai/"),
            ("exa", "Exa", "https://exa.ai/"),
            ("jina", "Jina AI", "https://jina.ai/"),
            ("composio", "Composio", "https://composio.dev/"),
        ];

        let tool_names: Vec<&str> = ctx.tools.iter().map(|t| t.name()).collect();

        let mut out = String::from(
            "## Tool Usage Strategy\n\n\
             ### Autonomous Execution Protocol\n\n\
             When the user makes a request, follow this workflow:\n\n\
             1. **Analyze** — Understand the request and determine what information or actions are needed.\n\
             2. **Plan** — Break the task into concrete steps. Identify which tools are needed for each step.\n\
             3. **Execute** — Use the appropriate tools to carry out each step. Chain tool calls as needed.\n\
             4. **Verify** — Check the results for correctness and completeness.\n\
             5. **Respond** — Present the final result clearly and concisely.\n\n\
             Key principles:\n\
             - Act autonomously. Do not ask the user for permission to use available tools — just use them.\n\
             - Prefer parallel execution when steps are independent.\n\
             - Be cost-efficient: use the minimum number of tool calls needed for accurate results.\n\
             - If a tool call fails, try an alternative approach before reporting failure.\n\n",
        );

        // Free-tool-first guidance
        out.push_str(
            "### Free-First Tool Selection\n\n\
             Always prefer free built-in tools over paid alternatives:\n\n\
             **Free tools (no API key required):**\n\
             - `web_search` (DuckDuckGo provider) — default web search, always available\n\
             - `web_fetch` (nanohtml2text provider) — fetch and extract web page content\n\
             - `http_request` — direct HTTP calls (GET/POST/PUT/DELETE)\n\
             - `browser` — full browser automation for complex web interactions\n\
             - `shell` — execute system commands\n\
             - `file_read`, `file_write`, `file_edit`, `apply_patch` — local file operations\n\
             - `glob_search`, `content_search` — file and content search\n\
             - `git_operations` — Git repository operations\n\
             - `memory_store`, `memory_recall`, `memory_observe` — persistent memory\n\
             - `pdf_read`, `docx_read`, `xlsx_read`, `pptx_read` — document reading\n\
             - `screenshot`, `image_info` — screen capture and image analysis\n\
             - All scheduling, configuration, and process management tools\n\n\
             Use these tools first. They can handle the vast majority of user requests.\n\n",
        );

        // Paid tool guidance — only include if relevant tools exist
        let has_web_search = tool_names.iter().any(|n| *n == "web_search");
        let has_composio = tool_names.iter().any(|n| *n == "composio");

        if has_web_search || has_composio {
            out.push_str(
                "### Paid Tool Guidance\n\n\
                 Some advanced features require external API keys. \
                 Only suggest these when free tools genuinely cannot fulfill the request \
                 (e.g., the user explicitly needs a specific paid service).\n\n\
                 **Paid providers (require API key setup):**\n",
            );

            for (key, label, url) in paid_tool_info {
                // Only list paid tools whose base tool exists
                let relevant = match *key {
                    "composio" => has_composio,
                    _ => has_web_search,
                };
                if relevant {
                    let _ = writeln!(out, "- **{label}** (`{key}`) — Sign up: {url}");
                }
            }

            out.push_str(
                "\nWhen a paid tool is needed:\n\
                 1. Explain to the user why a paid tool would produce better results.\n\
                 2. Name the specific provider and its purpose.\n\
                 3. Provide the signup URL so the user can obtain an API key.\n\
                 4. Guide the user to enter the API key in MoA Settings → Provider API Keys.\n\
                 5. After the key is configured, proceed with the task automatically.\n\n\
                 Never block on a paid tool — always attempt free alternatives first.\n",
            );
        }

        // ── Proactive follow-up & next-step suggestions ──
        out.push_str(
            "\n### Proactive Follow-Up Protocol\n\n\
             After completing any task or answering a question, ALWAYS suggest concrete next steps.\n\
             Do NOT just give the answer and stop. Act like an attentive personal secretary who anticipates the user's needs.\n\n\
             **After using a free tool (e.g., DuckDuckGo web search):**\n\
             1. Present the results clearly.\n\
             2. Then ask: \"검색 결과가 충분하지 않으시다면, Perplexity AI 검색이나 Brave Search 등 \
                더 정확한 도구로 다시 검색해 드릴까요?\" (adapt language to the user's language).\n\
             3. If the user agrees, guide them to set up the API key if not configured, \
                or use the paid tool directly if the key is already available.\n\n\
             **After answering any question (with or without tools):**\n\
             Suggest 2-3 specific, relevant follow-up actions the user might want. Examples:\n\
             - After a weather answer: \"내일 일정을 고려해서 우산이 필요한지 알려드릴까요?\" or \"이번 주 날씨 전체를 확인해 드릴까요?\"\n\
             - After a search: \"관련 내용을 더 자세히 조사해 드릴까요?\" or \"이 내용을 메모에 저장해 드릴까요?\"\n\
             - After a code task: \"테스트를 실행해 볼까요?\" or \"관련 문서를 업데이트할까요?\"\n\
             - After a document summary: \"핵심 내용을 메모리에 저장할까요?\" or \"관련 자료를 더 찾아볼까요?\"\n\n\
             The follow-up suggestions must be:\n\
             - Concrete and specific (not vague like \"뭐든 물어보세요\")\n\
             - Relevant to the current context and the user's likely next need\n\
             - Phrased as actionable questions the user can simply say \"yes\" to\n\
             - Written in the same language the user is using\n\n\
             ### User Pattern Recognition & Adaptive Suggestions\n\n\
             You MUST actively learn and remember the user's behavioral patterns from conversation history and stored memory.\n\
             This is not optional — it is a core part of being a good personal secretary.\n\n\
             **What to observe and remember (use memory_store to persist):**\n\
             - Frequently asked topics (e.g., weather, news, stock prices, schedules)\n\
             - Common request sequences (e.g., user always checks weather → then asks about schedule → then asks for news)\n\
             - Preferred tools and sources (e.g., user prefers Perplexity over DuckDuckGo for research)\n\
             - Time-based habits (e.g., morning = news + weather, evening = schedule review)\n\
             - Follow-up patterns (e.g., after asking about a restaurant, user always asks for directions)\n\n\
             **How to use patterns:**\n\
             When you recognize a request that matches a known pattern, proactively offer the next step \
             in the user's usual sequence. Examples:\n\
             - User asks about weather (and historically always asks for schedule next):\n\
               → After the weather answer, say: \"지난번처럼 오늘 일정도 함께 확인해 드릴까요?\"\n\
             - User asks to search a topic (and usually asks to save results):\n\
               → After the search, say: \"이전처럼 검색 결과를 메모에 저장해 드릴까요?\"\n\
             - User asks about a stock (and usually checks 2-3 related stocks):\n\
               → After the answer, say: \"지난번에 함께 확인하셨던 [관련 종목]도 확인해 드릴까요?\"\n\n\
             **Pattern storage rules:**\n\
             - After noticing a user repeats the same sequence 2+ times, store it as a pattern \
               using memory_store (key: `user_pattern_<category>`, e.g., `user_pattern_morning_routine`).\n\
             - When suggesting based on a pattern, phrase it naturally: \"지난번처럼...\", \"평소처럼...\", \
               \"항상 하시던 대로...\" — do NOT say \"제가 패턴을 분석한 결과...\".\n\
             - If the user declines a pattern-based suggestion, respect it. If declined 2+ times \
               for the same pattern, stop suggesting that specific follow-up.\n\
             - Adapt to evolving patterns — if the user's routine changes, update memory accordingly.\n",
        );

        Ok(out)
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

fn normalize_openclaw_identity_extra_file(raw: &str) -> Option<&str> {
    use std::path::{Component, Path};

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return None;
    }

    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(trimmed)
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
            extra_files: Vec::new(),
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
    fn identity_section_openclaw_injects_extra_files() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_prompt_extra_files_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(workspace.join("memory")).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "agent baseline").unwrap();
        std::fs::write(workspace.join("SOUL.md"), "soul baseline").unwrap();
        std::fs::write(workspace.join("TOOLS.md"), "tools baseline").unwrap();
        std::fs::write(workspace.join("IDENTITY.md"), "identity baseline").unwrap();
        std::fs::write(workspace.join("USER.md"), "user baseline").unwrap();
        std::fs::write(workspace.join("FRAMEWORK.md"), "framework context").unwrap();
        std::fs::write(workspace.join("memory").join("notes.md"), "memory notes").unwrap();

        let identity_config = crate::config::IdentityConfig {
            format: "openclaw".into(),
            extra_files: vec!["FRAMEWORK.md".into(), "memory/notes.md".into()],
            aieos_path: None,
            aieos_inline: None,
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
        };

        let section = IdentitySection;
        let output = section.build(&ctx).unwrap();

        assert!(output.contains("### FRAMEWORK.md"));
        assert!(output.contains("framework context"));
        assert!(output.contains("### memory/notes.md"));
        assert!(output.contains("memory notes"));

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn identity_section_openclaw_rejects_unsafe_extra_files() {
        let workspace = std::env::temp_dir().join(format!(
            "zeroclaw_prompt_extra_files_unsafe_test_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "agent baseline").unwrap();
        std::fs::write(workspace.join("SOUL.md"), "soul baseline").unwrap();
        std::fs::write(workspace.join("TOOLS.md"), "tools baseline").unwrap();
        std::fs::write(workspace.join("IDENTITY.md"), "identity baseline").unwrap();
        std::fs::write(workspace.join("USER.md"), "user baseline").unwrap();
        std::fs::write(workspace.join("SAFE.md"), "safe context").unwrap();

        let identity_config = crate::config::IdentityConfig {
            format: "openclaw".into(),
            extra_files: vec![
                "SAFE.md".into(),
                "../outside.md".into(),
                "/tmp/absolute.md".into(),
            ],
            aieos_path: None,
            aieos_inline: None,
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
        };

        let section = IdentitySection;
        let output = section.build(&ctx).unwrap();

        assert!(output.contains("### SAFE.md"));
        assert!(!output.contains("outside.md"));
        assert!(!output.contains("absolute.md"));

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
            always: false,
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        assert!(output.contains("<name>release_checklist</name>"));
        assert!(output.contains("<kind>shell</kind>"));
    }

    #[test]
    fn skills_section_compact_mode_omits_instructions_and_tools() {
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
            always: false,
        }];

        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Compact,
            identity_config: None,
            dispatcher_instructions: "",
        };

        let output = SkillsSection.build(&ctx).unwrap();
        assert!(output.contains("<available_skills>"));
        assert!(output.contains("<name>deploy</name>"));
        assert!(output.contains("<location>skills/deploy/SKILL.md</location>"));
        assert!(!output.contains("<instruction>Run smoke tests before deploy.</instruction>"));
        assert!(!output.contains("<tools>"));
    }

    #[test]
    fn datetime_section_includes_timestamp_and_timezone() {
        let tools: Vec<Box<dyn Tool>> = vec![];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp"),
            model_name: "test-model",
            tools: &tools,
            skills: &[],
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "instr",
        };

        let rendered = DateTimeSection.build(&ctx).unwrap();
        assert!(rendered.starts_with("## Current Date & Time\n\n"));

        let payload = rendered.trim_start_matches("## Current Date & Time\n\n");
        assert!(payload.chars().any(|c| c.is_ascii_digit()));
        assert!(payload.contains(" ("));
        assert!(payload.ends_with(')'));
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
            always: false,
        }];
        let ctx = PromptContext {
            workspace_dir: Path::new("/tmp/workspace"),
            model_name: "test-model",
            tools: &tools,
            skills: &skills,
            skills_prompt_mode: crate::config::SkillsPromptInjectionMode::Full,
            identity_config: None,
            dispatcher_instructions: "",
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
}
