//! Tool subsystem for agent-callable capabilities.

pub mod attribution;
pub mod cron_add;
pub(crate) mod cron_common;
pub mod cron_list;
pub mod cron_remove;
pub mod cron_run;
pub mod cron_runs;
pub mod cron_update;
pub mod delegation;
pub mod file_read;
pub mod model_switch;
pub mod notify;
pub mod param_options;
#[cfg(test)]
mod provider_wire_budget;
pub mod read_skill;
mod runtime_command_error;
pub mod schedule;
pub mod scoped;
pub mod send_message_to_peer;
pub mod shell;
pub mod skill_http;
pub mod skill_manage;
pub mod skill_tool;
pub mod task_context;
pub mod task_list;
pub mod task_section;
pub mod todo_write;

// Tool types from zeroclaw-tools (direct imports, no shims)
pub use zeroclaw_tools::ask_user::AskUserTool;
pub use zeroclaw_tools::ask_user::ChannelMapHandle;
pub use zeroclaw_tools::backup_tool::BackupTool;
pub use zeroclaw_tools::browser::{BrowserTool, ComputerUseConfig};
pub use zeroclaw_tools::browser_open::BrowserOpenTool;
pub use zeroclaw_tools::content_search::ContentSearchTool;
pub use zeroclaw_tools::data_management::DataManagementTool;
pub use zeroclaw_tools::discord_search::DiscordSearchTool;
#[cfg(feature = "email-tools")]
pub use zeroclaw_tools::email_read::EmailReadTool;
#[cfg(feature = "email-tools")]
pub use zeroclaw_tools::email_search::EmailSearchTool;
pub use zeroclaw_tools::escalate::EscalateToHumanTool;
pub use zeroclaw_tools::file_download::FileDownloadTool;
pub use zeroclaw_tools::file_edit::FileEditTool;
pub use zeroclaw_tools::file_upload::FileUploadTool;
pub use zeroclaw_tools::file_upload_bundle::FileUploadBundleTool;
pub use zeroclaw_tools::file_write::FileWriteTool;
pub use zeroclaw_tools::git_operations::GitOperationsTool;
pub use zeroclaw_tools::glob_search::GlobSearchTool;
pub use zeroclaw_tools::http_request::HttpRequestTool;
pub use zeroclaw_tools::image_gen::ImageGenTool;
pub use zeroclaw_tools::image_info::ImageInfoTool;
pub use zeroclaw_tools::knowledge_tool::KnowledgeTool;
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
pub use zeroclaw_tools::pipeline::PipelineTool;
pub use zeroclaw_tools::propose_soul_change::ProposeSoulChangeTool;
pub use zeroclaw_tools::screenshot::ScreenshotTool;
pub use zeroclaw_tools::sessions::{
    SessionsCurrentTool, SessionsHistoryTool, SessionsListTool, SessionsSendTool,
};
pub use zeroclaw_tools::text_browser::TextBrowserTool;
pub use zeroclaw_tools::tool_search::ToolSearchTool;
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
pub use file_read::FileReadTool;
pub use model_switch::ModelSwitchTool;
pub use notify::NotifyTool;
pub use read_skill::ReadSkillTool;
pub use schedule::ScheduleTool;
pub use send_message_to_peer::SendMessageToPeerTool;
pub use shell::ShellTool;
pub use skill_http::SkillHttpTool;
pub use skill_tool::{SkillBuiltinTool, SkillShellTool};
pub use task_list::TaskListTool;
pub use task_section::TaskListSection;
pub use todo_write::TodoWriteTool;

/// Re-entrant agent-spawning tools that must never be collapsed by the
/// per-turn duplicate-call guard: launching several with the same prompt
/// (redundancy, sampling, fan-out) is intentional, not an accidental
/// repeat. Unioned with config-provided exemptions in the tool-call loop.
pub const REENTRANT_AGENT_TOOLS: &[&str] = &[
    // `spawn_subagent` retired from this list with the spawn_subagent wall;
    // the V1 entrypoint is the surviving re-entrant spawn surface.
    crate::subagent_v1::ReasoningSubagentTool::NAME,
];

use crate::platform::{NativeRuntime, RuntimeAdapter};
use crate::security::{SecurityPolicy, create_sandbox};
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_config::schema::{AliasedAgentConfig, Config};
use zeroclaw_memory::Memory;

