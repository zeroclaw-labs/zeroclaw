//! Tool subsystem for agent-callable capabilities.
//!
//! This module implements the tool execution surface exposed to the LLM during
//! agentic loops. Each tool implements the [`Tool`] trait defined in [`traits`],
//! which requires a name, description, JSON parameter schema, and an async
//! `execute` method returning a structured [`ToolResult`].
//!
//! Tools are assembled into registries by [`default_tools`] (shell, file read/write)
//! and [`all_tools`] (full set including memory, browser, cron, HTTP, delegation,
//! and optional integrations). Security policy enforcement is injected via
//! [`SecurityPolicy`](crate::security::SecurityPolicy) at construction time.
//!
//! # Extension
//!
//! To add a new tool, implement [`Tool`] in a new submodule and register it in
//! [`all_tools_with_runtime`]. See `AGENTS.md` §7.3 for the full change playbook.

pub mod agents_ipc;
pub mod apply_patch;
pub mod browser;
pub mod browser_open;
pub mod cli_discovery;
pub mod composio;
pub mod content_search;
pub mod cron_add;
pub mod cron_list;
pub mod cron_remove;
pub mod cron_run;
pub mod cron_runs;
pub mod cron_update;
pub mod delegate;
pub mod delegate_coordination_status;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod git_operations;
pub mod glob_search;
#[cfg(feature = "hardware")]
pub mod hardware_board_info;
#[cfg(feature = "hardware")]
pub mod hardware_memory_map;
#[cfg(feature = "hardware")]
pub mod hardware_memory_read;
pub mod http_request;
pub mod image_info;
pub mod mcp_client;
pub mod mcp_protocol;
pub mod mcp_tool;
pub mod mcp_transport;
pub mod memory_forget;
pub mod memory_recall;
pub mod memory_store;
pub mod model_routing_config;
pub mod pdf_read;
pub mod process;
pub mod proxy_config;
pub mod pushover;
pub mod schedule;
pub mod schema;
pub mod screenshot;
pub mod shell;
pub mod subagent_list;
pub mod subagent_manage;
pub mod subagent_registry;
pub mod subagent_spawn;
pub mod task_plan;
pub mod traits;
pub mod url_validation;
pub mod wasm_module;
pub mod wasm_tool;
pub mod web_fetch;
pub mod web_search_tool;

pub use apply_patch::ApplyPatchTool;
pub use browser::{BrowserTool, ComputerUseConfig};
pub use browser_open::BrowserOpenTool;
pub use composio::ComposioTool;
pub use content_search::ContentSearchTool;
pub use cron_add::CronAddTool;
pub use cron_list::CronListTool;
pub use cron_remove::CronRemoveTool;
pub use cron_run::CronRunTool;
pub use cron_runs::CronRunsTool;
pub use cron_update::CronUpdateTool;
pub use delegate::DelegateTool;
pub use delegate_coordination_status::DelegateCoordinationStatusTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use git_operations::GitOperationsTool;
pub use glob_search::GlobSearchTool;
#[cfg(feature = "hardware")]
pub use hardware_board_info::HardwareBoardInfoTool;
#[cfg(feature = "hardware")]
pub use hardware_memory_map::HardwareMemoryMapTool;
#[cfg(feature = "hardware")]
pub use hardware_memory_read::HardwareMemoryReadTool;
pub use http_request::HttpRequestTool;
pub use image_info::ImageInfoTool;
pub use mcp_client::McpRegistry;
pub use mcp_tool::McpToolWrapper;
pub use memory_forget::MemoryForgetTool;
pub use memory_recall::MemoryRecallTool;
pub use memory_store::MemoryStoreTool;
pub use model_routing_config::ModelRoutingConfigTool;
pub use pdf_read::PdfReadTool;
pub use process::ProcessTool;
pub use proxy_config::ProxyConfigTool;
pub use pushover::PushoverTool;
pub use schedule::ScheduleTool;
#[allow(unused_imports)]
pub use schema::{CleaningStrategy, SchemaCleanr};
pub use screenshot::ScreenshotTool;
pub use shell::ShellTool;
pub use subagent_list::SubAgentListTool;
pub use subagent_manage::SubAgentManageTool;
pub use subagent_registry::SubAgentRegistry;
pub use subagent_spawn::SubAgentSpawnTool;
pub use task_plan::TaskPlanTool;
pub use traits::Tool;
#[allow(unused_imports)]
pub use traits::{ToolResult, ToolSpec};
pub use wasm_module::WasmModuleTool;
pub use web_fetch::WebFetchTool;
pub use web_search_tool::WebSearchTool;

