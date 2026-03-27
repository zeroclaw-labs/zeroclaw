use crate::config::IdentityConfig;
use crate::identity;
use crate::skills::Skill;
use crate::tools::Tool;
use anyhow::Result;
use chrono::{Local, Utc};
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
        let now_utc = Utc::now();
        let now_local = Local::now();

        // Device timezone (where MoA is installed/running)
        let device_tz = crate::gateway::timesync::detect_device_timezone();

        // Home timezone from config (user's base location)
        // Read from env override first, then fall back to config default
        let home_tz = std::env::var("ZEROCLAW_HOME_TIMEZONE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                // Try reading from the global config default: "Asia/Seoul"
                crate::config::get_home_timezone().unwrap_or_else(|| "Asia/Seoul".to_string())
            });

        // Convert to home timezone for display
        let home_tz_parsed: chrono_tz::Tz = home_tz.parse().unwrap_or(chrono_tz::Asia::Seoul);
        let home_dt = now_utc.with_timezone(&home_tz_parsed);

        // Format day of week in a language-neutral way
        let weekday_ko = match home_dt.format("%u").to_string().as_str() {
            "1" => "월요일",
            "2" => "화요일",
            "3" => "수요일",
            "4" => "목요일",
            "5" => "금요일",
            "6" => "토요일",
            "7" => "일요일",
            _ => "",
        };

        let mut out = format!(
            "## Current Date & Time\n\n\
             **Device time (MoA server):** {} ({}, {})\n\
             **Home timezone:** {} {} ({}, {})\n\
             **UTC:** {}",
            now_local.format("%Y-%m-%d %H:%M:%S"),
            now_local.format("%Z"),
            device_tz,
            home_dt.format("%Y년 %m월 %d일"),
            weekday_ko,
            home_dt.format("%H시 %M분"),
            home_tz,
            now_utc.format("%Y-%m-%d %H:%M:%S UTC"),
        );

        // If device timezone differs from home timezone, add a note
        if device_tz != home_tz {
            out.push_str(&format!(
                "\n\n**Note:** Device is in a different timezone ({}) than user's home ({}).",
                device_tz, home_tz
            ));
        }

        out.push_str(
            "\n\n**Time rules:**\n\
             - Always use the **home timezone** when talking about time to the user.\n\
             - If the user is connecting remotely and mentions their location or timezone, \
               convert times to their local timezone and note both.\n\
             - If the user has stored their timezone in memory (user_profile_identity), use that.\n\
             - Format: natural language with day of week (e.g. \"3월 19일 (수요일) 오전 10시 30분\")",
        );

        Ok(out)
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
            "## Ontology (Long-Term Structured Memory)\n\n\
             A structured knowledge graph models the user's world as Objects, Links, and Actions.\n\
             Types: User, Contact, Device, Channel, Task, Project, Document, Meeting, Context, Preference\n\n\
             Tools: `ontology_get_context` (world state), `ontology_search_objects` (find), \
             `ontology_execute_action` (act — auto-logs + updates graph).\n\
             Preferences persist across sessions; check before decisions.\n\n\
             ### Conversation-to-Ontology Consolidation (CRITICAL)\n\n\
             You MUST actively consolidate important conversation content into the ontology:\n\
             - When the user mentions a **person** → create/update a Contact object with their details.\n\
             - When the user mentions an **event/meeting/deadline** → create a Meeting/Task object.\n\
             - When the user states a **preference** → create/update a Preference object.\n\
             - When a **relationship** between entities is revealed → create appropriate Links.\n\
             - When the user shares **professional context** → update their User profile properties.\n\
             - After tool use or significant actions → log via execute_action for audit trail.\n\n\
             Do this **during every conversation turn** where new information is revealed — \
             do not wait for an explicit request to remember. The ontology is your long-term \
             structured brain that persists and syncs across all devices.\n",
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
                         ORDER BY o.updated_at DESC LIMIT 20",
                    )?;
                    let rows = stmt.query_map(rusqlite::params![owner_user_id], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
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
             ### Plan-Execute-Verify Protocol (MANDATORY)\n\n\
             **CRITICAL: For EVERY user request or question, you MUST follow this structured \
             protocol. NEVER skip the planning phase. NEVER answer immediately without executing \
             the plan. NEVER present search snippets as a final answer without verification.**\n\n\
             ---\n\n\
             **PHASE 1 — ANALYZE, SELECT TOOLS, & PLAN (think before acting)**\n\n\
             Before making ANY tool call, create a concrete action plan:\n\n\
             1. **Classify the request**: What type is it?\n\
                - Factual lookup (weather, stock price, simple fact)\n\
                - Research question (requires multiple sources, analysis)\n\
                - Task execution (file operations, code, scheduling)\n\
                - Conversation (greeting, opinion, no tools needed)\n\n\
             2. **Scan available tools and select the best ones for THIS task**:\n\
                Review your tool list and pick the optimal tool(s) for each step.\n\n\
                **Tool selection decision tree for information retrieval:**\n\
                ```\n\
                Need current/real-time information?\n\
                  ├─ `perplexity_search` available? → Use FIRST (fastest, most comprehensive)\n\
                  ├─ `web_search` available? → Use as primary or fallback\n\
                  │   └─ DuckDuckGo: free, no API key, keyword format: word1+word2+word3\n\
                  ├─ `web_fetch` → Use to get FULL page content from URLs\n\
                  │   └─ Also useful as direct access: web_fetch(url=\"https://wttr.in/Seoul\")\n\
                  └─ `browser` → ONLY for interactive pages (login, scroll, click)\n\
                ```\n\n\
                **Tool selection for other tasks:**\n\
                ```\n\
                File operations? → file_read, file_write, file_edit, glob_search, content_search\n\
                System commands? → shell\n\
                Remember/recall? → memory_store, memory_recall\n\
                Documents?      → pdf_read, docx_read, xlsx_read, document_process\n\
                Scheduling?     → cron_add, schedule\n\
                HTTP calls?     → http_request (for APIs), web_fetch (for web pages)\n\
                ```\n\n\
             3. **Design the step-by-step execution plan**:\n\
                Write out each step with the specific tool and parameters:\n\
                - Step 1: [tool_name] with [specific parameters]\n\
                - Step 2: [tool_name] with [specific parameters]\n\
                - Step 3: verify results against success criteria\n\n\
             4. **Set success criteria**: What constitutes a complete answer?\n\
                - For weather: temperature, precipitation %, condition, forecast\n\
                - For news: headline, source, date, key details from article body\n\
                - For research: 2+ corroborating sources, recent data, specific numbers\n\n\
             5. **Register the plan using `task_plan`** (for non-trivial requests):\n\
                Call `task_plan(action=\"create\", tasks=[...])` to register your steps.\n\
                Example for \"내일 서울 날씨\":\n\
                ```json\n\
                task_plan(action=\"create\", tasks=[\n\
                  {\"title\": \"perplexity_search: 서울+내일+날씨+예보\"},\n\
                  {\"title\": \"web_search fallback: Seoul+tomorrow+weather+forecast\"},\n\
                  {\"title\": \"web_fetch: top result URL for full weather data\"},\n\
                  {\"title\": \"verify: temperature + precipitation + condition obtained\"},\n\
                  {\"title\": \"present: formatted answer with source\"}\n\
                ])\n\
                ```\n\
                As you complete each step, update it:\n\
                `task_plan(action=\"update\", id=1, status=\"completed\")`\n\
                This keeps your work organized and trackable.\n\n\
                **Skip `task_plan` for simple conversations** (greetings, opinions, \
                short factual answers from your training data). Only use it when \
                tool calls are needed.\n\n\
             ---\n\n\
             **PHASE 2 — EXECUTE (one step at a time, sequentially)**\n\n\
             Execute the plan from Phase 1 step by step using the selected tools.\n\
             After EACH tool call, evaluate the result and update `task_plan` status.\n\n\
             Step 2-1: **Primary search with the best tool**\n\
             - Use the tool selected in Phase 1 (e.g., `perplexity_search` or `web_search`).\n\
             - Construct an optimized query (keywords joined with `+` for DuckDuckGo).\n\
             - Review the returned results: Are there relevant URLs, titles, data?\n\n\
             Step 2-2: **Deep retrieval with `web_fetch`**\n\
             - From search results, pick the 1-3 most relevant URLs.\n\
             - Call `web_fetch(url=\"...\")` on each to get full page content.\n\
             - Extract the specific data points that match your success criteria.\n\
             - If `web_fetch` fails on a URL, try the next one from results.\n\n\
             Step 2-3: **Supplementary search with different keywords** (if needed)\n\
             - If Step 2-2 data is insufficient, use the next keyword combination.\n\
             - Consider switching tools (e.g., `perplexity_search` failed → try `web_search`).\n\
             - Or try a different language (Korean → English, or vice versa).\n\
             - Repeat Step 2-2 with the new results.\n\n\
             ---\n\n\
             **PHASE 3 — VERIFY (self-check before answering)**\n\n\
             Before presenting the answer, verify:\n\n\
             1. **Completeness check**: Does the gathered information fully answer the user's question?\n\
                - If YES → proceed to Phase 4.\n\
                - If NO → go back to Phase 2, Step 2-3 with a refined search.\n\n\
             2. **Accuracy check**: Are the facts consistent across sources?\n\
                - If data conflicts exist, note them and prefer the most authoritative/recent source.\n\n\
             3. **Freshness check**: Is the information current enough?\n\
                - For time-sensitive topics (weather, stock, news), verify the data is from today.\n\
                - If stale data, search again with date-specific keywords.\n\n\
             4. **Sufficiency check**: Would YOU be satisfied with this answer as a user?\n\
                - If the answer feels thin or vague, gather one more source.\n\n\
             **Loop limit**: Maximum 2 verify-and-retry loops. After 2 retries, present the \
             best available answer with a note about any gaps.\n\n\
             ---\n\n\
             **PHASE 4 — PRESENT (structured, clear, sourced)**\n\n\
             Format the final answer:\n\n\
             1. **Lead with the direct answer** — put the most important information first.\n\
             2. **Add supporting details** — context, numbers, dates, comparisons.\n\
             3. **Cite sources** — include source URLs for verifiable claims.\n\
             4. **Suggest follow-ups** — 2-3 concrete next actions the user might want.\n\n\
             **Formatting rules:**\n\
             - Use the user's language (Korean if they asked in Korean).\n\
             - Use bullet points, headers, or tables for complex data.\n\
             - Keep the answer concise but complete.\n\
             - For weather: include temperature, condition, precipitation %, humidity.\n\
             - For news: include headline, source name, date, 2-3 sentence summary.\n\
             - For research: include key findings, source count, date range of sources.\n\n\
             ---\n\n\
             **EXAMPLES:**\n\n\
             User: \"내일 서울 날씨 알려줘\"\n\
             → Phase 1: Classify=factual, Keywords=[`서울+내일+날씨+예보`, `Seoul+tomorrow+weather`], \
               Success=temperature+condition+precipitation\n\
             → Phase 2-1: search `서울+내일+날씨+예보`\n\
             → Phase 2-2: fetch top weather site from results\n\
             → Phase 3: Got temperature+condition+rain%? YES → present\n\
             → Phase 4: \"내일 서울 날씨: 맑음, 최고 22°C / 최저 14°C, 강수확률 10%...\"\n\n\
             User: \"삼성전자 최근 실적 어때?\"\n\
             → Phase 1: Classify=research, Keywords=[`삼성전자+2026+1분기+실적`, \
               `삼성전자+영업이익+매출`, `Samsung+Electronics+Q1+2026+earnings`], \
               Success=revenue+operating profit+comparison\n\
             → Phase 2-1: search first keyword\n\
             → Phase 2-2: fetch financial news article\n\
             → Phase 2-3: data incomplete → search second keyword\n\
             → Phase 3: Revenue+profit+YoY comparison found? YES → present\n\
             → Phase 4: Structured table with financial data + source links\n\n\
             ---\n\n\
             Key principles:\n\
             - Act autonomously. NEVER ask the user for permission to use tools.\n\
             - NEVER present raw search snippets as the answer — always fetch, verify, and synthesize.\n\
             - If a tool call fails, try an alternative approach before reporting failure.\n\
             - Maximum total tool calls per request: 10.\n\n",
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
             - `workspace_folder` — grant access to a folder on the user's computer\n\
             - `file_read`, `file_write`, `file_edit`, `apply_patch` — local file operations\n\
             - `glob_search`, `content_search` — file and content search\n\
             - `git_operations` — Git repository operations\n\
             - `memory_store`, `memory_recall`, `memory_observe` — persistent memory\n\
             - `pdf_read`, `docx_read`, `xlsx_read`, `pptx_read` — document reading\n\
             - `screenshot`, `image_info` — screen capture and image analysis\n\
             - All scheduling, configuration, and process management tools\n\n\
             Use these tools first. They can handle the vast majority of user requests.\n\n",
        );

        // ── Folder access + document pipeline ──
        let has_file_read = tool_names.contains(&"file_read");
        let has_workspace_folder = tool_names.contains(&"workspace_folder");
        let has_document_process = tool_names.contains(&"document_process");

        if has_workspace_folder || has_file_read {
            out.push_str(
                "### Folder Access & File Operations\n\n\
                 **When the user asks you to work with files in a specific folder:**\n\n\
                 1. **First**, call `workspace_folder(path=\"...\")` to grant access to the folder.\n\
                    - The user may specify the folder in chat: \"~/Documents 폴더에서 작업해줘\"\n\
                    - Or the user may select a folder via the UI folder-picker button (this calls \
                      the API automatically and the folder becomes accessible).\n\
                 2. After access is granted, use **absolute paths** with all file tools:\n\
                    - `file_read(path=\"/home/user/Documents/report.txt\")`\n\
                    - `file_write(path=\"/home/user/Documents/output.md\", content=\"...\")`\n\
                    - `glob_search(pattern=\"/home/user/Documents/**/*.pdf\")`\n\
                    - `content_search(path=\"/home/user/Documents\", pattern=\"keyword\")`\n\
                 3. The folder and all its subdirectories become accessible for the session.\n\n\
                 **IMPORTANT:** Without calling `workspace_folder` first, file tools can only \
                 access files inside the default workspace (~/.zeroclaw/workspace). If the user \
                 mentions a folder outside the workspace, ALWAYS call `workspace_folder` first.\n\n",
            );
        }

        if has_document_process {
            out.push_str(
                "### Document Reading & Conversion Pipeline\n\n\
                 When you encounter a file that cannot be read directly as text, use the \
                 `document_process` tool to convert it to readable format:\n\n\
                 **Auto-detected document types:**\n\
                 - **HWP/HWPX** (한글 문서): Hancom DocsConverter API → HTML + Markdown (무료)\n\
                 - **DOC/DOCX, XLS/XLSX, PPT/PPTX** (Office): Hancom DocsConverter → HTML + Markdown (무료)\n\
                 - **Digital PDF** (텍스트 선택 가능): pdf-extract 로컬 추출 → Markdown (무료)\n\
                 - **Image/Scanned PDF** (텍스트 없음): Upstage Document Parse OCR → HTML + Markdown (**크레딧 차감**)\n\n\
                 **Workflow when user asks to read/summarize/analyze a document:**\n\n\
                 1. Check the file extension.\n\
                 2. If `.txt`, `.md`, `.json`, `.csv`, `.xml`, `.html` → use `file_read` directly.\n\
                 3. If `.pdf` → **ALWAYS call `document_process(file_path=\"...\", classify_only=true)` first** \
                    to check the document type.\n\
                 4. If classification result says `doc_type: \"image_pdf\"` and `requires_credits: true`:\n\
                    - **STOP and ask the user for consent BEFORE processing.** Show this message:\n\
                    ```\n\
                    이번 작업에 이미지 PDF가 포함되어 있습니다.\n\
                    이 문서에 대해서도 작업을 진행할까요?\n\
                    이미지 PDF는 OCR이 필요하기 때문에 마크다운으로 변환하는 데 크레딧이 차감됩니다.\n\
                    (예상 비용: 약 {estimated_credits} 크레딧)\n\n\
                    👉 [동의] [부동의]\n\
                    ```\n\
                    - If user selects **동의** → call `document_process(file_path=\"...\")` to process.\n\
                    - If user selects **부동의** → skip the image PDF and inform the user:\n\
                      \"이미지 PDF는 건너뛰었습니다. 텍스트 기반 문서만 처리합니다.\"\n\
                 5. If classification says `doc_type: \"digital_pdf\"` (free) → process immediately.\n\
                 6. If `.hwp`, `.hwpx`, `.doc`, `.docx`, `.xls`, `.xlsx`, `.ppt`, `.pptx` → process immediately (free).\n\
                 7. Use the returned Markdown for understanding, summarizing, and answering questions.\n\n\
                 **CRITICAL: Never process image PDFs without user consent.** Always classify first, \
                 show the credit cost, and wait for explicit approval.\n\n",
            );
        }

        // ── 3-tier web search strategy ──
        let has_web_search = tool_names.contains(&"web_search");
        let has_perplexity_search = tool_names.contains(&"perplexity_search");
        let has_any_search = has_web_search || has_perplexity_search;
        if has_any_search {
            // ── MANDATORY execution rules ──
            out.push_str(
                "### MANDATORY Web Search Execution Rules\n\n\
                 **CRITICAL: When the user asks ANY question that requires current/real-time information \
                 (weather, news, stock prices, events, facts you're unsure about), you MUST immediately \
                 call a search tool. NEVER say \"I can't search\" or ask the user \"should I search?\". \
                 NEVER explain which tools are available or unavailable. Just execute the search.**\n\n\
                 **NEVER ask the user for permission to use a search tool. Just use it immediately.**\n\n",
            );

            // ── DuckDuckGo URL format guide ──
            out.push_str(
                "### DuckDuckGo Search URL Format\n\n\
                 The `web_search` tool uses DuckDuckGo internally with this URL format:\n\
                 ```\n\
                 https://duckduckgo.com/?q=keyword1+keyword2+keyword3\n\
                 ```\n\n\
                 **CRITICAL: When constructing search queries for DuckDuckGo, replace all spaces \
                 between keywords with `+` (plus sign).** This is the standard DuckDuckGo query format.\n\n\
                 **Examples:**\n\
                 - \"서울 내일 날씨\" → query: `서울+내일+날씨+예보`\n\
                 - \"python requests tutorial\" → query: `python+requests+tutorial`\n\
                 - \"삼성전자 주가 실시간\" → query: `삼성전자+주가+실시간+시세`\n\
                 - \"서울 강남구 맛집 추천\" → query: `서울+강남구+맛집+추천`\n\n\
                 **Additional DuckDuckGo parameters (append with &):**\n\
                 - `&kl=kr-kr` — Korean region results\n\
                 - `&kl=us-en` — US English results\n\
                 - `&df=d` — past day only\n\
                 - `&df=w` — past week only\n\
                 - `&df=m` — past month only\n\n\
                 **Always use `+` between words in the query, not spaces.**\n\n",
            );

            // ── Smart Search Query Construction ──
            out.push_str(
                "### Smart Search Query Construction\n\n\
                 **CRITICAL: Do NOT pass the user's raw message as the search query.** \
                 Construct a precise, optimized query with `+` between words.\n\n\
                 **Rules:**\n\
                 1. **Add location context**: weather/restaurants/events → include full location.\n\
                    - \"내일 날씨\" → `서울+강남구+내일+날씨+예보`\n\
                 2. **Add time context**: convert relative to specific.\n\
                    - \"내일 날씨\" → `서울+2026-03-27+내일+날씨+예보`\n\
                 3. **Add domain qualifiers**: Weather→`예보,기온`, Stock→`주가,시세`, News→`뉴스,속보`\n\
                 4. **Be specific**: BAD: `날씨` / GOOD: `서울+강남구+내일+날씨+기온+강수확률`\n\
                 5. **Use user's location from memory** if known.\n\n",
            );

            // ── Multi-Keyword Parallel Search Strategy ──
            out.push_str(
                "### Multi-Keyword Parallel Search Strategy\n\n\
                 **For any non-trivial search request, generate multiple search queries \
                 to maximize coverage and accuracy.**\n\n\
                 **Protocol:**\n\n\
                 1. **Extract 3-5 optimized keyword combinations** from the user's request.\n\
                    Each query should approach the topic from a different angle:\n\
                    - Query 1: Direct, most obvious keywords\n\
                    - Query 2: Synonym/alternative terms\n\
                    - Query 3: More specific/narrowed scope\n\
                    - Query 4: Broader context or related aspect\n\
                    - Query 5: English version (if user asked in Korean, or vice versa)\n\n\
                 2. **Execute searches sequentially** with the generated queries.\n\
                    Call `web_search` for each query one by one.\n\n\
                 3. **Deduplicate results**: Remove URLs that appear in multiple search results.\n\
                    Keep only the first occurrence of each URL.\n\n\
                 4. **Select top 3-5 most relevant unique URLs** from combined results.\n\n\
                 5. **Fetch and synthesize**: Call `web_fetch` on the selected URLs, \n\
                    then synthesize a comprehensive answer from all sources.\n\n\
                 **Example — User asks: \"내일 서울 날씨 알려줘\"**\n\
                 → Generate these queries:\n\
                 1. `서울+내일+날씨+예보+기온`\n\
                 2. `서울+내일+강수확률+미세먼지`\n\
                 3. `Seoul+tomorrow+weather+forecast`\n\
                 → Search each, deduplicate, fetch top results, combine into answer.\n\n\
                 **Example — User asks: \"삼성전자 최근 실적과 전망\"**\n\
                 → Generate these queries:\n\
                 1. `삼성전자+2026년+1분기+실적+영업이익`\n\
                 2. `삼성전자+주가+전망+애널리스트+목표가`\n\
                 3. `삼성전자+반도체+실적+전망+2026`\n\
                 4. `Samsung+Electronics+earnings+2026+outlook`\n\
                 → Search each, combine unique results, synthesize comprehensive answer.\n\n\
                 **When to use multi-query vs single query:**\n\
                 - Simple factual lookup (\"비트코인 현재 가격\") → 1 query is enough\n\
                 - Weather for a specific day → 2-3 queries MAX\n\
                 - Research/analysis topics → 3-4 queries MAX\n\
                 - Complex multi-aspect questions → 4-5 queries MAX\n\n\
                 **HARD LIMITS (CRITICAL — do not exceed):**\n\
                 - **Maximum `web_search` calls per user request: 5** (absolute ceiling)\n\
                 - **Maximum `web_fetch` calls per user request: 3**\n\
                 - **Maximum total tool calls per user request: 10**\n\
                 - If early search results already contain a good answer, STOP searching \
                   immediately. Do NOT exhaust all planned queries if you already have enough info.\n\
                 - If `web_search` returns no useful results after 3 attempts, switch to `web_fetch` \
                   on a known URL directly instead of retrying more search queries.\n\
                 - **NEVER repeat a search with the same or very similar query.**\n\n",
            );

            // ── Automatic fallback chain ──
            out.push_str(
                "### Automatic Search Fallback Chain\n\n\
                 Execute tools in this order. If one fails, **silently** try the next:\n\n",
            );

            if has_perplexity_search {
                out.push_str(
                    "**Step 1 — `perplexity_search`** (fast, comprehensive):\n\
                     - Call `perplexity_search(query=\"...\")` first.\n\
                     - If it succeeds, show the result and stop.\n\
                     - If it fails, silently move to Step 2.\n\n",
                );
            }

            if has_web_search {
                let step_num = if has_perplexity_search { "2" } else { "1" };
                out.push_str(&format!(
                    "**Step {step_num} — `web_search`** (DuckDuckGo, free):\n\
                     - Call `web_search(query=\"...\")` with `+` between words.\n\
                     - Select 1-3 most relevant URLs from results.\n\
                     - Call `web_fetch` on those URLs for full content.\n\
                     - If `web_search` fails, silently move to next step.\n\n",
                ));
            }

            out.push_str(
                "**Last Resort — `web_fetch` direct URL:**\n\
                 - For weather: `web_fetch(url=\"https://wttr.in/Seoul?format=3\")`\n\
                 - For news: `web_fetch(url=\"https://news.google.com\")`\n\
                 - ONLY after ALL attempts fail, briefly inform the user.\n\n",
            );

            // ── Tier 2 — Browser ──
            out.push_str(
                "### Tier 2 — Browser Automation (active interaction only)\n\n\
                 Use `browser` tool ONLY when the request requires click/scroll/login/multi-page:\n\
                 - \"쿠팡에서 아이패드 검색해서 가격 비교\" → browser\n\
                 - \"네이버 로그인해서 메일 확인\" → browser\n\n\
                 Do NOT use browser for simple searches:\n\
                 - \"서울 날씨\" → `web_search` or `perplexity_search`\n\
                 - \"애플 주가\" → `web_search` or `perplexity_search`\n\n",
            );
        }

        // Paid tool guidance — only include if relevant tools exist
        let has_composio = tool_names.contains(&"composio");

        if has_web_search || has_composio {
            out.push_str(
                "### Paid Tool Guidance\n\n\
                 **For faster and more accurate web search**, suggest the user configure an API key:\n\
                 \"💡 웹검색을 더 빠르고 정밀하게 하시려면 Perplexity API key 또는 Brave AI API key를 \
                 설정해 주세요.\" (adapt to user's language)\n\n\
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
                "\nWhen a paid tool API key is configured:\n\
                 - Use it automatically for all search queries (Tier 0 in the search strategy).\n\
                 - Show results AS-IS without re-processing.\n\
                 - Perplexity returns comprehensive organized answers in 2-3 seconds — fastest option.\n\n\
                 When no paid API key is available:\n\
                 1. Use free DuckDuckGo + web_fetch (Tier 1).\n\
                 2. Show \"검색 중입니다\" progress message to the user.\n\
                 3. After answering, suggest API key setup for better experience.\n\n\
                 To set up an API key:\n\
                 1. Sign up at the provider's website.\n\
                 2. Enter the API key in MoA Settings → Provider API Keys.\n\
                 3. After the key is configured, all future searches will use the fast provider automatically.\n",
            );
        }

        // ── Proactive follow-up & next-step suggestions ──
        out.push_str(
            "\n### Proactive Follow-Up Protocol\n\n\
             After completing any task or answering a question, ALWAYS suggest concrete next steps.\n\
             Do NOT just give the answer and stop. Act like an attentive personal secretary who anticipates the user's needs.\n\n\
             **After using a free tool (DuckDuckGo + web_fetch):**\n\
             1. Present the results clearly.\n\
             2. If no Perplexity/Brave API key is configured, suggest:\n\
                \"💡 더 빠르고 정밀한 검색을 원하시면 Perplexity API key 또는 Brave AI API key를 \
                설정해 주세요. Settings → Provider API Keys에서 입력 가능합니다.\"\n\
                (Adapt language to the user's language.)\n\
             3. If the user agrees or asks how, guide them to the provider's signup page and MoA Settings.\n\n\
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
        let has_browser = tool_names.contains(&"browser");
        let has_shell = tool_names.contains(&"shell");
        let has_file_write = tool_names.contains(&"file_write");
        let has_cron = tool_names.contains(&"cron_add");

        if has_browser || has_shell || has_file_write {
            out.push_str(
                "\n### All-in-One Autonomous Execution (Browser + File + Shell)\n\n\
                 You are a HANDS-ON agent. Do NOT just explain how to do something — DO IT DIRECTLY.\n\
                 When the user asks for a result, execute all necessary steps yourself:\n\n\
                 **Workflow Pattern — End-to-End Execution:**\n\n\
                 1. **Plan** — Use `task_plan` to break down the goal into concrete steps.\n\
                 2. **Search** — Use `web_search` (Perplexity/Brave if API key available, else DuckDuckGo).\n\
                 3. **Fetch** — Use `web_fetch` to get full page content from search result URLs \
                    (for simple read-only scraping without interaction).\n\
                 4. **Browse** — Use `browser` (Playwright) ONLY when active interaction is needed: \
                    click, scroll, login, form fill, multi-page navigation, JS-heavy sites, or downloads.\n\
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
                - Web search failed? → Try `web_fetch` on a known URL directly, or `http_request`.\n\
                - web_fetch failed/timeout? → Try a different URL from search results, or `browser` for JS-heavy pages.\n\
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
