//! Tool subsystem for agent-callable capabilities.

pub mod attribution;
pub mod cron_add;
pub(crate) mod cron_common;
pub mod cron_list;
pub mod cron_remove;
pub mod cron_run;
pub mod cron_runs;
pub mod cron_update;
pub mod delegate;
pub mod file_read;
pub mod model_switch;
pub mod param_options;
pub mod read_skill;
pub mod schedule;
pub mod scoped;
pub mod security_ops;
pub mod send_message_to_peer;
pub mod shell;
pub mod skill_http;
pub mod skill_manage;
pub mod skill_tool;
pub mod sop_advance;
pub mod sop_approve;
pub mod sop_execute;
pub mod sop_list;
pub mod sop_status;
pub mod sop_workshop;
pub mod spawn_subagent;
pub mod todo_write;
pub mod verifiable_intent;

// Tool types from zeroclaw-tools (direct imports, no shims)
pub use zeroclaw_tools::ask_user::AskUserTool;
pub use zeroclaw_tools::ask_user::ChannelMapHandle;
pub use zeroclaw_tools::backup_tool::BackupTool;
pub use zeroclaw_tools::browser::{BrowserTool, ComputerUseConfig};
pub use zeroclaw_tools::browser_delegate::BrowserDelegateTool;
pub use zeroclaw_tools::browser_open::BrowserOpenTool;
pub use zeroclaw_tools::calculator::CalculatorTool;
pub use zeroclaw_tools::canvas::{ALLOWED_CONTENT_TYPES, MAX_CONTENT_SIZE};
pub use zeroclaw_tools::canvas::{CanvasStore, CanvasTool};
pub use zeroclaw_tools::channel_room::ChannelRoomTool;
pub use zeroclaw_tools::claude_code::ClaudeCodeTool;
pub use zeroclaw_tools::claude_code_runner::ClaudeCodeRunnerTool;
pub use zeroclaw_tools::cli_discovery::{DiscoveredCli, discover_cli_tools};
pub use zeroclaw_tools::cloud_ops::CloudOpsTool;
pub use zeroclaw_tools::cloud_patterns::CloudPatternsTool;
pub use zeroclaw_tools::codex_cli::CodexCliTool;
pub use zeroclaw_tools::composio::ComposioTool;
pub use zeroclaw_tools::content_search::ContentSearchTool;
pub use zeroclaw_tools::data_management::DataManagementTool;
pub use zeroclaw_tools::discord_search::DiscordSearchTool;
pub use zeroclaw_tools::email_read::EmailReadTool;
pub use zeroclaw_tools::email_search::EmailSearchTool;
pub use zeroclaw_tools::escalate::EscalateToHumanTool;
pub use zeroclaw_tools::file_download::FileDownloadTool;
pub use zeroclaw_tools::file_edit::FileEditTool;
pub use zeroclaw_tools::file_upload::FileUploadTool;
pub use zeroclaw_tools::file_upload_bundle::FileUploadBundleTool;
pub use zeroclaw_tools::file_write::FileWriteTool;
pub use zeroclaw_tools::gemini_cli::GeminiCliTool;
pub use zeroclaw_tools::git_forge::GitForgeTool;
pub use zeroclaw_tools::git_operations::GitOperationsTool;
pub use zeroclaw_tools::glob_search::GlobSearchTool;
pub use zeroclaw_tools::google_workspace::GoogleWorkspaceTool;
pub use zeroclaw_tools::hardware_board_info::HardwareBoardInfoTool;
pub use zeroclaw_tools::hardware_memory_map::HardwareMemoryMapTool;
pub use zeroclaw_tools::hardware_memory_read::HardwareMemoryReadTool;
pub use zeroclaw_tools::http_request::HttpRequestTool;
pub use zeroclaw_tools::image_gen::ImageGenTool;
pub use zeroclaw_tools::image_info::ImageInfoTool;
pub use zeroclaw_tools::jira_tool::JiraTool;
pub use zeroclaw_tools::knowledge_tool::KnowledgeTool;
pub use zeroclaw_tools::linkedin::LinkedInTool;
pub use zeroclaw_tools::llm_task::LlmTaskTool;
pub use zeroclaw_tools::mcp_client::{McpRegistry, McpServer};
pub use zeroclaw_tools::mcp_context;
pub use zeroclaw_tools::mcp_deferred::{
    ActivatedToolSet, DeferredMcpToolSet, build_deferred_tools_section,
    build_deferred_tools_section_excluding, build_deferred_tools_section_filtered,
};
pub use zeroclaw_tools::mcp_prompts_tool::McpPromptsTool;
pub use zeroclaw_tools::mcp_resources_tool::McpResourcesTool;
pub use zeroclaw_tools::mcp_tool::McpToolWrapper;
pub use zeroclaw_tools::memory_export::MemoryExportTool;
pub use zeroclaw_tools::memory_forget::MemoryForgetTool;
pub use zeroclaw_tools::memory_purge::MemoryPurgeTool;
pub use zeroclaw_tools::memory_recall::MemoryRecallTool;
pub use zeroclaw_tools::memory_store::MemoryStoreTool;
pub use zeroclaw_tools::microsoft365::Microsoft365Tool;
pub use zeroclaw_tools::model_routing_config::ModelRoutingConfigTool;
pub use zeroclaw_tools::notion_tool::NotionTool;
pub use zeroclaw_tools::opencode_cli::OpenCodeCliTool;
pub use zeroclaw_tools::pipeline::PipelineTool;
pub use zeroclaw_tools::poll::PollTool;
pub use zeroclaw_tools::project_intel::ProjectIntelTool;
pub use zeroclaw_tools::proxy_config::ProxyConfigTool;
pub use zeroclaw_tools::pushover::PushoverTool;
pub use zeroclaw_tools::reaction::ReactionTool;
pub use zeroclaw_tools::report_template_tool::ReportTemplateTool;
pub use zeroclaw_tools::screenshot::ScreenshotTool;
pub use zeroclaw_tools::send_via::{
    AgentPeerGroupResolver, SendViaTool, TURN_ROUTING, TurnRoutingHandle,
};
pub use zeroclaw_tools::sessions::{
    SessionDeleteTool, SessionResetTool, SessionsCurrentTool, SessionsHistoryTool,
    SessionsListTool, SessionsSendTool,
};
pub use zeroclaw_tools::text_browser::TextBrowserTool;
pub use zeroclaw_tools::tool_search::ToolSearchTool;
pub use zeroclaw_tools::weather_tool::WeatherTool;
pub use zeroclaw_tools::web_fetch::WebFetchTool;
pub use zeroclaw_tools::web_search_tool::WebSearchTool;
pub use zeroclaw_tools::wrappers::{PathGuardedTool, RateLimitedTool};

// Traits from zeroclaw-api
pub use zeroclaw_api::schema::{CleaningStrategy, SchemaCleanr};
pub use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult, ToolSpec};

// Local tool re-exports (tools with root deps, kept in misc)
pub use cron_add::CronAddTool;
pub use cron_list::CronListTool;
pub use cron_remove::CronRemoveTool;
pub use cron_run::CronRunTool;
pub use cron_runs::CronRunsTool;
pub use cron_update::CronUpdateTool;
pub use delegate::DelegateTool;
pub use file_read::FileReadTool;
pub use model_switch::ModelSwitchTool;
pub use read_skill::ReadSkillTool;
pub use schedule::ScheduleTool;
pub use security_ops::SecurityOpsTool;
pub use send_message_to_peer::SendMessageToPeerTool;
pub use shell::ShellTool;
pub use skill_http::SkillHttpTool;
pub use skill_tool::{SkillBuiltinTool, SkillShellTool};
pub use sop_advance::SopAdvanceTool;
pub use sop_approve::SopApproveTool;
pub use sop_execute::SopExecuteTool;
pub use sop_list::SopListTool;
pub use sop_status::SopStatusTool;
pub use sop_workshop::SopWorkshopTool;
pub use spawn_subagent::SpawnSubagentTool;
pub use todo_write::TodoWriteTool;
pub use verifiable_intent::VerifiableIntentTool;

/// Re-entrant agent-spawning tools that must never be collapsed by the
/// per-turn duplicate-call guard: launching several with the same prompt
/// (redundancy, sampling, fan-out) is intentional, not an accidental
/// repeat. Unioned with config-provided exemptions in the tool-call loop.
pub const REENTRANT_AGENT_TOOLS: &[&str] = &[SpawnSubagentTool::NAME, DelegateTool::NAME];

use crate::platform::{NativeRuntime, RuntimeAdapter};
use crate::security::{SecurityPolicy, create_sandbox};
use crate::sop::audit::SopAuditLogger;
use crate::sop::engine::SopEngine;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeroclaw_config::schema::{AliasedAgentConfig, Config};
use zeroclaw_memory::Memory;

pub type PerToolChannelHandle =
    Arc<RwLock<HashMap<String, Arc<dyn zeroclaw_api::channel::Channel>>>>;

/// Shared handle to the delegate tool's parent-tools list.
/// Callers can push additional tools (e.g. MCP wrappers) after construction.
pub type DelegateParentToolsHandle = Arc<RwLock<Vec<Arc<dyn Tool>>>>;

