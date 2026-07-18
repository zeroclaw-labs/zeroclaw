//! Centralized `Attributable` impls for every concrete `Tool` in this
//! crate. Each invocation surfaces `Role::Tool(ToolKind::*)` and uses
//! the tool's `name()` as its alias so log emissions can attribute
//! tool activity with the same `<kind>.<alias>` composite the rest of

use zeroclaw_api::attribution::ToolKind;
use zeroclaw_api::tool_attribution;

use crate::ask_user::AskUserTool;
use crate::backup_tool::BackupTool;
use crate::browser::BrowserTool;
use crate::browser_delegate::BrowserDelegateTool;
use crate::browser_open::BrowserOpenTool;
use crate::calculator::CalculatorTool;
use crate::canvas::CanvasTool;
use crate::channel_room::ChannelRoomTool;
use crate::claude_code::ClaudeCodeTool;
use crate::claude_code_runner::ClaudeCodeRunnerTool;
use crate::cloud_ops::CloudOpsTool;
use crate::cloud_patterns::CloudPatternsTool;
use crate::codex_cli::CodexCliTool;
use crate::composio::ComposioTool;
use crate::content_search::ContentSearchTool;
use crate::data_management::DataManagementTool;
use crate::discord_search::DiscordSearchTool;
use crate::escalate::EscalateToHumanTool;
use crate::file_download::FileDownloadTool;
use crate::file_edit::FileEditTool;
use crate::file_upload::FileUploadTool;
use crate::file_upload_bundle::FileUploadBundleTool;
use crate::file_write::FileWriteTool;
use crate::gemini_cli::GeminiCliTool;
use crate::git_forge::GitForgeTool;
use crate::git_operations::GitOperationsTool;
use crate::glob_search::GlobSearchTool;
use crate::google_workspace::GoogleWorkspaceTool;
use crate::hardware_board_info::HardwareBoardInfoTool;
use crate::hardware_memory_map::HardwareMemoryMapTool;
use crate::hardware_memory_read::HardwareMemoryReadTool;
use crate::http_request::HttpRequestTool;
use crate::image_gen::ImageGenTool;
use crate::image_info::ImageInfoTool;
use crate::jira_tool::JiraTool;
use crate::knowledge_tool::KnowledgeTool;
use crate::linkedin::LinkedInTool;
use crate::llm_task::LlmTaskTool;
use crate::mcp_tool::McpToolWrapper;
use crate::memory_export::MemoryExportTool;
use crate::memory_forget::MemoryForgetTool;
use crate::memory_purge::MemoryPurgeTool;
use crate::memory_recall::MemoryRecallTool;
use crate::memory_store::MemoryStoreTool;
use crate::microsoft365::Microsoft365Tool;
use crate::model_routing_config::ModelRoutingConfigTool;
use crate::notion_tool::NotionTool;
use crate::opencode_cli::OpenCodeCliTool;
use crate::pipeline::PipelineTool;
use crate::poll::PollTool;
use crate::project_intel::ProjectIntelTool;
use crate::proxy_config::ProxyConfigTool;
use crate::pushover::PushoverTool;
use crate::reaction::ReactionTool;
use crate::report_template_tool::ReportTemplateTool;
use crate::screenshot::ScreenshotTool;
use crate::send_via::SendViaTool;
use crate::sessions::{
    SessionDeleteTool, SessionResetTool, SessionsCurrentTool, SessionsHistoryTool,
    SessionsListTool, SessionsSendTool,
};
use crate::text_browser::TextBrowserTool;
use crate::tool_search::ToolSearchTool;
use crate::weather_tool::WeatherTool;
use crate::web_fetch::WebFetchTool;
use crate::web_search_tool::WebSearchTool;

