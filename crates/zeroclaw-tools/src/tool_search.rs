//! Built-in `tool_search` tool for on-demand MCP tool schema loading.
//!
//! When `mcp.deferred_loading` is enabled, this tool lets the LLM discover and
//! activate deferred MCP tools. Supports two query modes:
//! - `select:name1,name2` — fetch exact tools by prefixed name.
//! - Free-text keyword search — returns the best-matching stubs.

use std::fmt::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::mcp_deferred::{ActivatedToolSet, DeferredMcpToolSet};
use zeroclaw_api::tool::{Tool, ToolResult};

/// Default maximum number of search results.
const DEFAULT_MAX_RESULTS: usize = 5;

type ActivationHook = Arc<dyn Fn(Arc<dyn Tool>) + Send + Sync>;

/// Tool-level access policy applied at discovery time.
///
/// When set on `ToolSearchTool`, deferred tools that fail this check are
/// never surfaced to the LLM and never activated — keeping them out of
/// the context window entirely.
///
/// The policy carries two independent allow-list gates that are AND-ed
/// together, plus a single deny-list:
///
/// - `allowed`: the agent's risk-profile allow-list. The MCP
///   `<server>__<tool>` auto-admit exception (any name containing `__`
///   passes when the list is non-empty) applies **only** to this gate.
///   This is the high-risk default-accept-unless-denied shift introduced
///   in PR #7547 so that the post-#7464 `mcp.enabled = true` default
///   actually surfaces discovered MCP tools to agents.
/// - `caller_allowed`: a caller-supplied per-run allow-list (cron job
///   `allowed_tools`, narrowed delegate invocations, etc.). This is a
///   strict explicit-list intersection — there is **no** MCP auto-admit
///   on this gate. PR #7547 review (Audacity88, singlerider) called out
///   that collapsing this list into `allowed` made per-run narrowing
///   stop working as a capability boundary the moment an MCP server was
///   configured.
/// - `denied`: subtracts from the final set. Applies to both gates and
///   to auto-admitted MCP names.
#[derive(Clone, Default)]
pub struct ToolAccessPolicy {
    pub allowed: Option<Vec<String>>,
    pub caller_allowed: Option<Vec<String>>,
    pub denied: Option<Vec<String>>,
}

impl ToolAccessPolicy {
    /// Construct from a `SecurityPolicy`'s tool fields and an optional
    /// caller-supplied allowlist. Used by both `run()` and
    /// `process_message()` to keep policy construction in sync.
    ///
    /// The risk-profile `allowed_tools` and the caller-supplied
    /// `caller_allowed` are kept as two separate gates inside the
    /// returned policy. Per PR #7547 review, this is required so the
    /// MCP `<server>__<tool>` auto-admit exception that applies to the
    /// risk-profile gate does **not** silently widen narrower per-run
    /// allow-lists.
    pub fn from_security(
        allowed_tools: Option<&[String]>,
        excluded_tools: Option<&[String]>,
        caller_allowed: Option<&[String]>,
    ) -> Option<Self> {
        let mut policy = Self::default();
        if let Some(list) = allowed_tools {
            policy.allowed = Some(list.to_vec());
        }
        if let Some(caller) = caller_allowed {
            policy.caller_allowed = Some(caller.to_vec());
        }
        if let Some(list) = excluded_tools {
            policy.denied = Some(list.to_vec());
        }
        if policy.allowed.is_some() || policy.caller_allowed.is_some() || policy.denied.is_some() {
            Some(policy)
        } else {
            None
        }
    }

    pub fn is_tool_allowed(&self, name: &str) -> bool {
        // Deny-list always wins.
        let in_deny = self
            .denied
            .as_ref()
            .is_some_and(|list| list.iter().any(|t| t == name));
        if in_deny {
            return false;
        }

        // Risk-profile gate: MCP `<server>__<tool>` names are auto-admitted
        // when the list is non-empty. An explicit empty list (`Some(vec![])`)
        // still means "deny everything".
        let risk_ok = match self.allowed.as_ref() {
            None => true,
            Some(list) if list.is_empty() => false,
            Some(list) => list.iter().any(|t| t == name) || name.contains("__"),
        };
        if !risk_ok {
            return false;
        }

        // Caller-supplied per-run gate: strict explicit-list intersection.
        // No MCP auto-admit here — per PR #7547 review, that exception is
        // scoped to the risk-profile gate so per-run narrowing (cron jobs,
        // narrowed delegate invocations) remains a reliable capability
        // boundary even when an MCP server is configured.
        match self.caller_allowed.as_ref() {
            None => true,
            Some(list) => list.iter().any(|t| t == name),
        }
    }
}

