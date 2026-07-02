//! `ScopedToolRegistry` - the one gated seam that mints the per-agent tool set.
//!
//! Epic A of the agent-policy enforcement-unification program. The per-agent tool
//! registry was assembled by hand at five construction sites (channels orchestrator,
//! runtime `run` / `process_message`, `Agent::from_config`, the gateway), and each
//! site re-applied the policy itself. That is why the built-in allow/deny filter and
//! the MCP scoping had to be patched repeatedly (#7064, #6960, #8120) and why the
//! gateway still leaked: a path that forgets a step silently widens the agent's
//! authority. `ScopedToolRegistry` makes that unrepresentable - its field is private
//! and the only constructor that mints one is [`ScopedToolRegistry::assemble`], which
//! always applies, in order: built-in `allowed_tools`/`excluded_tools` filtering,
//! per-agent MCP server scoping (`mcp_bundles`, omission is not a grant), MCP tool
//! gating, and skill registration under the same `SecurityPolicy`.
//!
//! Per-site variation is expressed as DATA, never as "skip a security step": the only
//! knobs are three documented divergences - a per-run caller allowlist that only
//! narrows, the ACP fast-boot that does not connect MCP, and the ACP memory-tool strip.
//! Peripherals, the policy filter, and skills run on every path (the gateway and
//! `from_config` omissions were latent bugs, now fixed by construction).

use std::sync::Arc;

use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

use crate::agent::loop_::{
    apply_policy_tool_filter, eager_mcp_tool_allowed, load_peripheral_tools, mcp_allowed_tool_count,
    mcp_tool_access_policy, register_eager_mcp_tool_if_allowed,
};
use crate::skills::Skill;
use crate::tools::{
    self, ActivatedToolSet, AllToolsResult, DelegateParentToolsHandle, PerToolChannelHandle, Tool,
    register_skill_tools_with_context_and_runtime,
};

/// A per-agent tool registry that has been scoped and gated. The inner field is
/// private; the engine accepts only a `ScopedToolRegistry`, so a construction path
/// physically cannot hand it an unfiltered registry - that is a compile error, not a
/// review checklist item.
pub struct ScopedToolRegistry(Vec<Box<dyn Tool>>);

impl std::ops::Deref for ScopedToolRegistry {
    type Target = [Box<dyn Tool>];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ScopedToolRegistry {
    /// Consume the assembled registry into the owned `Vec` (for the few callers that
    /// still pass `&[Box<dyn Tool>]` into the engine during the P1 cut-over).
    pub fn into_inner(self) -> Vec<Box<dyn Tool>> {
        self.0
    }

    /// Test-only escape hatch. Production code has no other way to build one.
    #[cfg(test)]
    pub fn from_raw_for_test(tools: Vec<Box<dyn Tool>>) -> Self {
        Self(tools)
    }
}

/// Inputs to [`ScopedToolRegistry::assemble`]. The eager built-ins arrive already
/// built (`built`); `assemble` does the policy-bearing steps the sites used to repeat.
pub struct ScopedAssembly<'a> {
    pub config: &'a Config,
    pub agent_alias: &'a str,
    pub security: &'a Arc<SecurityPolicy>,
    /// Eager built-in tools + the channel/delegate handle bundle, consumed here.
    pub built: AllToolsResult,
    /// Skills loaded by the caller's (single) loader; registered under the same gate.
    pub skills: &'a [Skill],
    pub runtime: Arc<dyn RuntimeAdapter>,
    /// Documented divergence: a per-run caller allowlist. It only NARROWS, and is
    /// threaded into BOTH the built-in filter and the MCP tool-access policy. `None`
    /// on every path except `run`.
    pub caller_allowed: Option<&'a [String]>,
    /// Documented divergence: ACP `session/new` must return promptly, so it skips
    /// connecting MCP servers (scoping is still computed; nothing is granted).
    pub connect_mcp: bool,
    /// Documented divergence: ACP excludes persistent memory tools.
    pub exclude_memory: bool,
}

