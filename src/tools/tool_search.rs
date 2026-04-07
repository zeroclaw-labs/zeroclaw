//! Built-in `tool_search` tool for discovering available tools.
//!
//! Searches both built-in tools (always available) and deferred MCP tools
//! (when `mcp.deferred_loading` is enabled). Supports two query modes:
//! - `select:name1,name2` — fetch exact tools by name.
//! - Free-text keyword search — returns the best-matching tools.

use std::fmt::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::tools::mcp_deferred::{ActivatedToolSet, DeferredMcpToolSet};
use crate::tools::traits::{Tool, ToolResult, ToolSpec};

/// Default maximum number of search results.
const DEFAULT_MAX_RESULTS: usize = 5;

/// Tool name constant used for registration checks.
pub const TOOL_NAME: &str = "tool_search";

/// Append a `<function>` XML tag for the given spec to `buf`.
fn write_function_tag(buf: &mut String, spec: &ToolSpec) {
    let _ = writeln!(
        buf,
        "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
        spec.name,
        spec.description.replace('"', "\\\""),
        spec.parameters
    );
}

/// Ensure a `tool_search` tool is present in `tools`.
///
/// If one was already registered (e.g. via MCP deferred loading), this is a
/// no-op. Otherwise a builtin-only instance is appended so the LLM can always
/// discover registered tools by keyword.
pub fn ensure_registered(tools: &mut Vec<Box<dyn Tool>>) {
    if tools.iter().any(|t| t.name() == TOOL_NAME) {
        return;
    }
    let specs = tools.iter().map(|t| t.spec()).collect();
    tools.push(Box::new(ToolSearchTool::builtin_only(specs)));
}

/// Built-in tool that discovers available tools by keyword search or exact name.
///
/// Searches both the built-in tool registry and deferred MCP tools (if configured).
pub struct ToolSearchTool {
    deferred: Option<DeferredMcpToolSet>,
    activated: Option<Arc<Mutex<ActivatedToolSet>>>,
    builtin_specs: Vec<ToolSpec>,
    /// Pre-lowercased `"name description"` per spec, for keyword matching.
    builtin_haystacks: Vec<String>,
}

impl ToolSearchTool {
    /// Create with both deferred MCP tools and built-in specs.
    pub fn new(
        deferred: DeferredMcpToolSet,
        activated: Arc<Mutex<ActivatedToolSet>>,
        builtin_specs: Vec<ToolSpec>,
    ) -> Self {
        let builtin_haystacks = build_haystacks(&builtin_specs);
        Self {
            deferred: Some(deferred),
            activated: Some(activated),
            builtin_specs,
            builtin_haystacks,
        }
    }

    /// Create with only built-in tool specs (no MCP deferred tools).
    pub fn builtin_only(builtin_specs: Vec<ToolSpec>) -> Self {
        let builtin_haystacks = build_haystacks(&builtin_specs);
        Self {
            deferred: None,
            activated: None,
            builtin_specs,
            builtin_haystacks,
        }
    }
}

