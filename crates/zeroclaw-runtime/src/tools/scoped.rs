//! `ScopedToolRegistry` - the one gated seam that mints the per-agent tool set.
//!
//! Assembly applies peripherals, built-in policy, ACP memory stripping, MCP
//! scope and policy, capability tools, pinned resources, and skills in that
//! order. This is the intended construction path; the type boundary remains
//! temporarily unsealed while legacy callers still accept raw tool vectors.

use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

use crate::agent::loop_::{
    append_pinned_mcp_section, apply_policy_tool_filter, eager_mcp_tool_allowed,
    load_peripheral_tools, mcp_allowed_tool_count, mcp_tool_access_policy,
    preactivate_always_filter_groups, register_eager_mcp_tool_if_allowed,
};
use crate::skills::Skill;
use crate::tools::{
    self, ActivatedToolSet, AllToolsResult, DelegateParentToolsHandle, PerToolChannelHandle, Tool,
    register_skill_tools_with_context_and_runtime,
};

/// A per-agent tool registry that has been scoped and gated. The inner field is
/// private and production code can only mint one through
/// [`ScopedToolRegistry::assemble`]. Today (the unsealed P1 phase) the engine still
/// takes `&[Box<dyn Tool>]`, so callers dissolve the type via [`std::ops::Deref`] or
/// [`Self::into_inner`] at the boundary; once every construction site is cut over,
/// the engine's tools field seals to this type and handing it an unfiltered
/// registry becomes a compile error instead of a review-checklist item.
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
    /// Documented divergence: ACP `session/new` must return promptly, so it does not
    /// connect MCP servers - they are neither resolved nor connected; nothing is
    /// granted.
    pub connect_mcp: bool,
    /// Documented divergence: loading peripherals physically connects hardware (the
    /// daemon's loader opens serial ports, exclusively for real devices). Listing-only
    /// surfaces (the gateway's `/api/tools` registries) MUST pass `false` so they never
    /// hold devices the live turn paths need; execution surfaces pass `true`.
    pub connect_peripherals: bool,
    /// Documented divergence: ACP excludes persistent memory tools.
    pub exclude_memory: bool,
    pub list_deferred_mcp_specs: bool,
    pub emit_assembly_logs: bool,
    /// Pre-built MCP registry supplied by the caller. The daemon heartbeat
    /// worker constructs this once at worker start and shares it across
    /// every tick so that stdio MCP children live for the daemon's
    /// lifetime rather than being orphaned and re-spawned per
    /// `agent::run` call. When `Some`, `assemble` MUST use this
    /// `Arc<McpRegistry>` and MUST NOT call `McpRegistry::connect_all`
    /// itself. `None` preserves the legacy per-call connect path
    /// (CLI / one-shot / process_message), which is correct for
    /// callers that have no cross-turn reuse contract.
    pub mcp_registry: Option<Arc<crate::tools::McpRegistry>>,
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
    /// The deferred-MCP tool-search listing on its own (deferred mode only): the
    /// `## Deferred Tools` section that names the policy-admitted `<server>__<tool>`
    /// stubs and instructs the model to call `tool_search`. Empty when deferred loading
    /// is off, no stubs are admitted, or `tool_search` itself is in `excluded_tools`
    /// (the registry and prompt surfaces move together).
    ///
    /// Private - deliberately not destructurable. Every caller that has ever needed
    /// this field also needs [`Self::pinned_section`] threaded correctly alongside it,
    /// and a `..` (or an unaware full destructure) silently drops it - which is exactly
    /// how the independent-delegate path lost `pinned_section` when the field was split
    /// out. Use [`Self::combined_mcp_prompt_section`] for the
    /// single-block shape (`run`, `process_message`, independent delegation) or
    /// [`Self::deferred_section`]/[`Self::pinned_section`] for the two-slot shape
    /// (`from_config`'s `Agent`, which injects each separately per-turn).
    deferred_section: String,
    /// The pinned-MCP-resources system-prompt section on its own. Empty when no pinned
    /// resources are granted. Private for the same reason as [`Self::deferred_section`]
    /// above - access via the same two accessor patterns.
    pinned_section: String,
    /// Live handle to the activated deferred-MCP set (present only when a deferred
    /// `tool_search` tool was registered).
    pub activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>>,
    pub mcp_tool_names: HashSet<String>,
}