/// Output of [`ScopedToolRegistry::assemble`]: the scoped registry plus the
/// side-channel handles + the deferred-MCP prompt section the callers thread on.
pub struct ScopedAssembled {
    pub registry: ScopedToolRegistry,
    pub delegate_handle: Option<DelegateParentToolsHandle>,
    pub ask_user_handle: Option<PerToolChannelHandle>,
    pub reaction_handle: PerToolChannelHandle,
    pub poll_handle: Option<PerToolChannelHandle>,
    pub escalate_handle: Option<PerToolChannelHandle>,
    pub channel_room_handle: Option<PerToolChannelHandle>,
    /// The deferred-MCP tool-search prompt section (empty when deferred loading is off
    /// or no MCP tools are granted). The caller injects this into its system prompt.
    pub deferred_section: String,
    /// Live handle to the activated deferred-MCP set (present only when a deferred
    /// `tool_search` tool was registered).
    pub activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>>,
}

impl ScopedToolRegistry {
    /// Mint a scoped, gated registry from already-built eager tools. The single seam
    /// every construction path goes through.
    pub async fn assemble(spec: ScopedAssembly<'_>) -> ScopedAssembled {
        let ScopedAssembly {
            config,
            agent_alias,
            security,
            built,
            skills,
            runtime,
            caller_allowed,
            connect_mcp,
            exclude_memory,
        } = spec;

        let AllToolsResult {
            tools: mut tools_registry,
            delegate_handle,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            unfiltered_tool_arcs,
        } = built;

        // 1. Peripherals (uniform: every path wires the agent's `config.peripherals`;
        //    the gateway + from_config used to skip this).
        let peripheral_tools = load_peripheral_tools(config.peripherals.clone()).await;
        tools_registry.extend(peripheral_tools);

        // 2. Built-in allow/deny filter (uniform: the gateway used to skip it entirely).
        //    `caller_allowed` narrows on top of the policy, for the `run` path only.
        apply_policy_tool_filter(&mut tools_registry, Some(security.as_ref()), caller_allowed);

        // 3. Documented divergence: ACP strips persistent memory tools.
        if exclude_memory {
            tools_registry.retain(|t| !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&t.name()));
        }

        // 4. MCP: scope servers per `mcp_bundles` (omission is not a grant), then gate
        //    each tool. Skipped only when this path does not connect MCP (ACP) or MCP
        //    is disabled - in both cases nothing is granted.
        let mut deferred_section = String::new();
        let mut activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>> = None;
        let mut mcp_elevation_arcs: Vec<Arc<dyn Tool>> = Vec::new();