pub type PerToolChannelHandle =
    Arc<RwLock<HashMap<String, Arc<dyn zeroclaw_api::channel::Channel>>>>;

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

    // Forward `spec()` so inner overrides keep their `Arc`-shared parameter
    // schemas; the trait default would rebuild the spec from
    // `parameters_schema()`, deep-cloning MCP schemas every loop iteration.
    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        self.0.spec()
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

    // Forward `spec()` so inner overrides keep their `Arc`-shared parameter
    // schemas; the trait default would rebuild the spec from
    // `parameters_schema()`, deep-cloning MCP schemas every loop iteration.
    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        self.inner.spec()
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
    (
        "Reasoning SubAgent",
        "Run one bounded, contract-admitted reasoning child (V1 entry point)",
    ),
];

/// The registry's reasoning-spawn construction site. Single point where the
/// run's spawn lineage (SA-9) is threaded into the surviving spawn-capable
/// tool, so `registry_rebuild_carries_spawn_lineage_and_cannot_reset_depth`
/// can discriminate a dropped thread-through.
fn reasoning_spawn_tool_for_registry(
    root_config: &Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
    security: &Arc<SecurityPolicy>,
    spawn_lineage: Option<zeroclaw_api::subagent_v1::LineageRef>,
) -> crate::subagent_v1::ReasoningSubagentTool {
    let tool = crate::subagent_v1::ReasoningSubagentTool::new(
        Arc::clone(root_config),
        agent_alias,
        security.clone(),
    )
    .with_lineage(spawn_lineage);
    // Advisor (#405): an agent with `advisor = "model:<type>.<alias>"`
    // consults that model through this same tool. `harness:` targets are
    // refused by `Config::validate()`, so only model targets reach here.
    let Some(agent) = root_config.agents.get(agent_alias) else {
        return tool;
    };
    match agent
        .advisor
        .as_ref()
        .and_then(zeroclaw_config::advisor::AdvisorTarget::model_ref)
    {
        Some(model) => {
            let tool = tool.with_advisor(model.trim());
            match agent.advisor_max_calls_per_turn {
                Some(max_calls) => tool.with_advisor_max_calls_per_turn(max_calls),
                None => tool,
            }
        }
        None => tool,
    }
}

/// Tool names retired from the ordinary model-visible registry. No assembly
/// path may register them. Most entries
/// keep their implementations compiled (operator surfaces and tests
/// construct them directly); the SOP run tools below were deleted outright
/// with the legacy run side. The registry totality test asserts this set.
#[cfg(test)]
pub(crate) const RETIRED_OPERATOR_TOOL_NAMES: &[&str] = &[
    "model_routing_config",
    "model_switch",
    "proxy_config",
    "security_ops",
    "backup",
    "data_management",
    // Toy built-ins retired outright (fork decision 2026-11-15): the
    // implementations are deleted, not operator-gated. Weather lookups, if
    // ever wanted again, belong to an HTTP skill or MCP wrapper.
    "calculator",
    "weather",
    "sop_execute",
    "sop_advance",
    "sop_approve",
    "sop_status",
    "sop_list",
    "sop_workshop",
    "delegate",
    // spawn_subagent wall: the legacy full-Parent-inheritance spawn entry
    // (same-alias child, full `Arc<Config>` clone, parent memory UUID) —
    // retired; `reasoning_subagent` is the single spawn surface.
    "spawn_subagent",
    // Wall 2 prune epic: Parent-visible raw harness/vendor launch surfaces.
    "claude_code",
    "claude_code_runner",
    "codex_cli",
    "gemini_cli",
    "opencode_cli",
    "browser_delegate",
];

/// Bundled return values from tool registry construction.
/// Named struct to avoid an ever-growing positional tuple that's painful
/// to destructure across many callers.
#[allow(clippy::type_complexity)]
pub struct AllToolsResult {
    pub tools: Vec<Box<dyn Tool>>,
    pub task_owner: Option<task_list::TaskOwnerResolver>,
    pub ask_user_handle: Option<PerToolChannelHandle>,
    pub escalate_handle: Option<PerToolChannelHandle>,
    /// Pre-boxed Arcs of every tool (before policy filter). Used by
    /// skill-scoped builtin elevation to resolve targets at registration.
    pub unfiltered_tool_arcs: Vec<Arc<dyn Tool>>,
}

