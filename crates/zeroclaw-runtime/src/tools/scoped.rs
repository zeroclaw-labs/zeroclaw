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
    /// `deliver_file` hands the client a typed file attachment that only an
    /// ACP-capable turn actually transports (the model history, WS, and RPC
    /// paths all drop the artifact). Every non-ACP assembly passes `false` so
    /// the tool is absent rather than returning a false success on a channel
    /// that cannot deliver it. Only the ACP turn path passes `true`.
    pub acp_delivery: bool,
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

fn tool_allowed_in_context(name: &str, exclude_memory: bool, acp_delivery: bool) -> bool {
    (!exclude_memory || !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&name))
        && (acp_delivery || name != "deliver_file")
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
            acp_delivery,
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
            // Test-only capture of the concrete delegate instance; `assemble`
            // has no use for it and must keep destructuring exhaustively so a
            // new field cannot be silently dropped here.
            #[cfg(test)]
                delegate_tool: _,
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

        // Mint the pipeline only after the effective caller policy is known. The
        // same immutable Arc is used for top-level registration and any
        // skill-scoped builtin elevation, so no unrestricted copy can escape.
        let context_filtered_tool_arcs: Vec<Arc<dyn Tool>> = unfiltered_tool_arcs
            .iter()
            .filter(|tool| tool_allowed_in_context(tool.name(), exclude_memory, acp_delivery))
            .cloned()
            .collect();
        let pipeline_tool = config.pipeline.enabled.then(|| {
            Arc::new(tools::PipelineTool::with_access_policy(
                config.pipeline.clone(),
                context_filtered_tool_arcs.clone(),
                zeroclaw_tools::tool_search::ToolAccessPolicy::from_security(
                    security.allowed_tools.as_deref(),
                    security.excluded_tools.as_deref(),
                    caller_allowed,
                ),
            )) as Arc<dyn Tool>
        });
        if let Some(tool) = pipeline_tool.as_ref() {
            tools_registry.push(Box::new(tools::ArcToolRef(Arc::clone(tool))));
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

        // 3. Apply the assembly context to every executable view. Pipeline children
        //    were minted above from this same predicate, so nested execution cannot
        //    recover memory or delivery tools removed from the outer registry.
        tools_registry
            .retain(|tool| tool_allowed_in_context(tool.name(), exclude_memory, acp_delivery));

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
                    // Centralized single source of truth for the deferred-MCP
                    // tool set: the same `filtered_deferred` drives both the
                    // prompt-side `build_deferred_tools_section_filtered` and
                    // the `ToolSearchTool` constructor, so a denied tool cannot
                    // leak into either side. The `with_access_policy` step on
                    // the search tool is now defense-in-depth — the stub set is
                    // already pre-filtered.
                    let filtered_deferred = deferred_set.filter_by_policy(mcp_policy.as_ref());
                    let allowed_stub_count = mcp_allowed_tool_count(
                        filtered_deferred
                            .stubs
                            .iter()
                            .map(|stub| stub.prefixed_name.as_str()),
                        mcp_policy.as_ref(),
                    );
                    deferred_section = tools::build_deferred_tools_section_filtered(
                        &filtered_deferred,
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
                            &filtered_deferred,
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
                            &filtered_deferred,
                            mcp_policy.as_ref(),
                            &preactivated_names,
                        );
                        let mut tool_search =
                            tools::ToolSearchTool::new(filtered_deferred, activated);
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
        //    `SecurityPolicy`, resolving builtin elevation against context-filtered arcs.
        let resolution_registry: Vec<Arc<dyn Tool>> = context_filtered_tool_arcs
            .iter()
            .cloned()
            .chain(mcp_elevation_arcs.iter().cloned())
            .chain(pipeline_tool.iter().cloned())
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
    use crate::skills::SkillTool;
    use crate::tools::{ToolOutput, ToolResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockTool(&'static str);

    impl zeroclaw_api::attribution::Attributable for MockTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            self.0
        }
    }

    struct CountingTool {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    zeroclaw_api::mock_tool_attribution!(CountingTool);

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "count calls"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: "ran".into(),
                error: None,
            })
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
            delegate_tool: None,
        }
    }

    fn built_with_counting_tools(
        calls: Arc<AtomicUsize>,
        names: &[&'static str],
    ) -> AllToolsResult {
        let unfiltered_tool_arcs: Vec<Arc<dyn Tool>> = names
            .iter()
            .map(|name| {
                Arc::new(CountingTool {
                    name,
                    calls: Arc::clone(&calls),
                }) as Arc<dyn Tool>
            })
            .collect();
        let tools = unfiltered_tool_arcs
            .iter()
            .cloned()
            .map(|tool| Box::new(tools::ArcToolRef(tool)) as Box<dyn Tool>)
            .collect();
        AllToolsResult {
            tools,
            delegate_handle: None,
            ask_user_handle: None,
            reaction_handle: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            poll_handle: None,
            escalate_handle: None,
            channel_room_handle: None,
            unfiltered_tool_arcs,
            delegate_tool: None,
        }
    }

    fn built_with_pipeline(calls: Arc<AtomicUsize>) -> AllToolsResult {
        built_with_counting_tools(calls, &["shell", "file_write"])
    }

    async fn assemble_pipeline(
        security: Arc<SecurityPolicy>,
        skills: &[Skill],
        calls: Arc<AtomicUsize>,
        caller_allowed: Option<&[String]>,
    ) -> ScopedAssembled {
        let mut config = Config::default();
        config.pipeline.enabled = true;
        config.pipeline.max_steps = 20;
        config.pipeline.allowed_tools = vec!["shell".to_string(), "file_write".to_string()];
        ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with_pipeline(calls),
            skills,
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await
    }

    #[tokio::test]
    async fn assembled_pipeline_rejects_agent_denied_step_before_execution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec![tools::PipelineTool::NAME.to_string()]),
            ..SecurityPolicy::default()
        });
        let assembled = assemble_pipeline(security, &[], Arc::clone(&calls), None).await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .expect("policy-admitted pipeline must be registered");

        let result = pipeline
            .execute(serde_json::json!({
                "steps": [{"tool": "shell", "args": {}}]
            }))
            .await
            .expect("pipeline denial is a tool result, not a transport error");

        assert!(!result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn pipeline_omitted_when_top_level_policy_denies_it() {
        let calls = Arc::new(AtomicUsize::new(0));
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec!["shell".to_string()]),
            ..SecurityPolicy::default()
        });
        let assembled = assemble_pipeline(security, &[], calls, None).await;

        assert!(
            assembled
                .registry
                .iter()
                .all(|tool| tool.name() != tools::PipelineTool::NAME)
        );
    }

    #[tokio::test]
    async fn skill_elevated_pipeline_keeps_the_same_agent_policy_ceiling() {
        let calls = Arc::new(AtomicUsize::new(0));
        let skill = Skill {
            name: "ops".to_string(),
            description: "pipeline wrapper".to_string(),
            description_localizations: Default::default(),
            version: "1.0.0".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "chain".to_string(),
                description: "run a pipeline".to_string(),
                kind: "builtin".to_string(),
                command: String::new(),
                args: Default::default(),
                target: Some(tools::PipelineTool::NAME.to_string()),
                locked_args: Default::default(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec!["ops__chain".to_string()]),
            ..SecurityPolicy::default()
        });
        let assembled = assemble_pipeline(
            security,
            std::slice::from_ref(&skill),
            Arc::clone(&calls),
            None,
        )
        .await;
        assert!(
            assembled
                .registry
                .iter()
                .all(|tool| tool.name() != tools::PipelineTool::NAME)
        );
        let elevated = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == "ops__chain")
            .expect("skill elevation must resolve the scoped pipeline target");

        let result = elevated
            .execute(serde_json::json!({
                "steps": [{"tool": "shell", "args": {}}]
            }))
            .await
            .expect("pipeline denial is a tool result, not a transport error");

        assert!(!result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    async fn assert_mixed_pipeline_is_prevalidated(parallel: bool) {
        let calls = Arc::new(AtomicUsize::new(0));
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec![
                tools::PipelineTool::NAME.to_string(),
                "shell".to_string(),
            ]),
            ..SecurityPolicy::default()
        });
        let assembled = assemble_pipeline(security, &[], Arc::clone(&calls), None).await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .expect("policy-admitted pipeline must be registered");

        let result = pipeline
            .execute(serde_json::json!({
                "steps": [
                    {"tool": "shell", "args": {}},
                    {"tool": "file_write", "args": {}}
                ],
                "parallel": parallel
            }))
            .await
            .expect("pipeline denial is a tool result, not a transport error");

        assert!(!result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sequential_pipeline_prevalidates_every_step() {
        assert_mixed_pipeline_is_prevalidated(false).await;
    }

    #[tokio::test]
    async fn parallel_pipeline_prevalidates_every_step() {
        assert_mixed_pipeline_is_prevalidated(true).await;
    }

    #[tokio::test]
    async fn pipeline_steps_respect_the_run_caller_allowlist() {
        let calls = Arc::new(AtomicUsize::new(0));
        let security = Arc::new(SecurityPolicy::default());
        let caller_allowed = vec![tools::PipelineTool::NAME.to_string(), "shell".to_string()];
        let assembled =
            assemble_pipeline(security, &[], Arc::clone(&calls), Some(&caller_allowed)).await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .expect("caller-admitted pipeline must be registered");

        let result = pipeline
            .execute(serde_json::json!({
                "steps": [{"tool": "file_write", "args": {}}]
            }))
            .await
            .expect("pipeline denial is a tool result, not a transport error");

        assert!(!result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    async fn assert_pipeline_context_prevalidates_excluded_tool(
        child_name: &'static str,
        exclude_memory: bool,
        acp_delivery: bool,
        parallel: bool,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut config = Config::default();
        config.pipeline.enabled = true;
        config.pipeline.allowed_tools = vec!["shell".to_string(), child_name.to_string()];
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec![
                tools::PipelineTool::NAME.to_string(),
                "shell".to_string(),
                child_name.to_string(),
            ]),
            ..SecurityPolicy::default()
        });
        let assembled = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with_counting_tools(Arc::clone(&calls), &["shell", child_name]),
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory,
            acp_delivery,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;

        assert!(
            assembled
                .registry
                .iter()
                .all(|tool| tool.name() != child_name),
            "context-excluded tool must be absent from the outer registry"
        );
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .expect("context filtering must not remove the admitted pipeline");
        let result = pipeline
            .execute(serde_json::json!({
                "steps": [
                    {"tool": "shell", "args": {}},
                    {"tool": child_name, "args": {}}
                ],
                "parallel": parallel
            }))
            .await
            .expect("pipeline denial is a tool result, not a transport error");

        assert!(!result.success);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sequential_pipeline_prevalidates_memory_excluded_by_assembly_context() {
        assert_pipeline_context_prevalidates_excluded_tool("memory_recall", true, true, false)
            .await;
    }

    #[tokio::test]
    async fn parallel_pipeline_prevalidates_memory_excluded_by_assembly_context() {
        assert_pipeline_context_prevalidates_excluded_tool("memory_recall", true, true, true).await;
    }

    #[tokio::test]
    async fn sequential_pipeline_prevalidates_delivery_outside_acp_context() {
        assert_pipeline_context_prevalidates_excluded_tool("deliver_file", false, false, false)
            .await;
    }

    #[tokio::test]
    async fn parallel_pipeline_prevalidates_delivery_outside_acp_context() {
        assert_pipeline_context_prevalidates_excluded_tool("deliver_file", false, false, true)
            .await;
    }

    async fn assert_skill_context_excludes_tool(
        child_name: &'static str,
        exclude_memory: bool,
        acp_delivery: bool,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let skill = Skill {
            name: "ops".to_string(),
            description: "context-filtered builtin wrapper".to_string(),
            description_localizations: Default::default(),
            version: "1.0.0".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "restricted".to_string(),
                description: "wrap a context-restricted builtin".to_string(),
                kind: "builtin".to_string(),
                command: String::new(),
                args: Default::default(),
                target: Some(child_name.to_string()),
                locked_args: Default::default(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(vec!["ops__restricted".to_string()]),
            ..SecurityPolicy::default()
        });
        let config = Config::default();
        let assembled = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with_counting_tools(Arc::clone(&calls), &[child_name]),
            skills: std::slice::from_ref(&skill),
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory,
            acp_delivery,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;

        assert!(
            assembled
                .registry
                .iter()
                .all(|tool| tool.name() != "ops__restricted")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn skill_cannot_recover_memory_excluded_by_assembly_context() {
        assert_skill_context_excludes_tool("memory_recall", true, true).await;
    }

    #[tokio::test]
    async fn skill_cannot_recover_delivery_outside_acp_context() {
        assert_skill_context_excludes_tool("deliver_file", false, false).await;
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
            acp_delivery: true, // keep deliver_file so name-filter tests are unaffected
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

    /// `deliver_file` emits a typed attachment only an ACP turn transports, so it
    /// is gated on `acp_delivery`: absent on every non-ACP assembly (where it would
    /// otherwise report a false success), present only when the ACP turn path opts in.
    async fn assemble_names_with_acp_delivery(acp_delivery: bool) -> Vec<String> {
        let config = Config::default();
        let security = Arc::new(SecurityPolicy::default());
        let out = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with(vec![
                Box::new(MockTool("shell")),
                Box::new(MockTool("deliver_file")),
            ]),
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;
        out.registry.iter().map(|t| t.name().to_string()).collect()
    }

    #[tokio::test]
    async fn non_acp_assembly_omits_deliver_file() {
        let names = assemble_names_with_acp_delivery(false).await;
        assert!(
            names.iter().any(|n| n == "shell"),
            "unrelated tool kept: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "deliver_file"),
            "deliver_file must be dropped on a non-ACP turn: {names:?}"
        );
    }

    #[tokio::test]
    async fn acp_assembly_keeps_deliver_file() {
        let names = assemble_names_with_acp_delivery(true).await;
        assert!(
            names.iter().any(|n| n == "deliver_file"),
            "deliver_file must survive on the ACP turn path: {names:?}"
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
                acp_delivery: false,
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
        mock_mcp_http_server_with_tools(&[("echo", "echo"), ("add_numbers", "add")]).await
    }

    /// Deterministic MCP HTTP server advertising the given `(name, description)`
    /// tools. Exercised through the real `connect_all` + `from_registry` path so
    /// the deferred set is built from the wire, not hand-assembled.
    async fn mock_mcp_http_server_with_tools(tools: &[(&str, &str)]) -> wiremock::MockServer {
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
        let tool_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|(name, desc)| {
                serde_json::json!({
                    "name": name,
                    "description": desc,
                    "inputSchema": {"type": "object"}
                })
            })
            .collect();
        Mock::given(method("POST"))
            .and(body_partial_json(
                serde_json::json!({"method":"tools/list"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc":"2.0","id":2,"result":{"tools":tool_json}
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
                acp_delivery: false,
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
            acp_delivery: false,
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

    #[tokio::test]
    async fn assemble_deferred_mcp_excludes_denied_tool_from_prompt_and_search() {
        // Regression for the deferred-MCP access-policy omission, driven at the
        // production assembly boundary: a real deferred MCP server advertising
        // one allowed and one denied tool, a policy that denies the latter, and
        // then assertions on the assembled prompt section AND the assembled
        // `tool_search` execution/activation.
        //
        // The setup pins two preconditions so the negative assertions below
        // cannot pass vacuously:
        // - BOTH advertised tools are asserted to have entered the source
        //   registry before assembly (the denied tool is not absent because it
        //   was never advertised).
        // - The allowed tool is positively asserted to become ACTIVATED through
        //   `tool_search`, not merely rendered as a schema.
        //
        // The keyword-search negative control is the observable signal for the
        // `filter_by_policy(...)` handoff: with the pre-filtered set, a search
        // for the denied keyword returns "No matching deferred tools found."
        // because the denied stub was removed before `ToolSearchTool` was
        // constructed. If a future edit feeds the UNfiltered stub set to the
        // consumers (while leaving their defense-in-depth `with_access_policy`
        // / prompt re-filter intact), the denied stub is still present in the
        // searchable set and the search returns an empty `<functions>` block
        // instead — failing this assertion.
        let server = mock_mcp_http_server_with_tools(&[
            ("allowed", "Allowed tool"),
            ("denied", "Denied tool"),
        ])
        .await;

        use zeroclaw_config::schema::{
            AliasedAgentConfig, McpBundleConfig, McpServerConfig, McpTransport, RiskProfileConfig,
        };

        let mut config = Config::default();
        config.mcp.enabled = true;
        config.mcp.deferred_loading = true;
        config.mcp.servers = vec![McpServerConfig {
            name: "test-srv".into(),
            transport: McpTransport::Http,
            url: Some(server.uri()),
            ..Default::default()
        }];
        config.mcp_bundles.insert(
            "test-bundle".into(),
            McpBundleConfig {
                servers: vec!["test-srv".into()],
                exclude: Vec::new(),
            },
        );
        config
            .risk_profiles
            .insert("test-profile".into(), RiskProfileConfig::default());
        config.agents.insert(
            "test-agent".into(),
            AliasedAgentConfig {
                enabled: true,
                model_provider: "openai.test-provider".into(),
                risk_profile: "test-profile".into(),
                mcp_bundles: vec!["test-bundle".into()],
                ..Default::default()
            },
        );

        // The denial is the policy-bearing input: `mcp_tool_access_policy`
        // reads `SecurityPolicy.excluded_tools`, and `filter_by_policy` applies
        // it to the deferred set before prompt and search are built.
        let security = Arc::new(SecurityPolicy {
            workspace_dir: std::env::temp_dir(),
            excluded_tools: Some(vec!["test-srv__denied".into()]),
            ..SecurityPolicy::default()
        });

        // Setup: connect the source registry through the real
        // `McpRegistry::connect_all` + `DeferredMcpToolSet::from_registry` path
        // and assert BOTH advertised tools entered it. Feeding this same
        // registry into `assemble` (via `mcp_registry`) proves the denied tool
        // started from a non-empty registry — the negative assertions below
        // cannot pass just because the denied tool was never advertised.
        let agent_mcp_servers = config.mcp_servers_for_agent("test-agent");
        let registry = Arc::new(
            tools::McpRegistry::connect_all(&agent_mcp_servers)
                .await
                .expect("source registry must connect to the mock MCP server"),
        );
        {
            let names = registry.tool_names();
            assert!(
                names.contains(&"test-srv__allowed".to_string()),
                "source registry must advertise the allowed tool: {names:?}"
            );
            assert!(
                names.contains(&"test-srv__denied".to_string()),
                "source registry must advertise the denied tool: {names:?}"
            );
        }

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            ScopedToolRegistry::assemble(ScopedAssembly {
                config: &config,
                agent_alias: "test-agent",
                security: &security,
                built: built_with(Vec::new()),
                skills: &[],
                runtime: Arc::new(crate::platform::NativeRuntime::new()),
                caller_allowed: None,
                connect_mcp: true,
                connect_peripherals: false,
                exclude_memory: false,
                acp_delivery: false,
                list_deferred_mcp_specs: false,
                emit_assembly_logs: false,
                mcp_registry: Some(Arc::clone(&registry)),
            }),
        )
        .await
        .expect("assemble must not hang");

        // 1. The assembled prompt section advertises the allowed stub and omits
        //    the denied one (proof at the model-visible boundary).
        let deferred_section = out.deferred_section();
        assert!(
            deferred_section.contains("test-srv__allowed"),
            "allowed stub must be advertised in the deferred prompt section: {deferred_section}"
        );
        assert!(
            !deferred_section.contains("test-srv__denied"),
            "denied tool must not appear in deferred prompt section: {deferred_section}"
        );

        // 2. The assembled registry contains a real `tool_search`: the denied
        //    tool must be neither returned nor activated through it.
        let activated_handle = out.activated_handle.clone();
        let tools = out.registry.into_inner();
        let tool_search = tools
            .iter()
            .find(|t| t.name() == "tool_search")
            .expect("deferred mode with an admitted stub must assemble tool_search");

        let denied_keyword = tool_search
            .execute(serde_json::json!({"query": "denied"}))
            .await
            .expect("tool_search must execute");
        // Observable signal for the `filter_by_policy(...)` handoff: with the
        // pre-filtered set the denied stub is not searchable, so the query
        // finds nothing. If the UNfiltered set leaked into `ToolSearchTool`,
        // the search would find the denied stub and return an empty
        // `<functions>` block instead — failing this exact-output assertion.
        assert_eq!(
            denied_keyword.output, "No matching deferred tools found.",
            "keyword search for the denied tool must find nothing because the \
             pre-filtered stub set carries no denied stub; an empty <functions> \
             block would mean the unfiltered set leaked through filter_by_policy"
        );

        let denied_select = tool_search
            .execute(serde_json::json!({"query": "select:test-srv__denied"}))
            .await
            .expect("tool_search must execute");
        assert!(
            !denied_select
                .output
                .contains("\"name\": \"test-srv__denied\""),
            "select must not return the denied tool as a function: {}",
            denied_select.output
        );
        assert!(
            denied_select.output.contains("Not found: test-srv__denied"),
            "select must route the denied tool into the not-found list: {}",
            denied_select.output
        );

        {
            let activated = activated_handle
                .as_ref()
                .expect("tool_search registers the activation handle");
            let activated = activated.lock().unwrap();
            assert!(
                !activated.is_activated("test-srv__denied"),
                "denied tool must never be activated"
            );
        }

        // 3. The allowed tool stays reachable through the same search surface
        //    (the filter narrows, it does not disable search wholesale).
        let allowed_hit = tool_search
            .execute(serde_json::json!({"query": "allowed"}))
            .await
            .expect("tool_search must execute");
        assert!(
            allowed_hit.output.contains("test-srv__allowed"),
            "allowed tool must remain searchable: {}",
            allowed_hit.output
        );

        // 4. Positive activation control: selecting the allowed tool must
        //    ACTIVATE it (not merely render its schema). This keeps the test
        //    from passing when schema rendering still works but the
        //    activation path is broken.
        let allowed_select = tool_search
            .execute(serde_json::json!({"query": "select:test-srv__allowed"}))
            .await
            .expect("tool_search must execute");
        assert!(
            allowed_select.output.contains("test-srv__allowed"),
            "select must return the allowed tool schema: {}",
            allowed_select.output
        );
        {
            let activated = activated_handle
                .as_ref()
                .expect("tool_search registers the activation handle");
            let activated = activated.lock().unwrap();
            assert!(
                activated.is_activated("test-srv__allowed"),
                "the allowed tool must be activated after a select, not just schema-rendered"
            );
        }
    }
}