use crate::config::{Config, DelegateAgentConfig};
use crate::memory::Memory;
use crate::runtime::{NativeRuntime, RuntimeAdapter};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct ArcDelegatingTool {
    inner: Arc<dyn Tool>,
}

impl ArcDelegatingTool {
    fn boxed(inner: Arc<dyn Tool>) -> Box<dyn Tool> {
        Box::new(Self { inner })
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
    let has_shell_access = runtime.has_shell_access();
    let has_filesystem_access = runtime.has_filesystem_access();
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();

    if has_shell_access {
        tools.push(Box::new(ShellTool::new(security.clone(), runtime.clone())));
    }
    if has_filesystem_access {
        tools.push(Box::new(FileReadTool::new(security.clone())));
        tools.push(Box::new(FileWriteTool::new(security.clone())));
        tools.push(Box::new(FileEditTool::new(security.clone())));
        tools.push(Box::new(ApplyPatchTool::new()));
        tools.push(Box::new(GlobSearchTool::new(security.clone())));
        tools.push(Box::new(ContentSearchTool::new(security.clone())));
    }
    if runtime.as_any().is::<crate::runtime::WasmRuntime>() {
        tools.push(Box::new(WasmModuleTool::new(security, runtime)));
    }

    tools
}

/// Create full tool registry including memory tools and optional Composio
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn all_tools(
    config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    composio_entity_id: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
    http_config: &crate::config::HttpRequestConfig,
    web_fetch_config: &crate::config::WebFetchConfig,
    workspace_dir: &std::path::Path,
    agents: &HashMap<String, DelegateAgentConfig>,
    fallback_api_key: Option<&str>,
    root_config: &crate::config::Config,
) -> Vec<Box<dyn Tool>> {
    all_tools_with_runtime(
        config,
        security,
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
    )
}

/// Create full tool registry including memory tools and optional Composio.
#[allow(clippy::implicit_hasher, clippy::too_many_arguments)]
pub fn all_tools_with_runtime(
    config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    composio_entity_id: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
    http_config: &crate::config::HttpRequestConfig,
    web_fetch_config: &crate::config::WebFetchConfig,
    workspace_dir: &std::path::Path,
    agents: &HashMap<String, DelegateAgentConfig>,
    fallback_api_key: Option<&str>,
    root_config: &crate::config::Config,
) -> Vec<Box<dyn Tool>> {
    let has_shell_access = runtime.has_shell_access();
    let has_filesystem_access = runtime.has_filesystem_access();
    let zeroclaw_dir = root_config
        .config_path
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| runtime.storage_path());
    let syscall_detector = Arc::new(crate::security::SyscallAnomalyDetector::new(
        root_config.security.syscall_anomaly.clone(),
        &zeroclaw_dir,
        root_config.security.audit.clone(),
    ));

