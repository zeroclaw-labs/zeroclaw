//! Tool subsystem for agent-callable capabilities.

pub mod attribution;
pub(crate) mod coding_cli_executor;
pub mod cron_add;
pub(crate) mod cron_common;
pub mod cron_list;
pub mod cron_remove;
pub mod cron_run;
pub mod cron_runs;
pub mod cron_update;
pub mod delegate;
pub mod deliver_file;
pub mod file_read;
pub mod model_switch;
pub mod param_options;
pub mod read_skill;
mod runtime_command_error;
pub mod schedule;
pub mod scoped;
pub mod security_ops;
pub mod send_message_to_peer;
pub mod shell;
pub(crate) mod shell_env;
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
pub use deliver_file::{
    DeliverFileTool, MAX_DELIVER_FILE_BYTES, attachment_deliver_uri,
    read_delivered_artifact_bounded,
};
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
use crate::security::{Sandbox, SecurityPolicy, create_sandbox};
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

    // Forward `spec()` so inner overrides keep their `Arc`-shared parameter
    // schemas; the trait default would rebuild the spec from
    // `parameters_schema()`, deep-cloning MCP schemas every loop iteration.
    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        self.0.spec()
    }

    fn invocation_triggers(&self) -> Vec<String> {
        self.0.invocation_triggers()
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.0.execute(args).await
    }
}

fn any_coding_cli_tool_enabled(root_config: &Config) -> bool {
    root_config.claude_code.enabled
        || root_config.codex_cli.enabled
        || root_config.gemini_cli.enabled
        || root_config.opencode_cli.enabled
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
    fn tool_provenance(&self) -> ::zeroclaw_api::attribution::ToolProvenance {
        self.inner.tool_provenance()
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

    fn invocation_triggers(&self) -> Vec<String> {
        self.inner.invocation_triggers()
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

/// Builds the plain (non-sandbox-aware) shell tool `default_tools_with_runtime`
/// registers, as a concrete `ShellTool` rather than a boxed `dyn Tool`. Kept as
/// its own function so tests can inspect fields (`sandbox_name()`,
/// `timeout_secs()`) a `Box<dyn Tool>` would otherwise erase, with zero risk
/// of drifting from what production actually constructs — this function IS
/// the construction production uses, not a hand-rebuilt mirror of it.
pub(crate) fn build_default_shell_tool(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
) -> ShellTool {
    let persistent_writes = runtime.has_filesystem_access();
    ShellTool::new(security, runtime).with_persistent_writes(persistent_writes)
}

/// Assembles a shell tool from an already-resolved `sandbox`, applying the
/// same `shell_timeout_secs == 0 -> inherit the global default` contract
/// production uses. Single source of truth for that assembly, reused by both
/// `runtime_shell_assembly` (the seam [`all_tools_with_runtime`] and
/// [`shell_tool_for_runtime`] build through, which resolves `sandbox` from the
/// caller's own `RiskProfileConfig`) and `delegate.rs`'s `Bounded` cross-profile
/// target reconstruction (which resolves it from the target's
/// `SecurityPolicy` via [`SecurityPolicy::sandbox_config`], since a Bounded
/// delegate only has a `SecurityPolicy`, not the `RiskProfileConfig` it came
/// from) — so the two paths cannot silently diverge on this assembly step
/// again. Sandbox *resolution* deliberately stays at each call site rather
/// than moving into this function, since the two callers resolve it from
/// different source types and `all_tools_with_runtime` already needs the
/// resolved `Arc<dyn Sandbox>` for its `coding_cli_executor` too — resolving
/// it again in here would mean two independent `create_sandbox` calls inside
/// the same registry build, not one.
pub(crate) fn build_sandboxed_shell_tool(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    sandbox: Arc<dyn crate::security::Sandbox>,
    global_shell_timeout_secs: u64,
) -> ShellTool {
    let persistent_writes = runtime.has_filesystem_access();
    let timeout_secs = if security.shell_timeout_secs > 0 {
        security.shell_timeout_secs
    } else {
        global_shell_timeout_secs
    };
    ShellTool::new_with_sandbox(security, runtime, sandbox)
        .with_timeout_secs(timeout_secs)
        .with_persistent_writes(persistent_writes)
}

/// Create the default tool registry with explicit runtime adapter.
pub fn default_tools_with_runtime(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Vec<Box<dyn Tool>> {
    let persistent_writes = runtime.has_filesystem_access();
    vec![
        // The shell tool owns its own dialect-aware command + forbidden-path
        // validation (see `ShellTool::execute`), so it is not wrapped in the
        // generic POSIX `PathGuardedTool` — matching `SkillShellTool`. Wrapping it
        // would run a dialect-less path scan ahead of the tool and wrongly reject
        // the Windows `\\.\nul` device on a native cmd.exe sink.
        Box::new(RateLimitedTool::new(
            build_default_shell_tool(security.clone(), runtime),
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
            PathGuardedTool::new(DeliverFileTool::new(security.clone()), security.clone()),
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

/// Tool names whose construction bakes in a `SecurityPolicy`
/// (`workspace_dir`/`allowed_roots`/`forbidden_paths`): the set built by
/// [`default_tools_with_runtime`] plus [`image_info_tool`]. A `Bounded` delegate to a
/// target whose risk profile differs from the caller's must rebuild exactly these
/// against the target's own policy rather than reuse the caller's already-built
/// instances — see `delegate.rs`'s `Bounded` branch. Kept in sync with the
/// constructors by `filesystem_tool_names_match_constructed_tools` below.
pub const FILESYSTEM_TOOL_NAMES: &[&str] = &[
    "shell",
    "file_read",
    "file_write",
    "file_edit",
    "glob_search",
    "content_search",
    "deliver_file",
    "image_info",
];

/// Tool names that bind `workspace_dir`/`SecurityPolicy` state at construction
/// time the exact same way as `FILESYSTEM_TOOL_NAMES`, but are only built by
/// [`all_tools_with_runtime`] (config-gated, several disabled by default) —
/// not by the smaller [`default_tools_with_runtime`]. A `Bounded` delegate to
/// a cross-profile target must rebuild these against the target's own policy
/// too; `delegate.rs`'s `Bounded` branch currently only checks
/// `FILESYSTEM_TOOL_NAMES`, so a target allowed one of these still falls
/// through to the caller's `ToolArcRef`-wrapped instance. See each
/// constructor function above for the exact captured field: `git_operations`,
/// `backup`, `data_management` (the most severe: its `purge` command deletes
/// files), `linkedin`, `image_gen`, `pushover` (reads credentials from
/// `workspace_dir/.env`), `screenshot`, `file_upload`, `file_upload_bundle`,
/// `file_download` (all capture `workspace_dir`/`SecurityPolicy` directly),
/// `browser` (resolves screenshot destinations through the captured
/// `SecurityPolicy` with the same guards as `file_write`/`file_edit`), and the
/// coding-CLI tools (each bound to `security` and a `coding_cli_executor`
/// built from the caller's own `sandbox`). Kept in sync with the constructors
/// by `workspace_bound_tool_names_beyond_default_are_actually_constructed`
/// below.
pub const WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT: &[&str] = &[
    "git_operations",
    "backup",
    "data_management",
    "linkedin",
    "image_gen",
    "pushover",
    "screenshot",
    "file_upload",
    "file_upload_bundle",
    "file_download",
    "browser",
    "claude_code",
    "claude_code_runner",
    "codex_cli",
    "gemini_cli",
    "opencode_cli",
];

/// Tool names that bind the CALLER's `agent_alias` (and sometimes `security`)
/// at construction time - a distinct capture mechanism from
/// `FILESYSTEM_TOOL_NAMES`/`WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT`
/// (`workspace_dir`/`SecurityPolicy`), but the exact same underlying bug: a
/// `Bounded` delegate to a cross-profile target that falls through to the
/// caller's `ToolArcRef`-wrapped instance acts using the CALLER's identity,
/// not the target's. Severity varies by tool - `spawn_subagent` is the most
/// severe (a target can synchronously spawn a SubAgent inheriting the
/// caller's full identity, `SecurityPolicy`, and permissions envelope);
/// `cron_add`/`cron_update`/`schedule` let a target create or modify
/// persistent scheduled jobs that later run - asynchronously, after the
/// delegate call ends - under the CALLER's risk profile (the scheduler
/// re-derives `SecurityPolicy::for_agent` from the job's stored
/// `agent_alias`, see `cron/scheduler.rs`); `cron_remove` lets a target
/// delete the caller's jobs; `send_message_to_peer` lets a target reach the
/// caller's peer set/channels; `read_skill` leaks the caller's workspace
/// skill files (same class of leak as the original bug, different
/// mechanism). Kept in sync with the constructors by
/// `identity_bound_tool_names_are_actually_constructed` below.
///
/// Capture is not always a field. `send_via` binds the caller's alias inside
/// a peer-group resolver closure, which is why grepping for an `agent_alias`
/// field misses it; `llm_task` bakes the caller's provider api_key.
///
/// `sop_approve` is in this list but cannot be rebuilt: the target may simply
/// not be in a checkpoint's `required_group`, so there is no correct instance
/// to hand it. It gets a refusing stub with the real schema instead
/// (`BoundedSopApproveDenied`). Note that this is authorization, not just
/// audit attribution: `authorize_checkpoint` resolves the approving principal
/// from this alias and returns `NotAuthorized` when it is outside the
/// required group, before it ever looks at the decision.
pub const IDENTITY_BOUND_TOOL_NAMES: &[&str] = &[
    "read_skill",
    "cron_add",
    "cron_update",
    "cron_remove",
    "cron_list",
    "cron_run",
    "cron_runs",
    "schedule",
    "send_message_to_peer",
    "spawn_subagent",
    "send_via",
    "llm_task",
    "sop_approve",
];

/// Tools a `Bounded` delegate target may reuse from the caller's registry
/// **as-is**, because they provably capture nothing that would differ if the
/// instance were built with the target's context.
///
/// This is the positive side of the inverted rule: the bounded fallback in
/// `DelegateTool` reuses a caller instance ONLY for a name listed here (or an
/// MCP tool the target's own bundles grant). Everything else that is not
/// rebuilt against the target's policy is OMITTED from the target registry.
/// A name added to the codebase and forgotten here loses functionality, it
/// does not silently inherit the caller's context - which is the property
/// the three hand-maintained inventories above cannot offer on their own.
///
/// Admission test, applied per tool by reading its constructor and its real
/// construction site in `all_tools_with_runtime`: *would any argument change
/// if this tool were built with the TARGET's context?* Every member below
/// answers no - none captures a `SecurityPolicy`, an agent alias, or a live
/// channel handle; their arguments come from `root_config`, from
/// process-wide shared handles, or from nothing at all.
///
/// Note that `sop_execute`, `sop_advance` and `web_search_tool` hold no
/// `security` field, so they run no internal `can_act` gate. Reusing them is
/// still correct under the capture test - an instance built with the target's
/// context would be byte-identical - but the absence of that gate is a
/// pre-existing property of those tools, identical outside bounded
/// delegation, and is not what this boundary fix addresses.
pub const SAFE_FOR_BOUNDED_REUSE: &[&str] = &[
    // Unit structs: no fields at all.
    "calculator",
    "weather",
    "report_template",
    "TodoWrite",
    // `CloudPatternsTool::new()` takes no arguments: static pattern data.
    "cloud_patterns",
    // `(default_language, risk_sensitivity)`, both from `root_config.project_intel`.
    "project_intel",
    // Holds `Arc<KnowledgeGraph>` opened from `root_config.knowledge.db_path`.
    "knowledge",
    // Holds `root_config.cloud_ops` - domain config, not a `SecurityPolicy`.
    "cloud_ops",
    // Holds `root_config.security_ops` + its playbooks: "security" here is the
    // tool's problem domain (IaC scanning), not the caller's policy.
    "security_ops",
    // NOT stateless: `CanvasStore` wraps `Arc<RwLock<HashMap<..>>>`. It is
    // shared session state, and sharing it with a bounded target is the
    // intended behaviour - a bounded child works inside the caller's turn.
    "canvas",
    // The SOP tools below hold the shared `sop_engine`/`sop_audit`/metrics
    // handles that `all_tools_with_runtime` receives as parameters: the same
    // objects for caller and target. `sop_approve` is deliberately absent -
    // it is the one SOP tool built `.with_agent_alias(agent_alias)`.
    "sop_list",
    "sop_execute",
    "sop_advance",
    "sop_status",
    // Its `PathBuf` is `root_config.install_root_dir()`, the global install
    // root that anchors SOP-definition writes - not the caller's workspace.
    "sop_workshop",
    // Hold only the session backend, built from `config.data_dir` and the
    // global session-backend setting. `sessions_history`/`sessions_send` are
    // deliberately absent: those two do capture `security`.
    "sessions_current",
    "sessions_list",
    // Reads the shared, global `discord` archive DB, not per-agent memory.
    "discord_search",
    // Hold the global e-mail account map and auth service.
    "email_search",
    "email_read",
];

/// The vision `image_info` tool, wrapped exactly like every other filesystem-boundary
/// tool (`RateLimitedTool` + `PathGuardedTool`). Factored out of the big assembly
/// function below so a `Bounded` delegate target can rebuild it against its own
/// `SecurityPolicy` without duplicating the wrapper stack (see
/// [`FILESYSTEM_TOOL_NAMES`]).
pub fn image_info_tool(security: Arc<SecurityPolicy>) -> Box<dyn Tool> {
    Box::new(RateLimitedTool::new(
        PathGuardedTool::new(ImageInfoTool::new(security.clone()), security.clone()),
        security,
    ))
}

// ── Factories for WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT ─────────────────
//
// Each function below rebuilds exactly one of the tools from
// `WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT`, gated the same way
// `all_tools_with_runtime` gates it. Factored out (same principle as
// `image_info_tool`/`build_sandboxed_shell_tool` above) so a `Bounded`
// delegate's target reconstruction (`delegate.rs`) calls the SAME
// construction code `all_tools_with_runtime` does, instead of a hand-rebuilt
// copy that could silently drift from it.

/// Rebuilds `git_operations` bound to the given `workspace_dir` - unconditional
/// in production (`all_tools_with_runtime` never gates it), so this always
/// returns a tool. `workspace_dir` is passed explicitly (not read from
/// `security.workspace_dir`) because in the normal (non-delegate) registry the
/// two can differ - callers pass the shared data dir here, while
/// `security.workspace_dir` is the per-agent workspace.
pub(crate) fn git_operations_tool(
    security: Arc<SecurityPolicy>,
    workspace_dir: &std::path::Path,
) -> Arc<dyn Tool> {
    Arc::new(GitOperationsTool::new(
        security,
        workspace_dir.to_path_buf(),
    ))
}

/// Rebuilds `backup` bound to the given `workspace_dir`, gated like
/// `all_tools_with_runtime` gates it (`root_config.backup.enabled`, true by
/// default). `None` when disabled. See [`git_operations_tool`] for why
/// `workspace_dir` is a separate parameter rather than read from a policy.
pub(crate) fn backup_tool(
    workspace_dir: &std::path::Path,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.backup.enabled {
        return None;
    }
    Some(Arc::new(BackupTool::new(
        workspace_dir.to_path_buf(),
        root_config.backup.include_dirs.clone(),
        root_config.backup.max_keep,
    )))
}

/// Rebuilds `data_management` bound to the given `workspace_dir`, gated like
/// `all_tools_with_runtime` gates it (`root_config.data_retention.enabled`,
/// false by default). `None` when disabled. See [`git_operations_tool`] for
/// why `workspace_dir` is a separate parameter rather than read from a policy.
pub(crate) fn data_management_tool(
    workspace_dir: &std::path::Path,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.data_retention.enabled {
        return None;
    }
    Some(Arc::new(DataManagementTool::new(
        workspace_dir.to_path_buf(),
        root_config.data_retention.retention_days,
    )))
}

/// Rebuilds `linkedin` bound to the given `workspace_dir`, gated like
/// `all_tools_with_runtime` gates it (`root_config.linkedin.enabled`). `None`
/// when disabled. See [`git_operations_tool`] for why `workspace_dir` is a
/// separate parameter rather than read from `security.workspace_dir`.
pub(crate) fn linkedin_tool(
    security: Arc<SecurityPolicy>,
    workspace_dir: &std::path::Path,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.linkedin.enabled {
        return None;
    }
    Some(Arc::new(LinkedInTool::new(
        security,
        workspace_dir.to_path_buf(),
        root_config.linkedin.api_version.clone(),
        root_config.linkedin.content.clone(),
        root_config.linkedin.image.clone(),
    )))
}

/// Rebuilds `image_gen` bound to the given `workspace_dir`, gated like
/// `all_tools_with_runtime` gates it (`root_config.image_gen.enabled`). `None`
/// when disabled OR when construction fails (logged, mirroring the
/// production registration path). See [`git_operations_tool`] for why
/// `workspace_dir` is a separate parameter rather than read from
/// `security.workspace_dir`.
pub(crate) fn image_gen_tool(
    security: Arc<SecurityPolicy>,
    workspace_dir: &std::path::Path,
    root_config: &zeroclaw_config::schema::Config,
    persistent_writes: bool,
) -> Option<Arc<dyn Tool>> {
    if !root_config.image_gen.enabled {
        return None;
    }
    match ImageGenTool::new_with_persistence(
        security,
        workspace_dir.to_path_buf(),
        root_config.image_gen.default_model.clone(),
        root_config.image_gen.api_key_env.clone(),
        persistent_writes,
        root_config.security.nat64_prefixes.clone(),
    ) {
        Ok(tool) => Some(Arc::new(tool)),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "image_gen: failed to construct tool for a Bounded delegate target, skipping"
            );
            None
        }
    }
}

/// Rebuilds `pushover` bound to the given `workspace_dir` - unconditional in
/// production (`all_tools_with_runtime` never gates it), so this always
/// returns a tool. `PushoverTool` reads `PUSHOVER_TOKEN`/`PUSHOVER_USER_KEY`
/// from `workspace_dir/.env`, so a `Bounded` cross-profile target must get its
/// own workspace here, not the caller's.
pub(crate) fn pushover_tool(
    security: Arc<SecurityPolicy>,
    workspace_dir: &std::path::Path,
) -> Arc<dyn Tool> {
    Arc::new(PushoverTool::new(security, workspace_dir.to_path_buf()))
}

/// Rebuilds `screenshot` bound to `security` - unconditional in production
/// (`all_tools_with_runtime` never gates it). `ScreenshotTool` writes its
/// output under `security.workspace_dir`, so a `Bounded` cross-profile target
/// must get a `security` bound to its own workspace here, not the caller's.
pub(crate) fn screenshot_tool(security: Arc<SecurityPolicy>) -> Arc<dyn Tool> {
    Arc::new(ScreenshotTool::new(security))
}

/// Rebuilds `file_upload` bound to `security`, gated like
/// `all_tools_with_runtime` gates it (`root_config.file_upload.url` set and
/// non-blank). `None` when disabled. `FileUploadTool` resolves its source
/// file path through the captured `SecurityPolicy`, so a `Bounded`
/// cross-profile target must get its own policy here, not the caller's.
pub(crate) fn file_upload_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if root_config
        .file_upload
        .url
        .as_deref()
        .is_none_or(|u| u.trim().is_empty())
    {
        return None;
    }
    Some(Arc::new(FileUploadTool::new(
        security,
        root_config.file_upload.clone(),
    )))
}

/// Rebuilds `file_upload_bundle` bound to `security`, gated like
/// `all_tools_with_runtime` gates it (`root_config.file_upload_bundle.url` set
/// and non-blank). `None` when disabled. See [`file_upload_tool`] for why
/// `security` must be the target's own policy.
pub(crate) fn file_upload_bundle_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if root_config
        .file_upload_bundle
        .url
        .as_deref()
        .is_none_or(|u| u.trim().is_empty())
    {
        return None;
    }
    Some(Arc::new(FileUploadBundleTool::new(
        security,
        root_config.file_upload_bundle.clone(),
    )))
}

/// Rebuilds `file_download` bound to `security`, gated like
/// `all_tools_with_runtime` gates it (`root_config.file_download.url` set and
/// non-blank). `None` when disabled. See [`file_upload_tool`] for why
/// `security` must be the target's own policy.
pub(crate) fn file_download_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
    persistent_writes: bool,
) -> Option<Arc<dyn Tool>> {
    if root_config
        .file_download
        .url
        .as_deref()
        .is_none_or(|u| u.trim().is_empty())
    {
        return None;
    }
    Some(Arc::new(FileDownloadTool::new_with_persistence(
        security,
        root_config.file_download.clone(),
        persistent_writes,
    )))
}

/// Rebuilds `browser` bound to `security`, gated like `all_tools_with_runtime`
/// gates it (`browser_config.enabled`). `None` when disabled OR when
/// construction fails (logged, mirroring the production registration path).
/// `BrowserTool` resolves screenshot destinations through the captured
/// `SecurityPolicy` with the same guards as `file_write`/`file_edit`
/// (`browser.rs`'s `validate_screenshot_target`), so a `Bounded`
/// cross-profile target must get its own policy here, not the caller's.
/// `browser_config` itself is not workspace-scoped (same config object for
/// caller and target), unlike `security`.
pub(crate) fn browser_tool(
    security: Arc<SecurityPolicy>,
    browser_config: &zeroclaw_config::schema::BrowserConfig,
) -> Option<Arc<dyn Tool>> {
    if !browser_config.enabled {
        return None;
    }
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
        Ok(tool) => Some(Arc::new(RateLimitedTool::new(tool, security))),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "browser: failed to construct tool, skipping registration"
            );
            None
        }
    }
}