tool_attribution!(AskUserTool, ToolKind::Wait);
tool_attribution!(BackupTool, ToolKind::Plugin);
tool_attribution!(BrowserTool, ToolKind::Plugin);
tool_attribution!(BrowserDelegateTool, ToolKind::Plugin);
tool_attribution!(BrowserOpenTool, ToolKind::Plugin);
tool_attribution!(CalculatorTool, ToolKind::Plugin);
tool_attribution!(CanvasTool, ToolKind::Plugin);
tool_attribution!(ChannelRoomTool, ToolKind::Plugin);
tool_attribution!(ClaudeCodeTool, ToolKind::Plugin);
tool_attribution!(ClaudeCodeRunnerTool, ToolKind::Plugin);
tool_attribution!(CloudOpsTool, ToolKind::Plugin);
tool_attribution!(CloudPatternsTool, ToolKind::Plugin);
tool_attribution!(CodexCliTool, ToolKind::Plugin);
tool_attribution!(ComposioTool, ToolKind::Plugin);
tool_attribution!(ContentSearchTool, ToolKind::Search);
tool_attribution!(DataManagementTool, ToolKind::Plugin);
tool_attribution!(DiscordSearchTool, ToolKind::Search);
tool_attribution!(EscalateToHumanTool, ToolKind::Wait);
tool_attribution!(FileDownloadTool, ToolKind::Plugin);
tool_attribution!(FileEditTool, ToolKind::Plugin);
tool_attribution!(FileUploadTool, ToolKind::Plugin);
tool_attribution!(FileUploadBundleTool, ToolKind::Plugin);
tool_attribution!(FileWriteTool, ToolKind::Plugin);
tool_attribution!(GeminiCliTool, ToolKind::Plugin);
tool_attribution!(GitOperationsTool, ToolKind::Shell);
tool_attribution!(GitForgeTool, ToolKind::Plugin);
tool_attribution!(GlobSearchTool, ToolKind::Search);
tool_attribution!(GoogleWorkspaceTool, ToolKind::Plugin);
tool_attribution!(HardwareBoardInfoTool, ToolKind::Plugin);
tool_attribution!(HardwareMemoryMapTool, ToolKind::Plugin);
tool_attribution!(HardwareMemoryReadTool, ToolKind::Plugin);
tool_attribution!(HttpRequestTool, ToolKind::HttpRequest);
tool_attribution!(ImageGenTool, ToolKind::Plugin);
tool_attribution!(ImageInfoTool, ToolKind::Plugin);
tool_attribution!(JiraTool, ToolKind::Plugin);
tool_attribution!(KnowledgeTool, ToolKind::Plugin);
tool_attribution!(LinkedInTool, ToolKind::Plugin);
tool_attribution!(LlmTaskTool, ToolKind::Plugin);
tool_attribution!(McpToolWrapper, ToolKind::Plugin);
tool_attribution!(MemoryExportTool, ToolKind::Memory);
tool_attribution!(MemoryForgetTool, ToolKind::Memory);
tool_attribution!(MemoryPurgeTool, ToolKind::Memory);
tool_attribution!(MemoryRecallTool, ToolKind::Memory);
tool_attribution!(MemoryStoreTool, ToolKind::Memory);
tool_attribution!(Microsoft365Tool, ToolKind::Plugin);
tool_attribution!(ModelRoutingConfigTool, ToolKind::Plugin);
tool_attribution!(NotionTool, ToolKind::Plugin);
tool_attribution!(OpenCodeCliTool, ToolKind::Plugin);
tool_attribution!(PipelineTool, ToolKind::Plugin);
tool_attribution!(PollTool, ToolKind::Wait);
tool_attribution!(ProjectIntelTool, ToolKind::Plugin);
tool_attribution!(ProxyConfigTool, ToolKind::Plugin);
tool_attribution!(PushoverTool, ToolKind::Plugin);
tool_attribution!(ReactionTool, ToolKind::Plugin);
tool_attribution!(ReportTemplateTool, ToolKind::Plugin);
tool_attribution!(ScreenshotTool, ToolKind::Plugin);
tool_attribution!(SendViaTool, ToolKind::Plugin);
tool_attribution!(SessionDeleteTool, ToolKind::Plugin);
tool_attribution!(SessionResetTool, ToolKind::Plugin);
tool_attribution!(SessionsCurrentTool, ToolKind::Plugin);
tool_attribution!(SessionsHistoryTool, ToolKind::Plugin);
tool_attribution!(SessionsListTool, ToolKind::Plugin);
tool_attribution!(SessionsSendTool, ToolKind::Plugin);
tool_attribution!(TextBrowserTool, ToolKind::Plugin);
tool_attribution!(ToolSearchTool, ToolKind::Search);
tool_attribution!(WeatherTool, ToolKind::Plugin);
tool_attribution!(WebFetchTool, ToolKind::FetchUrl);
tool_attribution!(WebSearchTool, ToolKind::Search);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::calculator::CalculatorTool;
    use zeroclaw_api::attribution::{Attributable, Role};

    #[test]
    fn macro_sets_role_to_tool_kind() {
        let tool = CalculatorTool;
        assert_eq!(tool.role(), Role::Tool(ToolKind::Plugin));
        assert_eq!(tool.alias(), "calculator");
    }

    #[test]
    fn attributable_via_arc_matches_inner() {
        let inner = CalculatorTool;
        let arc: Arc<CalculatorTool> = Arc::new(inner);
        assert_eq!(arc.alias(), "calculator");
        assert_eq!(arc.role(), Role::Tool(ToolKind::Plugin));
    }

    #[test]
    fn tool_name_has_no_dot_separator() {
        let tool = CalculatorTool;
        assert!(
            !tool.alias().contains('.'),
            "alias `{}` must not contain `.` — would break `<kind>.<alias>` composite parsing",
            tool.alias()
        );
    }

    #[test]
    fn tool_name_is_nonempty() {
        let tool = CalculatorTool;
        assert!(!tool.alias().is_empty());
    }
}
