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

        // ── Tiered web search strategy: snippet-first, deep-scrape only when needed ──
        let has_web_search = tool_names.iter().any(|n| *n == "web_search");
        if has_web_search {
            out.push_str(
                "### Tiered Web Search Strategy (Snippet-First)\n\n\
                 **CRITICAL: Do NOT automatically call `web_fetch` or `browser` after `web_search`.**\n\
                 Most simple questions (weather, facts, definitions, current events) can be answered \
                 directly from DuckDuckGo search result snippets without any additional fetching.\n\n\
                 **Tier 1 — Snippet-Based Fast Answer (default):**\n\
                 1. Call `web_search` with the query.\n\
                 2. Read the returned titles, URLs, and snippets.\n\
                 3. If the snippets contain enough information to answer the question → \
                    **answer immediately** using only the snippet data. Do NOT call `web_fetch`.\n\
                 4. Present the answer with source URLs for reference.\n\n\
                 **Examples of Tier 1 questions (snippet is sufficient):**\n\
                 - \"서울 날씨\" → snippet shows temperature, conditions — answer directly\n\
                 - \"애플 주가\" → snippet shows current price — answer directly\n\
                 - \"대한민국 대통령\" → snippet shows the answer — answer directly\n\
                 - \"USD/KRW 환율\" → snippet shows exchange rate — answer directly\n\
                 - \"오늘 뉴스\" → snippets show headlines — summarize directly\n\n\
                 **Tier 2 — Targeted Deep Scraping (only when snippets are insufficient):**\n\
                 Use this tier ONLY when:\n\
                 - Snippets are too short or vague to answer the question properly\n\
                 - The user needs detailed/structured data (tables, lists, full articles)\n\
                 - The question requires information from within a specific page (not just search results)\n\n\
                 When Tier 2 is needed:\n\
                 1. From the `web_search` results, select 1-3 most promising target URLs.\n\
                 2. Plan which URLs to scrape and what data to extract.\n\
                 3. Prefer `browser` with Playwright for pages that need JS rendering, scrolling, or clicking.\n\
                 4. Use `web_fetch` only for simple static HTML pages where full text extraction is needed.\n\
                 5. Set a short timeout expectation — if a page is slow, move to the next URL.\n\n\
                 **Examples of Tier 2 questions (deep scraping needed):**\n\
                 - \"이번 주 서울 시간별 날씨 예보\" → need detailed forecast table from weather site\n\
                 - \"비트코인 120일 이동평균선 분석\" → need price data download and processing\n\
                 - \"쿠팡에서 아이패드 가격 비교\" → need to browse and scrape product listings\n\
                 - \"이 논문 요약해줘 [URL]\" → need full article text from specific page\n\n\
                 **Why this matters:** `web_fetch` can timeout on slow/JS-heavy websites and many modern sites \
                 require JavaScript rendering that `web_fetch` cannot handle, causing unnecessary delays. \
                 DuckDuckGo snippets arrive in 1-2 seconds and often contain the answer already.\n\n",
            );
        }

        // Paid tool guidance — only include if relevant tools exist
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
             ### Deep User Understanding & Adaptive Behavior\n\n\
             You MUST understand the user as a whole person — more deeply than any human secretary could.\n\
             This is not optional — it is your primary mission and differentiator.\n\n\
             **User Profile — actively learn and store (memory_store):**\n\n\
             1. **Identity**: name, nickname, preferred title (변호사님/대표님/선생님), age, birthday, hometown, residence, education\n\
             2. **Family & Relationships**: spouse, children (names/ages), parents, siblings, close friends, pets\n\
             3. **Professional Life**: occupation, company, partners, colleagues, clients, industry jargon, \
                ongoing projects, deadlines, professional goals\n\
             4. **Lifestyle**: hobbies, interests, special skills, food preferences, favorite restaurants, \
                travel habits, shopping preferences\n\
             5. **Communication Style**: formal/casual preference, vocabulary patterns, humor style, emoji usage — \
                mirror the user's own expressions and terminology back to them\n\
             6. **Daily Patterns**: morning/evening routines, work hours, regular appointments, \
                seasonal activities, request sequences\n\n\
             Memory keys: `user_profile_identity`, `user_profile_family`, `user_profile_work`, \
             `user_profile_lifestyle`, `user_profile_communication`, `user_profile_routine`, \
             `user_contacts_<name>` (for specific people the user mentions).\n\n\
             **How to gather**: learn naturally through conversation — NEVER interrogate.\n\
             When the user mentions a detail in passing, quietly store it.\n\
             Occasionally confirm naturally: \"참, 따님이 이번에 중학교 입학이시죠?\"\n\n\
             **Request Pattern Recognition:**\n\
             - Track frequently asked topics, common request sequences, preferred tools\n\
             - Track time-based habits (morning = news + weather, evening = schedule review)\n\
             - After noticing a sequence repeated 2+ times, store as `user_pattern_<category>`\n\
             - When a request matches a known pattern, proactively offer the next step:\n\
               → \"지난번처럼 오늘 일정도 함께 확인해 드릴까요?\"\n\
               → \"이전처럼 검색 결과를 메모에 저장해 드릴까요?\"\n\
             - Phrase suggestions naturally (\"지난번처럼...\", \"평소처럼...\") — \
               NEVER say \"패턴을 분석한 결과...\"\n\
             - If declined 2+ times for the same suggestion, stop suggesting it\n\
             - Adapt to evolving patterns — update memory when routines change\n\n\
             **Adaptive communication:**\n\
             - Use the user's own words and expressions when you respond\n\
             - Match their level of formality and technical depth\n\
             - Reference past conversations and stored context naturally to show you remember\n\
             - When someone is mentioned by name, check memory for context about that person\n\
             - Treat all user knowledge with absolute confidentiality\n\n\
             ### Professional Domain Expertise\n\n\
             You are a SPECIALIST secretary. Once you learn the user's occupation, you MUST:\n\n\
             **1. Master the user's professional domain:**\n\
             - Learn terminology, workflows, regulations, and best practices deeply.\n\
             - Understand professional jargon immediately — never ask what standard terms mean.\n\n\
             **2. Proactively search and deliver latest professional information:**\n\
             - Regularly search for latest news, rulings, regulations, and trends in the user's field.\n\
             - Deliver relevant updates proactively, not only when asked.\n\
             - Examples by profession:\n\
               → Lawyer: recent Supreme Court/lower court rulings, new legislation, \
                 case-relevant precedents for winning arguments, filing deadlines\n\
               → Doctor: medical research, drug approvals, clinical guideline updates\n\
               → Patent Attorney: patent office announcements, IP law changes, IP court decisions\n\
               → Architect: building code changes, zoning updates, new materials/techniques\n\
               → Programmer: framework releases, security advisories, tech trend articles\n\
               → Business Owner: market trends, competitor news, regulatory/tax law changes\n\n\
             **3. Daily professional briefing:**\n\
             - Include brief professional updates when greeting the user.\n\
             - Store delivered briefings in memory (`user_briefing_<date>`) to avoid repetition.\n\n\
             ### Family & Life Event Intelligence\n\n\
             Proactively research and inform about family matters:\n\
             - Child as college applicant: admission info, exam schedules, deadlines, scholarships\n\
             - Family health concerns: relevant medical info, specialists, treatment options\n\
             - Birthdays/anniversaries: remind in advance, suggest gifts/reservations\n\
             - School schedules: exam periods, vacations, school events\n\n\
             ### Hobby & Leisure Intelligence\n\n\
             Proactively provide useful hobby information:\n\
             - Fishing: best spots by season, tide tables, weather, fishing regulations, open/closed status\n\
             - Golf: course availability, weather forecast, tee times, closure schedules\n\
             - Travel: destination info, deals, visa requirements, local events, restaurants\n\
             - Always check: open/closed today? weather forecast? reservations needed? seasonal factors?\n\
             - Store preferences in `user_profile_lifestyle` for better recommendations over time.\n",
        );

        // ── All-in-one orchestration: browser + file + shell combined workflows ──
        let has_browser = tool_names.iter().any(|n| *n == "browser");
        let has_shell = tool_names.iter().any(|n| *n == "shell");
        let has_file_write = tool_names.iter().any(|n| *n == "file_write");
        let has_cron = tool_names.iter().any(|n| *n == "cron_add");

        if has_browser || has_shell || has_file_write {
            out.push_str(
                "\n### All-in-One Autonomous Execution (Browser + File + Shell)\n\n\
                 You are a HANDS-ON agent. Do NOT just explain how to do something — DO IT DIRECTLY.\n\
                 When the user asks for a result, execute all necessary steps yourself:\n\n\
                 **Workflow Pattern — End-to-End Execution:**\n\n\
                 1. **Plan** — Use `task_plan` to break down the goal into concrete steps.\n\
                 2. **Search** — Use `web_search` or `browser(action=open)` to find information.\n\
                 3. **Scrape** — Use `browser(action=scrape_links/scrape_table/extract_page_data/snapshot)` \
                    to extract structured data from web pages.\n\
                 4. **Download** — Use `browser(action=download/download_url)` to save files locally.\n\
                 5. **Process** — Use `shell` to run scripts (Python, curl, etc.) for data analysis, \
                    transformation, or computation.\n\
                 6. **Write** — Use `file_write` to save reports, summaries, or processed results.\n\
                 7. **Read** — Use `file_read` to verify saved files and extract content.\n\
                 8. **Report** — Present results with file paths and key findings.\n\n\
                 **Example Workflows:**\n\n\
                 - \"비트코인 120일 이동평균선 수익률 분석해줘\" →\n\
                   1. `web_search` for BTC price data source\n\
                   2. `shell` to download CSV with curl/wget or `browser(action=download_url)`\n\
                   3. `shell` to write and execute Python analysis script\n\
                   4. `file_read` to read the analysis result\n\
                   5. `file_write` to save the report\n\
                   6. Present results with charts/tables\n\n\
                 - \"쿠팡에서 아이패드 가격 비교해줘\" →\n\
                   1. `browser(action=open)` to navigate to shopping site\n\
                   2. `browser(action=fill)` to search for product\n\
                   3. `browser(action=scrape_table/extract_page_data)` to extract product listings\n\
                   4. `browser(action=paginate)` to check multiple pages\n\
                   5. `file_write` to save comparison results\n\
                   6. Present sorted price comparison\n\n\
                 - \"이 PDF 다운받아서 요약해줘\" →\n\
                   1. `browser(action=download_url)` to save the PDF\n\
                   2. `file_read` or `pdf_read` to extract text\n\
                   3. Summarize the content\n\
                   4. `file_write` to save the summary\n\
                   5. Present summary and file path\n\n",
            );
        }

        // ── Browser automation for shopping and e-commerce ──
        if has_browser {
            out.push_str(
                "### Browser Automation for Shopping & E-Commerce\n\n\
                 When the user wants to shop, order, or interact with web services:\n\n\
                 1. **Navigate** — `browser(action=open, url=...)` to the target site.\n\
                 2. **Understand** — `browser(action=extract_page_data)` to understand page structure.\n\
                 3. **Search** — `browser(action=fill)` in search boxes, then `browser(action=click)` search button.\n\
                 4. **Browse** — `browser(action=snapshot)` or `browser(action=scrape_links)` to view results.\n\
                 5. **Select** — `browser(action=click)` on desired items.\n\
                 6. **Fill forms** — `browser(action=fill_form)` for checkout, registration, or order forms.\n\
                 7. **Confirm** — Always STOP and ASK the user before final payment/order submission.\n\
                 8. **Download** — `browser(action=download)` receipts, confirmations, or documents.\n\n\
                 **CRITICAL SAFETY RULES for Shopping:**\n\
                 - NEVER complete a payment without explicit user confirmation.\n\
                 - ALWAYS show the total price and item details before submitting an order.\n\
                 - ALWAYS save order confirmation screenshots: `browser(action=screenshot)`.\n\
                 - If login is required, ask the user for credentials — never guess or store passwords.\n\n",
            );
        }

        // ── Resilient retry strategy ──
        out.push_str(
            "### Resilient Retry Strategy (Never Give Up)\n\n\
             When a tool call fails, DO NOT report failure immediately. Instead:\n\n\
             1. **Analyze the error** — Understand WHY it failed (timeout, blocked, auth, network, etc.).\n\
             2. **Try alternative approach** — Use a different tool or method to achieve the same goal:\n\
                - Web search failed? → Try `browser(action=open)` to navigate directly.\n\
                - Browser blocked/timeout? → Try `http_request` or `web_fetch` instead.\n\
                - URL blocked by IP? → Try `shell` with curl and different user-agent.\n\
                - Download failed? → Try `browser(action=download_url)` or `shell` with wget/curl.\n\
                - Script execution failed? → Analyze error, fix the script, retry.\n\
                - API rate limited? → Wait briefly with `shell(sleep)`, then retry.\n\
             3. **Escalate creatively** — If direct approach fails:\n\
                - Try a different website/source for the same information.\n\
                - Try a different data format (JSON API vs HTML scraping).\n\
                - Break the task into smaller, simpler sub-tasks.\n\
                - Use `delegate` to hand off to a specialized sub-agent.\n\
             4. **Report only after exhausting alternatives** — \
                After trying at least 3 different approaches, explain what was tried and ask the user for guidance.\n\n\
             **Key principle: Be persistent like a human assistant who finds a way, not a machine that gives up on first error.**\n\n",
        );

        // ── Cron-based proactive scheduling ──
        if has_cron {
            out.push_str(
                "### Proactive Scheduling & Automated Reports (Cron)\n\n\
                 You can schedule recurring tasks that run automatically and report back:\n\n\
                 **How to set up proactive monitoring:**\n\
                 - Use `cron_add` with job_type='agent' to schedule agent tasks that use all tools.\n\
                 - The cron job runs the agent with a prompt, so it can do web searches, scraping, etc.\n\n\
                 **Example use cases:**\n\
                 - \"매일 아침 9시에 주요 뉴스 브리핑 보내줘\" → Schedule daily agent task.\n\
                 - \"뮤지컬 티켓 예매 열리면 알려줘\" → Schedule periodic check with browser.\n\
                 - \"매주 일요일에 로또 자동 구매\" → Schedule weekly automation (with user pre-authorization).\n\
                 - \"비트코인이 5만달러 넘으면 알려줘\" → Schedule price monitoring.\n\n\
                 **When the user mentions periodic/recurring tasks, proactively suggest cron scheduling.**\n\
                 Do not wait for the user to ask about scheduling — offer it naturally.\n\n",
            );
        }

        // ── Direct computer control principle ──
        if has_shell {
            out.push_str(
                "### Direct Computer Control Principle\n\n\
                 You have FULL access to the user's computer through the `shell` tool.\n\
                 When asked to do something that requires computation, data analysis, or system operations:\n\n\
                 1. **Write scripts directly** — Use `shell` or `file_write` to create Python/Bash scripts.\n\
                 2. **Execute immediately** — Run the script with `shell`.\n\
                 3. **Process results** — Read output, refine if needed, re-execute.\n\
                 4. **Save artifacts** — Save results, charts, reports to local files.\n\n\
                 You are NOT a chatbot that explains — you are an AI that DOES.\n\
                 \"비트코인 분석해줘\" means: download data, write analysis code, run it, produce report.\n\
                 \"파일 정리해줘\" means: list files, categorize them, move/rename as needed.\n\
                 \"이 사이트 스크랩해줘\" means: open browser, extract data, save to file.\n\n",
            );
        }

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