/// Tools a `Bounded` delegate target must never receive, even though the
/// deny-by-default fallback would already drop them.
///
/// The fallback denies anything unclassified, so listing them changes no
/// behaviour. It records a decision instead: without this list, each of these
/// is denied by the accident of nobody having classified it, and a later
/// commit could "restore" one to `SAFE_FOR_BOUNDED_REUSE` without ever
/// confronting why it was absent.
///
/// - `model_switch` switches the active model process-wide. The repo already
///   sets this precedent for ephemeral children: `all_tools_with_runtime`
///   strips it for a SubAgent caller, on the grounds that such a child "must
///   not be able to switch the active model out from under the parent". A
///   bounded delegate is the same case.
/// - `mcp_resources` and `mcp_prompts` each hold an `Arc<McpRegistry>` - the
///   CALLER's registry, listing the resources and prompts of the CALLER's MCP
///   servers. They are MCP-origin but carry no `<server>__` prefix, so the
///   per-server rule that admits real MCP tools cannot classify them: it
///   matches a target's granted servers by prefix, and these names have none.
///   Reusing them would hand a target the caller's MCP surface - the same
///   boundary bug, on a surface the tool inventory never enumerated because
///   they are built by `build_mcp_capability_tools`, outside
///   `all_tools_with_runtime`.
pub const BOUNDED_DENIED_TOOL_NAMES: &[&str] = &["model_switch", "mcp_resources", "mcp_prompts"];

/// Tools that capture BOTH the caller's `SecurityPolicy` and a live channel
/// handle at construction time.
///
/// They cannot be reused (the policy gate is inside each tool, not in the
/// `RateLimitedTool` wrapper, so re-wrapping a caller instance changes
/// nothing), and they cannot simply be rebuilt either: a fresh handle map
/// would be empty, giving the target a tool that looks available in the prompt
/// and then fails at runtime with no channel to answer on. So they are rebuilt
/// against `target_policy` while KEEPING the caller's live handle - the gate
/// becomes the target's, the delivery route stays connected.
///
/// A missing handle means the tool is omitted rather than reused: an empty map
/// is worse than an absent tool.
pub const CHANNEL_REBOUND_TOOL_NAMES: &[&str] = &[
    "ask_user",
    "poll",
    "reaction",
    "channel_room",
    "git_forge",
    "escalate_to_human",
];

/// The live channel handles a `DelegateTool` needs to rebuild the
/// [`CHANNEL_REBOUND_TOOL_NAMES`] tools (and `send_via`) for a `Bounded`
/// target without disconnecting them.
///
/// These are the same maps the caller's own tools hold, created once in
/// `all_tools_with_runtime` and bound to real channels later by the daemon.
/// Two of them are shared by design: `reaction` backs `git_forge` as well, and
/// `ask_user` backs `send_via`.
#[derive(Clone, Default)]
pub struct DelegateChannelHandles {
    pub poll: Option<PerToolChannelHandle>,
    pub reaction: Option<PerToolChannelHandle>,
    pub channel_room: Option<PerToolChannelHandle>,
    pub ask_user: Option<PerToolChannelHandle>,
    pub escalate: Option<PerToolChannelHandle>,
}

/// Rebuilds `ask_user` bound to `security`, keeping the caller's live channel
/// handle. `AskUserTool` gates on its captured policy internally, so a
/// `Bounded` cross-profile target reusing the caller's instance would prompt
/// under the CALLER's autonomy.
pub(crate) fn ask_user_tool(
    security: Arc<SecurityPolicy>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(AskUserTool::new(security, handle))
}

/// Rebuilds `poll` bound to `security`, keeping the caller's live handle.
pub(crate) fn poll_tool(
    security: Arc<SecurityPolicy>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(PollTool::new(security, handle))
}

/// Rebuilds `reaction` bound to `security`, keeping the caller's live handle.
pub(crate) fn reaction_tool(
    security: Arc<SecurityPolicy>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(ReactionTool::new(security, handle))
}

/// Rebuilds `channel_room` bound to `security`, keeping the caller's live
/// handle.
pub(crate) fn channel_room_tool(
    security: Arc<SecurityPolicy>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(ChannelRoomTool::new(security, handle))
}

/// Rebuilds `git_forge` bound to `security`. Shares the `reaction` handle in
/// production, and does so here too.
pub(crate) fn git_forge_tool(
    security: Arc<SecurityPolicy>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(GitForgeTool::new(security, handle))
}

/// Rebuilds `escalate_to_human` bound to `security`, keeping the caller's live
/// handle. The alert-channel list is global (`root_config.escalation`), so it
/// is the same for caller and target.
pub(crate) fn escalate_to_human_tool(
    security: Arc<SecurityPolicy>,
    alert_channels: Vec<String>,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    Arc::new(EscalateToHumanTool::new(security, alert_channels, handle))
}

/// Rebuilds `send_via` bound to `security` and to `agent_alias`'s OWN peer
/// groups, keeping the caller's live handle.
///
/// The alias capture here is a closure, not a field: `SendViaTool` takes an
/// `AgentPeerGroupResolver` that filters peer groups by alias. Reused from the
/// caller, a `Bounded` target routes through the CALLER's peer groups - which
/// is why a grep for an `agent_alias` field misses this tool entirely.
///
/// `live_config` mirrors `all_tools_with_runtime`: a live handle so reloads
/// take effect, falling back to a snapshot when there is none.
pub(crate) fn send_via_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
    live_config: Option<Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>>,
    agent_alias: &str,
    handle: PerToolChannelHandle,
) -> Arc<dyn Tool> {
    let agent_peer_groups: AgentPeerGroupResolver = if let Some(live) = live_config {
        let alias = agent_alias.to_string();
        Arc::new(move || filter_agent_peer_groups(&live.read(), &alias))
    } else {
        let snapshot = filter_agent_peer_groups(root_config, agent_alias);
        Arc::new(move || snapshot.clone())
    };
    Arc::new(SendViaTool::new(security, handle, agent_peer_groups))
}

/// Tools that bind the caller's `SecurityPolicy` for its autonomy/rate gate but
/// whose remaining constructor arguments are global (credentials and endpoint
/// config read from `root_config`, or process-wide shared handles).
///
/// Reusing a caller instance would gate a `Bounded` target's network and SaaS
/// calls against the CALLER's autonomy: a read-only target would act with the
/// caller's `can_act()`. Rebuilding them against `target_policy` restores the
/// capability under the target's own limits.
///
/// Verified per tool by reading the constructor AND its construction site in
/// `all_tools_with_runtime`: every non-`security` argument comes from
/// `root_config` (or from a parameter that all seven production callers of
/// `all_tools_with_runtime` supply from that same global config), so none of
/// them would differ if the tool were built for the target instead. The
/// per-agent resolution that does exist in this function - the model provider
/// api_key - feeds other tools, not these.
pub const AUTONOMY_REBOUND_TOOL_NAMES: &[&str] = &[
    "http_request",
    "web_fetch",
    "text_browser",
    "browser_open",
    "browser_delegate",
    "notion",
    "jira",
    "composio",
    "google_workspace",
    "microsoft365",
    "model_routing_config",
    "proxy_config",
    "sessions_history",
    "sessions_send",
    // Its inner tool takes only global config, but the INSTANCE registered for
    // it is `RateLimitedTool<WebSearchTool>`, and the wrapper meters against
    // the policy it was built with. Reusing the caller's would spend a
    // Bounded target's calls out of the caller's hourly budget - and refuse
    // them once the caller's is spent, whatever the target's own budget says.
    "web_search_tool",
];

/// Rebuilds `http_request` bound to `security`, gated and wrapped exactly as
/// `all_tools_with_runtime` does it (`http_config.enabled`, `RateLimitedTool`).
/// `None` when disabled or when construction fails (logged, mirroring the
/// production registration path).
/// Build the registered `web_search_tool` INSTANCE - the rate-limiting wrapper
/// included, not just the inner search tool.
///
/// The wrapper is where the policy lives: `WebSearchTool` itself has no
/// `security` field, so reasoning about the inner struct says the instance is
/// free of caller capture while the instance a registry actually holds meters
/// every call against whichever policy the wrapper was built with. Both the
/// production registry and the `Bounded` delegate rebuild go through here, so
/// the two cannot construct it differently.
pub(crate) fn web_search_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.web_search.enabled {
        return None;
    }
    // Rate-limited like every other outbound-network tool (see web_fetch and
    // http_request): without the wrapper an agent loop could issue unbounded
    // searches against the configured provider, and against the default
    // scrape path, which gets the machine blocked.
    Some(Arc::new(RateLimitedTool::new(
        WebSearchTool::new_with_config(
            root_config.web_search.search_provider.clone(),
            root_config.web_search.brave_api_key.clone(),
            root_config.web_search.tavily_api_key.clone(),
            root_config.web_search.jina_api_key.clone(),
            root_config.web_search.searxng_instance_url.clone(),
            root_config.web_search.max_results,
            root_config.web_search.timeout_secs,
            root_config.config_path.clone(),
            root_config.secrets.encrypt,
        ),
        security,
    )))
}

pub(crate) fn http_request_tool(
    security: Arc<SecurityPolicy>,
    http_config: &zeroclaw_config::schema::HttpRequestConfig,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !http_config.enabled {
        return None;
    }
    match HttpRequestTool::new_with_config(
        security.clone(),
        http_config.allowed_domains.clone(),
        http_config.max_response_size,
        http_config.timeout_secs,
        http_config.allow_private_hosts,
        http_config.allowed_private_hosts.clone(),
        root_config.security.nat64_prefixes.clone(),
        root_config.config_path.clone(),
        root_config.secrets.encrypt,
    ) {
        Ok(tool) => Some(Arc::new(RateLimitedTool::new(tool, security))),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "http_request: failed to construct tool, skipping registration"
            );
            None
        }
    }
}

/// Rebuilds `web_fetch` bound to `security`, gated and wrapped exactly as
/// `all_tools_with_runtime` does it (`web_fetch_config.enabled`,
/// `RateLimitedTool`).
pub(crate) fn web_fetch_tool(
    security: Arc<SecurityPolicy>,
    web_fetch_config: &zeroclaw_config::schema::WebFetchConfig,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !web_fetch_config.enabled {
        return None;
    }
    match WebFetchTool::new(
        security.clone(),
        web_fetch_config.allowed_domains.clone(),
        web_fetch_config.blocked_domains.clone(),
        web_fetch_config.max_response_size,
        web_fetch_config.timeout_secs,
        web_fetch_config.firecrawl.clone(),
        web_fetch_config.allowed_private_hosts.clone(),
        root_config.security.nat64_prefixes.clone(),
    ) {
        Ok(tool) => Some(Arc::new(RateLimitedTool::new(tool, security))),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "web_fetch: failed to construct tool, skipping registration"
            );
            None
        }
    }
}

