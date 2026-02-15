pub mod browser;
pub mod browser_open;
pub mod composio;
pub mod file_read;
pub mod file_write;
pub mod memory_forget;
pub mod memory_recall;
pub mod memory_store;
pub mod shell;
pub mod traits;

pub use browser::BrowserTool;
pub use browser_open::BrowserOpenTool;
pub use composio::ComposioTool;
pub use file_read::FileReadTool;
pub use file_write::FileWriteTool;
pub use memory_forget::MemoryForgetTool;
pub use memory_recall::MemoryRecallTool;
pub use memory_store::MemoryStoreTool;
pub use shell::ShellTool;
pub use traits::Tool;
#[allow(unused_imports)]
pub use traits::{ToolResult, ToolSpec};

use crate::memory::Memory;
use crate::runtime::{NativeRuntime, RuntimeAdapter};
use crate::security::SecurityPolicy;
use std::sync::Arc;

/// Create the default tool registry
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
        Box::new(FileWriteTool::new(security)),
    ]
}

/// Create full tool registry including memory tools and optional Composio
pub fn all_tools(
    security: &Arc<SecurityPolicy>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
) -> Vec<Box<dyn Tool>> {
    all_tools_with_runtime(
        security,
        Arc::new(NativeRuntime::new()),
        memory,
        composio_key,
        browser_config,
    )
}

/// Create full tool registry including memory tools and optional Composio.
pub fn all_tools_with_runtime(
    security: &Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    memory: Arc<dyn Memory>,
    composio_key: Option<&str>,
    browser_config: &crate::config::BrowserConfig,
) -> Vec<Box<dyn Tool>> {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ShellTool::new(security.clone(), runtime)),
        Box::new(FileReadTool::new(security.clone())),
        Box::new(FileWriteTool::new(security.clone())),
        Box::new(MemoryStoreTool::new(memory.clone())),
        Box::new(MemoryRecallTool::new(memory.clone())),
        Box::new(MemoryForgetTool::new(memory)),
    ];

    if browser_config.enabled {
        // Add legacy browser_open tool for simple URL opening
        tools.push(Box::new(BrowserOpenTool::new(
            security.clone(),
            browser_config.allowed_domains.clone(),
        )));
        // Add full browser automation tool (agent-browser)
        tools.push(Box::new(BrowserTool::new(
            security.clone(),
            browser_config.allowed_domains.clone(),
            browser_config.session_name.clone(),
        )));
    }

    if let Some(key) = composio_key {
        if !key.is_empty() {
            tools.push(Box::new(ComposioTool::new(key)));
        }
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BrowserConfig, MemoryConfig};
    use tempfile::TempDir;

    #[test]
    fn default_tools_has_three() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        assert_eq!(tools.len(), 3);
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
        };

        let tools = all_tools(&security, mem, None, &browser);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(!names.contains(&"browser_open"));
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
        };

        let tools = all_tools(&security, mem, None, &browser);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"browser_open"));
    }

    #[test]
    fn default_tools_names() {
        let security = Arc::new(SecurityPolicy::default());
        let tools = default_tools(security);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"file_write"));
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
}
