//! `ScopedToolRegistry` - the one gated seam that mints the per-agent tool set.
//!
//! Epic A of the agent-policy enforcement-unification program (see the contributing
//! page `agent-policy-parity-harness.md`). The per-agent tool registry has
//! historically been assembled by hand at six construction sites (channels
//! orchestrator, runtime `run` / `process_message`, `Agent::from_config`, the
//! gateway, and the delegate independent-target builder), each re-applying the
//! policy itself. That is why the built-in filter and the MCP scoping had to be
//! patched per-site (#7064, #6960, #8120) and why the gateway's `/api/tools`
//! listings misreported the tool set a real turn receives (its live chat resolves
//! through `process_message`, which filters; its listing registries never did).
//!
//! [`ScopedToolRegistry::assemble`] is the seam that ends the copying: it applies,
//! in order, peripherals, the built-in `allowed_tools`/`excluded_tools` filter, the
//! ACP memory strip, per-agent MCP server scoping (`mcp_bundles`, omission is not a
//! grant) with per-tool gating plus the MCP capability tools and pinned-resources
//! section, and skill registration under the same `SecurityPolicy`.
//!
//! Cut-over status: the gateway's two registry builders are the first consumers;
//! the remaining sites migrate one PR at a time, after which the engine's tools
//! field seals to this newtype and handing it an unfiltered registry becomes a
//! compile error instead of a review-checklist item. Until that seal lands, the
//! guarantee is that every path routed through `assemble` shares one
//! implementation; paths not yet routed remain hand-rolled by convention.
//!
//! Per-site variation is expressed as DATA, never as "skip a security step": the
//! knobs are documented divergences - a per-run caller allowlist that only narrows,
//! `connect_mcp` (ACP fast-boot), `connect_peripherals` (listing-only surfaces must
//! not open hardware), the ACP memory-tool strip, and `emit_assembly_logs` (only
//! execution paths emit the assembly audit records; listing surfaces stay quiet).
//! With `process_message` now routed through `assemble`, every construction path
//! shares one built-in filter: the plain `allowed_tools`/`excluded_tools` policy
//! filter that `run` and the orchestrator already used. This retired the former
//! `filter_channel_builtin_tools`, which admitted the canonical read-only defaults
//! past `allowed_tools` at non-Full autonomy on the gateway live-chat and
//! peer-delegation paths - a narrowing, since no construction path now bypasses
//! `allowed_tools`.

use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

use crate::agent::loop_::{
    apply_policy_tool_filter, eager_mcp_tool_allowed, load_peripheral_tools,
    mcp_allowed_tool_count, mcp_tool_access_policy, preactivate_always_filter_groups,
    register_eager_mcp_tool_if_allowed,
};
use crate::skills::Skill;
use crate::tools::{
    self, ActivatedToolSet, AllToolsResult, DelegateParentToolsHandle, PerToolChannelHandle, Tool,
    register_skill_tools_with_context_and_runtime,
};

