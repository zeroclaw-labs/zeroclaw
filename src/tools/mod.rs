//! Tool subsystem for agent-callable capabilities.
//!
//! Each tool implements the [`Tool`] trait defined in [`traits`], which requires
//! a name, description, JSON parameter schema, and an async `execute` method.

pub mod browser;
pub mod browser_open;
pub mod content_search;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod git_operations;
pub mod glob_search;
pub mod http_request;
pub mod image_info;
pub mod memory_forget;
pub mod memory_recall;
pub mod memory_store;
pub mod schema;
pub mod screenshot;
pub mod shell;
pub mod traits;
pub mod web_fetch;

pub use browser::{BrowserTool, ComputerUseConfig};
pub use browser_open::BrowserOpenTool;
pub use content_search::ContentSearchTool;
pub use file_edit::FileEditTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use git_operations::GitOperationsTool;
pub use glob_search::GlobSearchTool;
pub use http_request::HttpRequestTool;
pub use image_info::ImageInfoTool;
pub use memory_forget::MemoryForgetTool;
pub use memory_recall::MemoryRecallTool;
pub use memory_store::MemoryStoreTool;
#[allow(unused_imports)]
pub use schema::{CleaningStrategy, SchemaCleanr};
pub use screenshot::ScreenshotTool;
pub use shell::ShellTool;
pub use traits::Tool;
#[allow(unused_imports)]
pub use traits::{ToolResult, ToolSpec};
pub use web_fetch::WebFetchTool;

use crate::config::Config;
use crate::memory::Memory;
use crate::runtime::{NativeRuntime, RuntimeAdapter};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
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

/// Create the default tool registry (shell + file ops)
pub fn default_tools(security: Arc<SecurityPolicy>) -> Vec<Box<dyn Tool>> {
    default_tools_with_runtime(security, Arc::new(NativeRuntime::new()))
}

/// Create the default tool registry with explicit runtime adapter.
pub fn default_tools_with_runtime(
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ShellTool::new(security.clone(), runtime)),
        Box::new(FileReadTool::new(security.clone())),
        Box::new(FileWriteTool::new(security.clone())),
        Box::new(FileEditTool::new(security.clone())),
        Box::new(GlobSearchTool::new(security.clone())),
        Box::new(ContentSearchTool::new(security)),
    ]
}

/// Create full tool registry including memory, browser, HTTP, and vision tools.
#[allow(clippy::too_many_arguments)]
pub fn all_tools(
    _config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    memory: Arc<dyn Memory>,
    browser_config: &crate::config::BrowserConfig,
    http_config: &crate::config::HttpRequestConfig,
    web_fetch_config: &crate::config::WebFetchConfig,
    workspace_dir: &std::path::Path,
    root_config: &crate::config::Config,
) -> Vec<Box<dyn Tool>> {
    all_tools_with_runtime(
        _config,
        security,
        Arc::new(NativeRuntime::new()),
        memory,
        browser_config,
        http_config,
        web_fetch_config,
        workspace_dir,
        root_config,
    )
}

/// Create full tool registry with explicit runtime adapter.
#[allow(clippy::too_many_arguments)]
pub fn all_tools_with_runtime(
    _config: Arc<Config>,
    security: &Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    memory: Arc<dyn Memory>,
    browser_config: &crate::config::BrowserConfig,
    http_config: &crate::config::HttpRequestConfig,
    web_fetch_config: &crate::config::WebFetchConfig,
    workspace_dir: &std::path::Path,
    _root_config: &crate::config::Config,
) -> Vec<Box<dyn Tool>> {
    let mut tool_arcs: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool::new(security.clone(), runtime)),
        Arc::new(FileReadTool::new(security.clone())),
        Arc::new(FileWriteTool::new(security.clone())),
        Arc::new(FileEditTool::new(security.clone())),
        Arc::new(GlobSearchTool::new(security.clone())),
        Arc::new(ContentSearchTool::new(security.clone())),
        Arc::new(MemoryStoreTool::new(memory.clone(), security.clone())),
        Arc::new(MemoryRecallTool::new(memory.clone())),
        Arc::new(MemoryForgetTool::new(memory, security.clone())),
        Arc::new(GitOperationsTool::new(
            security.clone(),
            workspace_dir.to_path_buf(),
        )),
    ];

    if browser_config.enabled {
        tool_arcs.push(Arc::new(BrowserOpenTool::new(
            security.clone(),
            browser_config.allowed_domains.clone(),
        )));
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
            http_config.max_response_size,
            http_config.timeout_secs,
        )));
    }

    if web_fetch_config.enabled {
        tool_arcs.push(Arc::new(WebFetchTool::new(
            security.clone(),
            web_fetch_config.allowed_domains.clone(),
            web_fetch_config.blocked_domains.clone(),
            web_fetch_config.max_response_size,
            web_fetch_config.timeout_secs,
        )));
    }

    // Vision tools always available
    tool_arcs.push(Arc::new(ScreenshotTool::new(security.clone())));
    tool_arcs.push(Arc::new(ImageInfoTool::new(security.clone())));

    // macOS desktop tools — will be registered when lightwave-macos crate is built
    // #[cfg(target_os = "macos")]
    // {
    //     if let Ok(mac_tool) = lightwave_macos::MacDesktopTool::new(security.clone()) {
    //         tool_arcs.push(Arc::new(mac_tool));
    //     }
    // }

    boxed_registry_from_arcs(tool_arcs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tools_has_expected_count() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        assert_eq!(tools.len(), 6);
    }
}