impl ScopedAssembled {
    /// The deferred-MCP tool-search listing and the pinned-MCP-resources section,
    /// composed into ONE prompt block. For callers that inject a single combined MCP
    /// prompt section: `run`, `process_message`, and independent delegation.
    ///
    /// Centralizing the composition here (instead of each caller hand-rolling
    /// `append_pinned_mcp_section(&mut deferred_section, &pinned_section)` after its own
    /// destructure) is what makes dropping `pinned_section` a thing that can no longer
    /// happen silently - the field isn't reachable except through this method or
    /// [`Self::pinned_section`], so a caller must consciously pick one.
    pub fn combined_mcp_prompt_section(&self) -> String {
        let mut combined = self.deferred_section.clone();
        append_pinned_mcp_section(&mut combined, &self.pinned_section);
        combined
    }

    /// The deferred-MCP tool-search listing on its own, for callers with two distinct
    /// prompt slots that inject each separately (`Agent::from_config`'s `Agent`, whose
    /// prompt is composed per-turn later rather than at `assemble`-call time). See
    /// [`Self::pinned_section`] for its counterpart, and
    /// [`Self::combined_mcp_prompt_section`] for the single-block shape.
    pub fn deferred_section(&self) -> &str {
        &self.deferred_section
    }

    /// The pinned-MCP-resources section on its own. See [`Self::deferred_section`].
    pub fn pinned_section(&self) -> &str {
        &self.pinned_section
    }
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
            connect_peripherals,
            exclude_memory,
            list_deferred_mcp_specs,
            emit_assembly_logs,
            mcp_registry: overrides_mcp_registry,
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