/// Built-in tool that fetches full schemas for deferred MCP tools.
pub struct ToolSearchTool {
    deferred: DeferredMcpToolSet,
    activated: Arc<Mutex<ActivatedToolSet>>,
    access_policy: Option<ToolAccessPolicy>,
    activation_hook: Option<ActivationHook>,
}

impl ToolSearchTool {
    pub fn new(deferred: DeferredMcpToolSet, activated: Arc<Mutex<ActivatedToolSet>>) -> Self {
        Self {
            deferred,
            activated,
            access_policy: None,
            activation_hook: None,
        }
    }

    pub fn with_access_policy(mut self, policy: ToolAccessPolicy) -> Self {
        self.access_policy = Some(policy);
        self
    }

    pub fn with_activation_hook(mut self, hook: ActivationHook) -> Self {
        self.activation_hook = Some(hook);
        self
    }

    fn is_allowed(&self, tool_name: &str) -> bool {
        self.access_policy
            .as_ref()
            .is_none_or(|p| p.is_tool_allowed(tool_name))
    }

    fn notify_activated(&self, tools: Vec<Arc<dyn Tool>>) {
        if let Some(hook) = &self.activation_hook {
            for tool in tools {
                hook(tool);
            }
        }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Fetch full schema definitions for deferred MCP tools so they can be called. \
         Use \"select:name1,name2\" for exact match or keywords to search."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "description": "Query to find deferred tools. Use \"select:<tool_name>\" for direct selection, or keywords to search.",
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

        // Parse query mode
        if let Some(names_str) = query.strip_prefix("select:") {
            // Exact selection mode
            let names: Vec<&str> = names_str.split(',').map(str::trim).collect();
            return self.select_tools(&names);
        }

        // Keyword search mode.
        // When a policy is active, fetch all matches so denied tools don't
        // consume result slots. The max_results cap is applied after filtering.
        let search_limit = if self.access_policy.is_some() {
            usize::MAX
        } else {
            max_results
        };
        let results = self.deferred.search(query, search_limit);
        if results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No matching deferred tools found.".into(),
                error: None,
            });
        }

        // Activate and return full specs (policy-filtered, then capped)
        let mut output = String::from("<functions>\n");
        let mut activated_count = 0;
        let mut returned_count = 0;
        let mut newly_activated = Vec::new();
        let mut guard = self.activated.lock().unwrap();

        for stub in &results {
            if returned_count >= max_results {
                break;
            }
            if !self.is_allowed(&stub.prefixed_name) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "tool_search: '{}' matched query but denied by access policy",
                        stub.prefixed_name
                    )
                );
                continue;
            }
            if let Some(spec) = self.deferred.tool_spec(&stub.prefixed_name) {
                if !guard.is_activated(&stub.prefixed_name)
                    && let Some(tool) = self.deferred.activate(&stub.prefixed_name)
                {
                    let tool: Arc<dyn Tool> = Arc::from(tool);
                    guard.activate(stub.prefixed_name.clone(), Arc::clone(&tool));
                    newly_activated.push(tool);
                    activated_count += 1;
                }
                let _ = writeln!(
                    output,
                    "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
                    spec.name,
                    spec.description.replace('"', "\\\""),
                    spec.parameters
                );
                returned_count += 1;
            }
        }

        output.push_str("</functions>\n");
        drop(guard);
        self.notify_activated(newly_activated);

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "tool_search: query={query:?}, matched={}, activated={activated_count}",
                results.len()
            )
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

