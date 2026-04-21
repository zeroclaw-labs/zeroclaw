//! Deferred MCP tool loading — stubs and activated-tool tracking.
//!
//! When `mcp.deferred_loading` is enabled, MCP tool schemas are NOT eagerly
//! included in the LLM context window. Instead, only lightweight stubs (name +
//! description) are exposed in the system prompt. The LLM must call the built-in
//! `tool_search` tool to fetch full schemas, which moves them into the
//! [`ActivatedToolSet`] for the current conversation.

use std::collections::HashMap;
use std::sync::Arc;

use crate::mcp_client::McpRegistry;
use crate::mcp_protocol::McpToolDef;
use crate::mcp_tool::McpToolWrapper;
use zeroclaw_api::tool::{Tool, ToolSpec};

// ── DeferredMcpToolStub ──────────────────────────────────────────────────

/// A lightweight stub representing a known-but-not-yet-loaded MCP tool.
/// Contains only the prefixed name, a human-readable description, and enough
/// information to construct the full [`McpToolWrapper`] on activation.
#[derive(Debug, Clone)]
pub struct DeferredMcpToolStub {
    /// Prefixed name: `<server_name>__<tool_name>`.
    pub prefixed_name: String,
    /// Human-readable description (extracted from the MCP tool definition).
    pub description: String,
    /// The full tool definition — stored so we can construct a wrapper later.
    def: McpToolDef,
}

impl DeferredMcpToolStub {
    pub fn new(prefixed_name: String, def: McpToolDef) -> Self {
        let description = def
            .description
            .clone()
            .unwrap_or_else(|| "MCP tool".to_string());
        Self {
            prefixed_name,
            description,
            def,
        }
    }

    /// Materialize this stub into a live [`McpToolWrapper`].
    pub fn activate(&self, registry: Arc<McpRegistry>) -> McpToolWrapper {
        McpToolWrapper::new(self.prefixed_name.clone(), self.def.clone(), registry)
    }
}

// ── DeferredMcpToolSet ───────────────────────────────────────────────────

/// Collection of all deferred MCP tool stubs discovered at startup.
/// Provides keyword search for `tool_search`.
#[derive(Clone)]
pub struct DeferredMcpToolSet {
    /// All stubs — exposed for test construction.
    pub stubs: Vec<DeferredMcpToolStub>,
    /// Shared registry — exposed for test construction.
    pub registry: Arc<McpRegistry>,
}

impl DeferredMcpToolSet {
    /// Build the set from a connected [`McpRegistry`].
    pub async fn from_registry(registry: Arc<McpRegistry>) -> Self {
        let names = registry.tool_names();
        let mut stubs = Vec::with_capacity(names.len());
        for name in names {
            if let Some(def) = registry.get_tool_def(&name).await {
                stubs.push(DeferredMcpToolStub::new(name, def));
            }
        }
        Self { stubs, registry }
    }

    /// All stub names (for rendering in the system prompt).
    pub fn stub_names(&self) -> Vec<&str> {
        self.stubs
            .iter()
            .map(|s| s.prefixed_name.as_str())
            .collect()
    }

    /// Number of deferred stubs.
    pub fn len(&self) -> usize {
        self.stubs.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.stubs.is_empty()
    }

    /// Look up stubs by exact name. Used for `select:name1,name2` queries.
    pub fn get_by_name(&self, name: &str) -> Option<&DeferredMcpToolStub> {
        self.stubs.iter().find(|s| s.prefixed_name == name)
    }

    /// Keyword search — returns stubs whose name or description contains any
    /// of the query terms (case-insensitive). Results are ranked by number of
    /// matching terms (descending).
    pub fn search(&self, query: &str, max_results: usize) -> Vec<&DeferredMcpToolStub> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return self.stubs.iter().take(max_results).collect();
        }

        let mut scored: Vec<(&DeferredMcpToolStub, usize)> = self
            .stubs
            .iter()
            .filter_map(|stub| {
                let haystack = format!(
                    "{} {}",
                    stub.prefixed_name.to_ascii_lowercase(),
                    stub.description.to_ascii_lowercase()
                );
                let hits = terms
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();
                if hits > 0 { Some((stub, hits)) } else { None }
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored
            .into_iter()
            .take(max_results)
            .map(|(s, _)| s)
            .collect()
    }

    /// Activate a stub by name, returning a boxed [`Tool`].
    pub fn activate(&self, name: &str) -> Option<Box<dyn Tool>> {
        self.get_by_name(name).map(|stub| {
            let wrapper = stub.activate(Arc::clone(&self.registry));
            Box::new(wrapper) as Box<dyn Tool>
        })
    }

    /// Return the full [`ToolSpec`] for a stub (for inclusion in `tool_search` results).
    pub fn tool_spec(&self, name: &str) -> Option<ToolSpec> {
        self.get_by_name(name).map(|stub| {
            let wrapper = stub.activate(Arc::clone(&self.registry));
            wrapper.spec()
        })
    }
}

