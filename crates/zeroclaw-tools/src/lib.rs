//! Tool implementations for agent-callable capabilities.

pub mod util_helpers;

pub mod ask_user;
pub mod calculator;
pub mod cli_discovery;
pub mod content_search;
pub mod data_management;
pub mod file_edit;
pub mod file_write;
pub mod git_operations;
pub mod glob_search;
pub mod image_info;
pub mod knowledge_tool;
// MCP modules migrated to crates/osagent-tools-mcp (Phase 1.4).
// Consumers must re-import from `osagent_tools_mcp::mcp_*` instead of
// `zeroclaw_tools::mcp_*`. The wizard binary's Cargo.toml MUST NOT list
// osagent-tools-mcp as a dependency — see bins/wizard/Cargo.toml.
pub mod memory_export;
pub mod memory_forget;
pub mod memory_purge;
pub mod memory_recall;
pub mod memory_store;
pub mod model_routing_config;
pub mod node_capabilities;
pub mod pdf_read;
pub mod pipeline;
pub mod poll;
pub mod proxy_config;
pub mod report_template_tool;
pub mod report_templates;
pub mod sessions;
pub mod tool_search;
pub mod workspace_tool;
pub mod wrappers;