    let mut tool_arcs: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CronAddTool::new(config.clone(), security.clone())),
        Arc::new(CronListTool::new(config.clone())),
        Arc::new(CronRemoveTool::new(config.clone(), security.clone())),
        Arc::new(CronUpdateTool::new(config.clone(), security.clone())),
        Arc::new(CronRunTool::new(config.clone(), security.clone())),
        Arc::new(CronRunsTool::new(config.clone())),
        Arc::new(MemoryStoreTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone())),
        Arc::new(MemoryForgetTool::new(memory, security.clone())),
        Arc::new(ScheduleTool::new(security.clone(), root_config.clone())),
        Arc::new(TaskPlanTool::new(security.clone())),
        Arc::new(ModelRoutingConfigTool::new(
            config.clone(),
            security.clone(),
        )),
        Arc::new(ProxyConfigTool::new(config.clone(), security.clone())),
        Arc::new(PushoverTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )),
    ];

    if has_shell_access {
        tool_arcs.push(Arc::new(ShellTool::new_with_syscall_detector(
            security.clone(),
            runtime.clone(),
            Some(syscall_detector.clone()),
        )));
        tool_arcs.push(Arc::new(ProcessTool::new_with_syscall_detector(
            security.clone(),
            runtime.clone(),
            Some(syscall_detector),
        )));
        tool_arcs.push(Arc::new(GitOperationsTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )));
    }

    if has_filesystem_access {
        tool_arcs.push(Arc::new(FileReadTool::new(security.clone())));
        tool_arcs.push(Arc::new(FileWriteTool::new(security.clone())));
        tool_arcs.push(Arc::new(FileEditTool::new(security.clone())));
        tool_arcs.push(Arc::new(ApplyPatchTool::new()));
        tool_arcs.push(Arc::new(GlobSearchTool::new(security.clone())));
        tool_arcs.push(Arc::new(ContentSearchTool::new(security.clone())));
    }
    if runtime.as_any().is::<crate::runtime::WasmRuntime>() {
        tool_arcs.push(Arc::new(WasmModuleTool::new(
            security.clone(),
            runtime.clone(),
        )));
    }

    if browser_config.enabled {
        // Add legacy browser_open tool for simple URL opening
        tool_arcs.push(Arc::new(BrowserOpenTool::new(
            security.clone(),
            browser_config.allowed_domains.clone(),
            root_config.security.url_access.clone(),
        )));
        // Add full browser automation tool (pluggable backend)
        tool_arcs.push(Arc::new(BrowserTool::new_with_backend(
            security.clone(),
            browser_config.allowed_domains.clone(),
            browser_config.session_name.clone(),
            browser_config.backend.clone(),
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
        )));
    }

    if http_config.enabled {
        tool_arcs.push(Arc::new(HttpRequestTool::new(
            security.clone(),
            http_config.allowed_domains.clone(),
            root_config.security.url_access.clone(),
            http_config.max_response_size,
            http_config.timeout_secs,
            http_config.user_agent.clone(),
        )));
    }

    if web_fetch_config.enabled {
        tool_arcs.push(Arc::new(WebFetchTool::new(
            security.clone(),
            web_fetch_config.provider.clone(),
            web_fetch_config.api_key.clone(),
            web_fetch_config.api_url.clone(),
            web_fetch_config.allowed_domains.clone(),
            web_fetch_config.blocked_domains.clone(),
            root_config.security.url_access.clone(),
            web_fetch_config.max_response_size,
            web_fetch_config.timeout_secs,
            web_fetch_config.user_agent.clone(),
        )));
    }

    // Web search tool (enabled by default for GLM and other models)
    if root_config.web_search.enabled {
        let provider = root_config.web_search.provider.trim().to_lowercase();
        let api_key = if provider == "brave" {
            root_config
                .web_search
                .brave_api_key
                .clone()
                .or_else(|| root_config.web_search.api_key.clone())
        } else {
            root_config.web_search.api_key.clone()
        };
        tool_arcs.push(Arc::new(WebSearchTool::new(
            security.clone(),
            root_config.web_search.provider.clone(),
            api_key,
            root_config.web_search.api_url.clone(),
            root_config.web_search.max_results,
            root_config.web_search.timeout_secs,
            root_config.web_search.user_agent.clone(),
        )));
    }

    // PDF extraction (feature-gated at compile time via rag-pdf)
    tool_arcs.push(Arc::new(PdfReadTool::new(security.clone())));

    // Vision tools are always available
    tool_arcs.push(Arc::new(ScreenshotTool::new(security.clone())));
    tool_arcs.push(Arc::new(ImageInfoTool::new(security.clone())));

    if let Some(key) = composio_key {
        if !key.is_empty() {
            tool_arcs.push(Arc::new(ComposioTool::new(
                key,
                composio_entity_id,
                security.clone(),
            )));
        }
    }

    // Add delegation and sub-agent orchestration tools when agents are configured
    if !agents.is_empty() {
        let delegate_agents: HashMap<String, DelegateAgentConfig> = agents
            .iter()
            .map(|(name, cfg)| (name.clone(), cfg.clone()))
            .collect();
        let delegate_fallback_credential = fallback_api_key.and_then(|value| {
            let trimmed_value = value.trim();
            (!trimmed_value.is_empty()).then(|| trimmed_value.to_owned())
        });
        let provider_runtime_options = crate::providers::ProviderRuntimeOptions {
            auth_profile_override: None,
            provider_api_url: root_config.api_url.clone(),
            zeroclaw_dir: root_config
                .config_path
                .parent()
                .map(std::path::PathBuf::from),
            secrets_encrypt: root_config.secrets.encrypt,
            reasoning_enabled: root_config.runtime.reasoning_enabled,
            reasoning_level: root_config.effective_provider_reasoning_level(),
            custom_provider_api_mode: root_config
                .provider_api
                .map(|mode| mode.as_compatible_mode()),
            max_tokens_override: None,
            model_support_vision: root_config.model_support_vision,
        };
        let parent_tools = Arc::new(tool_arcs.clone());
        let mut delegate_tool = DelegateTool::new_with_options(
            delegate_agents.clone(),
            delegate_fallback_credential.clone(),
            security.clone(),
            provider_runtime_options.clone(),
        )
        .with_parent_tools(parent_tools.clone())
        .with_multimodal_config(root_config.multimodal.clone());

        if root_config.coordination.enabled {
            let coordination_lead_agent = {
                let value = root_config.coordination.lead_agent.trim();
                if value.is_empty() {
                    "delegate-lead".to_string()
                } else {
                    value.to_string()
                }
            };
            let coordination_bus = crate::coordination::InMemoryMessageBus::with_limits(
                crate::coordination::InMemoryMessageBusLimits {
                    max_inbox_messages_per_agent: root_config
                        .coordination
                        .max_inbox_messages_per_agent,
                    max_dead_letters: root_config.coordination.max_dead_letters,
                    max_context_entries: root_config.coordination.max_context_entries,
                    max_seen_message_ids: root_config.coordination.max_seen_message_ids,
                },
            );
            if let Err(error) = coordination_bus.register_agent(coordination_lead_agent.clone()) {
                tracing::warn!(
                    "delegate coordination: failed to register lead agent '{coordination_lead_agent}': {error}"
                );
            }
            for agent_name in agents.keys() {
                if let Err(error) = coordination_bus.register_agent(agent_name.clone()) {
                    tracing::warn!(
                        "delegate coordination: failed to register agent '{agent_name}': {error}"
                    );
                }
            }

            delegate_tool = delegate_tool
                .with_coordination_bus(coordination_bus.clone(), coordination_lead_agent);
            tool_arcs.push(Arc::new(delegate_tool));
            tool_arcs.push(Arc::new(DelegateCoordinationStatusTool::new(
                coordination_bus,
                security.clone(),
            )));
        } else {
            delegate_tool = delegate_tool.with_coordination_disabled();
            tool_arcs.push(Arc::new(delegate_tool));
        }

        let subagent_registry = Arc::new(SubAgentRegistry::new());
        tool_arcs.push(Arc::new(SubAgentSpawnTool::new(
            delegate_agents,
            delegate_fallback_credential,
            security.clone(),
            provider_runtime_options,
            subagent_registry.clone(),
            parent_tools,
            root_config.multimodal.clone(),
        )));
        tool_arcs.push(Arc::new(SubAgentListTool::new(subagent_registry.clone())));
        tool_arcs.push(Arc::new(SubAgentManageTool::new(
            subagent_registry,
            security.clone(),
        )));
    }

    // Inter-process agent communication (opt-in)
    if root_config.agents_ipc.enabled {
        match agents_ipc::IpcDb::open(workspace_dir, &root_config.agents_ipc) {
            Ok(ipc_db) => {
                let ipc_db = Arc::new(ipc_db);
                tool_arcs.push(Arc::new(agents_ipc::AgentsListTool::new(ipc_db.clone())));
                tool_arcs.push(Arc::new(agents_ipc::AgentsSendTool::new(
                    ipc_db.clone(),
                    security.clone(),
                )));
                tool_arcs.push(Arc::new(agents_ipc::AgentsInboxTool::new(ipc_db.clone())));
                tool_arcs.push(Arc::new(agents_ipc::StateGetTool::new(ipc_db.clone())));
                tool_arcs.push(Arc::new(agents_ipc::StateSetTool::new(
                    ipc_db,
                    security.clone(),
                )));
            }
            Err(e) => {
                tracing::warn!("agents_ipc: failed to open IPC database: {e}");
            }
        }
    }

    // Load WASM plugin tools from the skills directory.
    // Each installed skill package may ship one or more WASM tools under
    // `<skill-dir>/tools/<tool-name>/{tool.wasm, manifest.json}`.
    // Failures are logged and skipped — a broken plugin must not block startup.
    let skills_dir = workspace_dir.join("skills");
    let mut boxed = boxed_registry_from_arcs(tool_arcs);
    let wasm_tools = wasm_tool::load_wasm_tools_from_skills(&skills_dir);
    boxed.extend(wasm_tools);
    boxed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BrowserConfig, Config, MemoryConfig, WasmRuntimeConfig};
    use crate::runtime::WasmRuntime;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        }
    }

    #[test]
    fn default_tools_has_expected_count() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        assert_eq!(tools.len(), 7);
        assert!(tools.iter().any(|tool| tool.name() == "apply_patch"));
    }

    #[test]
    fn default_tools_with_runtime_includes_wasm_module_for_wasm_runtime() {
        let security = Arc::new(SecurityPolicy::default());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tools = default_tools_with_runtime(security, runtime);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"wasm_module"));
    }

    #[test]
    fn default_tools_with_runtime_excludes_shell_and_fs_for_wasm_runtime() {
        let security = Arc::new(SecurityPolicy::default());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));
        let tools = default_tools_with_runtime(security, runtime);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"file_read"));
        assert!(!names.contains(&"file_write"));
        assert!(!names.contains(&"file_edit"));
        assert!(!names.contains(&"apply_patch"));
        assert!(!names.contains(&"glob_search"));
        assert!(!names.contains(&"content_search"));
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
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: false,
            allowed_domains: vec!["example.com".into()],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = crate::config::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
        );
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
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig {
            enabled: true,
            allowed_domains: vec!["example.com".into()],
            session_name: None,
            ..BrowserConfig::default()
        };
        let http = crate::config::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"browser_open"));
        assert!(names.contains(&"content_search"));
        assert!(names.contains(&"model_routing_config"));
        assert!(names.contains(&"pushover"));
        assert!(names.contains(&"proxy_config"));
    }

    #[test]
    fn all_tools_with_runtime_includes_wasm_module_for_wasm_runtime() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let runtime: Arc<dyn RuntimeAdapter> =
            Arc::new(WasmRuntime::new(WasmRuntimeConfig::default()));

        let browser = BrowserConfig::default();
        let http = crate::config::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools_with_runtime(
            Arc::new(Config::default()),
            &security,
            runtime,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"wasm_module"));
        assert!(!names.contains(&"shell"));
        assert!(!names.contains(&"process"));
        assert!(!names.contains(&"git_operations"));
        assert!(!names.contains(&"file_read"));
        assert!(!names.contains(&"file_write"));
        assert!(!names.contains(&"file_edit"));
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
            output: String::new(),
            error: Some("boom".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();
        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn tool_spec_serde() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
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
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = crate::config::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &agents,
            Some("delegate-test-credential"),
            &cfg,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"delegate"));
        assert!(names.contains(&"delegate_coordination_status"));
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
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = crate::config::HttpRequestConfig::default();
        let cfg = test_config(&tmp);

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"delegate"));
        assert!(!names.contains(&"delegate_coordination_status"));
    }

    #[test]
    fn all_tools_disables_coordination_tool_when_coordination_is_disabled() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(crate::memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

        let browser = BrowserConfig::default();
        let http = crate::config::HttpRequestConfig::default();
        let mut cfg = test_config(&tmp);
        cfg.coordination.enabled = false;

        let mut agents = HashMap::new();
        agents.insert(
            "researcher".to_string(),
            DelegateAgentConfig {
                provider: "ollama".to_string(),
                model: "llama3".to_string(),
                system_prompt: None,
                api_key: None,
                temperature: None,
                max_depth: 3,
                agentic: false,
                allowed_tools: Vec::new(),
                max_iterations: 10,
            },
        );

        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            mem,
            None,
            None,
            &browser,
            &http,
            &crate::config::WebFetchConfig::default(),
            tmp.path(),
            &agents,
            Some("delegate-test-credential"),
            &cfg,
        );
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"delegate"));
        assert!(!names.contains(&"delegate_coordination_status"));
    }
}