/// A per-agent tool registry that has been scoped and gated. The inner field is
/// private and production code can only mint one through
/// [`ScopedToolRegistry::assemble`]. Today (the unsealed P1 phase) the engine still
/// takes `&[Box<dyn Tool>]`, so callers dissolve the type via [`Deref`] or
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
    /// Listing-only divergence: when deferred MCP loading is on, the live turn
    /// paths collapse the whole MCP set into a single `tool_search` stub to save
    /// prompt tokens. Enumeration surfaces (the gateway's `/api/tools` registries)
    /// pass `true` so each policy-allowed MCP tool is ALSO listed by its own
    /// `<server>__<tool>` spec - matching eager mode, so the dashboard Tools
    /// screen shows the same tool set regardless of the deferred-loading knob
    /// (#8302). Execution surfaces pass `false`; deferral is unchanged for them.
    pub list_deferred_mcp_specs: bool,
    /// Emit the per-step assembly diagnostics (peripheral count, the built-in
    /// filter before/after audit line, and the MCP init/deferred/eager counts) as
    /// INFO records. Execution paths (`run`, `process_message`, ...) pass `true` so
    /// operators keep the "why didn't my tool appear / did policy drop tools"
    /// breadcrumbs the sites used to log inline; listing-only surfaces (gateway
    /// `/api/tools`, ACP) pass `false` so a registry no turn runs against does not
    /// emit spurious "MCP: N registered" / "Peripheral tools added" lines.
    pub emit_assembly_logs: bool,
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
    /// (the registry and prompt surfaces move together). Callers that inject one
    /// combined MCP prompt block (`run`, `process_message`) append [`Self::pinned_section`] onto this via
    /// `append_pinned_mcp_section`; `from_config` threads it into the Agent's separate
    /// `mcp_deferred_section` slot alongside `pinned_section`.
    pub deferred_section: String,
    /// The pinned-MCP-resources system-prompt section on its own. Empty when no pinned
    /// resources are granted. Kept separate from [`Self::deferred_section`] so callers
    /// with two distinct prompt slots (`from_config`'s Agent) inject each without
    /// duplication; single-block callers append it onto `deferred_section`.
    pub pinned_section: String,
    /// Live handle to the activated deferred-MCP set (present only when a deferred
    /// `tool_search` tool was registered).
    pub activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>>,
    /// MCP-origin ground truth for `tool_filter_groups` (#6699): every tool name
    /// this assembly admitted from MCP — the registry's `<server>__<tool>` names
    /// (covering eager wrappers, deferred stubs, and later `tool_search`
    /// activations, which share those names) plus the registered capability
    /// tools (`mcp_resources` / `mcp_prompts`). The per-turn filter gates
    /// classify by membership here, never by name shape, because skill tools
    /// use the same `<x>__<y>` convention. Created at the seam: the registry it
    /// derives from is immutable after `connect_all` and is consumed by this
    /// assembly, and the set is a superset of the *registered* tools (it may
    /// retain policy-skipped names) — safe because both gates intersect it with
    /// the live registry, so do not "tighten" it to admitted-only without
    /// treating that as a behavior change. Any future MCP registration source
    /// added to `assemble` MUST extend this set; a missed extension silently
    /// un-filters those tools (the same no-op bug class as #6699). Empty when
    /// MCP is disabled, unconfigured, or failed to connect — the gates then
    /// classify nothing as MCP and `tool_filter_groups` is inert.
    pub mcp_tool_names: HashSet<String>,
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
        // MCP-origin ground truth for the tool_filter_groups gates (#6699); see
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
            match tools::McpRegistry::connect_all(&agent_mcp_servers).await {
                Ok(registry) => {
                    let registry = Arc::new(registry);
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
                            // registry) and were the only names the pre-#6699
                            // prefix gate matched — they stay classifiable so a
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
                                if !eager_mcp_tool_allowed(&stub.prefixed_name, mcp_policy.as_ref())
                                {
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
                            let activated =
                                Arc::new(std::sync::Mutex::new(ActivatedToolSet::new()));
                            activated_handle = Some(Arc::clone(&activated));
                            // Pre-activate `mode = "always"` tool_filter_groups
                            // entries (#6699) before `ToolSearchTool::new` consumes
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
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "agent_alias": agent_alias,
                                            "count": preactivated_names.len(),
                                        })
                                    ),
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
                Err(err) => {
                    // Non-fatal (the assembly proceeds without MCP), but an ERROR
                    // with structured attrs - parity with the run/process_message
                    // connect-failure logging.
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_category(::zeroclaw_log::EventCategory::Tool)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "agent_alias": agent_alias,
                                "error": format!("{err}"),
                            })),
                        "MCP registry failed to initialize (assembly proceeds without MCP)"
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

        // 6. Final denylist sweep. The documented contract is that `excluded_tools`
        //    ALWAYS subtracts (docs/book/src/tools/mcp.md, tools/overview.md,
        //    agents/delegation.md). The step-2 built-in filter and the step-4 MCP
        //    policy already drop excluded EAGER tools, but two tools are registered
        //    AFTER the built-in filter and so escaped it: the deferred-MCP `tool_search`
        //    wrapper (pushed in step 4) and skill wrappers (step 5). Enforce the
        //    denylist once more here so no explicitly-excluded tool name survives on
        //    any construction path. `allowed_tools` is deliberately NOT re-applied:
        //    scoped elevation wrappers must survive an allowlist that dropped their
        //    raw target (only the exact excluded name is removed).
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

    /// Regression pin for #7733 at the seam (ported from the gateway's
    /// `append_scoped_mcp_tools_is_a_noop_for_agent_without_bundles` when the
    /// gateway cut over to `assemble`): an agent with NO `mcp_bundles` grant
    /// must get no MCP tools even when `[[mcp.servers]]` is non-empty and MCP
    /// is enabled - omission is not a grant. Bounded by a timeout so a
    /// regression that tries to spawn the phantom stdio server fails fast
    /// instead of hanging CI.
    ///
    /// Note (carried from the original): this is a behavior-pinning test, not a
    /// mutation-discriminating one - the phantom stdio server would also yield
    /// zero tools if the scoping regressed to `&config.mcp.servers` (the connect
    /// fails non-fatally). The stronger guards are
    /// `crates/zeroclaw-channels/tests/orchestrator_mcp_scope.rs` and the
    /// resolver-level pins in `zeroclaw-config`.
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
            }),
        )
        .await
        .expect("assemble must not hang");
        out.registry.iter().map(|t| t.name().to_string()).collect()
    }

    /// Regression pin for #8302: a bundle-granted MCP server's individual tools
    /// must appear in the `/api/tools` listing registry that `assemble` mints, in
    /// BOTH eager and deferred loading modes. In v0.8.1 the listing was eager and
    /// surfaced each `<server>__<tool>` spec; deferred loading collapsed the whole
    /// server into a single `tool_search` stub, so the dashboard Tools screen
    /// stopped showing MCP tools even for a correctly-bundled agent. The listing
    /// must also match eager mode exactly: the deferral-internal `tool_search`
    /// helper is never invoked from a listing registry and must not leak onto the
    /// dashboard. Two bundled servers guard the multi-server case from #8302.
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

    #[tokio::test]
    async fn assemble_without_mcp_yields_empty_origin_set() {
        // No MCP connected => nothing is classified MCP-origin, so the
        // tool_filter_groups gates treat every tool as a pass-through
        // built-in/skill and the groups are inert by construction (#6699).
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
        })
        .await;
        assert!(
            out.mcp_tool_names.is_empty(),
            "no-MCP assembly must export an empty origin set; got {:?}",
            out.mcp_tool_names
        );
    }
}