        let agent_mcp_servers = if connect_mcp && config.mcp.enabled {
            config.mcp_servers_for_agent(agent_alias)
        } else {
            Vec::new()
        };
        if !agent_mcp_servers.is_empty() {
            match tools::McpRegistry::connect_all(&agent_mcp_servers).await {
                Ok(registry) => {
                    let registry = Arc::new(registry);
                    mcp_elevation_arcs = tools::collect_mcp_elevation_arcs(&registry).await;
                    let mcp_policy = mcp_tool_access_policy(security.as_ref(), caller_allowed);
                    if config.mcp.deferred_loading {
                        let deferred_set =
                            tools::DeferredMcpToolSet::from_registry(Arc::clone(&registry)).await;
                        let allowed_stub_count = mcp_allowed_tool_count(
                            deferred_set.stubs.iter().map(|stub| stub.prefixed_name.as_str()),
                            mcp_policy.as_ref(),
                        );
                        deferred_section = tools::build_deferred_tools_section_filtered(
                            &deferred_set,
                            mcp_policy.as_ref(),
                        );
                        if allowed_stub_count > 0 {
                            let activated =
                                Arc::new(std::sync::Mutex::new(ActivatedToolSet::new()));
                            activated_handle = Some(Arc::clone(&activated));
                            let mut tool_search =
                                tools::ToolSearchTool::new(deferred_set, activated);
                            if let Some(policy) = mcp_policy {
                                tool_search = tool_search.with_access_policy(policy);
                            }
                            // Newly-activated deferred tools are also exposed to the
                            // delegate parent set, matching the run/process_message paths.
                            if let Some(ref handle) = delegate_handle {
                                let delegate_tools = Arc::clone(handle);
                                tool_search =
                                    tool_search.with_activation_hook(Arc::new(move |tool| {
                                        let mut tools = delegate_tools.write();
                                        let already = tools
                                            .iter()
                                            .any(|existing| existing.name() == tool.name());
                                        if !already {
                                            tools.push(tool);
                                        }
                                    }));
                            }
                            tools_registry.push(Box::new(tool_search));
                        }
                    } else {
                        let names = registry.tool_names();
                        for name in names {
                            if !eager_mcp_tool_allowed(&name, mcp_policy.as_ref()) {
                                continue;
                            }
                            if let Some(def) = registry.get_tool_def(&name).await {
                                let wrapper: Arc<dyn Tool> = Arc::new(tools::McpToolWrapper::new(
                                    name,
                                    def,
                                    Arc::clone(&registry),
                                ));
                                register_eager_mcp_tool_if_allowed(
                                    wrapper,
                                    &mut tools_registry,
                                    delegate_handle.as_ref(),
                                    mcp_policy.as_ref(),
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                            .with_category(::zeroclaw_log::EventCategory::Tool),
                        &format!("MCP connect failed (non-fatal): {err}")
                    );
                }
            }
        }

        // 5. Skills (uniform: the gateway used to skip them). Registered under the same
        //    `SecurityPolicy`, resolving builtin/MCP elevation against the pre-filter arcs.
        let resolution_registry: Vec<Arc<dyn Tool>> = unfiltered_tool_arcs
            .iter()
            .cloned()
            .chain(mcp_elevation_arcs.iter().cloned())
            .collect();
        register_skill_tools_with_context_and_runtime(
            &mut tools_registry,
            skills,
            Arc::clone(security),
            &resolution_registry,
            runtime,
        );

        ScopedAssembled {
            registry: ScopedToolRegistry(tools_registry),
            delegate_handle,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            deferred_section,
            activated_handle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolResult;
    use async_trait::async_trait;

    struct MockTool(&'static str);

    impl zeroclaw_api::attribution::Attributable for MockTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            self.0
        }
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    fn built_with(tools: Vec<Box<dyn Tool>>) -> AllToolsResult {
        AllToolsResult {
            tools,
            delegate_handle: None,
            ask_user_handle: None,
            reaction_handle: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            poll_handle: None,
            escalate_handle: None,
            unfiltered_tool_arcs: Vec::new(),
        }
    }

    async fn assemble_names(
        security: Arc<SecurityPolicy>,
        tools: Vec<Box<dyn Tool>>,
        caller_allowed: Option<&[String]>,
    ) -> Vec<String> {
        let config = Config::default();
        let out = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with(tools),
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed,
            connect_mcp: false, // exercise the filter path without MCP fixtures
            exclude_memory: false,
        })
        .await;
        out.registry.iter().map(|t| t.name().to_string()).collect()
    }

    #[tokio::test]
    async fn assemble_applies_the_builtin_filter_uniformly() {
        // The gateway path historically SKIPPED the built-in allow/deny filter, leaking
        // excluded tools. Through the one seam the filter ALWAYS runs - the leak is fixed
        // by construction, not by remembering to call it.
        let security = Arc::new(SecurityPolicy {
            excluded_tools: Some(vec!["spawn_subagent".into()]),
            ..SecurityPolicy::default()
        });
        let names = assemble_names(
            security,
            vec![Box::new(MockTool("shell")), Box::new(MockTool("spawn_subagent"))],
            None,
        )
        .await;
        assert!(names.iter().any(|n| n == "shell"), "unlisted tool kept: {names:?}");
        assert!(
            !names.iter().any(|n| n == "spawn_subagent"),
            "excluded tool dropped: {names:?}"
        );
    }

    #[tokio::test]
    async fn assemble_threads_caller_allowed_narrowing() {
        // The documented per-run caller allowlist (run() path) narrows further, and is
        // honored through the seam like every other path that narrows.
        let allow = vec!["shell".to_string()];
        let names = assemble_names(
            Arc::new(SecurityPolicy::default()),
            vec![Box::new(MockTool("shell")), Box::new(MockTool("file_read"))],
            Some(&allow),
        )
        .await;
        assert_eq!(names, vec!["shell".to_string()], "caller_allowed narrows: {names:?}");
    }
}