/// Rebuilds `text_browser` bound to `security`, gated as
/// `all_tools_with_runtime` gates it (`root_config.text_browser.enabled`).
pub(crate) fn text_browser_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.text_browser.enabled {
        return None;
    }
    match TextBrowserTool::new_with_private_hosts(
        security,
        root_config.text_browser.preferred_browser.clone(),
        root_config.text_browser.timeout_secs,
        root_config.text_browser.allowed_private_hosts.clone(),
        root_config.security.nat64_prefixes.clone(),
    ) {
        Ok(tool) => Some(Arc::new(tool)),
        Err(e) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "text_browser: failed to construct tool, skipping registration"
            );
            None
        }
    }
}

/// Rebuilds `browser_open` bound to `security`, gated as
/// `all_tools_with_runtime` gates it (`browser_config.enabled`).
/// `browser_config` itself is not workspace-scoped (same config object for
/// caller and target), unlike `security` - the same split as [`browser_tool`].
pub(crate) fn browser_open_tool(
    security: Arc<SecurityPolicy>,
    browser_config: &zeroclaw_config::schema::BrowserConfig,
) -> Option<Arc<dyn Tool>> {
    if !browser_config.enabled {
        return None;
    }
    match BrowserOpenTool::new_with_private_hosts(
        security,
        browser_config.allowed_domains.clone(),
        browser_config.allowed_private_hosts.clone(),
    ) {
        Ok(tool) => Some(Arc::new(tool)),
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "browser_open: failed to construct tool, skipping registration"
            );
            None
        }
    }
}

/// Rebuilds `browser_delegate` bound to `security`, gated as
/// `all_tools_with_runtime` gates it (config plus runtime shell access).
pub(crate) fn browser_delegate_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
    has_shell_access: bool,
) -> Option<Arc<dyn Tool>> {
    if !root_config.browser_delegate.enabled {
        return None;
    }
    if !has_shell_access {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "browser_delegate: skipped registration because the current runtime does not allow shell access"
        );
        return None;
    }
    Some(Arc::new(BrowserDelegateTool::new(
        security,
        root_config.browser_delegate.clone(),
    )))
}

/// Rebuilds `notion` bound to `security`. The API key is global
/// (`root_config.notion.api_key`, or the `NOTION_API_KEY` env var), never
/// per-agent, so only the policy differs between caller and target.
pub(crate) fn notion_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.notion.enabled {
        return None;
    }
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
        return None;
    }
    Some(Arc::new(NotionTool::new(notion_api_key, security)))
}

/// Rebuilds `jira` bound to `security`. Base URL, credentials and allowed
/// actions are global (`root_config.jira`), never per-agent.
pub(crate) fn jira_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.jira.enabled {
        return None;
    }
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
        return None;
    }
    if root_config.jira.base_url.trim().is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "Jira tool enabled but jira.base_url is empty — skipping registration"
        );
        return None;
    }
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
    Some(Arc::new(JiraTool::new(
        root_config.jira.base_url.trim().to_string(),
        email,
        api_token,
        root_config.jira.allowed_actions.clone(),
        security,
        root_config.jira.timeout_secs,
    )))
}

/// Rebuilds `composio` bound to `security`. The key and entity id are the
/// global `root_config.composio` values every caller of
/// `all_tools_with_runtime` passes down, not per-agent credentials.
pub(crate) fn composio_tool(
    security: Arc<SecurityPolicy>,
    composio_key: Option<&str>,
    composio_entity_id: Option<&str>,
) -> Option<Arc<dyn Tool>> {
    let key = composio_key.filter(|key| !key.is_empty())?;
    Some(Arc::new(ComposioTool::new(
        key,
        composio_entity_id,
        security,
    )))
}

/// Rebuilds `google_workspace` bound to `security`, gated as
/// `all_tools_with_runtime` gates it (config plus runtime shell access).
pub(crate) fn google_workspace_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
    has_shell_access: bool,
) -> Option<Arc<dyn Tool>> {
    if !root_config.google_workspace.enabled {
        return None;
    }
    if !has_shell_access {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "google_workspace: skipped registration because shell access is unavailable"
        );
        return None;
    }
    Some(Arc::new(GoogleWorkspaceTool::new(
        security,
        root_config.google_workspace.allowed_services.clone(),
        root_config.google_workspace.allowed_operations.clone(),
        root_config.google_workspace.credentials_path.clone(),
        root_config.google_workspace.default_account.clone(),
        root_config.google_workspace.rate_limit_per_minute,
        root_config.google_workspace.timeout_secs,
        root_config.google_workspace.audit_log,
    )))
}

/// Outcome of building `microsoft365`, which unlike every other tool here can
/// abort the whole registry: a `client_credentials` flow with no client secret
/// is a fail-fast misconfiguration, not a tool to skip.
pub(crate) enum Microsoft365Registration {
    /// Register this tool.
    Tool(Arc<dyn Tool>),
    /// Not configured, or construction failed - skip it (already logged).
    Skip,
    /// Misconfigured credentials: `all_tools_with_runtime` returns early here.
    AbortRegistry,
}

/// Rebuilds `microsoft365` bound to `security`. Tenant, client and secret are
/// global (`root_config.microsoft365`); the token cache lives next to
/// `config.toml`, falling back to `workspace_dir` only when the config path has
/// no parent - which is why the caller passes the workspace it is building for.
pub(crate) fn microsoft365_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
    workspace_dir: &std::path::Path,
) -> Microsoft365Registration {
    if !root_config.microsoft365.enabled {
        return Microsoft365Registration::Skip;
    }
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
    if tenant_id.is_empty() || client_id.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "microsoft365: skipped registration because tenant_id or client_id is empty"
        );
        return Microsoft365Registration::Skip;
    }
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
        return Microsoft365Registration::AbortRegistry;
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
    match Microsoft365Tool::new(resolved, security, cache_dir) {
        Ok(tool) => Microsoft365Registration::Tool(Arc::new(tool)),
        Err(e) => {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "microsoft365: failed to initialize tool"
            );
            Microsoft365Registration::Skip
        }
    }
}

/// Rebuilds `model_routing_config` bound to `security`. Always registered.
pub(crate) fn model_routing_config_tool(
    security: Arc<SecurityPolicy>,
    config: Arc<Config>,
) -> Arc<dyn Tool> {
    Arc::new(ModelRoutingConfigTool::new(config, security))
}

/// Rebuilds `proxy_config` bound to `security`. Always registered.
pub(crate) fn proxy_config_tool(
    security: Arc<SecurityPolicy>,
    config: Arc<Config>,
) -> Arc<dyn Tool> {
    Arc::new(ProxyConfigTool::new(config, security))
}

/// Rebuilds `sessions_history` bound to `security`. The backend is built from
/// the global `config.data_dir` + session-backend setting, so it is the same
/// object for caller and target; only the gate differs.
pub(crate) fn sessions_history_tool(
    security: Arc<SecurityPolicy>,
    backend: Arc<dyn zeroclaw_infra::session_backend::SessionBackend>,
) -> Arc<dyn Tool> {
    Arc::new(SessionsHistoryTool::new(backend, security))
}

/// Rebuilds `sessions_send` bound to `security`. Same backend reasoning as
/// [`sessions_history_tool`].
pub(crate) fn sessions_send_tool(
    security: Arc<SecurityPolicy>,
    backend: Arc<dyn zeroclaw_infra::session_backend::SessionBackend>,
) -> Arc<dyn Tool> {
    Arc::new(SessionsSendTool::new(backend, security))
}

// ── Factories for IDENTITY_BOUND_TOOL_NAMES ──────────────────────────────
//
// Each function below rebuilds exactly one of the tools from
// `IDENTITY_BOUND_TOOL_NAMES`, gated the same way `all_tools_with_runtime`
// gates it. These bind the CALLER's `agent_alias` (not `workspace_dir`/
// `SecurityPolicy` alone), so a `Bounded` delegate to a cross-profile target
// must rebuild them against the target's own `agent_alias` too.

/// Rebuilds `read_skill` bound to `agent_alias`, gated like
/// `all_tools_with_runtime` gates it (`config.effective_skills_prompt_mode(agent_alias)
/// == Compact`). `None` when not in compact mode. `ReadSkillTool` resolves
/// workspace skills via `config.agent_workspace_dir(agent_alias)`
/// (`skills/mod.rs`'s `load_skills_for_agent_from_config`), so a `Bounded`
/// cross-profile target must get its own alias here, not the caller's.
pub(crate) fn read_skill_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
) -> Option<Arc<dyn Tool>> {
    if !matches!(
        config.effective_skills_prompt_mode(agent_alias),
        zeroclaw_config::schema::SkillsPromptInjectionMode::Compact
    ) {
        return None;
    }
    Some(Arc::new(ReadSkillTool::new(
        config,
        agent_alias.to_string(),
    )))
}

/// Rebuilds `cron_add` bound to `agent_alias` - unconditional in production
/// (`all_tools_with_runtime` never gates it), so this always returns a tool.
/// Jobs created through this tool are stored under `agent_alias` and later
/// run - asynchronously - under that identity's own risk profile
/// (`cron/scheduler.rs` re-derives `SecurityPolicy::for_agent` from the
/// job's stored alias), so a `Bounded` cross-profile target must get its own
/// alias here, not the caller's.
pub(crate) fn cron_add_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    security: Arc<SecurityPolicy>,
    agent_alias: &str,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Arc<dyn Tool> {
    Arc::new(CronAddTool::new_with_runtime(
        config,
        security,
        agent_alias.to_string(),
        runtime,
    ))
}

/// Rebuilds `cron_update` bound to `agent_alias` - unconditional in
/// production. See [`cron_add_tool`] for why `agent_alias` must be the
/// target's own.
pub(crate) fn cron_update_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    security: Arc<SecurityPolicy>,
    agent_alias: &str,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Arc<dyn Tool> {
    Arc::new(CronUpdateTool::new_with_runtime(
        config,
        security,
        agent_alias.to_string(),
        runtime,
    ))
}

/// Rebuilds `cron_remove` bound to `agent_alias` - unconditional in
/// production. `CronRemoveTool` scopes job-name resolution to
/// `agent_alias`'s own jobs, so a `Bounded` cross-profile target reusing the
/// caller's instance could delete the CALLER's jobs.
pub(crate) fn cron_remove_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    security: Arc<SecurityPolicy>,
    agent_alias: &str,
) -> Arc<dyn Tool> {
    Arc::new(CronRemoveTool::new(
        config,
        security,
        agent_alias.to_string(),
    ))
}

/// Rebuilds `llm_task` bound to `agent_alias`'s OWN model provider.
///
/// `LlmTaskTool` bakes the resolved provider's `api_key` into a field, so
/// reusing the caller's instance hands a `Bounded` target the CALLER's
/// credential and bills its provider. `None` when the target resolves no
/// provider - the tool is then omitted rather than falling back to the
/// caller's, which fails before any network call rather than after.
pub(crate) fn llm_task_tool(
    security: Arc<SecurityPolicy>,
    root_config: &Config,
    agent_alias: &str,
) -> Option<Arc<dyn Tool>> {
    let (family, alias, entry) = root_config.resolved_model_provider_for_agent(agent_alias)?;
    let llm_task_provider = family.to_string();
    let llm_task_model = entry
        .model
        .clone()
        .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
    let llm_task_runtime_options =
        zeroclaw_providers::provider_runtime_options_for_alias(root_config, family, alias);
    Some(Arc::new(LlmTaskTool::new(
        security,
        llm_task_provider,
        llm_task_model,
        entry.temperature,
        entry.api_key.clone(),
        llm_task_runtime_options,
    )))
}

/// Rebuilds `cron_list` bound to `agent_alias` - unconditional in
/// production. `CronListTool` enumerates through
/// `cron::list_jobs_by_agent(.., &self.agent_alias)`, so a `Bounded`
/// cross-profile target reusing the caller's instance would be shown the
/// CALLER's schedule instead of its own.
pub(crate) fn cron_list_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
) -> Arc<dyn Tool> {
    Arc::new(CronListTool::new(config, agent_alias.to_string()))
}

/// Rebuilds `cron_run` bound to `agent_alias` - unconditional in production.
/// The most severe of the three: `CronRunTool` resolves the job through
/// `cron::get_job_for_agent(.., &self.agent_alias)` and then EXECUTES it, so
/// a `Bounded` cross-profile target reusing the caller's instance could fire
/// the caller's jobs, which run under the identity stored on the job.
pub(crate) fn cron_run_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    security: Arc<SecurityPolicy>,
    agent_alias: &str,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Arc<dyn Tool> {
    Arc::new(CronRunTool::new_with_runtime(
        config,
        security,
        agent_alias.to_string(),
        runtime,
    ))
}

/// Rebuilds `cron_runs` bound to `agent_alias` - unconditional in production.
/// Reads run history through the same `cron::get_job_for_agent` resolution as
/// [`cron_run_tool`], so the caller's instance exposes the CALLER's run log.
pub(crate) fn cron_runs_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
) -> Arc<dyn Tool> {
    Arc::new(CronRunsTool::new(config, agent_alias.to_string()))
}

/// Rebuilds `schedule` bound to `agent_alias` - unconditional in production.
/// Combines `cron_add`/`cron_remove`-equivalent semantics (create, pause,
/// resume, one-shot) in a single tool; see [`cron_add_tool`] for why
/// `agent_alias` must be the target's own. `ScheduleTool` takes an owned
/// `Config`, not `Arc<Config>`, matching `all_tools_with_runtime`'s own call.
pub(crate) fn schedule_tool(
    security: Arc<SecurityPolicy>,
    config: zeroclaw_config::schema::Config,
    agent_alias: &str,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Arc<dyn Tool> {
    Arc::new(ScheduleTool::new_with_runtime(
        security,
        config,
        agent_alias.to_string(),
        runtime,
    ))
}

/// Rebuilds `send_message_to_peer` bound to `agent_alias` - unconditional in
/// production. `SendMessageToPeerTool` resolves the sender's peer set and
/// channel membership from `agent_alias` (`send_message_to_peer.rs`'s own
/// doc comment: "validates every send against that agent's resolved peer
/// set"), so a `Bounded` cross-profile target reusing the caller's instance
/// could reach the CALLER's peers/channels, not its own.
pub(crate) fn send_message_to_peer_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
) -> Arc<dyn Tool> {
    Arc::new(SendMessageToPeerTool::new(config, agent_alias.to_string()))
}

/// Rebuilds `spawn_subagent` bound to `agent_alias`/`security` -
/// unconditional in production. The single most severe tool in this class:
/// `SpawnSubagentTool` spawns a SubAgent that synchronously inherits
/// `agent_alias`'s full identity, `SecurityPolicy`, and permissions envelope
/// (its own doc comment: "runs the supplied prompt to completion under the
/// parent's permissions envelope"), and even its own admission check
/// (`config.risk_profile_for_agent(&self.parent_alias)`) is scoped to that
/// alias. A `Bounded` cross-profile target reusing the caller's instance
/// could spawn a SubAgent running arbitrary prompts under the CALLER's full
/// permissions, regardless of the target's own (possibly far more
/// restricted) risk profile. `delegate.rs`'s `Bounded` reconstruction always
/// passes `is_subagent_caller = false`, matching what `independent`-mode
/// delegation already passes when building a target's own registry
/// (`delegate.rs`'s `independent` branch) - `Bounded` delegation is not
/// itself a "subagent" in the depth-1-cap sense this flag guards.
pub(crate) fn spawn_subagent_tool(
    config: Arc<zeroclaw_config::schema::Config>,
    agent_alias: &str,
    security: Arc<SecurityPolicy>,
    is_subagent_caller: bool,
    caller_ceiling: Option<Arc<std::sync::OnceLock<Vec<String>>>>,
) -> Arc<dyn Tool> {
    Arc::new(
        SpawnSubagentTool::new(config, agent_alias.to_string(), security)
            .with_caller_ceiling(caller_ceiling)
            .with_subagent_caller(is_subagent_caller),
    )
}