/// Create full tool registry including memory tools
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
    browser_config: &zeroclaw_config::schema::BrowserConfig,
    http_config: &zeroclaw_config::schema::HttpRequestConfig,
    web_fetch_config: &zeroclaw_config::schema::WebFetchConfig,
    workspace_dir: &std::path::Path,
    // Formerly the delegate tool's agent roster / parent fallback key.
    // `delegate` is retired (wall 1); the parameters stay in the
    // signature (underscored, intentionally unused) so call sites do not
    // churn and the registry contract is unchanged for callers.
    _agents: &HashMap<String, AliasedAgentConfig>,
    _fallback_api_key: Option<&str>,
    root_config: &zeroclaw_config::schema::Config,
    tui_env: Option<HashMap<String, String>>,
) -> AllToolsResult {
    all_tools_with_runtime(
        config,
        security,
        risk_profile,
        agent_alias,
        Arc::new(NativeRuntime::new()),
        memory,
        browser_config,
        http_config,
        web_fetch_config,
        workspace_dir,
        _agents,
        _fallback_api_key,
        root_config,
        tui_env,
        // No runtime adapter / live-config here; and no lineage —
        // callers of the non-runtime variant are top-level origins.
        None,
        None,
        None,
    )
}

/// Create full tool registry including memory tools.
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
    browser_config: &zeroclaw_config::schema::BrowserConfig,
    http_config: &zeroclaw_config::schema::HttpRequestConfig,
    web_fetch_config: &zeroclaw_config::schema::WebFetchConfig,
    workspace_dir: &std::path::Path,
    // Formerly the delegate tool's agent roster / parent fallback key.
    // `delegate` is retired (wall 1); the parameters stay in the
    // signature (underscored, intentionally unused) so call sites do not
    // churn and the registry contract is unchanged for callers.
    _agents: &HashMap<String, AliasedAgentConfig>,
    _fallback_api_key: Option<&str>,
    root_config: &zeroclaw_config::schema::Config,
    tui_env: Option<HashMap<String, String>>,
    // Live config handle for task authority and reminder resolution. Daemon
    // callers observe reloads; one-shot callers use the root config snapshot.
    live_config: Option<Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>>,
    // Unified spawn lineage (SA-9): the lineage of the run this registry
    // is being built for. Spawn-capable tools constructed here carry it,
    // so depth survives registry rebuilds (SA-11) and hops stay on one
    // ledger (SA-10). `None` for top-level origins (the run mints a
    // root) and legacy test callers.
    spawn_lineage: Option<zeroclaw_api::subagent_v1::LineageRef>,
    task_identity: Option<task_context::TaskIdentity>,
) -> AllToolsResult {
    // These tools retain the same immutable root snapshot. `config` may be
    // agent-specific, so share one copy of `root_config` rather than that Arc.
    let shared_root_config = Arc::new(root_config.clone());
    let persistent_writes = runtime.has_filesystem_access();
    let task_identity = if persistent_writes {
        task_identity
            .unwrap_or_else(|| task_context::TaskIdentity::capture(root_config, agent_alias))
    } else {
        task_context::TaskIdentity::unavailable(
            root_config,
            agent_alias,
            "the durable task list requires persistent filesystem access",
        )
    };
    let runtime_kind = root_config.runtime.kind.as_wire();
    let sandbox_cfg = risk_profile.sandbox_config();
    let sandbox = create_sandbox(&sandbox_cfg, runtime_kind, Some(&security.workspace_dir));
    let mut tool_arcs: Vec<Arc<dyn Tool>> = vec![
        Arc::new(RateLimitedTool::new(
            PathGuardedTool::new(
                ShellTool::new_with_sandbox(security.clone(), runtime.clone(), sandbox.clone())
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
        // The model's only path into its own Soul: records a proposal for
        // owner review and changes nothing (ADR-015 §3).
        Arc::new(
            ProposeSoulChangeTool::new(root_config.data_dir.clone(), agent_alias)
                .with_tone_resolver({
                    let live = live_config.clone();
                    let fixed = Arc::clone(&config);
                    let agent = agent_alias.to_string();
                    move || {
                        // One-shot registries own an immutable Config for the
                        // request; long-lived callers resolve the live handle.
                        Some(match &live {
                            Some(live) => live
                                .read()
                                .persona_for_agent(&agent)
                                .copied()
                                .unwrap_or_default(),
                            None => fixed.persona_for_agent(&agent).copied().unwrap_or_default(),
                        })
                    }
                }),
        ),
        Arc::new(reasoning_spawn_tool_for_registry(
            &shared_root_config,
            agent_alias,
            security,
            spawn_lineage.clone(),
        )),
        Arc::new(SendMessageToPeerTool::with_task_identity(
            Arc::clone(&shared_root_config),
            agent_alias,
            task_identity.clone(),
        )),
        // Operator/admin tools are deliberately absent from this registry:
        // model_routing_config, model_switch, and proxy_config mutate
        // routing/proxy state whose authority is operator-level. The
        // trusted surfaces are the gateway config API
        // (PUT/DELETE /api/config...) for routing and proxy config, the
        // channel `/model` command for runtime model switching, and
        // startup application of persisted proxy config. The tool
        // implementations stay compiled and are re-exported below; they
        // are simply never handed to the model.
        Arc::new(GitOperationsTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )),
    ];

    // Proactive messages through channel bridges; only offered when a
    // `[gateway.bridges.<name>]` exists to deliver them.
    if !root_config.gateway.bridges.is_empty() {
        tool_arcs.push(Arc::new(NotifyTool::new(
            Arc::clone(&shared_root_config),
            security.clone(),
        )));
    }

    // Disabled delegation must be absent from the model-visible registry.
    // Existing tools still recheck live configuration on every invocation.
    if root_config.tachi.enabled {
        // L2 has one transport owner. Resolve live routing and agent/card
        // permissions for every operation; do not capture a policy snapshot.
        let delegation_agent = agent_alias.to_string();
        let delegation_config = config.clone();
        let delegation_security = security.clone();
        let delegation_live = live_config.clone();
        tool_arcs.extend(zeroclaw_tools::tachi_delegation::tools(
            agent_alias,
            move |name, operation| {
                let resolve = |current: &Config| {
                    delegation::resolve(
                        current,
                        &delegation_config.data_dir,
                        &delegation_agent,
                        &delegation_security.tracker,
                        name,
                        operation,
                    )
                };
                match &delegation_live {
                    Some(live) => resolve(&live.read()),
                    None => resolve(&delegation_config),
                }
            },
        ));
    }
    // Corrections require live policy. Unsupported snapshot-only entry points
    // fail closed rather than caching owner authority in a long-lived tool.
    let correction_live = live_config.clone();
    let correction_root = config.data_dir.clone();
    let correction_agent = agent_alias.to_string();
    tool_arcs.push(Arc::new(
        zeroclaw_tools::note_owner_correction::NoteOwnerCorrectionTool::new(
            agent_alias,
            move || {
                let live = correction_live.as_ref()?;
                let current = live.read();
                let policy = SecurityPolicy::for_agent(&current, &correction_agent).ok()?;
                (persistent_writes
                    && policy.can_act()
                    && policy.is_tool_allowed("note_owner_correction")
                    && current.data_dir == correction_root)
                    .then(|| {
                        (
                            current.companion_memory.owner.gate(),
                            correction_root.clone(),
                        )
                    })
            },
        ),
    ));
    tool_arcs.push(Arc::new(TodoWriteTool::new()));

    // Durable cross-session commitment list (#567). Owner identity is the
    // mint-once companion UUID (alias renames keep the list); the model
    // never chooses the owner. Requires the SQLite session backend.
    let (task_owner, task_writer) = task_context::resolvers(
        shared_root_config.clone(),
        agent_alias,
        security.clone(),
        persistent_writes,
        live_config.clone(),
        Some(task_identity),
    );
    let task_reminder_target =
        task_context::reminder_target_resolver(shared_root_config.clone(), live_config.clone());
    tool_arcs.push(Arc::new(TaskListTool::new(
        task_writer,
        task_reminder_target,
    )));

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
    #[cfg(feature = "email-tools")]
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
        // workspace skills, open-skills, and agent-bound bundles.
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

    // MCSS Security Operations: no longer registered as a model tool. The
    // diagnostics module stays compiled; `security_ops.enabled` no longer
    // admits it to any registry (the daemon notes the withheld section at
    // boot and reload instead).
    //
    // Backup and data management: operator-only surfaces. The gateway
    // operator API (`/api/agents/{alias}/backup*`,
    // `/api/agents/{alias}/data-retention*`) dispatches to the same
    // BackupTool / DataManagementTool command methods; the `[backup]` and
    // `[data_retention]` sections keep configuring that surface, not a
    // model tool.

    // Vision tools are always available
    tool_arcs.push(Arc::new(ScreenshotTool::new(security.clone())));
    tool_arcs.push(Arc::new(RateLimitedTool::new(
        PathGuardedTool::new(ImageInfoTool::new(security.clone()), security.clone()),
        security.clone(),
    )));

    if let Ok(backend) =
        zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
    {
        tool_arcs.push(Arc::new(SessionsCurrentTool::new(backend.clone())));
        tool_arcs.push(Arc::new(SessionsListTool::new(backend.clone())));
        tool_arcs.push(Arc::new(SessionsHistoryTool::new(
            backend.clone(),
            security.clone(),
        )));
        tool_arcs.push(Arc::new(SessionsSendTool::new(backend, security.clone())));
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

    // Interactive ask_user tool — always registered; owns its own late-bound channel map.
    let ask_user_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    let ask_user_tool =
        AskUserTool::new(security.clone(), ask_user_handle.as_ref().cloned().unwrap());
    tool_arcs.push(Arc::new(ask_user_tool));

    // Human escalation tool — always registered; owns its own late-bound channel map.
    let escalate_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    let escalate_tool = EscalateToHumanTool::new(
        security.clone(),
        root_config.escalation.alert_channels.clone(),
        escalate_handle.as_ref().cloned().unwrap(),
    );
    tool_arcs.push(Arc::new(escalate_tool));

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

    // `delegate` is retired (wall 1): the legacy full-parent-inheritance
    // delegation tool is no longer constructed on any composition. Its
    // replacement-first surfaces are the V1 `reasoning_subagent` (minimal and
    // full alike) and the Tachi bridge for durable/heavy work. The name stays
    // reserved in RETIRED_OPERATOR_TOOL_NAMES.

    // `vi_verify` is deliberately absent while no chain verifier exists: it checked
    // caller-supplied constraints against a caller-supplied fulfillment with nothing
    // establishing that either came from a signed credential. The operator-facing
    // notice lives at config load, since this function also runs per gateway request
    // and per nested registry rebuild. Register it again only behind a
    // verify-and-evaluate path that consumes a verified chain result.

    // Pipeline construction waits for ScopedToolRegistry::assemble(), where the
    // effective per-agent policy and optional caller allowlist are both known.

    apply_install_composition(&mut tool_arcs, root_config);

    AllToolsResult {
        task_owner: tool_arcs
            .iter()
            .any(|tool| tool.name() == "task_list")
            .then_some(task_owner),
        unfiltered_tool_arcs: tool_arcs.clone(),
        tools: boxed_registry_from_arcs(tool_arcs),
        ask_user_handle,
        escalate_handle,
    }
}

/// Apply the install-wide composition cut to the assembled registry.
///
/// Under `composition = "minimal"` the registry is reduced to the explicit
/// membership table (`zeroclaw_config::composition::MINIMAL_TOOL_MEMBERSHIP`)
/// before anything derives from it — the boxed registry and the
/// skill-elevation arcs both clone the filtered set — so no later stage can
/// resurrect a built-in non-member (scoped assembly gates its own built-in
/// appends the same way). Extension surfaces — MCP tools admitted by the
/// effective policy and skill-defined tools — are not built-ins and stay
/// governed by their own admission policies. An absent field keeps today's assembly: existing
/// installs must not lose tools on upgrade. Individual `enabled = true`
/// flags do not widen the minimal profile back; the exclusion is logged.
fn apply_install_composition(
    tool_arcs: &mut Vec<Arc<dyn Tool>>,
    root_config: &zeroclaw_config::schema::Config,
) {
    use zeroclaw_config::composition::Composition;

    if Composition::effective(root_config.composition) != Composition::Minimal {
        return;
    }
    let before = tool_arcs.len();
    tool_arcs.retain(|tool| Composition::is_minimal_member(tool.name()));
    let dropped = before - tool_arcs.len();
    if dropped > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "dropped": dropped,
                    "composition": "minimal"
                })),
            "Minimal composition excluded non-member tools from assembly"
        );
    }
}

#[cfg(test)]
mod tests;
