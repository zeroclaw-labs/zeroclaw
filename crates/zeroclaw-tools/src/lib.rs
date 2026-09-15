//! Tool implementations for agent-callable capabilities.

pub mod attribution;
pub mod helpers;
pub(crate) mod i18n;
pub mod microsoft365;
pub mod util_helpers;

pub mod ask_user;
pub mod backup_tool;
pub mod browser;
pub mod browser_delegate;
pub mod browser_open;
pub mod calculator;
pub mod canvas;
pub mod channel_room;
pub mod claude_code;
pub mod claude_code_runner;
pub mod cli_discovery;
pub mod cloud_ops;
pub mod cloud_patterns;
pub mod codex_cli;
pub mod coding_cli;
pub mod composio;
pub mod content_search;
pub mod data_management;
pub mod discord_search;
pub mod email_imap;
pub mod email_read;
pub mod email_search;
pub mod embedded_resource;
pub mod escalate;
pub mod file_download;
pub mod file_edit;
pub mod file_upload;
pub mod file_upload_bundle;
pub mod file_write;
pub mod gemini_cli;
pub mod git_forge;
pub mod git_operations;
pub mod glob_search;
pub mod google_workspace;
pub mod hardware_board_info;
pub mod hardware_memory_map;
pub mod hardware_memory_read;
mod http_decode;
pub mod http_request;
pub mod image_gen;
pub mod image_info;
pub mod jira_tool;
pub mod knowledge_tool;
pub mod linkedin;
pub mod linkedin_client;
pub mod llm_task;
pub mod mcp_client;
pub mod mcp_context;
pub mod mcp_deferred;
pub mod mcp_prompt;
pub mod mcp_prompts_tool;
pub mod mcp_protocol;
pub mod mcp_resource;
pub mod mcp_resources_tool;
pub mod mcp_tool;
pub mod mcp_transport;
pub mod memory_export;
pub mod memory_forget;
pub mod memory_purge;
pub mod memory_recall;
pub mod memory_store;
pub mod model_routing_config;
pub mod node_capabilities;
pub mod notion_tool;
pub mod opencode_cli;
pub mod pipeline;
pub mod poll;
pub mod project_intel;
pub mod proxy_config;
pub mod pushover;
pub mod reaction;
pub mod report_template_tool;
pub mod report_templates;
pub mod screenshot;
pub mod send_via;
pub mod sessions;
pub mod text_browser;
pub mod tool_search;
pub mod weather_tool;
pub mod web_fetch;
pub mod web_search_provider_routing;
pub mod web_search_tool;
pub mod wrappers;

#[cfg(all(test, unix))]
mod coding_agent_budget_tests;

pub const MEMORY_TOOL_NAMES: &[&str] = &[
    "memory_store",
    "memory_recall",
    "memory_forget",
    "memory_export",
    "memory_purge",
];

/// Shared test-only isolation for the process-global runtime proxy state that
/// production `Tool::execute` paths read (`http_request`, `web_fetch`) and
/// `proxy_config` tests mutate through the real setters.
///
/// One binary-wide async mutex serializes those tests, and the guard restores
/// the value observed on entry when it drops, so a writer test can neither leak
/// its configured proxy into later tests nor race a concurrent reader mid-test.
/// A reset alone would still race; production proxy state is untouched.
#[cfg(test)]
pub(crate) mod test_support {
    use tokio::sync::{Mutex, MutexGuard};
    use zeroclaw_config::schema::{ProxyConfig, runtime_proxy_config, set_runtime_proxy_config};

    static RUNTIME_PROXY_STATE_LOCK: Mutex<()> = Mutex::const_new(());

    pub(crate) struct RuntimeProxyStateGuard {
        _lock: MutexGuard<'static, ()>,
        snapshot: ProxyConfig,
    }

    impl RuntimeProxyStateGuard {
        /// Hold while the test reads or mutates the runtime proxy state.
        pub(crate) async fn acquire() -> Self {
            let lock = RUNTIME_PROXY_STATE_LOCK.lock().await;
            let snapshot = runtime_proxy_config();
            Self {
                _lock: lock,
                snapshot,
            }
        }
    }

    impl Drop for RuntimeProxyStateGuard {
        fn drop(&mut self) {
            set_runtime_proxy_config(self.snapshot.clone());
        }
    }
}

#[cfg(test)]
mod memory_tool_names_guard {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::policy::SecurityPolicy;
    use zeroclaw_memory::NoneMemory;

    #[test]
    fn memory_tool_names_match_tools() {
        let memory = Arc::new(NoneMemory::new("none"));
        let security = Arc::new(SecurityPolicy::default());
        let tools: Vec<Box<dyn Tool>> = vec![
            Box::new(memory_store::MemoryStoreTool::new(
                memory.clone(),
                security.clone(),
            )),
            Box::new(memory_recall::MemoryRecallTool::new(memory.clone())),
            Box::new(memory_forget::MemoryForgetTool::new(
                memory.clone(),
                security.clone(),
            )),
            Box::new(memory_export::MemoryExportTool::new(memory.clone())),
            Box::new(memory_purge::MemoryPurgeTool::new(
                memory.clone(),
                security.clone(),
            )),
        ];
        let actual: BTreeSet<&str> = tools.iter().map(|t| t.name()).collect();
        let listed: BTreeSet<&str> = MEMORY_TOOL_NAMES.iter().copied().collect();
        assert_eq!(
            actual, listed,
            "MEMORY_TOOL_NAMES is out of sync with the constructed memory tools — \
             update the const in zeroclaw-tools/src/lib.rs"
        );
    }
}