        // 1. Peripherals. Loading CONNECTS hardware (serial opens are exclusive for
        //    real devices), so this is gated: execution surfaces pass
        //    `connect_peripherals: true`; listing-only surfaces pass `false` and
        //    enumerate without holding devices.
        if connect_peripherals {
            let peripheral_tools = load_peripheral_tools(config.peripherals.clone()).await;
            if emit_assembly_logs && !peripheral_tools.is_empty() {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_attrs(::serde_json::json!({"count": peripheral_tools.len()})),
                    "Peripheral tools added"
                );
            }
            tools_registry.extend(peripheral_tools);
        }

        // 2. Built-in allow/deny filter (uniform: the gateway used to skip it entirely).
        //    `caller_allowed` narrows on top of the policy, for the `run` path only.
        let before_filter = tools_registry.len();
        apply_policy_tool_filter(&mut tools_registry, Some(security.as_ref()), caller_allowed);
        if emit_assembly_logs && tools_registry.len() != before_filter {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "before": before_filter,
                        "retained": tools_registry.len(),
                        "policy_allowed": security.allowed_tools.as_ref().map(|v| v.len()),
                        "policy_excluded": security.excluded_tools.as_ref().map(|v| v.len()),
                        "caller_allowed": caller_allowed.map(|v| v.len()),
                    })),
                "Applied capability-based tool access filter"
            );
        }

        // 3. Documented divergence: ACP strips persistent memory tools.
        if exclude_memory {
            tools_registry.retain(|t| !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&t.name()));
        }

        // 4. MCP: scope servers per `mcp_bundles` (omission is not a grant), then gate
        //    each tool. Skipped only when this path does not connect MCP (ACP) or MCP
        //    is disabled - in both cases nothing is granted.
        let mut deferred_section = String::new();
        // Pinned MCP resources are surfaced on their own field. Single-block callers
        // (`run`, `process_message`) append this onto their `deferred_section` copy;
        // `from_config` injects it into the Agent's distinct pinned-section slot.
        let mut pinned_section = String::new();
        let mut activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>> = None;
        let mut mcp_elevation_arcs: Vec<Arc<dyn Tool>> = Vec::new();
        // MCP-origin ground truth for the tool_filter_groups gates; see
        // the `ScopedAssembled::mcp_tool_names` field doc for the contract.
        let mut mcp_tool_names: HashSet<String> = HashSet::new();

        let agent_mcp_servers = if connect_mcp && config.mcp.enabled {
            config.mcp_servers_for_agent(agent_alias)
        } else {
            Vec::new()
        };
        if !agent_mcp_servers.is_empty() {
            if emit_assembly_logs {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_category(::zeroclaw_log::EventCategory::Tool),
                    &format!(
                        "Initializing MCP client - {} server(s) granted via mcp_bundles",
                        agent_mcp_servers.len()
                    )
                );
            }
            // Caller-supplied registry wins: the daemon heartbeat worker
            // constructs the registry once and reuses it across every
            // tick so stdio MCP children live for the daemon lifetime.
            // Falling back to per-call `connect_all` keeps the legacy
            // CLI / one-shot / process_message path intact.
            let shared_registry: Option<Arc<tools::McpRegistry>> =
                if let Some(shared) = overrides_mcp_registry.as_ref() {
                    Some(Arc::clone(shared))
                } else {
                    match tools::McpRegistry::connect_all(&agent_mcp_servers).await {
                        Ok(registry) => Some(Arc::new(registry)),
                        Err(err) => {
                            // Non-fatal (the assembly proceeds without MCP), but an ERROR
                            // with structured attrs - parity with the run/process_message
                            // connect-failure logging.
                            ::zeroclaw_log::record!(
                                ERROR,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Fail
                                )
                                .with_category(::zeroclaw_log::EventCategory::Tool)
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "agent_alias": agent_alias,
                                    "error": format!("{err}"),
                                })),
                                "MCP registry failed to initialize (assembly proceeds without MCP)"
                            );
                            None
                        }
                    }
                };
            if let Some(registry) = shared_registry {
                // Origin set: every `<server>__<tool>` name the registry knows.
                // Deferred stubs derive from the same `tool_names()` call, so
                // one extension covers eager, deferred, and later activations.
                mcp_tool_names.extend(registry.tool_names());
                // Elevation arcs exist only to resolve skill-declared MCP
                // elevation in step 5; skip the collection when no skills are
                // registered through this assembly.
                if !skills.is_empty() {
                    mcp_elevation_arcs = tools::collect_mcp_elevation_arcs(&registry).await;
                }
                let mcp_policy = mcp_tool_access_policy(security.as_ref(), caller_allowed);
                // Generic MCP resource/prompt capability tools (policy-gated in
                // deferred-loading and eager modes) - parity with run/process_message.
                for tool in tools::build_mcp_capability_tools(&registry, mcp_policy.as_ref()) {
                    let capability_name = tool.name().to_string();
                    if register_eager_mcp_tool_if_allowed(
                        tool,
                        &mut tools_registry,
                        delegate_handle.as_ref(),
                        mcp_policy.as_ref(),
                    ) {
                        // Capability tools are MCP-origin (built from the
                        // registry) and were the only names the pre- prefix
                        // gate matched — they stay classifiable so a
                        // non-matching group set keeps excluding them.
                        mcp_tool_names.insert(capability_name);
                    }
                }
                pinned_section = tools::mcp_context::build_pinned_resources_section(
                    &registry,
                    &agent_mcp_servers,
                    mcp_policy.as_ref(),
                )
                .await;
                if config.mcp.deferred_loading {
                    let deferred_set =
                        tools::DeferredMcpToolSet::from_registry(Arc::clone(&registry)).await;
                    if emit_assembly_logs {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Load
                            )
                            .with_category(::zeroclaw_log::EventCategory::Tool),
                            &format!(
                                "MCP deferred: {} tool stub(s) from {} server(s)",
                                deferred_set.len(),
                                registry.server_count()
                            )
                        );
                    }
                    if list_deferred_mcp_specs {
                        for stub in &deferred_set.stubs {
                            if !eager_mcp_tool_allowed(&stub.prefixed_name, mcp_policy.as_ref()) {
                                continue;
                            }
                            let wrapper: Arc<dyn Tool> =
                                Arc::new(stub.activate(Arc::clone(&registry)));
                            register_eager_mcp_tool_if_allowed(
                                wrapper,
                                &mut tools_registry,
                                delegate_handle.as_ref(),
                                mcp_policy.as_ref(),
                            );
                        }
                    }
                    let allowed_stub_count = mcp_allowed_tool_count(
                        deferred_set
                            .stubs
                            .iter()
                            .map(|stub| stub.prefixed_name.as_str()),
                        mcp_policy.as_ref(),
                    );
                    deferred_section = tools::build_deferred_tools_section_filtered(
                        &deferred_set,
                        mcp_policy.as_ref(),
                    );
                    // Listing registries expose the real deferred MCP tools as
                    // eager wrappers above and never consume the deferred prompt
                    // section, the activation handle, or invoke tools. Skip
                    // `tool_search` there so `/api/tools` matches eager-mode
                    // listing (real MCP tools, no deferral-internal helper).
                    if allowed_stub_count > 0 && !list_deferred_mcp_specs {
                        let activated = Arc::new(std::sync::Mutex::new(ActivatedToolSet::new()));
                        activated_handle = Some(Arc::clone(&activated));
                        // Pre-activate `mode = "always"` tool_filter_groups
                        // entries before `ToolSearchTool::new` consumes
                        // the stub set, so `always` tools are live on the very
                        // first turn. Groups resolve from the agent's runtime
                        // profile — the same source `Config::resolved_agent_config`
                        // clones into `agent.resolved.tool_filter_groups`, which
                        // the per-turn gates read; if profile resolution ever
                        // grows merge logic, both lookups must move together.
                        let filter_groups = config
                            .runtime_profile_for_agent(agent_alias)
                            .map(|profile| profile.tool_filter_groups.as_slice())
                            .unwrap_or(&[]);
                        let preactivated_names = preactivate_always_filter_groups(
                            &deferred_set,
                            &activated,
                            filter_groups,
                            mcp_policy.as_ref(),
                            delegate_handle.as_ref(),
                        );
                        if emit_assembly_logs && !preactivated_names.is_empty() {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Register
                                )
                                .with_category(::zeroclaw_log::EventCategory::Tool)
                                .with_attrs(::serde_json::json!({
                                    "agent_alias": agent_alias,
                                    "count": preactivated_names.len(),
                                })),
                                "MCP deferred: pre-activated tool(s) via tool_filter_groups mode=always"
                            );
                        }
                        // Build the prompt section AFTER pre-activation and
                        // exclude the just-activated names: the section tells
                        // the model listed tools are "NOT yet loaded" and MUST
                        // be fetched via tool_search — advertising a live tool
                        // there would burn the exact first-turn round-trip
                        // `mode = "always"` pre-activation exists to remove.
                        deferred_section = tools::build_deferred_tools_section_excluding(
                            &deferred_set,
                            mcp_policy.as_ref(),
                            &preactivated_names,
                        );
                        let mut tool_search = tools::ToolSearchTool::new(deferred_set, activated);
                        if let Some(policy) = mcp_policy {
                            tool_search = tool_search.with_access_policy(policy);
                        }
                        // Newly-activated deferred tools are also exposed to the
                        // delegate parent set, matching the run/process_message paths.
                        if let Some(ref handle) = delegate_handle {
                            let delegate_tools = Arc::clone(handle);
                            tool_search = tool_search.with_activation_hook(Arc::new(move |tool| {
                                let mut tools = delegate_tools.write();
                                let already =
                                    tools.iter().any(|existing| existing.name() == tool.name());
                                if !already {
                                    tools.push(tool);
                                }
                            }));
                        }
                        tools_registry.push(Box::new(tool_search));
                    }
                } else {
                    let names = registry.tool_names();
                    let mut registered = 0usize;
                    let mut skipped = 0usize;
                    for name in names {
                        if !eager_mcp_tool_allowed(&name, mcp_policy.as_ref()) {
                            skipped += 1;
                            continue;
                        }
                        if let Some(def) = registry.get_tool_def(&name).await {
                            let wrapper: Arc<dyn Tool> = Arc::new(tools::McpToolWrapper::new(
                                name,
                                def,
                                Arc::clone(&registry),
                            ));
                            if register_eager_mcp_tool_if_allowed(
                                wrapper,
                                &mut tools_registry,
                                delegate_handle.as_ref(),
                                mcp_policy.as_ref(),
                            ) {
                                registered += 1;
                            }
                        }
                    }
                    if emit_assembly_logs {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Register
                            )
                            .with_category(::zeroclaw_log::EventCategory::Tool),
                            &format!(
                                "MCP: {} tool(s) registered from {} server(s), {} skipped by policy",
                                registered,
                                registry.server_count(),
                                skipped
                            )
                        );
                    }
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

        // Skills and deferred MCP helpers are registered after the built-in filter,
        // so the explicit denylist must subtract once more at the final boundary.
        if let Some(excluded) = security.excluded_tools.as_deref() {
            tools_registry.retain(|t| !excluded.iter().any(|ex| ex == t.name()));
            // The registry and prompt surfaces must move together: if `tool_search`
            // itself is excluded, the deferred-MCP prompt section - which always
            // instructs the model to call `tool_search` - must not survive either,
            // or the model is told to call a tool the policy just removed.
            if excluded.iter().any(|ex| ex == "tool_search") {
                deferred_section.clear();
            }
        }

        ScopedAssembled {
            registry: ScopedToolRegistry(tools_registry),
            delegate_handle,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            deferred_section,
            pinned_section,
            activated_handle,
            mcp_tool_names,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolOutput, ToolResult};
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
                output: ToolOutput::default(),
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
            channel_room_handle: None,
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
            connect_peripherals: false,
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
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
            vec![
                Box::new(MockTool("shell")),
                Box::new(MockTool("spawn_subagent")),
            ],
            None,
        )
        .await;
        assert!(
            names.iter().any(|n| n == "shell"),
            "unlisted tool kept: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "spawn_subagent"),
            "excluded tool dropped: {names:?}"
        );
    }

    #[tokio::test]
    async fn assemble_grants_no_mcp_to_agent_without_bundles() {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, McpServerConfig, McpTransport, RiskProfileConfig,
        };

        let mut config = Config::default();
        config.mcp.enabled = true;
        config.mcp.servers = vec![McpServerConfig {
            name: "fs".into(),
            transport: McpTransport::Stdio,
            command: "/usr/bin/mcp-fs".into(),
            ..Default::default()
        }];
        // Critically: NO mcp_bundles configured and NO agent grants.
        config
            .risk_profiles
            .insert("test-profile".into(), RiskProfileConfig::default());
        config.agents.insert(
            "unscoped".into(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: "openai.test-provider".into(),
                risk_profile: "test-profile".into(),
                mcp_bundles: Vec::new(),
                ..Default::default()
            },
        );
        let security = Arc::new(SecurityPolicy {
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ScopedToolRegistry::assemble(ScopedAssembly {
                config: &config,
                agent_alias: "unscoped",
                security: &security,
                built: built_with(Vec::new()),
                skills: &[],
                runtime: Arc::new(crate::platform::NativeRuntime::new()),
                caller_allowed: None,
                connect_mcp: true,
                connect_peripherals: false,
                exclude_memory: false,
                list_deferred_mcp_specs: false,
                emit_assembly_logs: false,
                mcp_registry: None,
            }),
        )
        .await
        .expect("assemble must not hang for an unscoped agent");

        assert!(
            out.registry.is_empty(),
            "assemble must not mint any MCP tool when the agent has no \
             mcp_bundles grant; got {:?}",
            out.registry.iter().map(|t| t.name()).collect::<Vec<_>>()
        );
        assert!(
            out.activated_handle.is_none() && out.deferred_section.is_empty(),
            "no deferred-MCP artifacts may exist for an unscoped agent"
        );
    }

    async fn mock_mcp_http_server() -> wiremock::MockServer {
        use wiremock::matchers::{body_partial_json, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"method": "initialize"}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Mcp-Session-Id", "s")
                    .set_body_json(serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"capabilities":{"tools":{}}}
                    })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"method":"notifications/initialized"}),
            ))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"method":"tools/list"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc":"2.0","id":2,"result":{"tools":[
                    {"name":"echo","description":"echo","inputSchema":{"type":"object"}},
                    {"name":"add_numbers","description":"add","inputSchema":{"type":"object"}}
                ]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"method":"resources/list"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc":"2.0","id":3,"result":{"resources":[]}
            })))
            .mount(&server)
            .await;
        server
    }

    fn config_with_bundled_mcp(server_uri: String, server2_uri: String) -> Config {
        use zeroclaw_config::schema::{
            AliasedAgentConfig, McpBundleConfig, McpServerConfig, McpTransport, RiskProfileConfig,
        };

        let mut config = Config::default();
        config.mcp.enabled = true;
        config.mcp.servers = vec![
            McpServerConfig {
                name: "remote".into(),
                transport: McpTransport::Http,
                url: Some(server_uri),
                ..Default::default()
            },
            McpServerConfig {
                name: "remote2".into(),
                transport: McpTransport::Http,
                url: Some(server2_uri),
                ..Default::default()
            },
        ];
        config.mcp_bundles.insert(
            "mockbundle".into(),
            McpBundleConfig {
                servers: vec!["remote".into(), "remote2".into()],
                exclude: Vec::new(),
            },
        );
        config
            .risk_profiles
            .insert("test-profile".into(), RiskProfileConfig::default());
        config.agents.insert(
            "scoped".into(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: "openai.test-provider".into(),
                risk_profile: "test-profile".into(),
                mcp_bundles: vec!["mockbundle".into()],
                ..Default::default()
            },
        );
        config
    }

    async fn assemble_listing_for(config: &Config) -> Vec<String> {
        let security = Arc::new(SecurityPolicy {
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ScopedToolRegistry::assemble(ScopedAssembly {
                config,
                agent_alias: "scoped",
                security: &security,
                built: built_with(Vec::new()),
                skills: &[],
                runtime: Arc::new(crate::platform::NativeRuntime::new()),
                caller_allowed: None,
                connect_mcp: true,
                connect_peripherals: false,
                exclude_memory: false,
                list_deferred_mcp_specs: true,
                emit_assembly_logs: false,
                mcp_registry: None,
            }),
        )
        .await
        .expect("assemble must not hang");
        out.registry.iter().map(|t| t.name().to_string()).collect()
    }

    #[tokio::test]
    async fn assemble_lists_bundled_mcp_tools_in_both_loading_modes() {
        let server = mock_mcp_http_server().await;
        let server2 = mock_mcp_http_server().await;

        let mut eager = config_with_bundled_mcp(server.uri(), server2.uri());
        eager.mcp.deferred_loading = false;
        let mut eager_names = assemble_listing_for(&eager).await;

        let mut deferred = config_with_bundled_mcp(server.uri(), server2.uri());
        deferred.mcp.deferred_loading = true;
        let mut deferred_names = assemble_listing_for(&deferred).await;

        for expected in [
            "remote__echo",
            "remote__add_numbers",
            "remote2__echo",
            "remote2__add_numbers",
        ] {
            assert!(
                eager_names.iter().any(|n| n == expected),
                "eager mode must list bundled MCP tool {expected}: {eager_names:?}"
            );
            assert!(
                deferred_names.iter().any(|n| n == expected),
                "deferred mode must still list bundled MCP tool {expected} in the \
                 enumeration registry (#8302); got {deferred_names:?}"
            );
        }

        // The deferral-internal turn helper is not a real listed tool. It must
        // not appear on the dashboard listing in deferred mode.
        assert!(
            !deferred_names.iter().any(|n| n == "tool_search"),
            "deferred listing registry must not expose tool_search (#8302); \
             got {deferred_names:?}"
        );

        // Eager and deferred listing registries must present the same tool set,
        // which is the parity contract this fix restores.
        eager_names.sort();
        eager_names.dedup();
        deferred_names.sort();
        deferred_names.dedup();
        assert_eq!(
            eager_names, deferred_names,
            "eager and deferred /api/tools listings must match (#8302)"
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
        assert_eq!(
            names,
            vec!["shell".to_string()],
            "caller_allowed narrows: {names:?}"
        );
    }

    fn assembled_with_sections(deferred: &str, pinned: &str) -> ScopedAssembled {
        ScopedAssembled {
            registry: ScopedToolRegistry(Vec::new()),
            delegate_handle: None,
            ask_user_handle: None,
            reaction_handle: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            poll_handle: None,
            escalate_handle: None,
            channel_room_handle: None,
            deferred_section: deferred.to_string(),
            pinned_section: pinned.to_string(),
            activated_handle: None,
            mcp_tool_names: HashSet::new(),
        }
    }

    #[test]
    fn combined_mcp_prompt_section_composes_both_when_present() {
        let assembled = assembled_with_sections("## Deferred Tools\n- x__y", "## Pinned\n- z");
        // Exact-string, not just contains()+ordering: pins the precise
        // `deferred + "\n\n" + pinned` format `append_pinned_mcp_section` produces, so a
        // regression in the separator or a stray transformation fails this test directly.
        assert_eq!(
            assembled.combined_mcp_prompt_section(),
            "## Deferred Tools\n- x__y\n\n## Pinned\n- z"
        );
    }

    #[test]
    fn combined_mcp_prompt_section_is_deferred_only_when_pinned_empty() {
        let assembled = assembled_with_sections("## Deferred Tools\n- x__y", "");
        assert_eq!(
            assembled.combined_mcp_prompt_section(),
            "## Deferred Tools\n- x__y"
        );
    }

    #[test]
    fn deferred_and_pinned_accessors_return_the_raw_unmerged_sections() {
        // The two-slot shape (`from_config`'s Agent) must get each section on its own,
        // NOT the combined block - this is what makes it safe for a caller with two
        // separate prompt-injection points to avoid duplicating pinned content.
        let assembled = assembled_with_sections("deferred-only", "pinned-only");
        assert_eq!(assembled.deferred_section(), "deferred-only");
        assert_eq!(assembled.pinned_section(), "pinned-only");
    }

    #[tokio::test]
    async fn assemble_without_mcp_yields_empty_origin_set() {
        // No MCP connected => nothing is classified MCP-origin, so the
        // tool_filter_groups gates treat every tool as a pass-through
        // built-in/skill and the groups are inert by construction
        let config = Config::default();
        let security = Arc::new(SecurityPolicy::default());
        let out = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with(vec![Box::new(MockTool("shell"))]),
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;
        assert!(
            out.mcp_tool_names.is_empty(),
            "no-MCP assembly must export an empty origin set; got {:?}",
            out.mcp_tool_names
        );
    }
}