fn build_haystacks(specs: &[ToolSpec]) -> Vec<String> {
    specs
        .iter()
        .map(|s| {
            format!(
                "{} {}",
                s.name.to_ascii_lowercase(),
                s.description.to_ascii_lowercase()
            )
        })
        .collect()
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        "Discover available tools by keyword or fetch exact tool schemas by name. \
         Searches both built-in tools and deferred MCP tools. \
         Use \"select:name1,name2\" for exact match or keywords to search."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "description": "Query to find tools. Use \"select:<tool_name>\" for direct selection, or keywords to search (e.g. \"git\", \"web search\", \"image\").",
                    "type": "string"
                },
                "max_results": {
                    "description": "Maximum number of results to return (default: 5)",
                    "type": "number",
                    "default": DEFAULT_MAX_RESULTS
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim();

        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| usize::try_from(v).unwrap_or(DEFAULT_MAX_RESULTS))
            .unwrap_or(DEFAULT_MAX_RESULTS);

        if query.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("query parameter is required".into()),
            });
        }

        if let Some(names_str) = query.strip_prefix("select:") {
            let names: Vec<&str> = names_str.split(',').map(str::trim).collect();
            return self.select_tools(&names);
        }

        let mut output = String::from("<functions>\n");
        let mut total_matched = 0usize;
        let mut activated_count = 0usize;
        let mut seen_names = std::collections::HashSet::new();

        // Built-in tools first — they're always available, no activation needed.
        let builtin_matches = self.search_builtins(query, max_results);
        for spec in &builtin_matches {
            seen_names.insert(spec.name.as_str());
            write_function_tag(&mut output, spec);
            total_matched += 1;
        }

        // Fill remaining slots from deferred MCP tools.
        if let (Some(deferred), Some(activated)) = (&self.deferred, &self.activated) {
            let remaining = max_results.saturating_sub(total_matched);
            if remaining > 0 {
                let deferred_results = deferred.search(query, remaining);
                let mut guard = activated.lock().unwrap();
                for stub in &deferred_results {
                    if seen_names.contains(stub.prefixed_name.as_str()) {
                        continue;
                    }
                    if let Some(spec) = deferred.tool_spec(&stub.prefixed_name) {
                        if !guard.is_activated(&stub.prefixed_name) {
                            if let Some(tool) = deferred.activate(&stub.prefixed_name) {
                                guard.activate(stub.prefixed_name.clone(), Arc::from(tool));
                                activated_count += 1;
                            }
                        }
                        write_function_tag(&mut output, &spec);
                        total_matched += 1;
                    }
                }
            }
        }

        output.push_str("</functions>\n");

        if total_matched == 0 {
            return Ok(ToolResult {
                success: true,
                output: "No matching tools found.".into(),
                error: None,
            });
        }

        tracing::debug!(
            "tool_search: query={query:?}, matched={total_matched}, activated={activated_count}",
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

impl ToolSearchTool {
    fn search_builtins(&self, query: &str, max_results: usize) -> Vec<&ToolSpec> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if terms.is_empty() {
            return self.builtin_specs.iter().take(max_results).collect();
        }
        let mut scored: Vec<(usize, usize)> = self
            .builtin_haystacks
            .iter()
            .enumerate()
            .filter_map(|(i, haystack)| {
                let hits = terms
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();
                if hits > 0 { Some((i, hits)) } else { None }
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored
            .into_iter()
            .take(max_results)
            .map(|(i, _)| &self.builtin_specs[i])
            .collect()
    }

    fn select_tools(&self, names: &[&str]) -> anyhow::Result<ToolResult> {
        let mut output = String::from("<functions>\n");
        let mut not_found = Vec::new();
        let mut activated_count = 0;

        for name in names {
            if name.is_empty() {
                continue;
            }

            if let Some(spec) = self.builtin_specs.iter().find(|s| s.name == *name) {
                write_function_tag(&mut output, spec);
                continue;
            }

            if let (Some(deferred), Some(activated)) = (&self.deferred, &self.activated) {
                if let Some(spec) = deferred.tool_spec(name) {
                    let mut guard = activated.lock().unwrap();
                    if !guard.is_activated(name) {
                        if let Some(tool) = deferred.activate(name) {
                            guard.activate(String::from(*name), Arc::from(tool));
                            activated_count += 1;
                        }
                    }
                    write_function_tag(&mut output, &spec);
                    continue;
                }
            }

            not_found.push(*name);
        }

        output.push_str("</functions>\n");

        if !not_found.is_empty() {
            let _ = write!(output, "\nNot found: {}", not_found.join(", "));
        }

        tracing::debug!(
            "tool_search select: requested={}, activated={activated_count}, not_found={}",
            names.len(),
            not_found.len()
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp_client::McpRegistry;
    use crate::tools::mcp_deferred::DeferredMcpToolStub;
    use crate::tools::mcp_protocol::McpToolDef;

    async fn make_deferred_set(stubs: Vec<DeferredMcpToolStub>) -> DeferredMcpToolSet {
        let registry = Arc::new(McpRegistry::connect_all(&[]).await.unwrap());
        DeferredMcpToolSet { stubs, registry }
    }

    fn make_stub(name: &str, desc: &str) -> DeferredMcpToolStub {
        let def = McpToolDef {
            name: name.to_string(),
            description: Some(desc.to_string()),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };
        DeferredMcpToolStub::new(name.to_string(), def)
    }

    fn make_builtin_spec(name: &str, desc: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: desc.to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    #[tokio::test]
    async fn tool_metadata() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
            vec![],
        );
        assert_eq!(tool.name(), TOOL_NAME);
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn empty_query_returns_error() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
            vec![],
        );
        let result = tool
            .execute(serde_json::json!({"query": ""}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn select_nonexistent_tool_reports_not_found() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
            vec![],
        );
        let result = tool
            .execute(serde_json::json!({"query": "select:nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Not found"));
    }

    #[tokio::test]
    async fn keyword_search_no_matches() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("fs__read", "Read file")]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
            vec![make_builtin_spec("shell", "Execute commands")],
        );
        let result = tool
            .execute(serde_json::json!({"query": "zzzzz_nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No matching"));
    }

    #[tokio::test]
    async fn keyword_search_finds_deferred_match() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("fs__read", "Read a file from disk")]).await,
            Arc::clone(&activated),
            vec![],
        );
        let result = tool
            .execute(serde_json::json!({"query": "read file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("<function>"));
        assert!(result.output.contains("fs__read"));
        assert!(activated.lock().unwrap().is_activated("fs__read"));
    }

    #[tokio::test]
    async fn keyword_search_finds_builtin_match() {
        let tool = ToolSearchTool::builtin_only(vec![
            make_builtin_spec(
                "git_operations",
                "Git status, diff, commit, branch operations",
            ),
            make_builtin_spec("shell", "Execute terminal commands"),
            make_builtin_spec("weather", "Get current weather and forecast"),
        ]);
        let result = tool
            .execute(serde_json::json!({"query": "git commit"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("git_operations"));
        assert!(!result.output.contains("weather"));
    }

    #[tokio::test]
    async fn select_finds_builtin_tool() {
        let tool = ToolSearchTool::builtin_only(vec![make_builtin_spec(
            "http_request",
            "Make HTTP requests",
        )]);
        let result = tool
            .execute(serde_json::json!({"query": "select:http_request"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("http_request"));
        assert!(!result.output.contains("Not found"));
    }

    #[tokio::test]
    async fn mixed_search_returns_both_sources() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("mcp__read_file", "Read file via MCP")]).await,
            Arc::clone(&activated),
            vec![make_builtin_spec(
                "file_read",
                "Read file contents with line numbers",
            )],
        );
        let result = tool
            .execute(serde_json::json!({"query": "read file", "max_results": 10}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("file_read"));
        assert!(result.output.contains("mcp__read_file"));
    }

    #[tokio::test]
    async fn multiple_servers_stubs_all_searchable() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("server_a__list_files", "List files on server A"),
            make_stub("server_a__read_file", "Read file on server A"),
            make_stub("server_b__query_db", "Query database on server B"),
            make_stub("server_b__insert_row", "Insert row on server B"),
        ];
        let tool = ToolSearchTool::new(
            make_deferred_set(stubs).await,
            Arc::clone(&activated),
            vec![],
        );

        let result = tool
            .execute(serde_json::json!({"query": "file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_a__list_files"));
        assert!(result.output.contains("server_a__read_file"));

        let result = tool
            .execute(serde_json::json!({"query": "database query"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_b__query_db"));
    }

    #[tokio::test]
    async fn select_activates_and_persists_across_calls() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("srv__tool_a", "Tool A"),
            make_stub("srv__tool_b", "Tool B"),
        ];
        let tool = ToolSearchTool::new(
            make_deferred_set(stubs).await,
            Arc::clone(&activated),
            vec![],
        );

        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_a"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(activated.lock().unwrap().is_activated("srv__tool_a"));
        assert!(!activated.lock().unwrap().is_activated("srv__tool_b"));

        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_b"}))
            .await
            .unwrap();
        assert!(result.success);

        let guard = activated.lock().unwrap();
        assert!(guard.is_activated("srv__tool_a"));
        assert!(guard.is_activated("srv__tool_b"));
        assert_eq!(guard.tool_specs().len(), 2);
    }

    #[tokio::test]
    async fn reactivation_is_idempotent() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("srv__tool", "A tool")]).await,
            Arc::clone(&activated),
            vec![],
        );

        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();
        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();

        assert_eq!(activated.lock().unwrap().tool_specs().len(), 1);
    }

    #[tokio::test]
    async fn builtin_only_mode_works() {
        let tool = ToolSearchTool::builtin_only(vec![
            make_builtin_spec("calculator", "Basic arithmetic"),
            make_builtin_spec("weather", "Get weather forecast"),
        ]);
        let result = tool
            .execute(serde_json::json!({"query": "arithmetic"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("calculator"));
        assert!(!result.output.contains("weather"));
    }

    #[tokio::test]
    async fn ensure_registered_adds_when_missing() {
        let mut tools: Vec<Box<dyn Tool>> = vec![];
        ensure_registered(&mut tools);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), TOOL_NAME);
    }

    #[tokio::test]
    async fn ensure_registered_noop_when_present() {
        let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(ToolSearchTool::builtin_only(vec![]))];
        ensure_registered(&mut tools);
        assert_eq!(tools.len(), 1);
    }
}