/// Thin wrapper that makes an `Arc<dyn Tool>` usable as `Box<dyn Tool>`.
pub struct ArcToolRef(pub Arc<dyn Tool>);
// ArcToolRef is the public constructor name for ArcToolWrapper

#[async_trait]
impl Tool for ArcToolRef {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn description(&self) -> &str {
        self.0.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.0.parameters_schema()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.0.output_schema()
    }

    fn param_domains(&self) -> Vec<(&'static str, ::zeroclaw_api::tool::OptionDomain)> {
        self.0.param_domains()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.0.execute(args).await
    }
}

#[derive(Clone)]
struct ArcDelegatingTool {
    inner: Arc<dyn Tool>,
}

impl ArcDelegatingTool {
    fn boxed(inner: Arc<dyn Tool>) -> Box<dyn Tool> {
        Box::new(Self { inner })
    }
}

impl ::zeroclaw_api::attribution::Attributable for ArcDelegatingTool {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

#[async_trait]
impl Tool for ArcDelegatingTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        self.inner.output_schema()
    }

    fn param_domains(&self) -> Vec<(&'static str, ::zeroclaw_api::tool::OptionDomain)> {
        self.inner.param_domains()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.inner.execute(args).await
    }
}

fn boxed_registry_from_arcs(tools: Vec<Arc<dyn Tool>>) -> Vec<Box<dyn Tool>> {
    tools.into_iter().map(ArcDelegatingTool::boxed).collect()
}

/// Create the default tool registry
pub fn default_tools(security: Arc<SecurityPolicy>) -> Vec<Box<dyn Tool>> {
    default_tools_with_runtime(security, Arc::new(NativeRuntime::new()))
}

/// Create the default tool registry with explicit runtime adapter.
pub fn default_tools_with_runtime(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Vec<Box<dyn Tool>> {
    let persistent_writes = runtime.has_filesystem_access();
    vec![
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(
                ShellTool::new(security.clone(), runtime).with_persistent_writes(persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileReadTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileWriteTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileEditTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(GlobSearchTool::new(security.clone()), security.clone()),
            security.clone(),
        )),
        Box::new(RateLimitedTool::new(
            PathGuardedTool::new(ContentSearchTool::new(security.clone()), security.clone()),
            security,
        )),
    ]
}

pub fn register_skill_tools(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
) {
    register_skill_tools_with_context(tools_registry, skills, security, &[]);
}

/// Register skill-defined tools with full context for builtin kinds.
/// `unfiltered_registry` provides the pre-policy tool list for `kind = "builtin"`
/// delegation.
pub fn register_skill_tools_with_context(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
    unfiltered_registry: &[Arc<dyn Tool>],
) {
    register_skill_tools_with_context_and_runtime(
        tools_registry,
        skills,
        security,
        unfiltered_registry,
        Arc::new(NativeRuntime::new()),
    );
}

pub fn register_skill_tools_with_context_and_runtime(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
    unfiltered_registry: &[Arc<dyn Tool>],
    runtime: Arc<dyn RuntimeAdapter>,
) {
    if skills.is_empty() {
        return;
    }

    let before = tools_registry.len();
    let policy = Arc::clone(&security);
    let skill_tools = crate::skills::skills_to_tools_with_context_and_runtime(
        skills,
        security,
        unfiltered_registry,
        runtime,
    );
    let existing_names: std::collections::HashSet<String> = tools_registry
        .iter()
        .map(|t| t.name().to_string())
        .collect();
    for tool in skill_tools {
        if existing_names.contains(tool.name()) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "Skill tool '{}' shadows built-in tool, skipping",
                    tool.name()
                )
            );
        } else if policy.is_tool_excluded(tool.name()) {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "Skill tool '{}' denied by excluded_tools, skipping",
                    tool.name()
                )
            );
        } else {
            tools_registry.push(tool);
        }
    }
    let registered = tools_registry.len() - before;

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
        &format!(
            "Registered {} skill tool(s) from {} skill(s): {}",
            registered,
            skills.len(),
            skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
    );
}

pub async fn collect_mcp_elevation_arcs(registry: &Arc<McpRegistry>) -> Vec<Arc<dyn Tool>> {
    let mut arcs: Vec<Arc<dyn Tool>> = Vec::new();
    for name in registry.tool_names() {
        if let Some(def) = registry.get_tool_def(&name).await {
            arcs.push(Arc::new(McpToolWrapper::new(
                name,
                def,
                Arc::clone(registry),
            )));
        }
    }
    arcs
}

/// Build the two generic MCP capability tools (`mcp_resources`, `mcp_prompts`),
/// including each only when the access `policy` admits its name. A `None` policy
/// admits both. Returned as `Arc<dyn Tool>` ready to register and/or expose to
/// delegates.
pub fn build_mcp_capability_tools(
    registry: &Arc<McpRegistry>,
    policy: Option<&zeroclaw_tools::tool_search::ToolAccessPolicy>,
) -> Vec<Arc<dyn Tool>> {
    let admit = |name: &str| policy.is_none_or(|p| p.is_tool_allowed(name));
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    if admit("mcp_resources") {
        out.push(Arc::new(McpResourcesTool::new(Arc::clone(registry))));
    }
    if admit("mcp_prompts") {
        out.push(Arc::new(McpPromptsTool::new(Arc::clone(registry))));
    }
    out
}

pub const BUILTIN_TOOL_INTEGRATIONS: &[(&str, &str)] = &[
    ("Shell", "Terminal command execution"),
    ("File System", "Read/write files"),
    ("Weather", "Forecasts & conditions (wttr.in)"),
    (
        "Spawn SubAgent",
        "Spawn an ephemeral SubAgent that inherits this agent's identity",
    ),
];

/// Bundled return values from tool registry construction.
/// Named struct to avoid an ever-growing positional tuple that's painful
/// to destructure across many callers.
#[allow(clippy::type_complexity)]
pub struct AllToolsResult {
    pub tools: Vec<Box<dyn Tool>>,
    pub delegate_handle: Option<DelegateParentToolsHandle>,
    pub ask_user_handle: Option<PerToolChannelHandle>,
    pub channel_room_handle: Option<PerToolChannelHandle>,
    pub reaction_handle: PerToolChannelHandle,
    pub poll_handle: Option<PerToolChannelHandle>,
    pub escalate_handle: Option<PerToolChannelHandle>,
    /// Pre-boxed Arcs of every tool (before policy filter). Used by
    /// skill-scoped builtin elevation to resolve targets at registration.
    pub unfiltered_tool_arcs: Vec<Arc<dyn Tool>>,
}

/// Create full tool registry including memory tools and optional Composio
#[allow(
    clippy::implicit_hasher,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
pub fn all_tools(
    config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    risk_profile: &zeroclaw_config::schema::RiskProfileConfig,
    agent_alias: &str,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    composio_entity_id: Option<&str>,
    browser_config: &zeroclaw_config::schema::BrowserConfig,
    http_config: &zeroclaw_config::schema::HttpRequestConfig,
    web_fetch_config: &zeroclaw_config::schema::WebFetchConfig,
    workspace_dir: &std::path::Path,
    agents: &HashMap<String, AliasedAgentConfig>,
    fallback_api_key: Option<&str>,
    root_config: &zeroclaw_config::schema::Config,
    canvas_store: Option<CanvasStore>,
    is_subagent_caller: bool,
    tui_env: Option<HashMap<String, String>>,
) -> AllToolsResult {
    all_tools_with_runtime(
        config,
        security,
        risk_profile,
        agent_alias,
        Arc::new(NativeRuntime::new()),
        memory,
        composio_key,
        composio_entity_id,
        browser_config,
        http_config,
        web_fetch_config,
        workspace_dir,
        agents,
        fallback_api_key,
        root_config,
        canvas_store,
        is_subagent_caller,
        tui_env,
        None,
        None,
        None,
    )
}