/// Rebuilds `claude_code_runner` bound to `security`, gated like
/// `all_tools_with_runtime` gates it (`root_config.claude_code_runner.enabled`
/// alone - unlike the 4 tools below, it never uses `coding_cli_executor`).
/// `None` when disabled.
pub(crate) fn claude_code_runner_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
) -> Option<Arc<dyn Tool>> {
    if !root_config.claude_code_runner.enabled {
        return None;
    }
    let gateway_url = format!(
        "http://{}:{}",
        root_config.gateway.host, root_config.gateway.port
    );
    Some(Arc::new(RateLimitedTool::new(
        ClaudeCodeRunnerTool::new(
            security.clone(),
            root_config.claude_code_runner.clone(),
            gateway_url,
        ),
        security,
    )))
}

/// Rebuilds `claude_code` bound to `security` and the given (already
/// target-resolved) `executor`, gated like `all_tools_with_runtime` gates it
/// (`register_coding_cli_tools && root_config.claude_code.enabled`). `None`
/// when disabled.
pub(crate) fn claude_code_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
    register_coding_cli_tools: bool,
    executor: &Arc<dyn zeroclaw_tools::coding_cli::CodingCliExecutor>,
) -> Option<Arc<dyn Tool>> {
    if !(register_coding_cli_tools && root_config.claude_code.enabled) {
        return None;
    }
    Some(Arc::new(RateLimitedTool::new(
        ClaudeCodeTool::new_with_executor(
            security.clone(),
            root_config.claude_code.clone(),
            executor.clone(),
        ),
        security,
    )))
}

/// Rebuilds `codex_cli` - see [`claude_code_tool`] above for the shared
/// gating/wiring rationale.
pub(crate) fn codex_cli_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
    register_coding_cli_tools: bool,
    executor: &Arc<dyn zeroclaw_tools::coding_cli::CodingCliExecutor>,
) -> Option<Arc<dyn Tool>> {
    if !(register_coding_cli_tools && root_config.codex_cli.enabled) {
        return None;
    }
    Some(Arc::new(RateLimitedTool::new(
        CodexCliTool::new_with_executor(
            security.clone(),
            root_config.codex_cli.clone(),
            executor.clone(),
        ),
        security,
    )))
}

/// Rebuilds `gemini_cli` - see [`claude_code_tool`] above.
pub(crate) fn gemini_cli_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
    register_coding_cli_tools: bool,
    executor: &Arc<dyn zeroclaw_tools::coding_cli::CodingCliExecutor>,
) -> Option<Arc<dyn Tool>> {
    if !(register_coding_cli_tools && root_config.gemini_cli.enabled) {
        return None;
    }
    Some(Arc::new(RateLimitedTool::new(
        GeminiCliTool::new_with_executor(
            security.clone(),
            root_config.gemini_cli.clone(),
            executor.clone(),
        ),
        security,
    )))
}

/// Rebuilds `opencode_cli` - see [`claude_code_tool`] above.
pub(crate) fn opencode_cli_tool(
    security: Arc<SecurityPolicy>,
    root_config: &zeroclaw_config::schema::Config,
    register_coding_cli_tools: bool,
    executor: &Arc<dyn zeroclaw_tools::coding_cli::CodingCliExecutor>,
) -> Option<Arc<dyn Tool>> {
    if !(register_coding_cli_tools && root_config.opencode_cli.enabled) {
        return None;
    }
    Some(Arc::new(RateLimitedTool::new(
        OpenCodeCliTool::new_with_executor(
            security.clone(),
            root_config.opencode_cli.clone(),
            executor.clone(),
        ),
        security,
    )))
}

#[cfg(test)]
pub(crate) fn register_skill_tools(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
) {
    register_skill_tools_with_context(tools_registry, skills, security, &[]);
}

/// Register skill-defined tools with full context for builtin kinds.
/// `unfiltered_registry` provides the pre-policy tool list for `kind = "builtin"`
/// delegation.
#[cfg(test)]
pub(crate) fn register_skill_tools_with_context(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
    unfiltered_registry: &[Arc<dyn Tool>],
) {
    register_skill_tools_with_context_and_runtime_optional_nat64(
        tools_registry,
        skills,
        security,
        unfiltered_registry,
        Arc::new(NativeRuntime::new()),
        Some(&[]),
    );
}

pub fn register_skill_tools_with_context_and_runtime(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
    unfiltered_registry: &[Arc<dyn Tool>],
    runtime: Arc<dyn RuntimeAdapter>,
    nat64_prefixes: &[zeroclaw_infra::net_guard::Nat64Prefix],
) {
    register_skill_tools_with_context_and_runtime_optional_nat64(
        tools_registry,
        skills,
        security,
        unfiltered_registry,
        runtime,
        Some(nat64_prefixes),
    );
}

