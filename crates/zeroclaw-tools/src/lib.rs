//! Tool implementations for agent-callable capabilities.

pub mod attribution;
pub mod helpers;
pub(crate) mod i18n;
pub mod util_helpers;

pub mod ask_user;
pub mod backup_tool;
pub mod browser;
pub mod browser_open;
pub mod content_search;
pub mod data_management;
pub mod discord_search;
#[cfg(feature = "email-tools")]
pub mod email_imap;
#[cfg(feature = "email-tools")]
pub mod email_read;
#[cfg(feature = "email-tools")]
pub mod email_search;
pub mod escalate;
pub mod file_download;
pub mod file_edit;
pub mod file_upload;
pub mod file_upload_bundle;
pub mod file_write;
mod fs_guard;
pub mod git_operations;
pub mod glob_search;
pub mod http_request;
pub mod image_gen;
pub mod image_info;
pub mod llm_task;
pub mod mcp_client;
pub mod mcp_context;
pub mod mcp_deferred;
pub mod mcp_era;
pub mod mcp_prompt;
pub mod mcp_prompts_tool;
pub mod mcp_protocol;
pub mod mcp_resource;
pub mod mcp_resources_tool;
pub mod mcp_task;
pub mod mcp_tool;
pub mod mcp_transport;
pub mod memory_export;
pub mod memory_forget;
pub mod memory_purge;
pub mod memory_recall;
pub mod memory_store;
pub mod node_capabilities;
pub mod note_owner_correction;
pub mod pipeline;
pub mod propose_soul_change;
pub mod screenshot;
pub mod sessions;
pub mod tachi_admission;
pub mod tachi_delegation;
pub mod tachi_staff;
pub mod text_browser;
pub mod tool_search;
pub mod web_fetch;
pub mod web_search_provider_routing;
pub mod web_search_tool;
pub mod wrappers;

pub const MEMORY_TOOL_NAMES: &[&str] = &[
    "memory_store",
    "memory_recall",
    "memory_forget",
    "memory_export",
    "memory_purge",
];

#[cfg(test)]
mod memory_tool_names_guard {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_memory::NoneMemory;
    use zeroclaw_policy::SecurityPolicy;

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