// ── ActivatedToolSet ─────────────────────────────────────────────────────

/// Per-conversation mutable state tracking which deferred tools have been
/// activated (i.e. their full schemas have been fetched via `tool_search`).
/// The agent loop consults this each iteration to decide which tool_specs
/// to include in the LLM request.
pub struct ActivatedToolSet {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ActivatedToolSet {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn activate(&mut self, name: String, tool: Arc<dyn Tool>) {
        self.tools.insert(name, tool);
    }

    pub fn is_activated(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Clone the Arc so the caller can drop the mutex guard before awaiting.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Resolve an activated tool by exact name first, then by unique MCP suffix.
    ///
    /// Some providers occasionally strip the `<server>__` prefix when calling a
    /// deferred MCP tool after `tool_search` activation. When the suffix maps to
    /// exactly one activated tool, allow that call to proceed.
    pub fn get_resolved(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(tool) = self.get(name) {
            return Some(tool);
        }
        if name.contains("__") {
            return None;
        }

        let mut resolved = None;
        for (tool_name, tool) in &self.tools {
            let Some((_, suffix)) = tool_name.split_once("__") else {
                continue;
            };
            if suffix != name {
                continue;
            }
            if resolved.is_some() {
                return None;
            }
            resolved = Some(Arc::clone(tool));
        }

        resolved
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for ActivatedToolSet {
    fn default() -> Self {
        Self::new()
    }
}

// ── System prompt helper ─────────────────────────────────────────────────

/// Build the `<available-deferred-tools>` section for the system prompt.
/// Lists only tool names so the LLM knows what is available without
/// consuming context window on full schemas. Includes an instruction
/// block that tells the LLM to call `tool_search` to activate them.
pub fn build_deferred_tools_section(deferred: &DeferredMcpToolSet) -> String {
    if deferred.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("## Deferred Tools\n\n");
    out.push_str(
        "The tools listed below are available but NOT yet loaded. \
         To use any of them you MUST first call the `tool_search` tool \
         to fetch their full schemas. Use `\"select:name1,name2\"` for \
         exact tools or keywords to search. Once activated, the tools \
         become callable for the rest of the conversation.\n\n",
    );
    out.push_str("<available-deferred-tools>\n");
    for stub in &deferred.stubs {
        out.push_str(&stub.prefixed_name);
        out.push_str(" - ");
        out.push_str(&stub.description);
        out.push('\n');
    }
    out.push_str("</available-deferred-tools>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stub(name: &str, desc: &str) -> DeferredMcpToolStub {
        let def = McpToolDef {
            name: name.to_string(),
            description: Some(desc.to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        DeferredMcpToolStub::new(name.to_string(), def)
    }

    #[test]
    fn stub_uses_description_from_def() {
        let stub = make_stub("fs__read", "Read a file");
        assert_eq!(stub.description, "Read a file");
    }

    #[test]
    fn stub_defaults_description_when_none() {
        let def = McpToolDef {
            name: "mystery".into(),
            description: None,
            input_schema: serde_json::json!({}),
        };
        let stub = DeferredMcpToolStub::new("srv__mystery".into(), def);
        assert_eq!(stub.description, "MCP tool");
    }

    #[test]
    fn activated_set_tracks_activation() {
        use async_trait::async_trait;
        use zeroclaw_api::tool::ToolResult;

        struct FakeTool;
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                "fake"
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        assert!(!set.is_activated("fake"));
        set.activate("fake".into(), Arc::new(FakeTool));
        assert!(set.is_activated("fake"));
        assert!(set.get("fake").is_some());
        assert_eq!(set.tool_specs().len(), 1);
    }

    #[test]
    fn activated_set_resolves_unique_suffix() {
        use async_trait::async_trait;
        use zeroclaw_api::tool::ToolResult;

        struct FakeTool;
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                "docker-mcp__extract_text"
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        set.activate("docker-mcp__extract_text".into(), Arc::new(FakeTool));
        assert!(set.get_resolved("extract_text").is_some());
    }

    #[test]
    fn activated_set_rejects_ambiguous_suffix() {
        use async_trait::async_trait;
        use zeroclaw_api::tool::ToolResult;

        struct FakeTool(&'static str);
        #[async_trait]
        impl Tool for FakeTool {
            fn name(&self) -> &str {
                self.0
            }
            fn description(&self) -> &str {
                "fake tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({})
            }
            async fn execute(&self, _: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: String::new(),
                    error: None,
                })
            }
        }

        let mut set = ActivatedToolSet::new();
        set.activate(
            "docker-mcp__extract_text".into(),
            Arc::new(FakeTool("docker-mcp__extract_text")),
        );
        set.activate(
            "ocr-mcp__extract_text".into(),
            Arc::new(FakeTool("ocr-mcp__extract_text")),
        );
        assert!(set.get_resolved("extract_text").is_none());
    }

    #[test]
    fn build_deferred_section_empty_when_no_stubs() {
        let set = DeferredMcpToolSet {
            stubs: vec![],
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };
        assert!(build_deferred_tools_section(&set).is_empty());
    }

    #[test]
    fn build_deferred_section_lists_names() {
        let stubs = vec![
            make_stub("fs__read_file", "Read a file"),
            make_stub("git__status", "Git status"),
        ];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };
        let section = build_deferred_tools_section(&set);
        assert!(section.contains("<available-deferred-tools>"));
        assert!(section.contains("fs__read_file - Read a file"));
        assert!(section.contains("git__status - Git status"));
        assert!(section.contains("</available-deferred-tools>"));
    }

    #[test]
    fn build_deferred_section_includes_tool_search_instruction() {
        let stubs = vec![make_stub("fs__read_file", "Read a file")];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };
        let section = build_deferred_tools_section(&set);
        assert!(
            section.contains("tool_search"),
            "deferred section must instruct the LLM to use tool_search"
        );
        assert!(
            section.contains("## Deferred Tools"),
            "deferred section must include a heading"
        );
    }

    #[test]
    fn build_deferred_section_multiple_servers() {
        let stubs = vec![
            make_stub("server_a__list", "List items"),
            make_stub("server_a__create", "Create item"),
            make_stub("server_b__query", "Query records"),
        ];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };
        let section = build_deferred_tools_section(&set);
        assert!(section.contains("server_a__list"));
        assert!(section.contains("server_a__create"));
        assert!(section.contains("server_b__query"));
        assert!(
            section.contains("tool_search"),
            "section must mention tool_search for multi-server setups"
        );
    }

    #[test]
    fn keyword_search_ranks_by_hits() {
        let stubs = vec![
            make_stub("fs__read_file", "Read a file from disk"),
            make_stub("fs__write_file", "Write a file to disk"),
            make_stub("git__log", "Show git log"),
        ];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };

        // "file read" should rank fs__read_file highest (2 hits vs 1)
        let results = set.search("file read", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].prefixed_name, "fs__read_file");
    }

    #[test]
    fn get_by_name_returns_correct_stub() {
        let stubs = vec![
            make_stub("a__one", "Tool one"),
            make_stub("b__two", "Tool two"),
        ];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };
        assert!(set.get_by_name("a__one").is_some());
        assert!(set.get_by_name("nonexistent").is_none());
    }

    #[test]
    fn search_across_multiple_servers() {
        let stubs = vec![
            make_stub("server_a__read_file", "Read a file from disk"),
            make_stub("server_b__read_config", "Read configuration from database"),
        ];
        let set = DeferredMcpToolSet {
            stubs,
            registry: std::sync::Arc::new(
                tokio::runtime::Runtime::new()
                    .unwrap()
                    .block_on(McpRegistry::connect_all(&[]))
                    .unwrap(),
            ),
        };

        // "read" should match stubs from both servers
        let results = set.search("read", 10);
        assert_eq!(results.len(), 2);

        // "file" should match only server_a
        let results = set.search("file", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].prefixed_name, "server_a__read_file");

        // "config database" should rank server_b highest (2 hits)
        let results = set.search("config database", 10);
        assert!(!results.is_empty());
        assert_eq!(results[0].prefixed_name, "server_b__read_config");
    }
}