/// Internal scoped assembly seam. `None` omits only HTTP tools after an invalid
/// NAT64 configuration; other skill kinds continue through their normal path.
pub(crate) fn register_skill_tools_with_context_and_runtime_optional_nat64(
    tools_registry: &mut Vec<Box<dyn Tool>>,
    skills: &[crate::skills::Skill],
    security: Arc<SecurityPolicy>,
    unfiltered_registry: &[Arc<dyn Tool>],
    runtime: Arc<dyn RuntimeAdapter>,
    nat64_prefixes: Option<&[zeroclaw_infra::net_guard::Nat64Prefix]>,
) {
    if skills.is_empty() {
        return;
    }

    let before = tools_registry.len();
    let policy = Arc::clone(&security);
    let skill_tools = crate::skills::skills_to_tools_with_context_and_runtime_optional_nat64(
        skills,
        security,
        unfiltered_registry,
        runtime,
        nat64_prefixes,
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

pub async fn collect_mcp_elevation_arcs(
    registry: &Arc<McpRegistry>,
    security: &Arc<zeroclaw_config::policy::SecurityPolicy>,
) -> Vec<Arc<dyn Tool>> {
    let mut arcs: Vec<Arc<dyn Tool>> = Vec::new();
    for name in registry.tool_names() {
        if let Some(def) = registry.get_tool_def(&name).await {
            arcs.push(Arc::new(McpToolWrapper::new(
                name,
                def,
                Arc::clone(registry),
                Arc::clone(security),
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
    /// The eager registry retained by this factory, before the per-agent
    /// `allowed_tools`/`excluded_tools` filter.
    ///
    /// This is the raw material the gated seam consumes, not an
    /// already-filtered view: `ScopedToolRegistry::assemble` applies the
    /// `allowed_tools`/`excluded_tools` policy filter (and MCP scoping) when it
    /// mints the per-agent tool set. Every production caller routes this field
    /// straight into `assemble`, so an unfiltered value here is correct and is
    /// not a policy bypass. Documented explicitly because the neighbouring
    /// `unfiltered_tool_arcs` name implies by contrast that this field is the
    /// filtered one, which has already misled readers into believing built-ins
    /// escaped `allowed_tools`.
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
    /// The exact `DelegateTool` this factory registered, in its concrete type.
    ///
    /// Test-only. `tools`/`unfiltered_tool_arcs` erase the type behind
    /// `dyn Tool`, so a regression cannot otherwise drive the *production*
    /// delegate instance's nested-registry construction - it can only
    /// re-derive the wiring by hand, which is exactly the thing that must not
    /// be trusted. `None` when no agents are configured.
    #[cfg(test)]
    pub(crate) delegate_tool: Option<Arc<DelegateTool>>,
}

impl AllToolsResult {
    /// Wrap an already-built tool vector as `assemble` INPUT, with every
    /// side-channel handle empty. This mints an `AllToolsResult` (the input to
    /// [`crate::tools::scoped::ScopedToolRegistry::assemble`]), NOT a
    /// `ScopedToolRegistry` - it does not touch the seal. (`AllToolsResult`'s
    /// fields are all `pub`, so a caller could already hand-roll this literal;
    /// the helper just centralizes the "all handles empty" shape.) Used by the
    /// paths that already own a fixed / pre-filtered tool set (the skill-review
    /// harness, bounded delegation, and the `zeroclaw-eval` replay harness) and
    /// route it through `assemble` only to seal it: they pass `skills: &[]`,
    /// `connect_mcp: false`, `connect_peripherals: false`, so the empty handles
    /// here are never read by the assembly. `pub` (not `pub(crate)`) so the
    /// out-of-crate `zeroclaw-eval` harness can reach it.
    pub fn from_prebuilt_tools(tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            tools,
            delegate_handle: None,
            ask_user_handle: None,
            channel_room_handle: None,
            reaction_handle: Arc::new(RwLock::new(HashMap::new())),
            poll_handle: None,
            escalate_handle: None,
            unfiltered_tool_arcs: Vec::new(),
            #[cfg(test)]
            delegate_tool: None,
        }
    }
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

struct RuntimeShellAssembly {
    shell_tool: ShellTool,
    sandbox: Arc<dyn Sandbox>,
}

/// Pair the canonical runtime kind with one shared sandbox instance for every
/// runtime-backed executor assembled by the production tool registry.
fn runtime_shell_assembly(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    risk_profile: &zeroclaw_config::schema::RiskProfileConfig,
    root_config: &Config,
) -> RuntimeShellAssembly {
    let sandbox_cfg = risk_profile.sandbox_config();
    let sandbox_extra_roots = crate::security::SandboxExtraRoots {
        read_write: security.allowed_roots.clone(),
        read_only: security.allowed_roots_read_only.clone(),
        write_only: security.allowed_roots_write_only.clone(),
    };
    let sandbox = create_sandbox(
        &sandbox_cfg,
        root_config.runtime.kind,
        Some(&security.workspace_dir),
        &sandbox_extra_roots,
    );
    // Built through `build_sandboxed_shell_tool` rather than
    // `ShellTool::new_with_sandbox` directly, so this production seam and
    // `delegate.rs`'s `Bounded` cross-profile target rebuild share ONE
    // assembly step - sandbox, the `shell_timeout_secs == 0 -> global
    // default` contract, and persistent writes - and cannot silently
    // diverge on it.
    let shell_tool = build_sandboxed_shell_tool(
        security,
        runtime,
        sandbox.clone(),
        root_config.shell_tool.timeout_secs,
    );
    RuntimeShellAssembly {
        shell_tool,
        sandbox,
    }
}

/// Assemble a shell tool through the same runtime/sandbox ownership seam used
/// by the production registry.
#[must_use]
pub fn shell_tool_for_runtime(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    risk_profile: &zeroclaw_config::schema::RiskProfileConfig,
    root_config: &Config,
) -> ShellTool {
    runtime_shell_assembly(security, runtime, risk_profile, root_config).shell_tool
}

/// One plugin instance's egress policy, read from canonical config at use time.
///
/// `instance_key` is the instance's
/// [`PluginInstanceScope::config_entry_key`][key] — the very same
/// `plugins.entries[].name` that [`plugin_config_values`] resolves against, so
/// a single row carries both an instance's config and its granted reach.
///
/// The operator's `plugins.entries[].egress_hosts` is the only source of reach.
/// The deployment's NAT64 prefixes and connection ceiling are read from the same
/// config so a plugin and a built-in tool cannot classify one answer set
/// differently.
///
/// [key]: zeroclaw_plugins::instance::PluginInstanceScope::config_entry_key
#[cfg(feature = "plugins-wasm")]
fn plugin_egress_policy(
    config: &Config,
    instance_key: &str,
) -> Result<zeroclaw_plugins::egress::EgressPolicy, zeroclaw_plugins::egress::EgressError> {
    let (hosts, allow_private) = config.plugins.entry_egress(instance_key);
    zeroclaw_plugins::egress::EgressPolicy::new(
        &hosts,
        &allow_private,
        &config.security.nat64_prefixes,
        config.plugins.limits.max_connections_per_instance,
    )
}

/// The one host-owned egress authority shared by every plugin instance in a
/// registry.
///
/// It resolves a *view* of canonical config at the moment each request is made
/// rather than snapshotting one here, so an operator edit takes effect without
/// re-instantiating the guest. The live handle is preferred; one-shot callers
/// that have none fall back to the documented `root_config` snapshot.
///
/// A fresh service is built per registry, and that is deliberately fine: the
/// per-instance connection budget does *not* live in the service. It lives in
/// `zeroclaw_plugins::egress`'s process-wide registry, keyed by canonical
/// instance identity, so the agent loop, the gateway, the channels
/// orchestrator, and the delegate tool spend one ceiling between them rather
/// than one each.
///
/// Egress and config resolve the **same** `[[plugins.entries]]` row: both key on
/// the instance's `config_entry_key()` (the opaque `zpi1_` instance key), never
/// on the package name or the raw binding. Keying egress by the binding would
/// miss the row `zeroclaw plugin install` seeds and deny every destination the
/// operator granted.
#[cfg(feature = "plugins-wasm")]
fn plugin_egress_service(
    config: Arc<Config>,
    live_config: Option<Arc<parking_lot::RwLock<Config>>>,
) -> zeroclaw_plugins::egress::EgressHostService {
    zeroclaw_plugins::egress::EgressHostService::new(
        zeroclaw_plugins::egress::EgressPolicyResolver::new(move |scope| {
            let instance_key = scope.id().config_entry_key().map_err(|error| {
                zeroclaw_plugins::egress::EgressError::PolicyUnavailable(error.to_string())
            })?;
            match live_config.as_ref() {
                Some(handle) => plugin_egress_policy(&handle.read(), &instance_key),
                None => plugin_egress_policy(&config, &instance_key),
            }
        }),
    )
}

#[cfg(feature = "plugins-wasm")]
fn plugin_config_values(
    config: &Config,
    instance_key: &str,
    package: &str,
) -> Result<Option<HashMap<String, String>>, zeroclaw_plugins::error::PluginError> {
    config
        .plugins
        .entry_config(instance_key)
        .map(|configured| configured.cloned())
        .map_err(|_| {
            zeroclaw_plugins::error::PluginError::InvalidConfig(format!(
                "plugin '{package}' has duplicate config entries for its instance key"
            ))
        })
}

#[cfg(feature = "plugins-wasm")]
pub(crate) fn plugin_host_services(
    host: Arc<zeroclaw_plugins::host::PluginHost>,
    config: Arc<Config>,
    live_config: Option<Arc<parking_lot::RwLock<Config>>>,
) -> zeroclaw_plugins::services::PluginHostServices {
    // A live daemon handle and a fallback snapshot are mutually exclusive in
    // the long-lived service, so the resolver never retains two config sources.
    let fallback_config = live_config.is_none().then_some(config);
    let config = zeroclaw_plugins::config::PluginConfigResolver::new(move |scope| {
        let package = scope.id().package();
        let config_entry_key = scope.id().config_entry_key()?;
        let manifest = host
            .manifest(package)
            .ok_or_else(|| zeroclaw_plugins::error::PluginError::NotFound(package.to_string()))?;
        if let Some(live_config) = &live_config {
            zeroclaw_plugins::config::resolve_plugin_config_from(manifest, scope, || {
                // Transient per-call view: schema/grant checks happen before
                // this access, and the global lock is released before guest
                // setup.
                plugin_config_values(&live_config.read(), &config_entry_key, package)
            })
        } else {
            let config = fallback_config.as_ref().ok_or_else(|| {
                zeroclaw_plugins::error::PluginError::InvalidConfig(
                    "plugin config source is unavailable".to_string(),
                )
            })?;
            zeroclaw_plugins::config::resolve_plugin_config_from(manifest, scope, || {
                plugin_config_values(config, &config_entry_key, package)
            })
        }
    });
    zeroclaw_plugins::services::PluginHostServices::new(config)
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
    let register_coding_cli_tools = has_shell_access && persistent_writes;
    let RuntimeShellAssembly {
        shell_tool,
        sandbox,
    } = runtime_shell_assembly(security.clone(), runtime.clone(), risk_profile, root_config);
    let coding_cli_executor = coding_cli_executor::RuntimeCodingCliExecutor::shared(
        runtime.clone(),
        sandbox.clone(),
        root_config.runtime.kind == zeroclaw_config::schema::RuntimeKind::Native,
    );
    // Keep a shared runtime adapter available after constructing ShellTool.
    // Independent agentic delegates use it later to build the target-owned tool
    // registry; bounded delegates continue to use the parent `tool_arcs`
    // snapshot below.
    let mut tool_arcs: Vec<Arc<dyn Tool>> = vec![
        Arc::new(RateLimitedTool::new(
            shell_tool.with_tui_env(tui_env),
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
            PathGuardedTool::new(DeliverFileTool::new(security.clone()), security.clone()),
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
        cron_add_tool(
            config.clone(),
            security.clone(),
            agent_alias,
            runtime.clone(),
        ),
        cron_list_tool(config.clone(), agent_alias),
        cron_remove_tool(config.clone(), security.clone(), agent_alias),
        cron_update_tool(
            config.clone(),
            security.clone(),
            agent_alias,
            runtime.clone(),
        ),
        cron_run_tool(
            config.clone(),
            security.clone(),
            agent_alias,
            runtime.clone(),
        ),
        cron_runs_tool(config.clone(), agent_alias),
        Arc::new(MemoryStoreTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone())),
        Arc::new(MemoryForgetTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryExportTool::new(memory.clone())),
        Arc::new(MemoryPurgeTool::new(memory.clone(), security.clone())),
        schedule_tool(
            security.clone(),
            root_config.clone(),
            agent_alias,
            runtime.clone(),
        ),
        spawn_subagent_tool(
            config.clone(),
            agent_alias,
            security.clone(),
            is_subagent_caller,
            // A registry assembled for an agent in its own right has no caller
            // above it, so there is no ceiling to carry.
            None,
        ),
        send_message_to_peer_tool(config.clone(), agent_alias),
        model_routing_config_tool(security.clone(), config.clone()),
        Arc::new(ModelSwitchTool::new(security.clone(), config.clone())),
        proxy_config_tool(security.clone(), config.clone()),
        git_operations_tool(security.clone(), workspace_dir),
        pushover_tool(security.clone(), workspace_dir),
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
    if let Some(tool) = llm_task_tool(security.clone(), root_config, agent_alias) {
        tool_arcs.push(tool);
    }

    // ReadSkillTool holds full config to support workspace skills,
    // open-skills, agent-bound bundles, and plugin skills.
    if let Some(tool) = read_skill_tool(config.clone(), agent_alias) {
        tool_arcs.push(tool);
    }

    if let Some(tool) = browser_open_tool(security.clone(), browser_config) {
        tool_arcs.push(tool);
    }
    if let Some(tool) = browser_tool(security.clone(), browser_config) {
        tool_arcs.push(tool);
    }

    // Browser delegation tool (conditionally registered; requires shell access)
    if let Some(tool) = browser_delegate_tool(security.clone(), root_config, has_shell_access) {
        tool_arcs.push(tool);
    }

    if let Some(tool) = http_request_tool(security.clone(), http_config, root_config) {
        tool_arcs.push(tool);
    }

    if let Some(tool) = web_fetch_tool(security.clone(), web_fetch_config, root_config) {
        tool_arcs.push(tool);
    }

    // Text browser tool (headless text-based browser rendering)
    if let Some(tool) = text_browser_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
    }

    // Web search tool (enabled by default for GLM and other models)
    if root_config.web_search.enabled {
        // Rate-limited like every other outbound-network tool (see web_fetch
        // and http_request above): without the wrapper an agent loop could
        // issue unbounded searches against the configured provider — and
        // against the default DuckDuckGo scrape path, which gets the machine
        // blocked.
        if let Some(tool) = web_search_tool(security.clone(), root_config) {
            tool_arcs.push(tool);
        }
    }

    // Notion API tool (conditionally registered)
    if let Some(tool) = notion_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
    }

    // Jira integration (config-gated)
    if let Some(tool) = jira_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
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
    if let Some(tool) = backup_tool(workspace_dir, root_config) {
        tool_arcs.push(tool);
    }

    // Data management tool (disabled by default)
    if let Some(tool) = data_management_tool(workspace_dir, root_config) {
        tool_arcs.push(tool);
    }

    // Cloud operations advisory tools (read-only analysis)
    if root_config.cloud_ops.enabled {
        tool_arcs.push(Arc::new(CloudOpsTool::new(root_config.cloud_ops.clone())));
        tool_arcs.push(Arc::new(CloudPatternsTool::new()));
    }

    // Google Workspace CLI (gws) integration — requires shell access
    if let Some(tool) = google_workspace_tool(security.clone(), root_config, has_shell_access) {
        tool_arcs.push(tool);
    }

    if any_coding_cli_tool_enabled(root_config) && !register_coding_cli_tools {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "coding_cli: skipped registration because runtime shell or filesystem access is unavailable"
        );
    }

    // Claude Code delegation tool
    if let Some(tool) = claude_code_tool(
        security.clone(),
        root_config,
        register_coding_cli_tools,
        &coding_cli_executor,
    ) {
        tool_arcs.push(tool);
    }

    // Claude Code task runner with Slack progress and SSH handoff
    if let Some(tool) = claude_code_runner_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
    }

    // Codex CLI delegation tool
    if let Some(tool) = codex_cli_tool(
        security.clone(),
        root_config,
        register_coding_cli_tools,
        &coding_cli_executor,
    ) {
        tool_arcs.push(tool);
    }

    // Gemini CLI delegation tool
    if let Some(tool) = gemini_cli_tool(
        security.clone(),
        root_config,
        register_coding_cli_tools,
        &coding_cli_executor,
    ) {
        tool_arcs.push(tool);
    }

    // OpenCode CLI delegation tool
    if let Some(tool) = opencode_cli_tool(
        security.clone(),
        root_config,
        register_coding_cli_tools,
        &coding_cli_executor,
    ) {
        tool_arcs.push(tool);
    }

    // Vision tools are always available
    tool_arcs.push(screenshot_tool(security.clone()));
    tool_arcs.push(Arc::from(image_info_tool(security.clone())));

    if let Ok(backend) =
        zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
    {
        tool_arcs.push(Arc::new(SessionsCurrentTool::new(backend.clone())));
        tool_arcs.push(Arc::new(SessionsListTool::new(backend.clone())));
        tool_arcs.push(sessions_history_tool(security.clone(), backend.clone()));
        tool_arcs.push(sessions_send_tool(security.clone(), backend));
    }

    // LinkedIn integration (config-gated)
    if let Some(tool) = linkedin_tool(security.clone(), workspace_dir, root_config) {
        tool_arcs.push(tool);
    }

    // Standalone image generation tool (config-gated)
    if let Some(tool) = image_gen_tool(
        security.clone(),
        workspace_dir,
        root_config,
        persistent_writes,
    ) {
        tool_arcs.push(tool);
    }

    // File upload tool — enabled iff [file_upload].url is set
    if let Some(tool) = file_upload_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
    }

    // File upload bundle tool — enabled iff [file_upload_bundle].url is set
    if let Some(tool) = file_upload_bundle_tool(security.clone(), root_config) {
        tool_arcs.push(tool);
    }

    // File download tool — enabled iff [file_download].url is set
    if let Some(tool) = file_download_tool(security.clone(), root_config, persistent_writes) {
        tool_arcs.push(tool);
    }

    // Poll tool — always registered; owns its own late-bound channel map.
    let poll_handle: PerToolChannelHandle = Arc::new(RwLock::new(HashMap::new()));
    tool_arcs.push(poll_tool(security.clone(), Arc::clone(&poll_handle)));

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
                root_config.install_root_dir(),
            )));
        }
    }

    if let Some(tool) = composio_tool(security.clone(), composio_key, composio_entity_id) {
        tool_arcs.push(tool);
    }

    // Emoji reaction tool — always registered; owns its own late-bound channel map.
    let reaction_handle: PerToolChannelHandle = Arc::new(RwLock::new(HashMap::new()));
    tool_arcs.push(reaction_tool(
        security.clone(),
        Arc::clone(&reaction_handle),
    ));

    // Unified forge operations tool, routes through the git channel via the
    // same late-bound channel map as the reaction tool. Resource/action grid
    // plus a raw catch-all over the channel's single forge_request transport.
    let forge_tool = git_forge_tool(security.clone(), Arc::clone(&reaction_handle));
    tool_arcs.push(forge_tool);

    // Channel room-management tool — always registered; owns its own late-bound channel map.
    let channel_room_handle: Option<PerToolChannelHandle> =
        Some(Arc::new(RwLock::new(HashMap::new())));
    tool_arcs.push(channel_room_tool(
        security.clone(),
        channel_room_handle.as_ref().cloned().unwrap(),
    ));

    // Interactive ask_user tool — always registered; owns its own late-bound channel map.
    let ask_user_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    tool_arcs.push(ask_user_tool(
        security.clone(),
        ask_user_handle.as_ref().cloned().unwrap(),
    ));

    tool_arcs.push(send_via_tool(
        security.clone(),
        root_config,
        live_config.clone(),
        agent_alias,
        ask_user_handle.as_ref().cloned().unwrap(),
    ));

    // Human escalation tool — always registered; owns its own late-bound channel map.
    let escalate_handle: Option<PerToolChannelHandle> = Some(Arc::new(RwLock::new(HashMap::new())));
    tool_arcs.push(escalate_to_human_tool(
        security.clone(),
        root_config.escalation.alert_channels.clone(),
        escalate_handle.as_ref().cloned().unwrap(),
    ));

    // Microsoft 365 Graph API integration
    match microsoft365_tool(security.clone(), root_config, workspace_dir) {
        Microsoft365Registration::Tool(tool) => tool_arcs.push(tool),
        Microsoft365Registration::Skip => {}
        // Preserved fail-fast: a client_credentials flow with no secret
        // aborts the whole registry rather than registering anything else.
        Microsoft365Registration::AbortRegistry => {
            return AllToolsResult {
                unfiltered_tool_arcs: tool_arcs.clone(),
                tools: boxed_registry_from_arcs(tool_arcs),
                delegate_handle: None,
                #[cfg(test)]
                delegate_tool: None,
                ask_user_handle,
                channel_room_handle,
                reaction_handle,
                poll_handle: Some(poll_handle),
                escalate_handle,
            };
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

    #[cfg(test)]
    let mut built_delegate_tool: Option<Arc<DelegateTool>> = None;
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
        // `with_root_config` above is only a snapshot. Delegated targets get
        // their own nested registry, whose plugin tools and `send_via`
        // authority resolve per execution; without the shared handle they would
        // resolve against that snapshot forever. Same contract as the
        // `live_config` argument this function received.
        .with_live_config(live_config.clone())
        // The caller's live channel handles: a bounded target's channel tools
        // are rebuilt against its own policy but must keep answering on these
        // very maps, which the daemon binds to real channels after this point.
        .with_channel_handles(DelegateChannelHandles {
            poll: Some(Arc::clone(&poll_handle)),
            reaction: Some(Arc::clone(&reaction_handle)),
            channel_room: channel_room_handle.as_ref().cloned(),
            ask_user: ask_user_handle.as_ref().cloned(),
            escalate: escalate_handle.as_ref().cloned(),
        })
        .with_caller_alias(agent_alias);
        let delegate_tool = Arc::new(delegate_tool);
        #[cfg(test)]
        {
            built_delegate_tool = Some(Arc::clone(&delegate_tool));
        }
        tool_arcs.push(delegate_tool as Arc<dyn Tool>);
        Some(parent_tools)
    };

    // `vi_verify` is deliberately absent while no chain verifier exists: it checked
    // caller-supplied constraints against a caller-supplied fulfillment with nothing
    // establishing that either came from a signed credential. The operator-facing
    // notice lives at config load, since this function also runs per gateway request
    // and per nested registry rebuild. Register it again only behind a
    // verify-and-evaluate path that consumes a verified chain result.

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
                    let host = Arc::new(host);
                    let host_services = plugin_host_services(
                        Arc::clone(&host),
                        Arc::clone(&config),
                        live_config.clone(),
                    );
                    let mut registered_names: std::collections::HashSet<String> = tool_arcs
                        .iter()
                        .map(|tool| tool.name().to_string())
                        .collect();
                    if root_config.pipeline.enabled {
                        registered_names.insert(PipelineTool::NAME.to_string());
                    }
                    let plugin_limits = zeroclaw_plugins::component::PluginLimits {
                        call_fuel: config.plugins.limits.call_fuel,
                        max_memory_bytes: config
                            .plugins
                            .limits
                            .max_memory_mb
                            .saturating_mul(1024 * 1024),
                        max_table_elements: config.plugins.limits.max_table_elements,
                        max_instances: config.plugins.limits.max_instances,
                        call_timeout: std::time::Duration::from_millis(
                            config.plugins.limits.call_timeout_ms,
                        ),
                    };
                    let egress_service =
                        plugin_egress_service(Arc::clone(&config), live_config.clone());
                    register_plugin_tools(
                        &config,
                        &host,
                        &host_services,
                        plugin_limits,
                        Some(egress_service),
                        &mut registered_names,
                        &mut tool_arcs,
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

    // Pipeline construction waits for ScopedToolRegistry::assemble(), where the
    // effective per-agent policy and optional caller allowlist are both known.

    AllToolsResult {
        unfiltered_tool_arcs: tool_arcs.clone(),
        tools: boxed_registry_from_arcs(tool_arcs),
        delegate_handle,
        ask_user_handle,
        channel_room_handle,
        reaction_handle,
        poll_handle: Some(poll_handle),
        escalate_handle,
        #[cfg(test)]
        delegate_tool: built_delegate_tool,
    }
}

#[cfg(feature = "plugins-wasm")]
fn claim_plugin_tool_name(
    registered_names: &mut std::collections::HashSet<String>,
    plugin_name: &str,
) -> bool {
    registered_names.insert(plugin_name.to_string())
}

/// Construct and register every tool-plugin instance the activation plan
/// admitted.
///
/// The shared `plugins.max_active_instances` ceiling spans channels, tools,
/// and skills, so this loader may not walk `tool_plugin_details()` directly:
/// it consults [`PluginActivationPlan`] and constructs only the tool scopes
/// that plan admitted, in the plan's enumeration order. The plan is a pure
/// function of the current config and host snapshot and holds no counter, so
/// rebuilding this registry (per agent, per CLI run, per delegate, per SOP
/// execution) re-derives the same admitted set rather than spending the same
/// logical slot again.
///
/// Admission composes ahead of, and does not weaken, the collision pre-gate.
/// For each admitted candidate the order still matters: the signed manifest
/// name is the only identifier the host knows before the guest runs, so it is
/// checked against the registered tool names *first*, and a package that could
/// never be registered is refused without resolving its scoped config,
/// instantiating its component, or calling its metadata export. The
/// guest-declared callable name is claimed afterwards, because nothing can
/// know it earlier.
#[cfg(feature = "plugins-wasm")]
fn register_plugin_tools(
    config: &Config,
    host: &zeroclaw_plugins::host::PluginHost,
    services: &zeroclaw_plugins::services::PluginHostServices,
    plugin_limits: zeroclaw_plugins::component::PluginLimits,
    egress: Option<zeroclaw_plugins::egress::EgressHostService>,
    registered_names: &mut std::collections::HashSet<String>,
    tool_arcs: &mut Vec<Arc<dyn Tool>>,
) {
    let plan = match crate::plugin_runtime::PluginActivationPlan::build(config, host) {
        Ok(plan) => plan,
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{error}")})),
                "Failed to admit logical plugin tool instances"
            );
            return;
        }
    };
    let details = host.tool_plugin_details();
    let discovered_count = details.len();
    let admitted: Vec<_> = plan
        .scopes(zeroclaw_plugins::PluginCapability::Tool)
        .collect();
    let admitted_count = admitted.len();
    let mut registered_count = 0_usize;

    for scope in admitted {
        let package = scope.id().package().to_string();
        let Some((manifest, wasm_path)) = details
            .iter()
            .copied()
            .find(|(manifest, _)| manifest.name == package)
        else {
            continue;
        };
        // Phase one: guest-free refusal. Deliberately a check and not a claim —
        // the reservation set holds callable tool names, and a package name is
        // not one until its guest declares it.
        if registered_names.contains(&manifest.name) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "plugin": manifest.name,
                        "error_key": "plugin_package_name_conflict",
                    })),
                "Plugin package name conflicts with an already registered tool; \
                 refused before loading its component"
            );
            continue;
        }

        let tool = zeroclaw_plugins::wasm_tool::WasmTool::from_wasm(
            wasm_path.to_path_buf(),
            scope,
            services.clone(),
            plugin_limits,
            egress.clone(),
        );
        match tool {
            Ok(tool) => {
                if !claim_plugin_tool_name(registered_names, tool.name()) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "plugin": manifest.name,
                                "tool": tool.name(),
                                "error_key": "plugin_tool_name_conflict",
                            })),
                        "Plugin tool conflicts with an already registered tool"
                    );
                    continue;
                }
                tool_arcs.push(Arc::new(tool));
                registered_count += 1;
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "plugin": manifest.name,
                            "error": format!("{e:#}"),
                        })),
                    "Failed to register WASM plugin tool"
                );
            }
        }
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            ::serde_json::json!({
                "discovered": discovered_count,
                "admitted": admitted_count,
                "registered": registered_count,
            })
        ),
        "Registered WASM plugin tools"
    );
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

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn plugin_host_services_isolate_live_instance_keys() {
        let plugins_dir = TempDir::new().unwrap();
        let plugin_dir = plugins_dir.path().join("fixture-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"name = "fixture-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["config_read"]

[config_schema]
type = "object"
required = ["enabled"]
additionalProperties = false

[config_schema.properties.enabled]
type = "boolean"
const = true
"#,
        )
        .unwrap();
        let host = Arc::new(
            zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins_dir.path()).unwrap(),
        );
        let manifest = host.manifest("fixture-plugin").unwrap();
        let scope = zeroclaw_plugins::instance::PluginInstanceScope::from_manifest(
            manifest,
            zeroclaw_plugins::PluginCapability::Tool,
            "work",
            [zeroclaw_plugins::PluginPermission::ConfigRead],
        )
        .unwrap();
        let backup_scope = zeroclaw_plugins::instance::PluginInstanceScope::from_manifest(
            manifest,
            zeroclaw_plugins::PluginCapability::Tool,
            "backup",
            [zeroclaw_plugins::PluginPermission::ConfigRead],
        )
        .unwrap();
        let instance_key = scope.id().config_entry_key().unwrap();
        let backup_instance_key = backup_scope.id().config_entry_key().unwrap();
        let entry = |name: &str, enabled: &str| zeroclaw_config::schema::PluginEntryConfig {
            name: name.to_string(),
            config: HashMap::from([("enabled".to_string(), enabled.to_string())]),
            egress_hosts: Vec::new(),
            egress_allow_private: Vec::new(),
        };
        let mut snapshot = Config::default();
        snapshot.plugins.entries = vec![
            entry(&instance_key, "false"),
            entry(&backup_instance_key, "false"),
        ];
        let mut current = Config::default();
        current.plugins.entries = vec![
            entry("fixture-plugin", "true"),
            entry("work", "true"),
            entry("backup", "true"),
            entry(&instance_key, "true"),
            entry(&backup_instance_key, "false"),
        ];
        let live = Arc::new(parking_lot::RwLock::new(current));
        let services = plugin_host_services(
            Arc::clone(&host),
            Arc::new(snapshot),
            Some(Arc::clone(&live)),
        );

        assert!(services.resolve_config(&scope).is_ok());
        assert!(
            services.resolve_config(&backup_scope).is_err(),
            "backup must use its invalid canonical entry, not a valid raw-name decoy"
        );
        for (key, enabled) in [(&instance_key, "false"), (&backup_instance_key, "true")] {
            live.write()
                .plugins
                .entries
                .iter_mut()
                .find(|entry| entry.name == key.as_str())
                .unwrap()
                .config
                .insert("enabled".to_string(), enabled.to_string());
        }
        assert!(
            services.resolve_config(&scope).is_err(),
            "work must observe its own canonical key's live update"
        );
        assert!(
            services.resolve_config(&backup_scope).is_ok(),
            "backup must resolve independently through the shared service"
        );
    }

    /// End-to-end over the resolver the registry actually installs: an operator
    /// grant authored on the instance-key row that `zeroclaw plugin install`
    /// seeds must reach the policy, and nothing else may stand in for it.
    ///
    /// The existing egress tests build an `EgressPolicy` directly, so they
    /// cannot see which `[[plugins.entries]]` row the runtime reads. This one
    /// starts from canonical config and goes through `plugin_egress_service`.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn plugin_egress_resolves_the_instance_key_row_config_resolves() {
        let plugins_dir = TempDir::new().unwrap();
        let plugin_dir = plugins_dir.path().join("egress-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            r#"name = "egress-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
permissions = ["http_client"]
"#,
        )
        .unwrap();
        let host =
            zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins_dir.path()).unwrap();
        let manifest = host.manifest("egress-plugin").unwrap();
        // The production tool path: binding == package name, which is exactly
        // what used to be handed to `entry_egress`.
        let scope = zeroclaw_plugins::instance::PluginInstanceScope::for_package_binding(
            manifest,
            zeroclaw_plugins::PluginCapability::Tool,
            manifest.permissions.iter().copied(),
        )
        .unwrap();
        let instance_key = scope.id().config_entry_key().unwrap();
        assert_ne!(
            instance_key,
            scope.id().binding(),
            "the canonical entry key must not coincide with the binding, or this test proves nothing"
        );

        let entry = |name: &str| zeroclaw_config::schema::PluginEntryConfig {
            name: name.to_string(),
            config: HashMap::new(),
            egress_hosts: vec!["api.example.com".to_string()],
            egress_allow_private: Vec::new(),
        };
        let request = || {
            zeroclaw_plugins::egress::EgressRequest::new(
                scope.clone(),
                zeroclaw_plugins::egress::EgressTransport::Http { encrypted: true },
                "api.example.com",
                443,
            )
            .unwrap()
        };
        let addresses = ["1.1.1.1:443".parse::<std::net::SocketAddr>().unwrap()];

        // Granted on the instance-key row: the operator's grant reaches policy.
        let mut granted = Config::default();
        granted.plugins.entries = vec![entry(&instance_key)];
        let service = plugin_egress_service(Arc::new(granted), None);
        service
            .authorize_addresses(request(), addresses)
            .expect("a grant on the instance-key row must reach the resolved policy");

        // No row at all: deny.
        let service = plugin_egress_service(Arc::new(Config::default()), None);
        assert!(
            matches!(
                service.authorize_addresses(request(), addresses),
                Err(zeroclaw_plugins::egress::EgressError::DestinationNotGranted { .. })
            ),
            "an unconfigured instance must have no reach"
        );

        // A legacy package/binding-named row must not stand in for the
        // canonical key, or egress and config would read two different rows.
        let mut legacy = Config::default();
        legacy.plugins.entries = vec![entry(scope.id().binding())];
        let service = plugin_egress_service(Arc::new(legacy), None);
        assert!(
            matches!(
                service.authorize_addresses(request(), addresses),
                Err(zeroclaw_plugins::egress::EgressError::DestinationNotGranted { .. })
            ),
            "a raw package or binding entry must not bypass the canonical key"
        );
    }

    #[test]
    fn default_tools_has_expected_count() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        assert_eq!(tools.len(), 7);
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn plugin_tool_names_cannot_shadow_native_reserved_or_prior_plugin_tools() {
        let mut registered_names =
            std::collections::HashSet::from(["shell".to_string(), PipelineTool::NAME.to_string()]);
        let accepted = ["shell", PipelineTool::NAME, "novel-tool", "novel-tool"]
            .into_iter()
            .filter(|name| claim_plugin_tool_name(&mut registered_names, name))
            .collect::<Vec<_>>();

        assert_eq!(accepted, vec!["novel-tool"]);
        assert_eq!(
            registered_names,
            std::collections::HashSet::from([
                "shell".to_string(),
                PipelineTool::NAME.to_string(),
                "novel-tool".to_string(),
            ])
        );
    }

    /// Write a discoverable package whose component bytes are deliberately
    /// invalid. Package admission never compiles the component, so reaching the
    /// guest is the only thing this payload would fail at — which makes the
    /// config-resolution probe below an exact witness for "construction was
    /// attempted".
    #[cfg(feature = "plugins-wasm")]
    fn write_plugin_package(root: &std::path::Path, name: &str, capabilities: &[&str]) {
        let package_dir = root.join(name);
        std::fs::create_dir_all(&package_dir).unwrap();
        let capabilities = capabilities
            .iter()
            .map(|capability| format!("\"{capability}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            package_dir.join("manifest.toml"),
            format!(
                "name = \"{name}\"\nversion = \"0.1.0\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [{capabilities}]\n"
            ),
        )
        .unwrap();
        std::fs::write(package_dir.join("plugin.wasm"), b"not a component").unwrap();
    }

    #[cfg(feature = "plugins-wasm")]
    fn write_tool_package(root: &std::path::Path, name: &str) {
        write_plugin_package(root, name, &["tool"]);
    }

    /// A skill package ships a directory, not a component, so the skill loader
    /// below returns real `Skill` values rather than a construction witness.
    #[cfg(feature = "plugins-wasm")]
    fn write_skill_package(root: &std::path::Path, name: &str) {
        let skill_dir = root.join(name).join("skills").join("sample");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            root.join(name).join("manifest.toml"),
            format!("name = \"{name}\"\nversion = \"0.1.0\"\ncapabilities = [\"skill\"]\n"),
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: sample\ndescription: sample skill\n---\n# Sample\n",
        )
        .unwrap();
    }

    #[cfg(feature = "plugins-wasm")]
    fn test_plugin_limits() -> zeroclaw_plugins::component::PluginLimits {
        zeroclaw_plugins::component::PluginLimits {
            call_fuel: 1_000_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_table_elements: 10_000,
            max_instances: 8,
            call_timeout: std::time::Duration::from_secs(30),
        }
    }

    /// A resolver that records the package of every scope it is asked about.
    ///
    /// `WasmTool::from_wasm` resolves the instance's scoped config as its very
    /// first act, before instantiating the component, so an entry here is an
    /// exact witness that the loader entered construction for that package —
    /// detectable even when the component itself is unloadable.
    #[cfg(feature = "plugins-wasm")]
    fn recording_resolver(
        host: &Arc<zeroclaw_plugins::host::PluginHost>,
    ) -> (
        zeroclaw_plugins::config::PluginConfigResolver,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let probed: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let resolver = {
            let host = Arc::clone(host);
            let probed = Arc::clone(&probed);
            zeroclaw_plugins::config::PluginConfigResolver::new(move |scope| {
                let package = scope.id().package().to_string();
                probed.lock().unwrap().push(package.clone());
                let manifest = host.manifest(&package).ok_or_else(|| {
                    zeroclaw_plugins::error::PluginError::NotFound(package.clone())
                })?;
                zeroclaw_plugins::config::resolve_plugin_config(manifest, scope, None)
            })
        };
        (resolver, probed)
    }

    /// Turn the plugin system on for `plugins_dir` with a ceiling high enough
    /// that admission is decided by capability rather than by the cap.
    #[cfg(feature = "plugins-wasm")]
    fn plugin_activation_config(plugins_dir: &std::path::Path) -> Config {
        let mut config = Config::default();
        config.plugins.enabled = true;
        config.plugins.auto_discover = true;
        config.plugins.max_active_instances = 50;
        config.plugins.plugins_dir = plugins_dir.display().to_string();
        config
    }

    /// Run the production tool loader once and report the packages whose guest
    /// construction it entered, in sorted order.
    #[cfg(feature = "plugins-wasm")]
    fn tool_packages_reaching_construction(
        config: &Config,
        host: &Arc<zeroclaw_plugins::host::PluginHost>,
        reserved: &[&str],
    ) -> Vec<String> {
        let (resolver, probed) = recording_resolver(host);
        let services = zeroclaw_plugins::services::PluginHostServices::new(resolver);
        let mut registered_names: std::collections::HashSet<String> =
            reserved.iter().map(|name| (*name).to_string()).collect();
        let mut tool_arcs: Vec<Arc<dyn Tool>> = Vec::new();
        register_plugin_tools(
            config,
            host,
            &services,
            test_plugin_limits(),
            None,
            &mut registered_names,
            &mut tool_arcs,
        );
        let mut probed = probed.lock().unwrap().clone();
        probed.sort();
        probed
    }

    /// A plugin whose name already belongs to a native, reserved, or
    /// earlier-plugin tool can never be registered, so it must be refused by
    /// the host before any of its guest code is constructed.
    ///
    /// Ceiling admission composes ahead of this gate and must not weaken it:
    /// the ceiling here is wide enough to admit both packages, so the only
    /// thing that can keep `shell` out of the witness is the collision check
    /// still running before construction.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn colliding_plugin_is_refused_before_its_guest_is_constructed() {
        let plugins = TempDir::new().unwrap();
        write_tool_package(plugins.path(), "shell");
        write_tool_package(plugins.path(), "novel-tool");

        let host =
            Arc::new(zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins.path()).unwrap());
        let config = plugin_activation_config(plugins.path());

        // `shell` is already taken by a native tool; `novel-tool` is free.
        let probed = tool_packages_reaching_construction(&config, &host, &["shell"]);

        assert!(
            !probed.iter().any(|package| package == "shell"),
            "a plugin named `shell` collides with a registered tool and must be \
             rejected before construction, but its guest setup was entered: {probed:?}"
        );
        assert!(
            probed.iter().any(|package| package == "novel-tool"),
            "a non-colliding plugin must still be constructed: {probed:?}"
        );
    }

    /// The pre-construction gate must not swallow the packages it is meant to
    /// let through: with nothing registered, both packages are constructed.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn non_colliding_plugins_all_reach_construction() {
        let plugins = TempDir::new().unwrap();
        write_tool_package(plugins.path(), "alpha");
        write_tool_package(plugins.path(), "beta");

        let host =
            Arc::new(zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins.path()).unwrap());
        let config = plugin_activation_config(plugins.path());

        let probed = tool_packages_reaching_construction(&config, &host, &[]);

        assert_eq!(probed, vec!["alpha".to_string(), "beta".to_string()]);
    }

    /// A mixed channel/tool/skill candidate set that exceeds the shared
    /// `plugins.max_active_instances` ceiling, exercised through the two
    /// production loaders rather than the plan vector.
    ///
    /// Candidate order is explicit channels first, then auto-discovered
    /// package bindings by package name: `alpha` channel `ops`, `alpha` tool,
    /// `beta` skill, `zeta` tool. A ceiling of two therefore admits the channel
    /// and the `alpha` tool, and the `beta` skill and `zeta` tool must not
    /// load — proving one package with a channel and a tool really does spend
    /// two slots.
    #[cfg(feature = "plugins-wasm")]
    fn mixed_capability_fixture(plugins: &TempDir) -> Config {
        write_plugin_package(plugins.path(), "alpha", &["channel", "tool"]);
        write_skill_package(plugins.path(), "beta");
        write_tool_package(plugins.path(), "zeta");

        let mut config = plugin_activation_config(plugins.path());
        config.channels.plugin = HashMap::from([(
            "ops".to_string(),
            zeroclaw_config::schema::PluginChannelConfig {
                package: "alpha".to_string(),
                enabled: true,
            },
        )]);
        config.agents = HashMap::from([(
            "operator".to_string(),
            AliasedAgentConfig {
                channels: vec![zeroclaw_config::providers::ChannelRef::new("plugin.ops")],
                ..AliasedAgentConfig::default()
            },
        )]);
        config
    }

    #[cfg(feature = "plugins-wasm")]
    fn plugin_skill_names(config: &Config) -> Vec<String> {
        let (skills, _) = crate::skills::load_plugin_skills_from_config(config);
        let mut names: Vec<String> = skills.into_iter().map(|skill| skill.name).collect();
        names.sort();
        names
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn shared_ceiling_keeps_an_unadmitted_tool_and_skill_out_of_the_real_loaders() {
        let plugins = TempDir::new().unwrap();
        let mut config = mixed_capability_fixture(&plugins);
        let host =
            Arc::new(zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins.path()).unwrap());

        // Control: a ceiling wide enough for every candidate loads all of them,
        // so the assertions below cannot pass for want of a working fixture.
        config.plugins.max_active_instances = 4;
        assert_eq!(
            tool_packages_reaching_construction(&config, &host, &[]),
            vec!["alpha".to_string(), "zeta".to_string()],
        );
        assert_eq!(
            plugin_skill_names(&config),
            vec!["plugin:beta/sample".to_string()],
        );

        config.plugins.max_active_instances = 2;

        assert_eq!(
            tool_packages_reaching_construction(&config, &host, &[]),
            vec!["alpha".to_string()],
            "the channel and the `alpha` tool spend both slots, so the `zeta` \
             tool must not reach the tool loader's construction at all"
        );
        assert!(
            plugin_skill_names(&config).is_empty(),
            "the `beta` skill is over the shared ceiling and must not load"
        );
    }

    /// Tool registries are rebuilt per agent, per CLI run, per delegate, and
    /// per SOP execution. Admission is a pure function of the config and host
    /// snapshot, so every rebuild must re-derive the same admitted set instead
    /// of spending the ceiling cumulatively.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn repeated_loader_construction_admits_the_same_set() {
        let plugins = TempDir::new().unwrap();
        let mut config = mixed_capability_fixture(&plugins);
        config.plugins.max_active_instances = 2;
        let host =
            Arc::new(zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins.path()).unwrap());

        let first_tools = tool_packages_reaching_construction(&config, &host, &[]);
        let second_tools = tool_packages_reaching_construction(&config, &host, &[]);
        let third_tools = tool_packages_reaching_construction(&config, &host, &[]);

        assert_eq!(first_tools, vec!["alpha".to_string()]);
        assert_eq!(
            first_tools, second_tools,
            "a second registry build must admit the same tools, not fewer"
        );
        assert_eq!(
            second_tools, third_tools,
            "a third registry build must admit the same tools, not fewer"
        );

        config.plugins.max_active_instances = 4;
        let first_skills = plugin_skill_names(&config);
        let second_skills = plugin_skill_names(&config);

        assert_eq!(first_skills, vec!["plugin:beta/sample".to_string()]);
        assert_eq!(
            first_skills, second_skills,
            "a second skill load must admit the same skills, not fewer"
        );
    }

    /// The documented default-`false` setup: `plugins.enabled = true` with
    /// `plugins.auto_discover` left at its schema default of `false`, an
    /// installed tool plugin and an installed skill plugin present, and a
    /// ceiling wide enough that admission is never the reason anything is held
    /// back. Auto-discovered tool and skill instances are gated on
    /// `auto_discover`, so both production loaders must load *nothing*. Flipping
    /// only `auto_discover` on the identical host and config then loads both,
    /// which proves the emptiness is the discovery gate rather than a broken
    /// fixture. The assertions read the real loader output (the construction
    /// witness and the skill list), not the activation plan vector.
    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn auto_discover_default_false_admits_no_tools_or_skills() {
        let plugins = TempDir::new().unwrap();
        write_tool_package(plugins.path(), "zeta");
        write_skill_package(plugins.path(), "beta");

        let host =
            Arc::new(zeroclaw_plugins::host::PluginHost::from_plugins_dir(plugins.path()).unwrap());

        let mut config = Config::default();
        config.plugins.enabled = true;
        config.plugins.plugins_dir = plugins.path().display().to_string();
        config.plugins.max_active_instances = 50;
        // Left at the schema default; this is exactly the documented setup an
        // operator reaches with `plugins.enabled = true` and nothing else.
        assert!(
            !config.plugins.auto_discover,
            "the schema default must be false for this regression to exercise the \
             documented default-false path"
        );

        assert!(
            tool_packages_reaching_construction(&config, &host, &[]).is_empty(),
            "with auto_discover=false the tool loader must not enter construction \
             for any installed tool package"
        );
        assert!(
            plugin_skill_names(&config).is_empty(),
            "with auto_discover=false the skill loader must load no plugin skills"
        );

        // Flip only the discovery gate on the identical host and config.
        config.plugins.auto_discover = true;
        assert_eq!(
            tool_packages_reaching_construction(&config, &host, &[]),
            vec!["zeta".to_string()],
            "with auto_discover=true the installed tool package must load, proving \
             the emptiness above is the discovery gate and not a broken fixture"
        );
        assert_eq!(
            plugin_skill_names(&config),
            vec!["plugin:beta/sample".to_string()],
            "with auto_discover=true the installed skill package must load"
        );
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

    /// `web_search_tool` must be registered behind `RateLimitedTool` like
    /// every other outbound-network tool. It was the lone exception, which let
    /// an agent loop issue unbounded searches — and unbounded scrapes against
    /// the default DuckDuckGo path.
    ///
    /// The probe uses an exhausted action budget plus the SearXNG provider
    /// with no instance URL configured, so the two outcomes are distinguishable
    /// without any network call:
    ///   * wrapped   → `Ok(success: false)` carrying the rate-limit error,
    ///                 because the wrapper short-circuits before the inner tool
    ///   * unwrapped → `Err("SearXNG instance URL not configured…")` from the
    ///                 inner tool's own config resolution
    #[tokio::test]
    async fn web_search_tool_is_registered_behind_the_rate_limiter() {
        let tmp = TempDir::new().unwrap();

        // A zero-action budget is rate-limited from the very first call.
        let security = Arc::new(SecurityPolicy {
            max_actions_per_hour: 0,
            ..SecurityPolicy::default()
        });

        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();

        let mut cfg = test_config(&tmp);
        cfg.web_search.enabled = true;
        // Resolves locally and fails without touching the network.
        cfg.web_search.search_provider = "searxng".to_string();
        cfg.web_search.searxng_instance_url = None;
        std::fs::write(&cfg.config_path, "[web_search]\n").unwrap();

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

        let web_search = tools
            .iter()
            .find(|t| t.name() == "web_search_tool")
            .expect("web_search_tool must be registered when enabled");

        let result = web_search
            .execute(serde_json::json!({"query": "test"}))
            .await
            .expect("the rate limiter returns Ok(success: false), not Err");

        assert!(
            !result.success,
            "a rate-limited call must not report success"
        );
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("Rate limit exceeded"),
            "web_search_tool is not wrapped in RateLimitedTool; got: {error}"
        );
    }

    /// Regression: SOP tools must NOT appear in the tool registry when the
    /// engine handle is not provided (i.e. no `sops_dir` configured).
    /// Proves the production gating path at `all_tools_with_runtime`.
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
    fn send_via_triggers_survive_production_registry_boxing() {
        // Regression: every arc in the registry is re-boxed as an
        // `ArcDelegatingTool`, which must forward `invocation_triggers()` —
        // otherwise the trait default erases send_via's vocabulary and the
        // pre-turn prefilter can never match it. Build the real registry and
        // assert the boxed send_via still carries its live triggers.
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
            None,
            None,
            None,
        )
        .tools;

        let send_via = tools
            .iter()
            .find(|t| t.name() == "send_via")
            .expect("send_via is always registered");
        let triggers = send_via.invocation_triggers();
        assert!(
            triggers.iter().any(|t| t == "send this to"),
            "boxed send_via must keep its static triggers; got {triggers:?}"
        );
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

    struct CapturingRuntime {
        seen_command: Arc<Mutex<Option<String>>>,
        filesystem_access: bool,
    }

    impl RuntimeAdapter for CapturingRuntime {
        fn name(&self) -> &str {
            "capturing-test"
        }
        fn has_filesystem_access(&self) -> bool {
            self.filesystem_access
        }
        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
        fn supports_long_running(&self) -> bool {
            false
        }
        fn shell_dialect(&self) -> crate::platform::ShellDialect {
            crate::platform::ShellDialect::Posix
        }
        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            *self.seen_command.lock().unwrap() = Some(command.to_string());
            #[cfg(windows)]
            let mut process = {
                let mut process = tokio::process::Command::new("cmd.exe");
                process.args(["/D", "/S", "/C", "echo zc-runtime"]);
                process
            };
            #[cfg(not(windows))]
            let mut process = tokio::process::Command::new("/bin/sh");
            #[cfg(not(windows))]
            process
                .args(["-c", "printf '%s' \"$0\"", "zc-runtime"])
                .current_dir(workspace_dir);
            #[cfg(windows)]
            process.current_dir(workspace_dir);
            Ok(process)
        }
    }

    #[tokio::test]
    async fn registered_coding_cli_tools_use_configured_runtime_executor() {
        type EnableCodingCli = fn(&mut Config);

        let cases: [(&str, &str, EnableCodingCli); 4] = [
            ("claude_code", "claude -p", |cfg: &mut Config| {
                cfg.claude_code.enabled = true;
                cfg.claude_code.timeout_secs = 5;
            }),
            ("codex_cli", "codex exec", |cfg: &mut Config| {
                cfg.codex_cli.enabled = true;
                cfg.codex_cli.timeout_secs = 5;
            }),
            ("gemini_cli", "gemini -p", |cfg: &mut Config| {
                cfg.gemini_cli.enabled = true;
                cfg.gemini_cli.timeout_secs = 5;
            }),
            ("opencode_cli", "opencode run", |cfg: &mut Config| {
                cfg.opencode_cli.enabled = true;
                cfg.opencode_cli.timeout_secs = 5;
            }),
        ];

        for (tool_name, expected_fragment, enable) in cases {
            let tmp = TempDir::new().unwrap();
            let security = Arc::new(SecurityPolicy {
                autonomy: crate::security::AutonomyLevel::Full,
                workspace_dir: tmp.path().to_path_buf(),
                ..SecurityPolicy::default()
            });
            let mem_cfg = MemoryConfig {
                backend: "markdown".into(),
                ..MemoryConfig::default()
            };
            let mem: Arc<dyn Memory> =
                Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
            let browser = BrowserConfig {
                enabled: false,
                ..BrowserConfig::default()
            };
            let mut cfg = test_config(&tmp);
            cfg.runtime.kind = zeroclaw_config::schema::RuntimeKind::Docker;
            cfg.claude_code.enabled = false;
            cfg.codex_cli.enabled = false;
            cfg.gemini_cli.enabled = false;
            cfg.opencode_cli.enabled = false;
            enable(&mut cfg);
            let risk = zeroclaw_config::schema::RiskProfileConfig {
                sandbox_enabled: Some(false),
                sandbox_backend: Some("none".to_string()),
                ..zeroclaw_config::schema::RiskProfileConfig::default()
            };
            let seen_command = Arc::new(Mutex::new(None));

            let tools = all_tools_with_runtime(
                Arc::new(cfg.clone()),
                &security,
                &risk,
                "test-agent",
                Arc::new(CapturingRuntime {
                    seen_command: Arc::clone(&seen_command),
                    filesystem_access: true,
                }),
                mem,
                None,
                None,
                &browser,
                &zeroclaw_config::schema::HttpRequestConfig::default(),
                &zeroclaw_config::schema::WebFetchConfig::default(),
                tmp.path(),
                &HashMap::new(),
                None,
                &cfg,
                None,
                false,
                None,
                None,
                None,
                None,
            )
            .tools;
            let tool = tools
                .iter()
                .find(|tool| tool.name() == tool_name)
                .unwrap_or_else(|| panic!("{tool_name} should register"));

            let result = tool
                .execute(serde_json::json!({"prompt": "route through runtime"}))
                .await
                .unwrap_or_else(|error| panic!("{tool_name} should return a tool result: {error}"));

            assert!(
                result.success,
                "{tool_name} unexpected error: {:?}",
                result.error
            );
            assert_eq!(result.output.trim(), "zc-runtime");
            let command = seen_command
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| panic!("registry-wired {tool_name} should call runtime"));
            assert!(
                command.contains(expected_fragment),
                "{tool_name} command was {command:?}"
            );
            assert!(
                command.contains("route through runtime"),
                "{tool_name} command was {command:?}"
            );
        }
    }

    #[tokio::test]
    async fn docker_without_workspace_mount_does_not_register_coding_cli_tools() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: crate::security::AutonomyLevel::Full,
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let browser = BrowserConfig {
            enabled: false,
            ..BrowserConfig::default()
        };
        let mut cfg = test_config(&tmp);
        cfg.runtime.kind = zeroclaw_config::schema::RuntimeKind::Docker;
        cfg.runtime.docker.mount_workspace = false;
        cfg.claude_code.enabled = true;
        cfg.codex_cli.enabled = true;
        cfg.gemini_cli.enabled = true;
        cfg.opencode_cli.enabled = true;
        let risk = zeroclaw_config::schema::RiskProfileConfig {
            sandbox_enabled: Some(false),
            sandbox_backend: Some("none".to_string()),
            ..zeroclaw_config::schema::RiskProfileConfig::default()
        };

        let tools = all_tools_with_runtime(
            Arc::new(cfg.clone()),
            &security,
            &risk,
            "test-agent",
            Arc::new(zeroclaw_config::platform::DockerRuntime::new(
                cfg.runtime.docker.clone(),
            )),
            mem,
            None,
            None,
            &browser,
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
            false,
            None,
            None,
            None,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();

        for tool_name in ["claude_code", "codex_cli", "gemini_cli", "opencode_cli"] {
            assert!(
                !names.contains(&tool_name),
                "{tool_name} must not register without runtime filesystem access"
            );
        }
        assert!(
            names.contains(&"shell"),
            "positive control: ordinary tools should still register"
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
        fn has_filesystem_access(&self) -> bool {
            false
        }
        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
        fn supports_long_running(&self) -> bool {
            false
        }
        fn shell_dialect(&self) -> crate::platform::ShellDialect {
            self.0.shell_dialect()
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

    /// `FILESYSTEM_TOOL_NAMES` drives which tools a Bounded delegate rebuilds against
    /// the target's own `SecurityPolicy` (see delegate.rs). If it drifts from what
    /// `default_tools_with_runtime`/`image_info_tool` actually construct,
    /// that rebuild silently misses a filesystem-boundary tool - exactly the class of
    /// bug this constant exists to prevent. Keep it in lockstep with the constructors.
    #[test]
    fn filesystem_tool_names_match_constructed_tools() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());

        let mut tools = default_tools_with_runtime(security.clone(), runtime);
        tools.push(image_info_tool(security));

        let constructed: std::collections::BTreeSet<&str> =
            tools.iter().map(|t| t.name()).collect();
        let declared: std::collections::BTreeSet<&str> =
            FILESYSTEM_TOOL_NAMES.iter().copied().collect();

        assert_eq!(
            constructed, declared,
            "FILESYSTEM_TOOL_NAMES is out of sync with the constructed filesystem-boundary \
             tools — update the constant (or the constructors) so a Bounded delegate rebuild \
             cannot silently miss one"
        );
    }

    /// Companion to `filesystem_tool_names_match_constructed_tools`: proves
    /// `WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT` doesn't reference a typo or
    /// a tool that no longer exists, using the SAME full-registry factory
    /// production uses, with every optional feature that gates one of these
    /// tools turned on.
    #[test]
    fn workspace_bound_tool_names_beyond_default_are_actually_constructed() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let browser = BrowserConfig {
            enabled: true,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        cfg.data_retention.enabled = true;
        cfg.linkedin.enabled = true;
        cfg.image_gen.enabled = true;
        cfg.claude_code.enabled = true;
        cfg.claude_code_runner.enabled = true;
        cfg.codex_cli.enabled = true;
        cfg.gemini_cli.enabled = true;
        cfg.opencode_cli.enabled = true;
        cfg.file_upload.url = Some("https://example.com/upload".into());
        cfg.file_upload_bundle.url = Some("https://example.com/upload-bundle".into());
        cfg.file_download.url = Some("https://example.com/download".into());

        let tools = all_tools_with_runtime(
            Arc::new(cfg.clone()),
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
            None,
            None,
            None,
        )
        .tools;

        let constructed: std::collections::BTreeSet<&str> =
            tools.iter().map(|t| t.name()).collect();
        for name in WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT {
            assert!(
                constructed.contains(name),
                "'{name}' listed in WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT was not \
                 constructed by all_tools_with_runtime with every relevant feature \
                 enabled - the name is stale or its enabling config flag changed"
            );
        }
    }

    /// Every bounded-delegation category, paired with its name for diagnostics.
    fn bounded_classification_lists() -> [(&'static str, &'static [&'static str]); 8] {
        [
            ("MEMORY_TOOL_NAMES", zeroclaw_tools::MEMORY_TOOL_NAMES),
            ("FILESYSTEM_TOOL_NAMES", FILESYSTEM_TOOL_NAMES),
            (
                "WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT",
                WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT,
            ),
            ("IDENTITY_BOUND_TOOL_NAMES", IDENTITY_BOUND_TOOL_NAMES),
            ("AUTONOMY_REBOUND_TOOL_NAMES", AUTONOMY_REBOUND_TOOL_NAMES),
            ("CHANNEL_REBOUND_TOOL_NAMES", CHANNEL_REBOUND_TOOL_NAMES),
            ("SAFE_FOR_BOUNDED_REUSE", SAFE_FOR_BOUNDED_REUSE),
            ("BOUNDED_DENIED_TOOL_NAMES", BOUNDED_DENIED_TOOL_NAMES),
        ]
    }

    /// Builds a registry with every config-gated integration switched on, so the
    /// inventory the completeness test walks is the widest one the production
    /// factory can produce.
    ///
    /// A default config hides most of these behind their `enabled` flag, which is
    /// exactly how a name can stay unclassified while a sync test reports green.
    fn maximal_tool_registry(tmp: &TempDir) -> Vec<String> {
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let mut cfg = test_config(tmp);
        cfg.skills.prompt_injection_mode =
            zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;

        // Per-agent resolution: `llm_task` needs the agent to resolve a provider.
        cfg.providers.models.custom.insert(
            "completeness".to_string(),
            zeroclaw_config::schema::CustomModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("test-model".to_string()),
                    api_key: Some("sk-test".to_string()),
                    ..Default::default()
                },
            },
        );
        cfg.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "custom.completeness".into(),
                ..zeroclaw_config::schema::AliasedAgentConfig::default()
            },
        );

        // Config-gated integrations, with dummy credentials.
        cfg.notion.enabled = true;
        cfg.notion.api_key = "dummy-notion-key".to_string();
        cfg.jira.enabled = true;
        cfg.jira.base_url = "https://example.invalid".to_string();
        cfg.jira.api_token = "dummy-jira-token".to_string();
        cfg.google_workspace.enabled = true;
        cfg.composio.enabled = true;
        cfg.composio.api_key = Some("dummy-composio-key".to_string());
        cfg.text_browser.enabled = true;
        cfg.browser_delegate.enabled = true;
        cfg.project_intel.enabled = true;
        cfg.security_ops.enabled = true;
        cfg.cloud_ops.enabled = true;
        cfg.knowledge.enabled = true;
        cfg.knowledge.db_path = tmp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .to_string();
        cfg.sop.procedural_memory_enabled = true;
        cfg.data_retention.enabled = true;
        cfg.linkedin.enabled = true;
        cfg.image_gen.enabled = true;
        cfg.file_upload.url = Some("https://example.invalid/upload".to_string());
        cfg.file_download.url = Some("https://example.invalid/download".to_string());
        cfg.file_upload_bundle.url = Some("https://example.invalid/bundle".to_string());
        cfg.claude_code_runner.enabled = true;
        // Coding CLIs are gated on config AND on the runtime granting shell +
        // filesystem access, which `NativeRuntime` does.
        cfg.claude_code.enabled = true;
        cfg.codex_cli.enabled = true;
        cfg.gemini_cli.enabled = true;
        cfg.opencode_cli.enabled = true;
        // Graph needs a tenant and a client to register at all.
        cfg.microsoft365.enabled = true;
        cfg.microsoft365.tenant_id = Some("dummy-tenant".to_string());
        cfg.microsoft365.client_id = Some("dummy-client".to_string());
        // The default auth_flow is `client_credentials`, which fails FAST without
        // a secret - and that fail-fast returns from `all_tools_with_runtime`
        // entirely, truncating the inventory. Without this line the test silently
        // walks a registry that stops at microsoft365.
        cfg.microsoft365.client_secret = Some("dummy-secret".to_string());
        // `TokenCache::new` refuses to run while token_cache_encrypted is set,
        // because that encryption is not implemented yet - and that refusal
        // makes the tool skip registration.
        cfg.microsoft365.token_cache_encrypted = false;
        // `discord_search` is registered when some Discord alias archives.
        cfg.channels.discord.insert(
            "completeness".to_string(),
            zeroclaw_config::schema::DiscordConfig {
                archive: true,
                ..Default::default()
            },
        );
        // `email_search`/`email_read` need at least one enabled email channel.
        cfg.channels.email.insert(
            "completeness".to_string(),
            zeroclaw_config::scattered_types::EmailConfig {
                enabled: true,
                ..Default::default()
            },
        );

        let browser = BrowserConfig {
            enabled: true,
            ..BrowserConfig::default()
        };
        let http = zeroclaw_config::schema::HttpRequestConfig {
            enabled: true,
            ..Default::default()
        };
        let web_fetch = zeroclaw_config::schema::WebFetchConfig {
            enabled: true,
            ..Default::default()
        };

        let sop_engine = Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));

        let built = all_tools_with_runtime(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            Arc::new(NativeRuntime::new()),
            mem,
            Some("dummy-composio-key"),
            Some("default"),
            &browser,
            &http,
            &web_fetch,
            tmp.path(),
            &cfg.agents.clone(),
            None,
            &cfg,
            None,
            false,
            None,
            Some(sop_engine),
            None,
            None,
        );
        built
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect()
    }

    /// Replaces the three hand-maintained sync tests with a real completeness
    /// check: every tool the production factory builds must fall in EXACTLY one
    /// bounded-delegation category.
    ///
    /// The three tests this supersedes only asked whether the names already in a
    /// list get constructed. That is circular - it cannot see a name nobody
    /// added, which is precisely how three identity-bound cron tools arrived
    /// upstream and sat unclassified while all three reported green.
    ///
    /// Note what this test is and is not for. The SECURITY guarantee is the
    /// deny-by-default fallback: an unclassified name is omitted, never
    /// inherited. This test protects against the other failure - silent LOSS of
    /// function - and forces a deliberate decision for each new tool.
    ///
    /// It walks a STATIC inventory, so it says nothing about MCP tools, whose
    /// names exist only at runtime. Those are asserted where they actually get
    /// decided, at the fallback itself, by
    /// `bounded_cross_profile_admits_only_target_granted_mcp_servers`: a
    /// synthetic `<server>__<tool>` from a server the target was not granted is
    /// omitted, while one from a granted server survives. Checking MCP here
    /// instead would reproduce the blind spot that let `send_via` through.
    ///
    /// Confirmed non-circular by removing `cron_run`/`cron_list`/`cron_runs`
    /// from `IDENTITY_BOUND_TOOL_NAMES` and re-running: this test names all
    /// three, while the three sync tests it complements stay green - which is
    /// the exact scenario that let them arrive unclassified in the first place.
    #[test]
    fn every_constructed_tool_is_classified_for_bounded_delegation() {
        let tmp = TempDir::new().unwrap();
        let constructed = maximal_tool_registry(&tmp);

        // A floor, so a config that quietly stops enabling integrations cannot
        // turn this into a green no-op over a handful of tools.
        assert!(
            constructed.len() >= 60,
            "the maximal registry should be far larger than this; only {} tools were \
             built, so the fixture has stopped enabling integrations and this test is \
             no longer checking what it claims: {constructed:?}",
            constructed.len()
        );

        let lists = bounded_classification_lists();
        let mut unclassified: Vec<&str> = Vec::new();
        let mut duplicated: Vec<String> = Vec::new();

        for name in &constructed {
            // `delegate` is the one name that never reaches a target: the bounded
            // filter strips it to prevent recursion.
            if name == crate::tools::delegate::DelegateTool::NAME {
                continue;
            }
            let hits: Vec<&str> = lists
                .iter()
                .filter(|(_, list)| list.contains(&name.as_str()))
                .map(|(list_name, _)| *list_name)
                .collect();
            match hits.len() {
                0 => unclassified.push(name),
                1 => {}
                _ => duplicated.push(format!("{name} in {hits:?}")),
            }
        }

        assert!(
            unclassified.is_empty(),
            "these tools are constructed by all_tools_with_runtime but belong to no \
             bounded-delegation category. The fallback denies them, so they are SAFE \
             but silently unavailable to every bounded target. Classify each one \
             (rebuilt / safe to reuse / denied) rather than leaving it to the \
             fallback: {unclassified:?}"
        );
        assert!(
            duplicated.is_empty(),
            "these tools are in more than one category, so which one wins depends on \
             the order the fallback happens to check: {duplicated:?}"
        );

        // The other direction: a classified name that nothing builds is a typo or
        // a tool that was deleted, and the fallback would never consult it. The
        // exceptions are the names built OUTSIDE `all_tools_with_runtime`, which
        // is the very reason they went unnoticed long enough to need classifying.
        let built_elsewhere = [
            // `build_mcp_capability_tools`, registered by the assembly seam.
            "mcp_resources",
            "mcp_prompts",
        ];
        let mut stale: Vec<String> = Vec::new();
        for (list_name, list) in lists {
            for name in list {
                if built_elsewhere.contains(name) {
                    continue;
                }
                if !constructed.iter().any(|built| built == name) {
                    stale.push(format!("{name} ({list_name})"));
                }
            }
        }
        assert!(
            stale.is_empty(),
            "these names are classified but nothing in the maximal registry builds \
             them - either a typo, or a tool that no longer exists. A name here is \
             dead weight the fallback will never match: {stale:?}"
        );
    }

    /// A name cannot be both denied and reusable. Without this, moving one of
    /// the denied names into `SAFE_FOR_BOUNDED_REUSE` would silently win - the
    /// fallback checks the safe list and never consults the denial list.
    #[test]
    fn denied_bounded_tool_names_are_disjoint_from_every_reuse_and_rebuild_list() {
        for denied in BOUNDED_DENIED_TOOL_NAMES {
            assert!(
                !SAFE_FOR_BOUNDED_REUSE.contains(denied),
                "'{denied}' is listed as denied for bounded targets and as safe to \
                 reuse; the safe list wins at runtime, so this is a real conflict"
            );
            for (list_name, list) in [
                ("FILESYSTEM_TOOL_NAMES", FILESYSTEM_TOOL_NAMES),
                (
                    "WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT",
                    WORKSPACE_BOUND_TOOL_NAMES_BEYOND_DEFAULT,
                ),
                ("IDENTITY_BOUND_TOOL_NAMES", IDENTITY_BOUND_TOOL_NAMES),
                ("AUTONOMY_REBOUND_TOOL_NAMES", AUTONOMY_REBOUND_TOOL_NAMES),
                ("CHANNEL_REBOUND_TOOL_NAMES", CHANNEL_REBOUND_TOOL_NAMES),
            ] {
                assert!(
                    !list.contains(denied),
                    "'{denied}' is listed as denied for bounded targets and also in \
                     {list_name}, which would rebuild it for the target instead"
                );
            }
        }
    }

    /// Proves `IDENTITY_BOUND_TOOL_NAMES` doesn't reference a typo or a tool
    /// that no longer exists, using the SAME full-registry factory
    /// production uses, with every optional gate that hides one of these
    /// tools (currently only `read_skill`'s Compact skills-prompt mode)
    /// turned on. Same principle as
    /// `workspace_bound_tool_names_beyond_default_are_actually_constructed`
    /// above, for the `agent_alias`-capture class instead of the
    /// `workspace_dir`/`SecurityPolicy`-capture class.
    #[test]
    fn identity_bound_tool_names_are_actually_constructed() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
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
        // Two members of this list are config-gated, so a default config would
        // let the loop below pass while never constructing them:
        // `llm_task` needs the agent to resolve a model provider, and
        // `sop_approve` needs a SOP engine handle. The assertion claims "with
        // every relevant feature enabled" - this is what makes that true.
        cfg.providers.models.custom.insert(
            "sync-test".to_string(),
            zeroclaw_config::schema::CustomModelProviderConfig {
                base: zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("test-model".to_string()),
                    ..Default::default()
                },
            },
        );
        cfg.agents.insert(
            "test-agent".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "custom.sync-test".into(),
                ..zeroclaw_config::schema::AliasedAgentConfig::default()
            },
        );
        let sop_engine = Arc::new(Mutex::new(SopEngine::new(
            zeroclaw_config::schema::SopConfig::default(),
        )));

        let tools = all_tools_with_runtime(
            Arc::new(cfg.clone()),
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
            Some(sop_engine),
            None,
            None,
        )
        .tools;

        let constructed: std::collections::BTreeSet<&str> =
            tools.iter().map(|t| t.name()).collect();
        for name in IDENTITY_BOUND_TOOL_NAMES {
            assert!(
                constructed.contains(name),
                "'{name}' listed in IDENTITY_BOUND_TOOL_NAMES was not constructed by \
                 all_tools_with_runtime with every relevant feature enabled - the name \
                 is stale or its enabling config flag changed"
            );
        }
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
        assert!(names.contains(&"deliver_file"));
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
    fn all_tools_excludes_read_skill_for_explicit_global_full() {
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
        // agent selects it via `runtime_profile`. The Full pin inlines skills
        // eagerly, so read_skill must be omitted.
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

    /// `vi_verify` checked caller-supplied constraints against a caller-supplied
    /// fulfillment with nothing establishing that either came from a signed
    /// credential. Until a chain verifier exists the tool must not reach the model
    /// even when an operator opts in.
    #[test]
    fn vi_verify_is_not_registered_even_when_verifiable_intent_is_enabled() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let mut cfg = test_config(&tmp);
        cfg.verifiable_intent.enabled = true;

        let tools = all_tools(
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
            false,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

        assert!(
            !names.contains(&"vi_verify"),
            "vi_verify must not be model-callable while no chain verifier exists"
        );
        assert!(
            names.contains(&"shell"),
            "positive control: the registry must still be populated"
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

#[cfg(test)]
mod wrapper_spec_forwarding_tests {
    use super::*;
    use async_trait::async_trait;
    use zeroclaw_api::tool::ToolSpec;

    /// Stand-in for `McpToolWrapper`: stores its schema once and overrides
    /// `spec()` to hand out `Arc::clone`, so tests can assert wrappers
    /// preserve `Arc` identity instead of falling back to the trait
    /// default (which would deep-clone via `parameters_schema()`).
    struct ArcSchemaTool {
        schema: Arc<serde_json::Value>,
    }

    impl ArcSchemaTool {
        fn new() -> Self {
            Self {
                schema: Arc::new(serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                })),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ArcSchemaTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            "arc-schema-tool"
        }
    }

    #[async_trait]
    impl Tool for ArcSchemaTool {
        fn name(&self) -> &str {
            "arc_schema_tool"
        }

        fn description(&self) -> &str {
            "test tool with Arc-shared schema"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            (*self.schema).clone()
        }

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name().to_string(),
                description: self.description().to_string(),
                parameters: Arc::clone(&self.schema),
                output: None,
                param_domains: std::collections::BTreeMap::new(),
            }
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn arc_tool_ref_forwards_spec_arc_identity() {
        let inner: Arc<dyn Tool> = Arc::new(ArcSchemaTool::new());
        let inner_params = inner.spec().parameters;
        let wrapped = ArcToolRef(Arc::clone(&inner));

        assert!(
            Arc::ptr_eq(&wrapped.spec().parameters, &inner_params),
            "ArcToolRef must forward spec() so the inner Arc-shared schema \
             survives; the trait default deep-clones it every call"
        );
        assert!(
            Arc::ptr_eq(&wrapped.spec().parameters, &wrapped.spec().parameters),
            "repeated spec() calls must hand out the same allocation"
        );
    }

    #[test]
    fn arc_delegating_tool_forwards_spec_arc_identity() {
        let inner: Arc<dyn Tool> = Arc::new(ArcSchemaTool::new());
        let inner_params = inner.spec().parameters;
        let boxed = ArcDelegatingTool::boxed(inner);

        assert!(
            Arc::ptr_eq(&boxed.spec().parameters, &inner_params),
            "ArcDelegatingTool must forward spec() so the inner Arc-shared \
             schema survives; the trait default deep-clones it every call"
        );
    }
}