/// Peer groups that include `agent_alias`, cloned from `config`. Used as the
/// live resolver body for `send_via` authority (and the snapshot fallback).
fn filter_agent_peer_groups(
    config: &Config,
    agent_alias: &str,
) -> HashMap<String, zeroclaw_config::multi_agent::PeerGroupConfig> {
    config
        .peer_groups
        .iter()
        .filter(|(_, pg)| pg.agents.iter().any(|a| a.as_str() == agent_alias))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Create full tool registry including memory tools and optional Composio.
#[allow(
    clippy::implicit_hasher,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
pub fn all_tools_with_runtime(
    config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    risk_profile: &zeroclaw_config::schema::RiskProfileConfig,
    agent_alias: &str,
    runtime: Arc<dyn RuntimeAdapter>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    composio_entity_id: Option<&str>,
    browser_config: &zeroclaw_config::schema::BrowserConfig,
    http_config: &zeroclaw_config::schema::HttpRequestConfig,
    web_fetch_config: &zeroclaw_config::schema::WebFetchConfig,
    workspace_dir: &std::path::Path,
    agents: &HashMap<String, AliasedAgentConfig>,
    fallback_api_key: Option<&str>,
    root_config: &zeroclaw_config::schema::Config,
    canvas_store: Option<CanvasStore>,
    is_subagent_caller: bool,
    tui_env: Option<HashMap<String, String>>,
    sop_engine: Option<Arc<Mutex<SopEngine>>>,
    sop_audit: Option<Arc<SopAuditLogger>>,
    // Live config handle for `send_via` peer-group authority. `Some` from the
    // channel daemon (so reloads take effect); `None` for one-shot / non-channel
    // callers, which fall back to a snapshot of `root_config`.
    live_config: Option<Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>>,
) -> AllToolsResult {
    let has_shell_access = runtime.has_shell_access();
    let persistent_writes = runtime.has_filesystem_access();
    let runtime_kind = root_config.runtime.kind.as_wire();
    let sandbox_cfg = risk_profile.sandbox_config();
    let sandbox = create_sandbox(&sandbox_cfg, runtime_kind, Some(&security.workspace_dir));
    // Keep a shared runtime adapter available after constructing ShellTool.
    // Independent agentic delegates use it later to build the target-owned tool
    // registry; bounded delegates continue to use the parent `tool_arcs`
    // snapshot below.
    let mut tool_arcs: Vec<Arc<dyn Tool>> = vec![
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(
                ShellTool::new_with_sandbox(security.clone(), runtime.clone(), sandbox)
                    .with_timeout_secs(if security.shell_timeout_secs > 0 {
                        security.shell_timeout_secs
                    } else {
                        root_config.shell_tool.timeout_secs
                    })
                    .with_tui_env(tui_env)
                    .with_persistent_writes(persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileReadTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileWriteTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(
                FileEditTool::new_with_persistence(security.clone(), persistent_writes),
                security.clone(),
            ),
            security.clone(),
        )),
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(GlobSearchTool::new(security.clone()), security.clone()),
            security.clone(),
        )),
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(ContentSearchTool::new(security.clone()), security.clone()),
            security.clone(),
        )),
        Arc::new(CronAddTool::new(
            config.clone(),
            security.clone(),
            agent_alias,
        )),
        Arc::new(CronListTool::new(config.clone())),
        Arc::new(CronRemoveTool::new(
            config.clone(),
            security.clone(),
            agent_alias,
        )),
        Arc::new(CronUpdateTool::new(
            config.clone(),
            security.clone(),
            agent_alias,
        )),
        Arc::new(CronRunTool::new(config.clone(), security.clone())),
        Arc::new(CronRunsTool::new(config.clone())),
        Arc::new(MemoryStoreTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone())),
        Arc::new(MemoryForgetTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryExportTool::new(memory.clone())),
        Arc::new(MemoryPurgeTool::new(memory.clone(), security.clone())),
        Arc::new(ScheduleTool::new(
            security.clone(),
            root_config.clone(),
            agent_alias,
        )),
        Arc::new(
            SpawnSubagentTool::new(Arc::new(root_config.clone()), agent_alias, security.clone())
                .with_subagent_caller(is_subagent_caller),
        ),
        Arc::new(SendMessageToPeerTool::new(
            Arc::new(root_config.clone()),
            agent_alias,
        )),
        Arc::new(ModelRoutingConfigTool::new(
            config.clone(),
            security.clone(),
        )),
        Arc::new(ModelSwitchTool::new(security.clone(), config.clone())),
        Arc::new(ProxyConfigTool::new(config.clone(), security.clone())),
        Arc::new(GitOperationsTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )),
        Arc::new(PushoverTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )),
        Arc::new(CalculatorTool::new()),
        Arc::new(WeatherTool::new()),
        Arc::new(CanvasTool::new(canvas_store.unwrap_or_default())),
        Arc::new(TodoWriteTool::new()),
    ];

    // A SubAgent runs as an ephemeral clone of its parent and inherits the
    // parent's model verbatim; it must not be able to switch the active
    // model out from under the parent (the switch signal is process-wide).
    if is_subagent_caller {
        tool_arcs.retain(|tool| tool.name() != ModelSwitchTool::NAME);
    }

    // Register discord_search if any configured Discord alias has
    // archive enabled. Multiple Discord aliases are supported (one per
    // bot/server set); the search tool reads from a shared archive DB
    // so it's enabled when at least one alias archives.
    if root_config.channels.discord.values().any(|d| d.archive) {
        match zeroclaw_memory::SqliteMemory::new_named("sqlite", &config.data_dir, "discord") {
            Ok(discord_mem) => {
                tool_arcs.push(Arc::new(DiscordSearchTool::new(Arc::new(discord_mem))));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "discord_search: failed to open discord.db"
                );
            }
        }
    }

    // email_search — registered when at least one email channel is enabled
    {
        let email_configs: std::collections::HashMap<
            String,
            zeroclaw_config::scattered_types::EmailConfig,
        > = root_config
            .channels
            .email
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        if !email_configs.is_empty() {
            let auth_service = if email_configs.values().any(|c| c.oauth2.is_some()) {
                Some(Arc::new(
                    zeroclaw_providers::auth::AuthService::from_config(root_config),
                ))
            } else {
                None
            };
            let configs = Arc::new(email_configs);
            tool_arcs.push(Arc::new(EmailSearchTool::new(
                Arc::clone(&configs),
                auth_service.clone(),
            )));
            tool_arcs.push(Arc::new(EmailReadTool::new(
                Arc::clone(&configs),
                auth_service,
            )));
        }
    }

    // LLM task tool — registered using the calling agent's provider
    if let Some((family, alias, entry)) = root_config.resolved_model_provider_for_agent(agent_alias)
    {
        let llm_task_provider = family.to_string();
        let llm_task_model = entry
            .model
            .clone()
            .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
        let llm_task_runtime_options =
            zeroclaw_providers::provider_runtime_options_for_alias(root_config, family, alias);
        tool_arcs.push(Arc::new(LlmTaskTool::new(
            security.clone(),
            llm_task_provider,
            llm_task_model,
            entry.temperature,
            entry.api_key.clone(),
            llm_task_runtime_options,
        )));
    }

    if matches!(
        root_config.effective_skills_prompt_mode(agent_alias),
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
    ) {
        // ReadSkillTool now holds full config to support all skill sources:
        // workspace skills, open-skills, agent-bound bundles, and plugin skills.
        tool_arcs.push(Arc::new(ReadSkillTool::new(
            config.clone(),
            agent_alias.to_string(),
        )));
    }

    if browser_config.enabled {
        // Add legacy browser_open tool for simple URL opening
        match BrowserOpenTool::new_with_private_hosts(
            security.clone(),
            browser_config.allowed_domains.clone(),
            browser_config.allowed_private_hosts.clone(),
        ) {
            Ok(tool) => {
                tool_arcs.push(Arc::new(tool));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "browser_open: failed to construct tool, skipping registration"
                );
            }
        }
        // Add full browser automation tool (pluggable backend)
        match BrowserTool::new_with_backend(
            security.clone(),
            browser_config.allowed_domains.clone(),
            browser_config.session_name.clone(),
            browser_config.backend.clone(),
            browser_config.headed,
            browser_config.native_headless,
            browser_config.native_webdriver_url.clone(),
            browser_config.native_chrome_path.clone(),
            ComputerUseConfig {
                endpoint: browser_config.computer_use.endpoint.clone(),
                api_key: browser_config.computer_use.api_key.clone(),
                timeout_ms: browser_config.computer_use.timeout_ms,
                allow_remote_endpoint: browser_config.computer_use.allow_remote_endpoint,
                window_allowlist: browser_config.computer_use.window_allowlist.clone(),
                max_coordinate_x: browser_config.computer_use.max_coordinate_x,
                max_coordinate_y: browser_config.computer_use.max_coordinate_y,
            },
            browser_config.allowed_private_hosts.clone(),
        ) {
            Ok(tool) => {
                tool_arcs.push(Arc::new(RateLimitedTool::new(tool, security.clone())));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "browser: failed to construct tool, skipping registration"
                );
            }
        }
    }

    // Browser delegation tool (conditionally registered; requires shell access)
    if root_config.browser_delegate.enabled {
        if has_shell_access {
            tool_arcs.push(Arc::new(BrowserDelegateTool::new(
                security.clone(),
                root_config.browser_delegate.clone(),
            )));
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "browser_delegate: skipped registration because the current runtime does not allow shell access"
            );
        }
    }

    if http_config.enabled {
        match HttpRequestTool::new_with_config(
            security.clone(),
            http_config.allowed_domains.clone(),
            http_config.max_response_size,
            http_config.timeout_secs,
            http_config.allow_private_hosts,
            http_config.allowed_private_hosts.clone(),
            root_config.config_path.clone(),
            root_config.secrets.encrypt,
        ) {
            Ok(tool) => {
                tool_arcs.push(Arc::new(RateLimitedTool::new(tool, security.clone())));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "http_request: failed to construct tool, skipping registration"
                );
            }
        }
    }

    if web_fetch_config.enabled {
        match WebFetchTool::new(
            security.clone(),
            web_fetch_config.allowed_domains.clone(),
            web_fetch_config.blocked_domains.clone(),
            web_fetch_config.max_response_size,
            web_fetch_config.timeout_secs,
            web_fetch_config.firecrawl.clone(),
            web_fetch_config.allowed_private_hosts.clone(),
        ) {
            Ok(tool) => {
                tool_arcs.push(Arc::new(RateLimitedTool::new(tool, security.clone())));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "web_fetch: failed to construct tool, skipping registration"
                );
            }
        }
    }

    // Text browser tool (headless text-based browser rendering)
    if root_config.text_browser.enabled {
        match TextBrowserTool::new_with_private_hosts(
            security.clone(),
            root_config.text_browser.preferred_browser.clone(),
            root_config.text_browser.timeout_secs,
            root_config.text_browser.allowed_private_hosts.clone(),
        ) {
            Ok(tool) => {
                tool_arcs.push(Arc::new(tool));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "text_browser: failed to construct tool, skipping registration"
                );
            }
        }
    }

    // Web search tool (enabled by default for GLM and other models)
    if root_config.web_search.enabled {
        tool_arcs.push(Arc::new(WebSearchTool::new_with_config(
            root_config.web_search.search_provider.clone(),
            root_config.web_search.brave_api_key.clone(),
            root_config.web_search.tavily_api_key.clone(),
            root_config.web_search.jina_api_key.clone(),
            root_config.web_search.searxng_instance_url.clone(),
            root_config.web_search.max_results,
            root_config.web_search.timeout_secs,
            root_config.config_path.clone(),
            root_config.secrets.encrypt,
        )));
    }

    // Notion API tool (conditionally registered)
    if root_config.notion.enabled {
        let notion_api_key = if root_config.notion.api_key.trim().is_empty() {
            std::env::var("NOTION_API_KEY").unwrap_or_default()
        } else {
            root_config.notion.api_key.trim().to_string()
        };
        if notion_api_key.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Notion tool enabled but no API key found (set notion.api_key or NOTION_API_KEY env var)"
            );
        } else {
            tool_arcs.push(Arc::new(NotionTool::new(notion_api_key, security.clone())));
        }
    }

    // Jira integration (config-gated)
    if root_config.jira.enabled {
        let api_token = if root_config.jira.api_token.trim().is_empty() {
            std::env::var("JIRA_API_TOKEN").unwrap_or_default()
        } else {
            root_config.jira.api_token.trim().to_string()
        };
        if api_token.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Jira tool enabled but no API token found (set jira.api_token or JIRA_API_TOKEN env var)"
            );
        } else if root_config.jira.base_url.trim().is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "Jira tool enabled but jira.base_url is empty — skipping registration"
            );
        } else {
            let email = root_config
                .jira
                .email
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from);
            if email.is_some() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Jira tool: Cloud mode (API v3, Basic auth)"
                );
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "Jira tool: Server/DC mode (API v2, Bearer auth)"
                );
            }
            tool_arcs.push(Arc::new(JiraTool::new(
                root_config.jira.base_url.trim().to_string(),
                email,
                api_token,
                root_config.jira.allowed_actions.clone(),
                security.clone(),
                root_config.jira.timeout_secs,
            )));
        }
    }

    // Project delivery intelligence
    if root_config.project_intel.enabled {
        tool_arcs.push(Arc::new(ProjectIntelTool::new(
            root_config.project_intel.default_language.clone(),
            root_config.project_intel.risk_sensitivity.clone(),
        )));
        // Report template tool — direct access to template engine
        tool_arcs.push(Arc::new(ReportTemplateTool::new()));
    }

    // MCSS Security Operations
    if root_config.security_ops.enabled {
        tool_arcs.push(Arc::new(SecurityOpsTool::new(
            root_config.security_ops.clone(),
        )));
    }

    // Backup tool (enabled by default)
    if root_config.backup.enabled {
        tool_arcs.push(Arc::new(BackupTool::new(
            workspace_dir.to_path_buf(),
            root_config.backup.include_dirs.clone(),
            root_config.backup.max_keep,
        )));
    }

    // Data management tool (disabled by default)
    if root_config.data_retention.enabled {
        tool_arcs.push(Arc::new(DataManagementTool::new(
            workspace_dir.to_path_buf(),
            root_config.data_retention.retention_days,
        )));
    }

    // Cloud operations advisory tools (read-only analysis)
    if root_config.cloud_ops.enabled {
        tool_arcs.push(Arc::new(CloudOpsTool::new(root_config.cloud_ops.clone())));
        tool_arcs.push(Arc::new(CloudPatternsTool::new()));
    }

    // Google Workspace CLI (gws) integration — requires shell access
    if root_config.google_workspace.enabled && has_shell_access {
        tool_arcs.push(Arc::new(GoogleWorkspaceTool::new(
            security.clone(),
            root_config.google_workspace.allowed_services.clone(),
            root_config.google_workspace.allowed_operations.clone(),
            root_config.google_workspace.credentials_path.clone(),
            root_config.google_workspace.default_account.clone(),
            root_config.google_workspace.rate_limit_per_minute,
            root_config.google_workspace.timeout_secs,
            root_config.google_workspace.audit_log,
        )));
    } else if root_config.google_workspace.enabled {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "google_workspace: skipped registration because shell access is unavailable"
        );
    }

    // Claude Code delegation tool
    if root_config.claude_code.enabled {
        tool_arcs.push(Arc::new(RateLimitedTool::new(
            ClaudeCodeTool::new(security.clone(), root_config.claude_code.clone()),
            security.clone(),
        )));
    }

    // Claude Code task runner with Slack progress and SSH handoff
    if root_config.claude_code_runner.enabled {
        let gateway_url = format!(
            "http://{}:{}",
            root_config.gateway.host, root_config.gateway.port
        );
        tool_arcs.push(Arc::new(RateLimitedTool::new(
            ClaudeCodeRunnerTool::new(
                security.clone(),
                root_config.claude_code_runner.clone(),
                gateway_url,
            ),
            security.clone(),
        )));
    }

    // Codex CLI delegation tool
    if root_config.codex_cli.enabled {
        tool_arcs.push(Arc::new(RateLimitedTool::new(
            CodexCliTool::new(security.clone(), root_config.codex_cli.clone()),
            security.clone(),
        )));
    }

    // Gemini CLI delegation tool
    if root_config.gemini_cli.enabled {
        tool_arcs.push(Arc::new(RateLimitedTool::new(
            GeminiCliTool::new(security.clone(), root_config.gemini_cli.clone()),
            security.clone(),
        )));
    }

    // OpenCode CLI delegation tool
    if root_config.opencode_cli.enabled {
        tool_arcs.push(Arc::new(RateLimitedTool::new(
            OpenCodeCliTool::new(security.clone(), root_config.opencode_cli.clone()),
            security.clone(),
        )));
    }

    // Vision tools are always available
    tool_arcs.push(Arc::new(ScreenshotTool::new(security.clone())));
    tool_arcs.push(Arc::new(RateLimitedTool::new(
        PathGuardedTool::new(ImageInfoTool::new(security.clone()), security.clone()),
        security.clone(),
    )));

    if let Ok(backend) = zeroclaw_infra::make_session_backend(
        &config.data_dir,
        &config.channels.session_backend,
        config.channels.postgres_url.as_deref(),
        config.channels.pool_size,
    ) {
        tool_arcs.push(Arc::new(SessionsCurrentTool::new(backend.clone())));
        tool_arcs.push(Arc::new(SessionsListTool::new(backend.clone())));
        tool_arcs.push(Arc::new(SessionsHistoryTool::new(
            backend.clone(),
            security.clone(),
        )));
        tool_arcs.push(Arc::new(SessionsSendTool::new(backend, security.clone())));
    }

    // LinkedIn integration (config-gated)
    if root_config.linkedin.enabled {
        tool_arcs.push(Arc::new(LinkedInTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
            root_config.linkedin.api_version.clone(),
            root_config.linkedin.content.clone(),
            root_config.linkedin.image.clone(),
        )));
    }

    // Standalone image generation tool (config-gated)
    if root_config.image_gen.enabled {
        tool_arcs.push(Arc::new(ImageGenTool::new_with_persistence(
            security.clone(),
            workspace_dir.to_path_buf(),
            root_config.image_gen.default_model.clone(),
            root_config.image_gen.api_key_env.clone(),
            persistent_writes,
        )));
    }

    // File upload tool — enabled iff [file_upload].url is set
    if root_config
        .file_upload
        .url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty())
    {
        tool_arcs.push(Arc::new(FileUploadTool::new(
            security.clone(),
            root_config.file_upload.clone(),
        )));
    }

    // File upload bundle tool — enabled iff [file_upload_bundle].url is set
    if root_config
        .file_upload_bundle
        .url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty())
    {
        tool_arcs.push(Arc::new(FileUploadBundleTool::new(
            security.clone(),
            root_config.file_upload_bundle.clone(),
        )));
    }

    // File download tool — enabled iff [file_download].url is set
    if root_config
        .file_download
        .url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty())
    {
        tool_arcs.push(Arc::new(FileDownloadTool::new_with_persistence(
            security.clone(),
            root_config.file_download.clone(),
            persistent_writes,
        )));
    }

    // Poll tool — always registered; owns its own late-bound channel map.
    let poll_handle: PerToolChannelHandle = Arc::new(RwLock::new(HashMap::new()));
    tool_arcs.push(Arc::new(PollTool::new(
        security.clone(),
        Arc::clone(&poll_handle),
    )));

    // SOP tools (registered when engine handle is provided)
    if let Some(ref sop_engine) = sop_engine {
        tool_arcs.push(Arc::new(SopListTool::new(Arc::clone(sop_engine))));
        if let Some(ref sop_audit) = sop_audit {
            tool_arcs.push(Arc::new(
                SopExecuteTool::new(Arc::clone(sop_engine)).with_audit(Arc::clone(sop_audit)),
            ));
            tool_arcs.push(Arc::new(
                SopAdvanceTool::new(Arc::clone(sop_engine)).with_audit(Arc::clone(sop_audit)),
            ));
            tool_arcs.push(Arc::new(
                SopApproveTool::new(Arc::clone(sop_engine))
                    .with_agent_alias(agent_alias)
                    .with_audit(Arc::clone(sop_audit)),
            ));
        } else {
            tool_arcs.push(Arc::new(SopExecuteTool::new(Arc::clone(sop_engine))));
            tool_arcs.push(Arc::new(SopAdvanceTool::new(Arc::clone(sop_engine))));
            tool_arcs.push(Arc::new(
                SopApproveTool::new(Arc::clone(sop_engine)).with_agent_alias(agent_alias),
            ));
        }
        tool_arcs.push(Arc::new(
            SopStatusTool::new(Arc::clone(sop_engine))
                .with_collector(crate::sop::SopMetricsCollector::shared()),
        ));
        if root_config.sop.procedural_memory_enabled {
            tool_arcs.push(Arc::new(SopWorkshopTool::new(
                Arc::clone(sop_engine),
                workspace_dir.to_path_buf(),
            )));
        }
    }

    if let Some(key) = composio_key
        && !key.is_empty()
    {
        tool_arcs.push(Arc::new(ComposioTool::new(
            key,
            composio_entity_id,
            security.clone(),
        )));
    }

    // Emoji reaction tool — always registered; owns its own late-bound channel map.
    let reaction_handle: PerToolChannelHandle = Arc::new(RwLock::new(HashMap::new()));
    let reaction_tool = ReactionTool::new(security.clone(), Arc::clone(&reaction_handle));
    tool_arcs.push(Arc::new(reaction_tool));

    // Unified forge operations tool, routes through the git channel via the
    // same late-bound channel map as the reaction tool. Resource/action grid
    // plus a raw catch-all over the channel's single forge_request transport.
    let git_forge_tool = GitForgeTool::new(security.clone(), Arc::clone(&reaction_handle));
    tool_arcs.push(Arc::new(git_forge_tool));

    // Channel room-management tool — always registered; owns its own late-bound channel map.
    let channel_room_handle: Option<PerToolChannelHandle> =
        Some(Arc::new(RwLock::new(HashMap::new())));
    let channel_room_tool = ChannelRoomTool::new(
        security.clone(),
        channel_room_handle.as_ref().cloned().unwrap(),
    );
    tool_arcs.push(Arc::new(channel_room_tool));

    // Interactive ask_user tool — always registered; owns its own late-bound channel map.
    let ask_user_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    let ask_user_tool =
        AskUserTool::new(security.clone(), ask_user_handle.as_ref().cloned().unwrap());
    tool_arcs.push(Arc::new(ask_user_tool));

    {
        let agent_peer_groups: AgentPeerGroupResolver = if let Some(live) = live_config.clone() {
            let alias = agent_alias.to_string();
            Arc::new(move || filter_agent_peer_groups(&live.read(), &alias))
        } else {
            let snapshot = filter_agent_peer_groups(root_config, agent_alias);
            Arc::new(move || snapshot.clone())
        };
        tool_arcs.push(Arc::new(SendViaTool::new(
            security.clone(),
            ask_user_handle.as_ref().cloned().unwrap(),
            agent_peer_groups,
        )));
    }

    // Human escalation tool — always registered; owns its own late-bound channel map.
    let escalate_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    let escalate_tool = EscalateToHumanTool::new(
        security.clone(),
        root_config.escalation.alert_channels.clone(),
        escalate_handle.as_ref().cloned().unwrap(),
    );
    tool_arcs.push(Arc::new(escalate_tool));

    // Microsoft 365 Graph API integration
    if root_config.microsoft365.enabled {
        let ms_cfg = &root_config.microsoft365;
        let tenant_id = ms_cfg
            .tenant_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        let client_id = ms_cfg
            .client_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_string();
        if !tenant_id.is_empty() && !client_id.is_empty() {
            // Fail fast: client_credentials flow requires a client_secret at registration time.
            if ms_cfg.auth_flow.trim() == "client_credentials"
                && ms_cfg
                    .client_secret
                    .as_deref()
                    .is_none_or(|s| s.trim().is_empty())
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "microsoft365: client_credentials auth_flow requires a non-empty client_secret"
                );
                return AllToolsResult {
                    unfiltered_tool_arcs: tool_arcs.clone(),
                    tools: boxed_registry_from_arcs(tool_arcs),
                    delegate_handle: None,
                    ask_user_handle,
                    channel_room_handle,
                    reaction_handle,
                    poll_handle: Some(poll_handle),
                    escalate_handle,
                };
            }

            let resolved = zeroclaw_tools::microsoft365::types::Microsoft365ResolvedConfig {
                tenant_id,
                client_id,
                client_secret: ms_cfg.client_secret.clone(),
                auth_flow: ms_cfg.auth_flow.clone(),
                scopes: ms_cfg.scopes.clone(),
                token_cache_encrypted: ms_cfg.token_cache_encrypted,
                user_id: ms_cfg.user_id.as_deref().unwrap_or("me").to_string(),
            };
            // Store token cache in the config directory (next to config.toml),
            // not the workspace directory, to keep bearer tokens out of the
            // project tree.
            let cache_dir = root_config.config_path.parent().unwrap_or(workspace_dir);
            match Microsoft365Tool::new(resolved, security.clone(), cache_dir) {
                Ok(tool) => tool_arcs.push(Arc::new(tool)),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "microsoft365: failed to initialize tool"
                    );
                }
            }
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "microsoft365: skipped registration because tenant_id or client_id is empty"
            );
        }
    }

    // Knowledge graph tool
    if root_config.knowledge.enabled {
        let db_path_str = root_config.knowledge.db_path.replace(
            '~',
            &directories::UserDirs::new()
                .map(|u| u.home_dir().to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string()),
        );
        let db_path = std::path::PathBuf::from(&db_path_str);
        match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &db_path,
            root_config.knowledge.max_nodes,
        ) {
            Ok(graph) => {
                tool_arcs.push(Arc::new(KnowledgeTool::new(Arc::new(graph))));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "knowledge graph disabled due to init error"
                );
            }
        }
    }

    // Add delegation tool when agents are configured
    let delegate_global_credential = fallback_api_key.and_then(|value| {
        let trimmed_value = value.trim();
        (!trimmed_value.is_empty()).then(|| trimmed_value.to_owned())
    });
    let provider_runtime_options =
        zeroclaw_providers::provider_runtime_options_for_agent(root_config, agent_alias);

    let delegate_handle: Option<DelegateParentToolsHandle> = if agents.is_empty() {
        None
    } else {
        let delegate_agents: HashMap<String, AliasedAgentConfig> = agents
            .iter()
            .map(|(name, cfg)| (name.clone(), cfg.clone()))
            .collect();
        let parent_tools = Arc::new(RwLock::new(tool_arcs.clone()));
        let delegate_tool = DelegateTool::new_with_options(
            delegate_agents,
            delegate_global_credential.clone(),
            security.clone(),
            provider_runtime_options.clone(),
        )
        .with_parent_tools(Arc::clone(&parent_tools))
        .with_runtime(runtime.clone())
        .with_multimodal_config(root_config.multimodal.clone())
        .with_delegate_config(root_config.delegate.clone())
        .with_workspace_dir(workspace_dir.to_path_buf())
        .with_memory(memory.clone())
        .with_providers_models({
            let mut m: std::collections::HashMap<
                String,
                std::collections::HashMap<String, zeroclaw_config::schema::ModelProviderConfig>,
            > = std::collections::HashMap::new();
            for (t, a, base) in root_config.providers.models.iter_entries() {
                m.entry(t.to_string())
                    .or_default()
                    .insert(a.to_string(), base.clone());
            }
            m
        })
        .with_risk_profiles(root_config.risk_profiles.clone())
        .with_runtime_profiles(root_config.runtime_profiles.clone())
        .with_skill_bundles(root_config.skill_bundles.clone())
        .with_root_config(config.clone())
        .with_caller_alias(agent_alias);
        tool_arcs.push(Arc::new(delegate_tool));
        Some(parent_tools)
    };

    // Verifiable Intent tool (opt-in via config)
    if root_config.verifiable_intent.enabled {
        let strictness = match root_config.verifiable_intent.strictness.as_str() {
            "permissive" => crate::verifiable_intent::StrictnessMode::Permissive,
            _ => crate::verifiable_intent::StrictnessMode::Strict,
        };
        tool_arcs.push(Arc::new(VerifiableIntentTool::new(
            security.clone(),
            strictness,
        )));
    }

    // ── WASM plugin tools (requires plugins-wasm feature) ──
    #[cfg(feature = "plugins-wasm")]
    {
        let plugin_path = config.plugins.resolved_plugins_dir();

        if plugin_path.exists() && config.plugins.enabled {
            let signature_mode = zeroclaw_plugins::host::PluginHost::resolve_signature_mode(
                &config.plugins.security.signature_mode,
            );
            let trusted_publisher_keys = config.plugins.security.trusted_publisher_keys.clone();
            match zeroclaw_plugins::host::PluginHost::from_plugins_dir_with_security(
                &plugin_path,
                signature_mode,
                trusted_publisher_keys,
            ) {
                Ok(host) => {
                    let details = host.tool_plugin_details();
                    let discovered_count = details.len();
                    let mut registered_count = 0_usize;
                    let plugin_limits = zeroclaw_plugins::component::PluginLimits {
                        call_fuel: config.plugins.limits.call_fuel,
                        max_memory_bytes: config
                            .plugins
                            .limits
                            .max_memory_mb
                            .saturating_mul(1024 * 1024),
                        max_table_elements: config.plugins.limits.max_table_elements,
                        max_instances: config.plugins.limits.max_instances,
                    };
                    for (manifest, wasm_path) in details {
                        let plugin_config = config
                            .plugins
                            .entry_config(&manifest.name)
                            .cloned()
                            .unwrap_or_default();
                        let tool = (|| -> anyhow::Result<_> {
                            let scope =
                                zeroclaw_plugins::instance::PluginInstanceScope::from_manifest(
                                    manifest,
                                    zeroclaw_plugins::PluginCapability::Tool,
                                    manifest.name.clone(),
                                    manifest.permissions.iter().copied(),
                                )?;
                            zeroclaw_plugins::wasm_tool::WasmTool::from_wasm(
                                wasm_path.to_path_buf(),
                                scope,
                                plugin_config,
                                plugin_limits,
                            )
                        })();
                        match tool {
                            Ok(tool) => {
                                tool_arcs.push(Arc::new(tool));
                                registered_count += 1;
                            }
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Load
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "plugin": manifest.name,
                                            "error": format!("{e:#}"),
                                        })
                                    ),
                                    "Failed to register WASM plugin tool"
                                );
                            }
                        }
                    }
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "discovered": discovered_count,
                                "registered": registered_count,
                            })),
                        "Registered WASM plugin tools"
                    );
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Failed to load WASM plugins"
                    );
                }
            }
        }

        // Surface plugins stranded in a legacy install dir so they aren't
        // silently ignored — the user can relocate them with `plugin migrate`.
        if config.plugins.enabled {
            for legacy in zeroclaw_config::schema::legacy_plugin_dirs_with_entries(&config) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "legacy_dir": legacy.display().to_string()
                        })),
                    "Plugins in a legacy directory are not loaded; run `zeroclaw plugin migrate`"
                );
            }
        }
    }

    // Pipeline tool (execute_pipeline) — multi-step tool chaining.
    if root_config.pipeline.enabled {
        let pipeline_tools: Vec<Arc<dyn Tool>> = tool_arcs.clone();
        tool_arcs.push(Arc::new(PipelineTool::new(
            root_config.pipeline.clone(),
            pipeline_tools,
        )));
    }

    AllToolsResult {
        unfiltered_tool_arcs: tool_arcs.clone(),
        tools: boxed_registry_from_arcs(tool_arcs),
        delegate_handle,
        ask_user_handle,
        channel_room_handle,
        reaction_handle,
        poll_handle: Some(poll_handle),
        escalate_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::schema::{
        ApprovalGroupConfig, ApprovalPolicyConfig, BrowserConfig, Config, MemoryConfig,
        SopApprovalConfig,
    };

    #[tokio::test]
    async fn mcp_capability_tools_respect_policy() {
        use zeroclaw_tools::tool_search::ToolAccessPolicy;
        let registry = std::sync::Arc::new(McpRegistry::connect_all(&[]).await.unwrap());

        // No policy → both tools present.
        let both = build_mcp_capability_tools(&registry, None);
        let names: Vec<_> = both.iter().map(|t| t.name().to_string()).collect();
        assert!(names.contains(&"mcp_resources".to_string()));
        assert!(names.contains(&"mcp_prompts".to_string()));

        // Deny mcp_prompts → only mcp_resources present.
        let policy =
            ToolAccessPolicy::from_security(None, Some(&["mcp_prompts".to_string()]), None);
        let one = build_mcp_capability_tools(&registry, policy.as_ref());
        let names: Vec<_> = one.iter().map(|t| t.name().to_string()).collect();
        assert!(names.contains(&"mcp_resources".to_string()));
        assert!(!names.contains(&"mcp_prompts".to_string()));
    }

    fn test_config(tmp: &TempDir) -> Config {
        Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        }
    }

    #[test]
    fn default_tools_has_expected_count() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        assert_eq!(tools.len(), 6);
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn component_with_failed_metadata_probe_is_not_registered() {
        let tmp = TempDir::new().unwrap();
        let package_dir = tmp.path().join("plugins").join("metadata-probe");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(
            package_dir.join("manifest.toml"),
            "name = \"metadata-probe\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\n",
        )
        .unwrap();
        std::fs::write(package_dir.join("plugin.wasm"), b"not a component").unwrap();

        let mut config = test_config(&tmp);
        config.plugins.enabled = true;
        config.plugins.plugins_dir = tmp.path().join("plugins").display().to_string();
        let security = Arc::new(SecurityPolicy::default());
        let memory: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(
                &MemoryConfig {
                    backend: "markdown".into(),
                    ..MemoryConfig::default()
                },
                tmp.path(),
                None,
            )
            .unwrap(),
        );
        let browser = BrowserConfig {
            enabled: false,
            ..BrowserConfig::default()
        };

        let tools = all_tools(
            Arc::new(config.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            memory,
            None,
            None,
            &browser,
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &config,
            None,
            false,
            None,
        )
        .tools;

        assert!(
            tools.iter().all(|tool| tool.name() != "metadata-probe"),
            "a component whose required metadata probe fails must not receive manifest fallback metadata"
        );
    }

    #[test]
    fn sop_tools_absent_when_engine_not_provided() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            allowed_domains: vec![],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

        let sop_tool_names = [
            "sop_list",
            "sop_execute",
            "sop_advance",
            "sop_approve",
            "sop_status",
        ];
        for name in &sop_tool_names {
            assert!(
                !names.contains(name),
                "SOP tool '{name}' must not be registered when engine is absent"
            );
        }
    }

    #[test]
    fn sop_tools_present_when_engine_provided() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            allowed_domains: vec![],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        // Build a minimal SOP engine — no sops_dir needed for this test.
        let engine = Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));

        let tools = all_tools_with_runtime(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            Arc::new(NativeRuntime::new()),
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
            Some(engine),
            None,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

        let sop_tool_names = [
            "sop_list",
            "sop_execute",
            "sop_advance",
            "sop_approve",
            "sop_status",
        ];
        for name in &sop_tool_names {
            assert!(
                names.contains(name),
                "SOP tool '{name}' must be registered when engine is provided"
            );
        }
        assert!(
            !names.contains(&"sop_workshop"),
            "sop_workshop must stay opt-in while procedural memory is disabled"
        );
    }

    #[test]
    fn sop_workshop_registered_only_when_procedural_memory_enabled() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            allowed_domains: vec![],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        cfg.sop.procedural_memory_enabled = true;

        let engine = Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));

        let tools = all_tools_with_runtime(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            Arc::new(NativeRuntime::new()),
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
            Some(engine),
            None,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

        assert!(
            names.contains(&"sop_workshop"),
            "sop_workshop must be registered when procedural memory is enabled"
        );
    }

    #[test]
    fn shared_sop_engine_arc_is_observed_by_multiple_registrations() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let cfg = test_config(&tmp);
        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let web = zeroclaw_config::schema::WebFetchConfig::default();
        let risk = zeroclaw_config::schema::RiskProfileConfig::default();

        let shared_engine = Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));
        let shared_audit = Arc::new(crate::sop::SopAuditLogger::new(mem.clone()));

        // Two independent registrations using clones of the same Arc — the
        // pattern the daemon uses when wiring gateway, channels, MQTT, and
        // RPC sessions from one engine pair.
        let session_a = all_tools_with_runtime(
            Arc::new(Config::default()),
            &security,
            &risk,
            "session-a",
            Arc::new(NativeRuntime::new()),
            mem.clone(),
            None,
            None,
            &browser,
            &http,
            &web,
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
            Some(shared_engine.clone()),
            Some(shared_audit.clone()),
            None,
        );
        let session_b = all_tools_with_runtime(
            Arc::new(Config::default()),
            &security,
            &risk,
            "session-b",
            Arc::new(NativeRuntime::new()),
            mem.clone(),
            None,
            None,
            &browser,
            &http,
            &web,
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
            Some(shared_engine.clone()),
            Some(shared_audit.clone()),
            None,
        );

        for tools in [&session_a.tools, &session_b.tools] {
            assert!(tools.iter().any(|t| t.name() == "sop_status"));
        }

        // Outer Arc + both registrations = 3+ strong refs. Confirms the
        // registries kept references to the same instance instead of
        // copying state.
        assert!(Arc::strong_count(&shared_engine) >= 3);
        assert!(Arc::strong_count(&shared_audit) >= 3);
    }

    #[tokio::test]
    async fn sop_approve_registry_binds_the_calling_agent_alias() {
        use crate::sop::types::{
            Sop, SopAdmissionPolicy, SopEvent, SopExecutionMode, SopPriority, SopRunAction,
            SopRunStatus, SopStep, SopStepKind, SopTrigger, SopTriggerSource,
        };

        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let mut groups = HashMap::new();
        groups.insert(
            "release".to_string(),
            ApprovalGroupConfig {
                members: vec!["agent:ZeroClawOperator".to_string()],
            },
        );
        let mut policies = HashMap::new();
        policies.insert(
            "prod".to_string(),
            ApprovalPolicyConfig {
                required_group: Some("release".to_string()),
                quorum: 1,
                request_route: None,
                escalation_route: None,
            },
        );
        let mut engine = SopEngine::new(zeroclaw_config::schema::SopConfig {
            approval: SopApprovalConfig { groups, policies },
            ..Default::default()
        })
        .with_approval_broker(Arc::new(crate::sop::approval::ApprovalBroker::disabled()));
        engine.set_sops_for_test(vec![Sop {
            name: "deploy".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: vec![SopTrigger::Manual],
            steps: vec![
                SopStep {
                    number: 1,
                    title: "gate".into(),
                    kind: SopStepKind::Execute,
                    requires_confirmation: true,
                    policy: Some("prod".into()),
                    ..SopStep::default()
                },
                SopStep {
                    number: 2,
                    title: "execute".into(),
                    kind: SopStepKind::Execute,
                    ..SopStep::default()
                },
            ],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: SopAdmissionPolicy::Parallel,
            max_pending_approvals: 0,
            agent: None,
        }]);
        let action = engine
            .start_run(
                "deploy",
                SopEvent {
                    source: SopTriggerSource::Manual,
                    topic: None,
                    payload: None,
                    timestamp: crate::sop::engine::now_iso8601(),
                },
            )
            .unwrap();
        let run_id = match action {
            SopRunAction::WaitApproval { run_id, .. } => run_id,
            other => panic!("expected WaitApproval, got {other:?}"),
        };
        let shared_engine = Arc::new(Mutex::new(engine));
        let cfg = test_config(&tmp);
        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let web = zeroclaw_config::schema::WebFetchConfig::default();
        let risk = zeroclaw_config::schema::RiskProfileConfig::default();

        let build = |agent_alias: &str, memory: Arc<dyn Memory>| {
            all_tools_with_runtime(
                Arc::new(Config::default()),
                &security,
                &risk,
                agent_alias,
                Arc::new(NativeRuntime::new()),
                memory,
                None,
                None,
                &browser,
                &http,
                &web,
                tmp.path(),
                &HashMap::new(),
                None,
                &cfg,
                None,
                false,
                None,
                Some(shared_engine.clone()),
                None,
                None,
            )
            .tools
        };
        let unauthorized_tools = build("ZeroClawAgent", mem.clone());
        let authorized_tools = build("ZeroClawOperator", mem);

        let unauthorized = unauthorized_tools
            .iter()
            .find(|tool| tool.name() == "sop_approve")
            .expect("unauthorized registry has sop_approve");
        let result = unauthorized
            .execute(serde_json::json!({ "run_id": run_id.clone() }))
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(
            shared_engine
                .lock()
                .unwrap()
                .get_run(&run_id)
                .map(|run| run.status),
            Some(SopRunStatus::WaitingApproval)
        );

        let authorized = authorized_tools
            .iter()
            .find(|tool| tool.name() == "sop_approve")
            .expect("authorized registry has sop_approve");
        let result = authorized
            .execute(serde_json::json!({ "run_id": run_id }))
            .await
            .unwrap();
        assert!(result.success, "authorized alias must resolve: {result:?}");
    }

    #[test]
    fn shared_store_tools_open_data_dir_not_per_agent_workspace() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data"); // shared store (writers' dir)
        let workspace_dir = tmp.path().join("agent-ws"); // per-agent, intentionally distinct
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&workspace_dir).unwrap();

        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let web = zeroclaw_config::schema::WebFetchConfig::default();
        let risk = zeroclaw_config::schema::RiskProfileConfig::default();

        // root_config: shared data_dir + a Discord alias that archives (this is
        // what gates discord_search registration).
        let mut root_config = test_config(&tmp);
        root_config.data_dir = data_dir.clone();
        root_config.channels.discord.insert(
            "oracle".to_string(),
            zeroclaw_config::schema::DiscordConfig {
                archive: true,
                ..Default::default()
            },
        );

        // `config` (arg 1) carries the canonical shared data_dir — exactly how
        // the production callers pass it (a clone of the runtime config).
        let config = Config {
            data_dir: data_dir.clone(),
            ..Config::default()
        };

        let tools = all_tools_with_runtime(
            Arc::new(config),
            &security,
            &risk,
            "test-agent",
            Arc::new(NativeRuntime::new()),
            mem,
            None,
            None,
            &browser,
            &http,
            &web,
            workspace_dir.as_path(), // DIFFERENT from data_dir
            &HashMap::new(),
            None,
            &root_config,
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .tools;

        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"discord_search"),
            "discord_search must register when a Discord alias archives"
        );
        assert!(
            names.iter().any(|n| n.starts_with("sessions")),
            "session tools must register"
        );

        // The fix: both stores open under the shared data_dir, never the
        // per-agent workspace. Pre-fix the readers created `memory/discord.db`
        // and `sessions/sessions.db` under the workspace_dir.
        assert!(
            !workspace_dir.join("memory").exists(),
            "discord_search must not open/create a store under the per-agent workspace_dir"
        );
        assert!(
            !workspace_dir.join("sessions").exists(),
            "session tools must not open/create a store under the per-agent workspace_dir"
        );
    }

    #[tokio::test]
    async fn sop_audit_memory_uses_agent_alias_not_default() {
        let tmp = TempDir::new().unwrap();
        let sops_dir = tmp.path().join("sops");
        std::fs::create_dir_all(&sops_dir).unwrap();

        let mut agents = HashMap::new();
        agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                ..Default::default()
            },
        );

        let config = Config {
            data_dir: tmp.path().join("data"),
            config_path: tmp.path().join("config.toml"),
            sop: zeroclaw_config::schema::SopConfig {
                sops_dir: Some(sops_dir.to_string_lossy().into_owned()),
                ..zeroclaw_config::schema::SopConfig::default()
            },
            agents: agents.clone(),
            ..Config::default()
        };

        // Using the session alias ("ops") must succeed even with no "default" agent.
        let mem = zeroclaw_memory::create_memory_for_agent(&config, "ops", None).await;
        assert!(
            mem.is_ok(),
            "create_memory_for_agent with session alias should succeed"
        );

        // The old hardcoded "default" must fail — proving the fix is load-bearing.
        let mem_default = zeroclaw_memory::create_memory_for_agent(&config, "default", None).await;
        assert!(
            mem_default.is_err(),
            "create_memory_for_agent(\"default\") must fail when agents.default is absent"
        );
    }

    /// A runtime that reports an ephemeral workspace (no host persistence) while
    /// delegating real shell execution to `NativeRuntime`. Used to exercise the
    /// registration wiring of `has_filesystem_access()` -> `persistent_writes`.
    struct EphemeralRuntime(NativeRuntime);

    impl RuntimeAdapter for EphemeralRuntime {
        fn name(&self) -> &str {
            "ephemeral-test"
        }
        fn has_shell_access(&self) -> bool {
            true
        }
        fn has_filesystem_access(&self) -> bool {
            false
        }
        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
        fn supports_long_running(&self) -> bool {
            false
        }
        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            self.0.build_shell_command(command, workspace_dir)
        }
    }

    #[tokio::test]
    async fn registered_tools_warn_or_block_on_ephemeral_runtime() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("notes.txt"), "data")
            .await
            .unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: crate::security::AutonomyLevel::Supervised,
            max_actions_per_hour: 100,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(EphemeralRuntime(NativeRuntime::new()));
        let tools = default_tools_with_runtime(security, runtime);
        let by_name = |n: &str| tools.iter().find(|t| t.name() == n).unwrap();

        // shell: warns on the executed command.
        let r = by_name("shell")
            .execute(serde_json::json!({"command": "echo hi"}))
            .await
            .unwrap();
        assert!(
            r.output.contains("EPHEMERAL WORKSPACE"),
            "shell must warn, got: {}",
            r.output
        );

        // file_read: warns on a successful text read.
        let r = by_name("file_read")
            .execute(serde_json::json!({"path": "notes.txt"}))
            .await
            .unwrap();
        assert!(
            r.success && r.output.contains("EPHEMERAL WORKSPACE"),
            "file_read must warn, got: {r:?}"
        );

        // file_edit: warns on a successful edit.
        let r = by_name("file_edit")
            .execute(
                serde_json::json!({"path": "notes.txt", "old_string": "data", "new_string": "x"}),
            )
            .await
            .unwrap();
        assert!(
            r.success && r.output.contains("EPHEMERAL WORKSPACE"),
            "file_edit must warn, got: {r:?}"
        );

        // file_write: refuses outright (does not warn-and-write).
        let r = by_name("file_write")
            .execute(serde_json::json!({"path": "new.txt", "content": "x"}))
            .await
            .unwrap();
        assert!(
            !r.success,
            "file_write must refuse on ephemeral, got: {r:?}"
        );
        assert!(
            r.error
                .as_deref()
                .unwrap_or("")
                .contains("ephemeral workspace"),
            "file_write error must name the cause, got: {:?}",
            r.error
        );
        assert!(
            !tmp.path().join("new.txt").exists(),
            "file_write must not write anything on ephemeral"
        );
    }

    #[test]
    fn all_tools_excludes_browser_when_disabled() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            allowed_domains: vec!["example.com".into()],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"browser_open"));
        assert!(names.contains(&"schedule"));
        assert!(names.contains(&"model_routing_config"));
        assert!(names.contains(&"pushover"));
        assert!(names.contains(&"proxy_config"));
    }

    #[test]
    fn all_tools_includes_browser_when_enabled() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: true,
            allowed_domains: vec!["example.com".into()],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"browser_open"));
        assert!(names.contains(&"content_search"));
        assert!(names.contains(&"model_routing_config"));
        assert!(names.contains(&"pushover"));
        assert!(names.contains(&"proxy_config"));
    }

    #[tokio::test]
    async fn registered_sop_tools_persist_audit_trail() {
        let tmp = TempDir::new().unwrap();
        let sops_dir = tmp.path().join("sops");
        let sop_subdir = sops_dir.join("canary");
        std::fs::create_dir_all(&sop_subdir).unwrap();
        std::fs::write(
            sop_subdir.join("SOP.toml"),
            "[sop]\nname = \"canary\"\ndescription = \"audit wiring guard\"\nversion = \"1.0.0\"\n\n[[triggers]]\ntype = \"manual\"\n",
        )
        .unwrap();
        std::fs::write(
            sop_subdir.join("SOP.md"),
            "## Steps\n\n1. **Resolve** Do the first step\n   - tools: shell\n",
        )
        .unwrap();

        let mem_cfg = MemoryConfig {
            backend: "sqlite".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let security = Arc::new(SecurityPolicy::default());
        let mut cfg = test_config(&tmp);
        cfg.sop.sops_dir = Some(sops_dir.to_string_lossy().into_owned());

        let tools = {
            let mut engine = crate::sop::SopEngine::new(cfg.sop.clone());
            engine.reload(tmp.path());
            let sop_engine = Arc::new(std::sync::Mutex::new(engine));
            let sop_audit = Arc::new(crate::sop::SopAuditLogger::new(mem.clone()));
            all_tools_with_runtime(
                Arc::new(Config::default()),
                &security,
                &zeroclaw_config::schema::RiskProfileConfig::default(),
                "test-agent",
                Arc::new(NativeRuntime::new()),
                mem.clone(),
                None,
                None,
                &BrowserConfig::default(),
                &zeroclaw_config::schema::HttpRequestConfig::default(),
                &zeroclaw_config::schema::WebFetchConfig::default(),
                tmp.path(),
                &HashMap::new(),
                None,
                &cfg,
                None,
                false,
                None,
                Some(sop_engine),
                Some(sop_audit),
                None,
            )
            .tools
        };

        let execute = tools
            .iter()
            .find(|t| t.name() == "sop_execute")
            .expect("sop_execute must be registered when sops_dir is set");
        let result = execute
            .execute(serde_json::json!({"name": "canary"}))
            .await
            .unwrap();
        assert!(result.success, "sop_execute failed: {result:?}");

        let audit = crate::sop::SopAuditLogger::new(mem.clone());
        let run_keys = audit.list_runs().await.unwrap();
        assert!(
            !run_keys.is_empty(),
            "registered sop_execute must persist a sop_run_* audit entry; got none (audit not wired)"
        );
    }

    #[test]
    fn default_tools_names() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"file_write"));
        assert!(names.contains(&"file_edit"));
        assert!(names.contains(&"glob_search"));
        assert!(names.contains(&"content_search"));
    }

    #[test]
    fn default_tools_all_have_descriptions() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        for tool in &tools {
            assert!(
                !tool.description().is_empty(),
                "Tool {} has empty description",
                tool.name()
            );
        }
    }

    #[test]
    fn default_tools_all_have_schemas() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        for tool in &tools {
            let schema = tool.parameters_schema();
            assert!(
                schema.is_object(),
                "Tool {} schema is not an object",
                tool.name()
            );
            assert!(
                schema["properties"].is_object(),
                "Tool {} schema has no properties",
                tool.name()
            );
        }
    }

    #[test]
    fn tool_spec_generation() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        for tool in &tools {
            let spec = tool.spec();
            assert_eq!(spec.name, tool.name());
            assert_eq!(spec.description, tool.description());
            assert!(spec.parameters.is_object());
        }
    }

    #[test]
    fn tool_result_serde() {
        let result = ToolResult {
            success: true,
            output: "hello".into(),
            error: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.output, "hello");
        assert!(parsed.error.is_none());
    }

    #[test]
    fn tool_result_with_error_serde() {
        let result = ToolResult {
            success: false,
            output: ToolOutput::default(),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn tool_spec_serde() {
        let spec = ToolSpec::new("test", "A test tool", serde_json::json!({"type": "object"}));
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: ToolSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(parsed.description, "A test tool");
    }

    #[test]
    fn all_tools_includes_delegate_when_agents_configured() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "ollama.researcher".into(),
                ..Default::default()
            },
        );

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &agents,
            Some("delegate-test-credential"),
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"delegate"));
    }

    #[test]
    fn all_tools_excludes_delegate_when_no_agents() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"delegate"));
    }

    #[test]
    fn all_tools_includes_read_skill_in_compact_mode() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        cfg.skills.prompt_injection_mode =
            zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;

        let tools = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"read_skill"));
    }

    #[test]
    fn all_tools_excludes_read_skill_in_full_mode() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Full;

        let tools = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"read_skill"));
    }

    fn registry_names(tmp: &TempDir, is_subagent_caller: bool) -> Vec<String> {
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let cfg = test_config(tmp);

        all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &BrowserConfig::default(),
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            is_subagent_caller,
            None,
        )
        .tools
        .iter()
        .map(|t| t.name().to_string())
        .collect()
    }

    #[test]
    fn model_switch_present_for_top_level_absent_for_subagent() {
        let tmp = TempDir::new().unwrap();
        let top = registry_names(&tmp, false);
        assert!(
            top.iter().any(|n| n == ModelSwitchTool::NAME),
            "top-level agent must keep model_switch"
        );
        let subagent = registry_names(&tmp, true);
        assert!(
            !subagent.iter().any(|n| n == ModelSwitchTool::NAME),
            "subagent must not be able to switch the inherited model"
        );
    }

    #[test]
    fn all_tools_registers_read_skill_for_compact_agent_override_over_global_full() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        // Global stays Full; a runtime profile flips this agent to Compact and
        // the agent selects it via `runtime_profile`.
        cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Full;
        cfg.runtime_profiles.insert(
            "compact_profile".to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                prompt_injection_mode: Some(
                    zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
                ),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                runtime_profile: "compact_profile".into(),
                ..Default::default()
            },
        );

        let tools = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            names.contains(&"read_skill"),
            "compact runtime-profile override should register read_skill even when global is full"
        );
    }

    #[test]
    fn all_tools_omits_read_skill_for_full_agent_override_over_global_compact() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        // Global is Compact; a runtime profile pins this agent to Full and the
        // agent selects it via `runtime_profile`.
        cfg.skills.prompt_injection_mode =
            zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;
        cfg.runtime_profiles.insert(
            "full_profile".to_string(),
            zeroclaw_config::schema::RuntimeProfileConfig {
                prompt_injection_mode: Some(
                    zeroclaw_config::schema::SkillsPromptInjectionMode::Full,
                ),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                runtime_profile: "full_profile".into(),
                ..Default::default()
            },
        );

        let tools = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            None,
            None,
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            !names.contains(&"read_skill"),
            "full runtime-profile override should omit read_skill even when global is compact"
        );
    }
}

#[cfg(test)]
mod todo_registration_tests {
    #[test]
    fn todo_write_tool_name_is_stable() {
        use zeroclaw_api::tool::Tool;
        assert_eq!(super::todo_write::TodoWriteTool::new().name(), "TodoWrite");
    }
}