impl ToolSearchTool {
    fn select_tools(&self, names: &[&str]) -> anyhow::Result<ToolResult> {
        let mut output = String::from("<functions>\n");
        let mut not_found = Vec::new();
        let mut activated_count = 0;
        let mut newly_activated = Vec::new();
        let mut guard = self.activated.lock().unwrap();

        for name in names {
            if name.is_empty() {
                continue;
            }
            if !self.is_allowed(name) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("tool_search select: '{}' denied by access policy", name)
                );
                not_found.push(*name);
                continue;
            }
            match self.deferred.tool_spec(name) {
                Some(spec) => {
                    if !guard.is_activated(name)
                        && let Some(tool) = self.deferred.activate(name)
                    {
                        let tool: Arc<dyn Tool> = Arc::from(tool);
                        guard.activate(String::from(*name), Arc::clone(&tool));
                        newly_activated.push(tool);
                        activated_count += 1;
                    }
                    let _ = writeln!(
                        output,
                        "<function>{{\"name\": \"{}\", \"description\": \"{}\", \"parameters\": {}}}</function>",
                        spec.name,
                        spec.description.replace('"', "\\\""),
                        spec.parameters
                    );
                }
                None => {
                    not_found.push(*name);
                }
            }
        }

        output.push_str("</functions>\n");
        drop(guard);
        self.notify_activated(newly_activated);

        if !not_found.is_empty() {
            let _ = write!(output, "\nNot found: {}", not_found.join(", "));
        }

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "tool_search select: requested={}, activated={activated_count}, not_found={}",
                names.len(),
                not_found.len()
            )
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
    use crate::mcp_client::McpRegistry;
    use crate::mcp_deferred::DeferredMcpToolStub;
    use crate::mcp_protocol::McpToolDef;

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

    #[tokio::test]
    async fn tool_metadata() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
        );
        assert_eq!(tool.name(), "tool_search");
        assert!(!tool.description().is_empty());
        assert!(tool.parameters_schema()["properties"]["query"].is_object());
    }

    #[tokio::test]
    async fn empty_query_returns_error() {
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![]).await,
            Arc::new(Mutex::new(ActivatedToolSet::new())),
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
        );
        let result = tool
            .execute(serde_json::json!({"query": "zzzzz_nonexistent"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No matching"));
    }

    #[tokio::test]
    async fn keyword_search_finds_match() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("fs__read", "Read a file from disk")]).await,
            Arc::clone(&activated),
        );
        let result = tool
            .execute(serde_json::json!({"query": "read file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("<function>"));
        assert!(result.output.contains("fs__read"));
        // Tool should now be activated
        assert!(activated.lock().unwrap().is_activated("fs__read"));
    }

    /// Verify tool_search works with stubs from multiple MCP servers,
    /// simulating a daemon-mode setup where several servers are deferred.
    #[tokio::test]
    async fn multiple_servers_stubs_all_searchable() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("server_a__list_files", "List files on server A"),
            make_stub("server_a__read_file", "Read file on server A"),
            make_stub("server_b__query_db", "Query database on server B"),
            make_stub("server_b__insert_row", "Insert row on server B"),
        ];
        let tool = ToolSearchTool::new(make_deferred_set(stubs).await, Arc::clone(&activated));

        // Search should find tools across both servers
        let result = tool
            .execute(serde_json::json!({"query": "file"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_a__list_files"));
        assert!(result.output.contains("server_a__read_file"));

        // Server B tools should also be searchable
        let result = tool
            .execute(serde_json::json!({"query": "database query"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("server_b__query_db"));
    }

    /// Verify select mode activates tools and they stay activated across calls,
    /// matching the daemon-mode pattern where a single ActivatedToolSet persists.
    #[tokio::test]
    async fn select_activates_and_persists_across_calls() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("srv__tool_a", "Tool A"),
            make_stub("srv__tool_b", "Tool B"),
        ];
        let tool = ToolSearchTool::new(make_deferred_set(stubs).await, Arc::clone(&activated));

        // Activate tool_a
        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_a"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(activated.lock().unwrap().is_activated("srv__tool_a"));
        assert!(!activated.lock().unwrap().is_activated("srv__tool_b"));

        // Activate tool_b in a separate call
        let result = tool
            .execute(serde_json::json!({"query": "select:srv__tool_b"}))
            .await
            .unwrap();
        assert!(result.success);

        // Both should remain activated
        let guard = activated.lock().unwrap();
        assert!(guard.is_activated("srv__tool_a"));
        assert!(guard.is_activated("srv__tool_b"));
        assert_eq!(guard.tool_specs().len(), 2);
    }

    /// Verify re-activating an already-activated tool does not duplicate it.
    #[tokio::test]
    async fn reactivation_is_idempotent() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("srv__tool", "A tool")]).await,
            Arc::clone(&activated),
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
    async fn activation_hook_receives_newly_activated_tools_once() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_hook = Arc::clone(&seen);
        let tool = ToolSearchTool::new(
            make_deferred_set(vec![make_stub("srv__tool", "A tool")]).await,
            Arc::clone(&activated),
        )
        .with_activation_hook(Arc::new(move |tool| {
            seen_hook.lock().unwrap().push(tool.name().to_string());
        }));

        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();
        tool.execute(serde_json::json!({"query": "select:srv__tool"}))
            .await
            .unwrap();

        assert_eq!(seen.lock().unwrap().as_slice(), ["srv__tool"]);
    }

    #[test]
    fn policy_none_is_unrestricted() {
        let p = ToolAccessPolicy::default();
        assert!(p.is_tool_allowed("shell"));
        assert!(p.is_tool_allowed("anything"));
    }

    #[test]
    fn policy_allowlist_admits_only_listed() {
        let p = ToolAccessPolicy {
            allowed: Some(vec!["shell".into(), "file_read".into()]),
            ..ToolAccessPolicy::default()
        };
        assert!(p.is_tool_allowed("shell"));
        assert!(!p.is_tool_allowed("file_write"));
    }

    #[test]
    fn policy_denylist_rejects_listed() {
        let p = ToolAccessPolicy {
            denied: Some(vec!["shell".into()]),
            ..ToolAccessPolicy::default()
        };
        assert!(!p.is_tool_allowed("shell"));
        assert!(p.is_tool_allowed("file_read"));
    }

    #[test]
    fn policy_deny_overrides_allow() {
        let p = ToolAccessPolicy {
            allowed: Some(vec!["shell".into(), "file_read".into()]),
            denied: Some(vec!["shell".into()]),
            ..ToolAccessPolicy::default()
        };
        assert!(!p.is_tool_allowed("shell"));
        assert!(p.is_tool_allowed("file_read"));
    }

    #[tokio::test]
    async fn policy_filters_keyword_search_results() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("srv__allowed_tool", "An allowed tool"),
            make_stub("srv__blocked_tool", "A blocked tool"),
        ];
        let policy = ToolAccessPolicy {
            denied: Some(vec!["srv__blocked_tool".into()]),
            ..ToolAccessPolicy::default()
        };
        let tool = ToolSearchTool::new(make_deferred_set(stubs).await, Arc::clone(&activated))
            .with_access_policy(policy);

        let result = tool
            .execute(serde_json::json!({"query": "tool"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("srv__allowed_tool"));
        assert!(!result.output.contains("srv__blocked_tool"));
        assert!(!activated.lock().unwrap().is_activated("srv__blocked_tool"));
    }

    #[tokio::test]
    async fn policy_denied_tool_does_not_consume_max_results_slot() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        // "denied_tool" ranks higher (more keyword matches) but is blocked.
        // "allowed_tool" ranks lower but should still be returned with max_results=1.
        let stubs = vec![
            make_stub("srv__denied_tool", "tool for searching files"),
            make_stub("srv__allowed_tool", "tool for files"),
        ];
        let policy = ToolAccessPolicy {
            denied: Some(vec!["srv__denied_tool".into()]),
            ..ToolAccessPolicy::default()
        };
        let tool = ToolSearchTool::new(make_deferred_set(stubs).await, Arc::clone(&activated))
            .with_access_policy(policy);

        let result = tool
            .execute(serde_json::json!({"query": "searching files", "max_results": 1}))
            .await
            .unwrap();
        assert!(result.success);
        // The allowed tool should be returned even though max_results=1
        // and the denied tool ranked higher.
        assert!(result.output.contains("srv__allowed_tool"));
        assert!(!result.output.contains("srv__denied_tool"));
        assert!(activated.lock().unwrap().is_activated("srv__allowed_tool"));
    }

    #[tokio::test]
    async fn policy_filters_select_results() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let stubs = vec![
            make_stub("srv__ok", "OK tool"),
            make_stub("srv__nope", "Blocked tool"),
        ];
        // Runtime-discovered MCP tools (names containing "__") are auto-admitted
        // when an allow-list is present, so the operator-visible way to block a
        // specific MCP tool is the deny-list (the `excluded_tools` equivalent).
        // See `ToolAccessPolicy::is_tool_allowed` and PR #7547.
        let policy = ToolAccessPolicy {
            allowed: Some(vec!["srv__ok".into()]),
            denied: Some(vec!["srv__nope".into()]),
            ..ToolAccessPolicy::default()
        };
        let tool = ToolSearchTool::new(make_deferred_set(stubs).await, Arc::clone(&activated))
            .with_access_policy(policy);

        let result = tool
            .execute(serde_json::json!({"query": "select:srv__ok,srv__nope"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("srv__ok"));
        assert!(!result.output.contains("\"name\": \"srv__nope\""));
        assert!(result.output.contains("Not found"));
        assert!(!activated.lock().unwrap().is_activated("srv__nope"));
    }

    /// PR #7547 review (Audacity88 / singlerider) — second-round blocking:
    /// the MCP `<server>__<tool>` auto-admit exception must apply ONLY to
    /// the risk-profile allow-list, not to the caller-supplied per-run
    /// `allowed_tools`. Otherwise a cron job that narrows
    /// `allowed_tools = ["cron_add"]` would still surface every
    /// runtime-discovered MCP wrapper, breaking per-job capability
    /// narrowing the moment an MCP server is configured.
    ///
    /// This test fixes `from_security` semantics so an MCP name the
    /// caller did not explicitly include is rejected even when the
    /// risk-profile allow-list would auto-admit it.
    #[test]
    fn caller_allowed_per_run_gate_does_not_auto_admit_mcp_names() {
        // The risk-profile gate is wide (unrestricted), so the MCP
        // auto-admit would happily pass any `__` name. The caller-supplied
        // per-run list narrows down to a single non-MCP tool (`cron_add`).
        let policy = ToolAccessPolicy::from_security(None, None, Some(&["cron_add".to_string()]))
            .expect("caller-supplied list should produce a policy");

        assert!(
            policy.is_tool_allowed("cron_add"),
            "cron_add must pass — it is in the caller list"
        );
        assert!(
            !policy.is_tool_allowed("filesystem__write_file"),
            "MCP wrapper outside the caller list must be rejected, but \
             was admitted — the per-run gate is leaking the risk-profile \
             MCP auto-admit exception (PR #7547 review regression)"
        );
        assert!(
            !policy.is_tool_allowed("github__search"),
            "second MCP wrapper outside the caller list must also be \
             rejected (PR #7547 review regression)"
        );
    }

    /// Companion to the test above: even when the risk profile DOES have
    /// a non-empty allow-list (so the auto-admit branch is live on that
    /// gate), the caller-supplied per-run list still narrows the final
    /// set strictly. The risk-profile auto-admit must not leak past the
    /// per-run gate.
    #[test]
    fn caller_allowed_per_run_gate_narrows_after_risk_profile_auto_admit() {
        let policy = ToolAccessPolicy::from_security(
            Some(&["shell".to_string()]),
            None,
            Some(&["shell".to_string(), "github__search".to_string()]),
        )
        .expect("risk + caller lists should produce a policy");

        // `shell`: in risk allow + in caller list → admitted.
        assert!(policy.is_tool_allowed("shell"));
        // `github__search`: auto-admitted by risk MCP exception + in caller
        // list → admitted.
        assert!(policy.is_tool_allowed("github__search"));
        // `filesystem__write_file`: auto-admitted by risk MCP exception
        // (would pass the risk gate) but NOT in caller list → rejected.
        // This is the per-run narrowing the bug used to break.
        assert!(
            !policy.is_tool_allowed("filesystem__write_file"),
            "MCP wrapper not in caller list must be rejected even when \
             the risk-profile auto-admit would let it through"
        );
        // Non-MCP outside both lists: rejected.
        assert!(!policy.is_tool_allowed("memory_recall"));
    }

    /// `excluded_tools` must subtract regardless of which gate admitted
    /// the name. Pins the deny-list contract across the refactor.
    #[test]
    fn caller_allowed_per_run_gate_still_honors_denylist() {
        let policy = ToolAccessPolicy::from_security(
            Some(&["shell".to_string()]),
            Some(&["filesystem__write_file".to_string()]),
            Some(&["shell".to_string(), "filesystem__write_file".to_string()]),
        )
        .expect("policy with all three fields should be constructed");

        assert!(policy.is_tool_allowed("shell"));
        assert!(
            !policy.is_tool_allowed("filesystem__write_file"),
            "denylist subtracts even when both gates would admit"
        );
    }
}
