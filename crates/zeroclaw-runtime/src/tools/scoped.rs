//! `ScopedToolRegistry` - the one gated seam that mints the per-agent tool set.
//!
//! Assembly applies peripherals, built-in policy, ACP memory stripping, MCP
//! scope and policy, capability tools, pinned resources, and skills in that
//! order. This is the only production construction path: the engine's
//! turn-entry carriers (`ResolvedIo` / `ResolvedAgentExecution`), `Agent`, and
//! the channel runtime context all store the sealed type, so an unscoped
//! registry cannot reach a turn without going through [`ScopedToolRegistry::assemble`].

use std::collections::HashSet;
use std::sync::Arc;

use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

use crate::agent::loop_::{
    append_pinned_mcp_section, eager_mcp_tool_allowed, load_peripheral_tools,
    mcp_allowed_tool_count, mcp_tool_access_policy, preactivate_always_filter_groups,
    register_eager_mcp_tool_if_allowed,
};
use crate::skills::Skill;
use crate::tools::skill_tool::{PreparedSkill, PreparedSkillBuiltin, SlotId, TargetRef};
use crate::tools::{
    self, ActivatedToolSet, AllToolsResult, DelegateParentToolsHandle, PerToolChannelHandle, Tool,
};

// Stable positions survive every filter until the one final emission. Only the
// concrete pipeline is kept after an outer filter: skills historically resolve
// it through a separate context even when it is not a top-level callable.
struct BuildEntry {
    value: Option<BuildSlot>,
    top_level: bool,
}

enum BuildSlot {
    Ready(Arc<dyn Tool>),
    Pipeline(tools::PipelineTool),
    Elevated {
        prepared: PreparedSkillBuiltin,
        target: TargetRef,
    },
}

impl BuildSlot {
    fn name(&self) -> &str {
        match self {
            Self::Ready(tool) => tool.name(),
            Self::Pipeline(tool) => tool.name(),
            Self::Elevated { prepared, .. } => &prepared.tool_name,
        }
    }

    fn spec(&self) -> zeroclaw_api::tool::ToolSpec {
        match self {
            Self::Ready(tool) => tool.spec(),
            Self::Pipeline(tool) => tool.spec(),
            Self::Elevated { prepared, .. } => prepared.spec(),
        }
    }
}

fn is_guest(tool: &dyn Tool) -> bool {
    matches!(
        tool.role(),
        zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::WasmPlugin)
    )
}

fn is_search_guest(tool: &dyn Tool) -> bool {
    is_guest(tool) && tool.name() == tools::ToolSearchTool::NAME
}

struct BuildRegistry {
    entries: Vec<BuildEntry>,
}

impl BuildRegistry {
    fn push(&mut self, value: BuildSlot) -> SlotId {
        let id = SlotId(self.entries.len());
        self.entries.push(BuildEntry {
            value: Some(value),
            top_level: true,
        });
        id
    }

    fn push_ready(&mut self, tool: Box<dyn Tool>) {
        self.push(BuildSlot::Ready(Arc::from(tool)));
    }

    fn iter(&self) -> impl Iterator<Item = (SlotId, &BuildSlot)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .value
                    .as_ref()
                    .filter(|_| entry.top_level)
                    .map(|value| (SlotId(index), value))
            })
    }

    fn len(&self) -> usize {
        self.iter().count()
    }

    fn retain(&mut self, mut keep: impl FnMut(&BuildSlot) -> bool) {
        for entry in &mut self.entries {
            if entry.top_level && entry.value.as_ref().is_some_and(|value| !keep(value)) {
                entry.top_level = false;
                if !matches!(entry.value.as_ref(), Some(BuildSlot::Pipeline(_))) {
                    entry.value = None;
                }
            }
        }
    }

    fn register_mcp(
        &mut self,
        wrapper: Arc<dyn Tool>,
        delegate_handle: Option<&DelegateParentToolsHandle>,
        policy: Option<&zeroclaw_tools::tool_search::ToolAccessPolicy>,
    ) -> bool {
        let mut added = Vec::new();
        let registered =
            register_eager_mcp_tool_if_allowed(wrapper, &mut added, delegate_handle, policy);
        for tool in added {
            self.push_ready(tool);
        }
        registered
    }

    /// Called only after an actual native creator succeeds. No native helper
    /// is inserted into the nested views that never originally contained it.
    fn claim_search(
        &mut self,
        unfiltered: &mut Vec<Arc<dyn Tool>>,
        context: &mut Vec<Arc<dyn Tool>>,
        delegate_handle: Option<&DelegateParentToolsHandle>,
    ) {
        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record(
            "claim",
            "tool_search",
            unfiltered.len(),
            context.len(),
        );
        for entry in &mut self.entries {
            let retired = match entry.value.as_ref() {
                Some(BuildSlot::Ready(tool)) => is_search_guest(tool.as_ref()),
                Some(BuildSlot::Elevated {
                    target: TargetRef::Existing(tool),
                    ..
                }) => is_search_guest(tool.as_ref()),
                _ => false,
            };
            if retired {
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                tools::late_tool_search_observer::record("claim-retire", "slot", 0, 0);
                entry.value = None;
                entry.top_level = false;
            } else if matches!(entry.value.as_ref(), Some(BuildSlot::Pipeline(_)))
                && let Some(BuildSlot::Pipeline(tool)) = entry.value.take()
            {
                entry.value = Some(BuildSlot::Pipeline(tool.retain_context_tools(|child| {
                    let retired = is_search_guest(child);
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record(
                        "pipeline-child",
                        if retired { "retire" } else { "keep" },
                        tools::late_tool_search_observer::borrowed_id(child),
                        0,
                    );
                    !retired
                })));
            }
        }
        unfiltered.retain(|tool| {
            let retired = is_search_guest(tool.as_ref());
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record(
                "unfiltered-child",
                if retired { "retire" } else { "keep" },
                tools::late_tool_search_observer::arc_id(tool),
                0,
            );
            !retired
        });
        context.retain(|tool| {
            let retired = is_search_guest(tool.as_ref());
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record(
                "context-child",
                if retired { "retire" } else { "keep" },
                tools::late_tool_search_observer::arc_id(tool),
                0,
            );
            !retired
        });
        if let Some(parent) = delegate_handle {
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record(
                "parent-handle",
                "claim",
                tools::late_tool_search_observer::arc_id(parent),
                0,
            );
            parent.write().retain(|tool| {
                let retired = is_search_guest(tool.as_ref());
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                tools::late_tool_search_observer::record(
                    "parent-child",
                    if retired { "retire" } else { "keep" },
                    tools::late_tool_search_observer::arc_id(tool),
                    0,
                );
                !retired
            });
        }
    }

    fn seal(mut self, pipeline_id: Option<SlotId>) -> Vec<Box<dyn Tool>> {
        let pipeline_needed = pipeline_id.is_some_and(|id| {
            self.entries[id.0].top_level
                || self.iter().any(|(_, value)| {
                    matches!(
                        value,
                        BuildSlot::Elevated {
                            target: TargetRef::Pipeline(target),
                            ..
                        } if *target == id
                    )
                })
        });
        // The sole owned value becomes an Arc once, after every creator claim.
        // An unused pipeline is dropped without publishing a replacement.
        let pipeline = pipeline_id.and_then(|id| match self.entries[id.0].value.take() {
            Some(BuildSlot::Pipeline(tool)) if pipeline_needed => {
                let tool = Arc::new(tool);
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                tools::late_tool_search_observer::record(
                    "pipeline-publish",
                    "sole",
                    id.0,
                    tools::late_tool_search_observer::arc_id(&tool),
                );
                Some((id, tool))
            }
            _ => None,
        });
        let mut tools: Vec<Box<dyn Tool>> = Vec::new();
        for (index, entry) in self.entries.into_iter().enumerate() {
            if !entry.top_level {
                continue;
            }
            if let Some((id, tool)) = &pipeline
                && *id == SlotId(index)
            {
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                tools::late_tool_search_observer::record(
                    "pipeline-top",
                    "same-arc",
                    tools::late_tool_search_observer::arc_id(tool),
                    0,
                );
                tools.push(Box::new(tools::ArcToolRef(
                    Arc::clone(tool) as Arc<dyn Tool>
                )));
                continue;
            }
            match entry.value {
                Some(BuildSlot::Ready(tool)) => tools.push(Box::new(tools::ArcToolRef(tool))),
                Some(BuildSlot::Elevated { prepared, target }) => {
                    let target = match target {
                        TargetRef::Existing(tool) => Some(tool),
                        // No name fallback: this reference can only bind to the
                        // original slot, or fail closed if that slot is absent.
                        TargetRef::Pipeline(target) => pipeline.as_ref().and_then(|(id, tool)| {
                            (*id == target).then(|| Arc::clone(tool) as Arc<dyn Tool>)
                        }),
                    };
                    if let Some(target) = target {
                        tools.push(Box::new(prepared.bind(target)));
                    }
                }
                _ => {}
            }
        }
        tools
    }
}

/// A per-agent tool registry that has been scoped and gated. The inner field is
/// private and production code can only mint one through
/// [`ScopedToolRegistry::assemble`]. The engine's turn-entry carriers
/// (`ResolvedIo` / `ResolvedAgentExecution`), `Agent`, and the channel runtime
/// context store this type directly, so handing the engine an unfiltered
/// registry is a compile error, not a review-checklist item. Read sites are
/// unchanged: the registry [`std::ops::Deref`]s to the same `[Box<dyn Tool>]`
/// slice the raw `Vec` used to expose.
pub struct ScopedToolRegistry(
    Vec<Box<dyn Tool>>,
    Option<Arc<std::sync::Mutex<ActivatedToolSet>>>,
);

impl std::ops::Deref for ScopedToolRegistry {
    type Target = [Box<dyn Tool>];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ScopedToolRegistry {
    /// Consume the assembled registry into the owned `Vec` for non-turn
    /// consumers such as the gateway's listing-only registry. Turn carriers
    /// accept only the sealed type.
    pub fn into_inner(self) -> Vec<Box<dyn Tool>> {
        self.0
    }

    /// Narrow an ALREADY-sealed registry in place. A passthrough to
    /// [`Vec::retain`] on the private inner vector. This is a mutator on an
    /// existing sealed registry (it removes tools, never adds), so it cannot
    /// mint a scope from raw tools and does not weaken the seal - the only way
    /// to obtain a `ScopedToolRegistry` to call this on is still
    /// [`Self::assemble`] (or the test-only constructor).
    pub(crate) fn retain(&mut self, f: impl FnMut(&Box<dyn Tool>) -> bool) {
        self.0.retain(f);
        self.refresh_builtin_schemas();
    }

    /// Schema exposure is derived from the current scoped registry and the
    /// selected-name state. It does not change which tools can execute.
    pub fn hidden_builtin_names(&self) -> HashSet<String> {
        self.1.as_ref().map_or_else(HashSet::new, |state| {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .hidden_builtin_names()
        })
    }

    fn refresh_builtin_schemas(&self) {
        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "post-seal-refresh", 8, 0);
        let Some(state) = &self.1 else { return };
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.iter().any(|tool| tool.name() == "tool_search") {
            state.refresh_deferred_builtin_specs(self.0.iter().map(|tool| tool.spec()).collect());
        } else {
            // A later caller ceiling can remove discovery itself. Keep the
            // surviving executable tools usable through their eager schemas.
            state.set_deferred_builtin_specs(Vec::new());
        }
    }

    /// Rebind the memory-backed tools of an ALREADY-sealed registry to a new
    /// backend handle. Session memory follows its owner: when a session is
    /// pinned to a principal's private plane the memory tools — which each
    /// captured a clone of the shared handle at assembly — must be re-pointed
    /// at the routed handle, or an owned session would keep issuing shared-plane
    /// recall/store/export/delete while the agent reports its memory as private.
    ///
    /// This replaces the SAME named memory tools in place with fresh instances
    /// over `memory`; it never introduces a new tool name, so the surface the
    /// seal admitted is unchanged. Any memory tool already withdrawn by policy
    /// narrowing stays withdrawn (only tools currently present are rebound).
    pub(crate) fn rebind_memory_tools(
        &mut self,
        memory: Arc<dyn zeroclaw_memory::Memory>,
        security: Arc<SecurityPolicy>,
    ) {
        use zeroclaw_tools::{
            memory_export::MemoryExportTool, memory_forget::MemoryForgetTool,
            memory_purge::MemoryPurgeTool, memory_recall::MemoryRecallTool,
            memory_store::MemoryStoreTool,
        };
        for tool in self.0.iter_mut() {
            let replacement: Option<Box<dyn Tool>> = match tool.name() {
                "memory_store" => Some(Box::new(MemoryStoreTool::new(
                    Arc::clone(&memory),
                    Arc::clone(&security),
                ))),
                "memory_recall" => Some(Box::new(MemoryRecallTool::new(Arc::clone(&memory)))),
                "memory_forget" => Some(Box::new(MemoryForgetTool::new(
                    Arc::clone(&memory),
                    Arc::clone(&security),
                ))),
                "memory_export" => Some(Box::new(MemoryExportTool::new(Arc::clone(&memory)))),
                "memory_purge" => Some(Box::new(MemoryPurgeTool::new(
                    Arc::clone(&memory),
                    Arc::clone(&security),
                ))),
                _ => None,
            };
            if let Some(replacement) = replacement {
                *tool = replacement;
            }
        }
        self.refresh_builtin_schemas();
    }

    /// Test-only constructor that mints a registry directly from raw tools,
    /// bypassing [`Self::assemble`]. Gated to `test` (this crate's own unit
    /// tests) OR the `test-util` feature (so OTHER crates' test builds -
    /// notably `zeroclaw-channels`, whose `ChannelRuntimeContext` fixtures need
    /// a sealed registry - can construct one). The `test-util` feature is never
    /// enabled by a production dependency edge, so this raw mint does not exist
    /// in shipped builds and the seal holds where it matters.
    #[cfg(any(test, feature = "test-util"))]
    pub fn from_raw_for_test(tools: Vec<Box<dyn Tool>>) -> Self {
        Self(tools, None)
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
    /// The same pinned resources as attributed blocks (`<server>__<uri>` key plus the
    /// rendered text), for holders that must be able to withdraw a block when the
    /// caller's tool selector later narrows past its key. Always renders to exactly
    /// [`Self::pinned_section`].
    pinned_blocks: Vec<tools::mcp_context::PinnedResourceBlock>,
    /// Live handle to the activated deferred-MCP set (present only when a deferred
    /// `tool_search` tool was registered).
    pub activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>>,
    /// The same search instance exposed in the registry, for session narrowing.
    pub tool_search_handle: Option<Arc<tools::ToolSearchTool>>,
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

    /// The pinned resources as attributed blocks, for a holder that re-renders the
    /// section itself and prunes blocks on live tool narrowing (`Agent`).
    pub fn pinned_blocks(&self) -> &[tools::mcp_context::PinnedResourceBlock] {
        &self.pinned_blocks
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

        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "factory", 1, 0);
        let AllToolsResult {
            tools: tools_registry,
            reserved_host_names: mut host_tool_names,
            delegate_handle,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            mut unfiltered_tool_arcs,
            // Test-only capture of the concrete delegate instance; `assemble`
            // has no use for it and must keep destructuring exhaustively so a
            // new field cannot be silently dropped here.
            #[cfg(test)]
                delegate_tool: _,
        } = built;

        // 1. Peripherals. Loading CONNECTS hardware (serial opens are exclusive for
        //    real devices), so this is gated: execution surfaces pass
        //    `connect_peripherals: true`; listing-only surfaces pass `false` and
        //    enumerate without holding devices. They are loaded before guest
        //    names are settled and registered after the guests.
        let peripheral_tools = if connect_peripherals {
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
            peripheral_tools
        } else {
            Vec::new()
        };

        // Builtin declarations own their names independently of whether the
        // current selection constructed them. Prebuilt inputs also retain the
        // canonical deferred/context-withdrawn owners.
        let (plugin_tools, mut tools_registry): (Vec<_>, Vec<_>) = tools_registry
            .into_iter()
            .partition(|tool| is_guest(tool.as_ref()));
        host_tool_names.extend(tools_registry.iter().map(|tool| tool.name().to_string()));
        tools::reserve_deferred_host_names(config, &mut host_tool_names);
        // Peripheral and skill tools register after the guests, so the plugin
        // loader cannot see their names. A skill tool's composed name loses its
        // `__` separator when it is shortened, so the MCP-namespace refusal does
        // not always cover it.
        host_tool_names.extend(peripheral_tools.iter().map(|tool| tool.name().to_string()));
        host_tool_names.extend(skills.iter().flat_map(|skill| {
            skill
                .tools
                .iter()
                .map(|tool| tools::skill_tool::composed_tool_name(&skill.name, &tool.name))
        }));
        tools_registry.extend(plugin_tools.into_iter().filter(|tool| {
            let taken = host_tool_names.contains(tool.name());
            if taken && emit_assembly_logs {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Load)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "tool": tool.name(),
                            "error_key": "plugin_tool_name_conflict",
                        })),
                    "Plugin tool conflicts with a host tool"
                );
            }
            !taken
        }));
        unfiltered_tool_arcs
            .retain(|tool| !is_guest(tool.as_ref()) || !host_tool_names.contains(tool.name()));
        if let Some(parent) = delegate_handle.as_ref() {
            parent
                .write()
                .retain(|tool| !is_guest(tool.as_ref()) || !host_tool_names.contains(tool.name()));
        }

        let mut builtin_names: HashSet<String> = tools_registry
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();

        // Peripheral tools follow the guests and stay out of the views built above.
        tools_registry.extend(peripheral_tools);

        let mut tools_registry = BuildRegistry {
            entries: tools_registry
                .into_iter()
                .map(|tool| BuildEntry {
                    value: Some(BuildSlot::Ready(Arc::from(tool))),
                    top_level: true,
                })
                .collect(),
        };

        // Mint once at the original point, but keep the concrete value owned
        // until late native ownership claims have narrowed its children.
        let mut context_filtered_tool_arcs: Vec<Arc<dyn Tool>> = unfiltered_tool_arcs
            .iter()
            .filter(|tool| tool_allowed_in_context(tool.name(), exclude_memory, acp_delivery))
            .cloned()
            .collect();
        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        {
            tools::late_tool_search_observer::record(
                "phase",
                "context",
                2,
                context_filtered_tool_arcs.len(),
            );
            for tool in &context_filtered_tool_arcs {
                tools::late_tool_search_observer::record(
                    "context-entry",
                    "original",
                    tools::late_tool_search_observer::arc_id(tool),
                    0,
                );
            }
        }
        let pipeline_id = (config.pipeline.enabled
            && config.tools.is_enabled(tools::PipelineTool::NAME))
        .then(|| {
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record(
                "constructor-enter",
                "pipeline",
                context_filtered_tool_arcs.len(),
                0,
            );
            let tool = tools::PipelineTool::with_access_policy(
                config.pipeline.clone(),
                context_filtered_tool_arcs.clone(),
                zeroclaw_tools::tool_search::ToolAccessPolicy::from_security(
                    security.allowed_tools.as_deref(),
                    security.excluded_tools.as_deref(),
                    caller_allowed,
                ),
            );
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record("constructor-return", "pipeline", 0, 0);
            builtin_names.insert(tool.name().to_string());
            tools_registry.push(BuildSlot::Pipeline(tool))
        });

        // 2. Built-in allow/deny filter (uniform: the gateway used to skip it entirely).
        //    `caller_allowed` narrows on top of the policy, for the `run` path only.
        let before_filter = tools_registry.len();
        tools_registry.retain(|tool| {
            let name = tool.name();
            let policy_ok = security.is_tool_allowed(name);
            let caller_ok = caller_allowed.is_none_or(|list| list.iter().any(|n| n == name));
            policy_ok && caller_ok
        });
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
        let mut pinned_blocks = Vec::new();
        let mut activated_handle: Option<Arc<std::sync::Mutex<ActivatedToolSet>>> = None;
        let mut tool_search_handle = None;
        let mut mcp_elevation_arcs: Vec<Arc<dyn Tool>> = Vec::new();
        // MCP-origin ground truth for the tool_filter_groups gates; see
        // the `ScopedAssembled::mcp_tool_names` field doc for the contract.
        let mut mcp_tool_names: HashSet<String> = HashSet::new();

        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "mcp", 3, 0);
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
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record(
                        "mcp-registry",
                        "override",
                        tools::late_tool_search_observer::arc_id(shared),
                        0,
                    );
                    Some(Arc::clone(shared))
                } else {
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record(
                        "mcp-connect",
                        "actual",
                        agent_mcp_servers.len(),
                        0,
                    );
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
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                tools::late_tool_search_observer::record(
                    "mcp-registry",
                    "used",
                    tools::late_tool_search_observer::arc_id(&registry),
                    registry.server_count(),
                );
                // Origin set: every `<server>__<tool>` name the registry knows.
                // Deferred stubs derive from the same `tool_names()` call, so
                // one extension covers eager, deferred, and later activations.
                mcp_tool_names.extend(registry.tool_names());
                // Elevation arcs exist only to resolve skill-declared MCP
                // elevation in step 5; skip the collection when no skills are
                // registered through this assembly.
                if !skills.is_empty() {
                    mcp_elevation_arcs =
                        tools::collect_mcp_elevation_arcs(&registry, security).await;
                }
                let mcp_policy = mcp_tool_access_policy(security.as_ref(), caller_allowed);
                // Generic MCP resource/prompt capability tools (policy-gated in
                // deferred-loading and eager modes) - parity with run/process_message.
                for tool in tools::build_mcp_capability_tools(&registry, mcp_policy.as_ref()) {
                    let capability_name = tool.name().to_string();
                    if tools_registry.register_mcp(
                        tool,
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
                pinned_blocks = tools::mcp_context::build_pinned_resource_blocks(
                    &registry,
                    &agent_mcp_servers,
                    mcp_policy.as_ref(),
                )
                .await;
                pinned_section =
                    tools::mcp_context::render_pinned_resources_section(&pinned_blocks);
                if config.mcp.deferred_loading {
                    let deferred_set = tools::DeferredMcpToolSet::from_registry(
                        Arc::clone(&registry),
                        Arc::clone(security),
                    )
                    .await;
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
                            let wrapper: Arc<dyn Tool> = Arc::new(
                                stub.activate(Arc::clone(&registry), Arc::clone(security)),
                            );
                            tools_registry.register_mcp(
                                wrapper,
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
                        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                        tools::late_tool_search_observer::record(
                            "constructor-enter",
                            "mcp-search",
                            allowed_stub_count,
                            0,
                        );
                        let mut tool_search =
                            tools::ToolSearchTool::new(filtered_deferred, activated);
                        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                        tools::late_tool_search_observer::record(
                            "constructor-return",
                            "mcp-search",
                            0,
                            0,
                        );
                        tools_registry.claim_search(
                            &mut unfiltered_tool_arcs,
                            &mut context_filtered_tool_arcs,
                            delegate_handle.as_ref(),
                        );
                        if let Some(mut policy) = mcp_policy {
                            // The caller ceiling already materialized the stub
                            // registry above. Do not retain a second, stale
                            // principal selector in the long-lived search tool.
                            policy.caller_allowed = None;
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
                        let tool_search = Arc::new(tool_search);
                        tool_search_handle = Some(Arc::clone(&tool_search));
                        tools_registry.push(BuildSlot::Ready(tool_search));
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
                                Arc::clone(security),
                            ));
                            if tools_registry.register_mcp(
                                wrapper,
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
        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "skills", 4, 0);
        let nat64_prefixes = match zeroclaw_infra::net_guard::parse_nat64_prefixes(
            &config.security.nat64_prefixes,
            "security.nat64_prefixes",
        ) {
            Ok(prefixes) => Some(prefixes),
            Err(error) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "Skipping skill HTTP tools because security.nat64_prefixes is invalid"
                );
                None
            }
        };
        if !skills.is_empty() {
            let skill_tools = crate::skills::prepare_skill_tools_with_runtime(
                skills,
                Arc::clone(security),
                runtime,
                nat64_prefixes.as_deref(),
                PreparedSkill::Ready,
                |skill_name, tool, kind_label| {
                    crate::skills::resolve_skill_elevation(
                        skill_name,
                        tool,
                        kind_label,
                        |target_name| {
                            if let Some(target) = context_filtered_tool_arcs
                                .iter()
                                .chain(mcp_elevation_arcs.iter())
                                .find(|target| target.name() == target_name)
                            {
                                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                                tools::late_tool_search_observer::record(
                                    "skill-target",
                                    "existing",
                                    tools::late_tool_search_observer::arc_id(target),
                                    0,
                                );
                                let target = Arc::clone(target);
                                return Some(PreparedSkill::Elevated {
                                    prepared: PreparedSkillBuiltin::prepare(
                                        skill_name,
                                        tool,
                                        tool.locked_args.clone(),
                                        target.as_ref(),
                                    ),
                                    target: TargetRef::Existing(target),
                                });
                            }
                            pipeline_id.and_then(|id| {
                                match tools_registry.entries[id.0].value.as_ref() {
                                    Some(BuildSlot::Pipeline(target))
                                        if target.name() == target_name =>
                                    {
                                        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                                        tools::late_tool_search_observer::record(
                                            "skill-target",
                                            "pipeline",
                                            id.0,
                                            0,
                                        );
                                        Some(PreparedSkill::Elevated {
                                            prepared: PreparedSkillBuiltin::prepare(
                                                skill_name,
                                                tool,
                                                tool.locked_args.clone(),
                                                target,
                                            ),
                                            target: TargetRef::Pipeline(id),
                                        })
                                    }
                                    _ => None,
                                }
                            })
                        },
                    )
                },
            );
            #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
            tools::late_tool_search_observer::record(
                "phase",
                "skills-prepared",
                5,
                skill_tools.len(),
            );
            // The complete batch is prepared before the one fixed snapshot.
            let existing_names = tools_registry
                .iter()
                .map(|(_, tool)| tool.name().to_string())
                .collect();
            tools::register_prepared_skill_tools(
                skill_tools,
                &existing_names,
                skills,
                security,
                |tool| match tool {
                    PreparedSkill::Ready(tool) => tool.name(),
                    PreparedSkill::Elevated { prepared, .. } => &prepared.tool_name,
                },
                |tool| match tool {
                    PreparedSkill::Ready(tool) => tools_registry.push_ready(tool),
                    PreparedSkill::Elevated { prepared, target } => {
                        tools_registry.push(BuildSlot::Elevated { prepared, target });
                    }
                },
            );
        }

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

        if caller_allowed.is_some_and(|allowed| !allowed.iter().any(|name| name == "tool_search")) {
            tools_registry.retain(|tool| tool.name() != "tool_search");
            deferred_section.clear();
        }

        // This is prompt visibility, not ownership or admission. The basic
        // workspace/memory actions stay directly usable; other built-ins keep
        // their executable instances in this same scoped registry.
        const EAGER_BUILTINS: &[&str] = &[
            "shell",
            "file_read",
            "file_write",
            "file_edit",
            "memory_recall",
            "memory_store",
        ];
        let builtin_deferral = !list_deferred_mcp_specs
            && config
                .runtime_profile_for_agent(agent_alias)
                .is_some_and(|profile| profile.deferred_builtin_tools);
        let mut search_policy = zeroclaw_tools::tool_search::ToolAccessPolicy::from_security(
            security.allowed_tools.as_deref(),
            security.excluded_tools.as_deref(),
            caller_allowed,
        );
        let search_admitted = search_policy
            .as_ref()
            .is_none_or(|policy| policy.is_tool_allowed("tool_search"));
        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "specs", 6, 0);
        let mut builtin_schema_handle = None;
        if builtin_deferral && search_admitted {
            let captured_specs: Vec<_> = tools_registry
                .iter()
                .filter(|(_, tool)| {
                    builtin_names.contains(tool.name())
                        && !EAGER_BUILTINS.contains(&tool.name())
                        && tool.name() != "tool_search"
                })
                .map(|(id, tool)| {
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record("spec-acquire", "assembly", id.0, 0);
                    (id, tool.spec())
                })
                .collect();
            if !captured_specs.is_empty() {
                #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                for (id, spec) in &captured_specs {
                    tools::late_tool_search_observer::record(
                        "captured-spec",
                        &spec.name,
                        id.0,
                        tools::late_tool_search_observer::arc_id(&spec.parameters),
                    );
                }
                // The original HashMap collection chooses the last duplicate.
                // Retiring that origin must not promote an earlier loser.
                let last_origins: std::collections::HashMap<_, _> = captured_specs
                    .iter()
                    .map(|(id, spec)| (spec.name.clone(), *id))
                    .collect();
                let activated = activated_handle.get_or_insert_with(|| {
                    Arc::new(std::sync::Mutex::new(ActivatedToolSet::new()))
                });
                activated
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_deferred_builtin_specs(
                        captured_specs
                            .iter()
                            .map(|(_, spec)| spec.clone())
                            .collect(),
                    );
                builtin_schema_handle = Some(Arc::clone(activated));
                if tool_search_handle.is_none() {
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record(
                        "constructor-enter",
                        "builtin-search",
                        captured_specs.len(),
                        0,
                    );
                    let mut search =
                        tools::ToolSearchTool::for_builtin_schemas(Arc::clone(activated));
                    #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
                    tools::late_tool_search_observer::record(
                        "constructor-return",
                        "builtin-search",
                        0,
                        0,
                    );
                    tools_registry.claim_search(
                        &mut unfiltered_tool_arcs,
                        &mut context_filtered_tool_arcs,
                        delegate_handle.as_ref(),
                    );
                    if let Some(mut policy) = search_policy.take() {
                        // The registry already applied this ceiling. Later
                        // narrowing removes catalog entries at the same seam.
                        policy.caller_allowed = None;
                        search = search.with_access_policy(policy);
                    }
                    let search = Arc::new(search);
                    tools_registry.push(BuildSlot::Ready(Arc::clone(&search) as Arc<dyn Tool>));
                    tool_search_handle = Some(search);
                }
                activated
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .refresh_deferred_builtin_specs(
                        captured_specs
                            .into_iter()
                            .filter(|(id, spec)| {
                                last_origins.get(&spec.name) == Some(id)
                                    && tools_registry.entries[id.0].top_level
                                    && tools_registry.entries[id.0].value.is_some()
                            })
                            .map(|(_, spec)| spec)
                            .collect(),
                    );
            }
        }

        #[cfg(all(test, feature = "plugins-wasm-cranelift"))]
        tools::late_tool_search_observer::record("phase", "seal", 7, 0);
        ScopedAssembled {
            registry: ScopedToolRegistry(tools_registry.seal(pipeline_id), builtin_schema_handle),
            delegate_handle,
            ask_user_handle,
            reaction_handle,
            poll_handle,
            escalate_handle,
            channel_room_handle,
            deferred_section,
            pinned_section,
            pinned_blocks,
            activated_handle,
            tool_search_handle,
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
            reserved_host_names: HashSet::new(),
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
            reserved_host_names: HashSet::new(),
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

    async fn assemble_real_delegate_pipeline(
        delegate: Arc<dyn Tool>,
        effects: Arc<AtomicUsize>,
        skills: &[Skill],
    ) -> ScopedAssembled {
        assemble_real_children_pipeline(vec![delegate], effects, skills).await
    }

    async fn assemble_real_children_pipeline(
        mut children: Vec<Arc<dyn Tool>>,
        effects: Arc<AtomicUsize>,
        skills: &[Skill],
    ) -> ScopedAssembled {
        let shell: Arc<dyn Tool> = Arc::new(CountingTool {
            name: "shell",
            calls: effects,
        });
        children.push(shell);
        let mut allowed: Vec<String> = children
            .iter()
            .map(|child| child.name().to_string())
            .collect();
        let step_allowed = allowed.clone();
        allowed.push(tools::PipelineTool::NAME.into());
        allowed.extend([
            "ops__chain".into(),
            "ops__manage".into(),
            "ops__create".into(),
        ]);
        let mut built = built_with(
            children
                .iter()
                .cloned()
                .map(|child| Box::new(tools::ArcToolRef(child)) as Box<dyn Tool>)
                .collect(),
        );
        built.unfiltered_tool_arcs = children;
        let mut config = Config::default();
        config.pipeline.enabled = true;
        config.pipeline.allowed_tools = step_allowed;
        config.tools.optional.push(tools::PipelineTool::NAME.into());
        let security = Arc::new(SecurityPolicy {
            allowed_tools: Some(allowed.clone()),
            ..SecurityPolicy::default()
        });
        let caller = allowed;
        ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "main",
            security: &security,
            built,
            skills,
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: Some(&caller),
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

    struct RegisteredGoalFixture {
        _directory: tempfile::TempDir,
        root: crate::goal_accounting::GoalForegroundContext,
        accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
        tracker: Arc<zeroclaw_config::cost::CostTracker>,
        handle: crate::control_plane::ControlPlaneHandle,
        identity: zeroclaw_goal::GoalExecutionScope,
    }

    struct ScopedHeldSettlement {
        inner: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for ScopedHeldSettlement {
        async fn admit(
            &self,
            request: zeroclaw_goal::accounting::GoalOperationRequest,
        ) -> anyhow::Result<()> {
            self.inner.admit(request).await
        }
        async fn settle(
            &self,
            settlement: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> anyhow::Result<()> {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.settle(settlement).await
        }
        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            self.inner.pricing_complete().await
        }
    }

    struct ScopedHeldTerminal {
        inner: Arc<dyn crate::control_plane::TaskRegistry>,
        mode: ScopedTerminalMode,
    }

    enum ScopedTerminalMode {
        Hold {
            entered: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
        },
        Fail {
            attempts: Arc<AtomicUsize>,
        },
    }

    #[derive(Debug, thiserror::Error)]
    #[error("registered pipeline child terminal storage failure")]
    struct ScopedChildTerminalFailure;
    #[async_trait]
    impl crate::control_plane::TaskRegistry for ScopedHeldTerminal {
        async fn create(&self, row: crate::control_plane::TaskRecord) -> anyhow::Result<()> {
            self.inner.create(row).await
        }
        async fn heartbeat(&self, id: &str, boot: &str) -> anyhow::Result<()> {
            self.inner.heartbeat(id, boot).await
        }
        async fn update_status(
            &self,
            id: &str,
            status: crate::control_plane::TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<()> {
            self.inner.update_status(id, status, output, error).await
        }
        async fn transition_terminal_if_owner(
            &self,
            id: &str,
            pid: u32,
            boot: &str,
            status: crate::control_plane::TaskStatus,
            output: Option<String>,
            error: Option<String>,
        ) -> anyhow::Result<bool> {
            match &self.mode {
                ScopedTerminalMode::Hold { entered, release } => {
                    entered.notify_one();
                    release.notified().await;
                }
                ScopedTerminalMode::Fail { attempts } => {
                    let row = self.inner.get_snapshot(id).await?.unwrap().task;
                    assert_eq!(row.kind, crate::control_plane::TaskKind::Subagent);
                    assert_eq!(row.status, crate::control_plane::TaskStatus::Running);
                    assert_eq!(row.owner_pid, pid);
                    assert_eq!(row.owner_boot_id, boot);
                    assert_eq!(status, crate::control_plane::TaskStatus::Completed);
                    attempts.fetch_add(1, Ordering::SeqCst);
                    return Err(ScopedChildTerminalFailure.into());
                }
            }
            self.inner
                .transition_terminal_if_owner(id, pid, boot, status, output, error)
                .await
        }
        async fn claim_owner(&self, id: &str, pid: u32, boot: &str) -> anyhow::Result<()> {
            self.inner.claim_owner(id, pid, boot).await
        }
        async fn get(&self, id: &str) -> anyhow::Result<Option<crate::control_plane::TaskRecord>> {
            self.inner.get(id).await
        }
        async fn get_snapshot(
            &self,
            id: &str,
        ) -> anyhow::Result<Option<crate::control_plane::TaskSnapshot>> {
            self.inner.get_snapshot(id).await
        }
        async fn list_running(&self) -> anyhow::Result<Vec<crate::control_plane::TaskRecord>> {
            self.inner.list_running().await
        }
        async fn list_by_agent(
            &self,
            agent: &str,
        ) -> anyhow::Result<Vec<crate::control_plane::TaskRecord>> {
            self.inner.list_by_agent(agent).await
        }
        async fn reconcile_lost(&self, id: &str, boot: &str) -> anyhow::Result<bool> {
            self.inner.reconcile_lost(id, boot).await
        }
    }

    fn registered_goal_resources() -> (
        tempfile::TempDir,
        crate::control_plane::ControlPlaneHandle,
        Arc<zeroclaw_config::cost::CostTracker>,
        Arc<crate::goal_accounting::RuntimeGoalOperationAccountingFactory>,
    ) {
        use std::collections::HashMap;
        let directory = tempfile::TempDir::new().unwrap();
        let handle = crate::control_plane::ControlPlaneHandle::open(directory.path()).unwrap();
        let store = handle.goal_store().unwrap();
        let tracker = Arc::new(
            zeroclaw_config::cost::CostTracker::new(
                zeroclaw_config::schema::CostConfig {
                    enabled: false,
                    ..Default::default()
                },
                directory.path(),
            )
            .unwrap(),
        );
        let rates = HashMap::from([
            ("fixture-model.input".to_string(), 1.0),
            ("fixture-model.output".to_string(), 2.0),
        ]);
        let pricer = Arc::new(crate::goal_accounting::ModelProviderGoalUsagePricer::new(
            Arc::new(HashMap::from([("ollama.local".to_string(), rates)])),
        ));
        let factory = Arc::new(
            crate::goal_accounting::RuntimeGoalOperationAccountingFactory::new(
                store,
                tracker.clone(),
                "main",
                pricer,
            )
            .unwrap(),
        );
        (directory, handle, tracker, factory)
    }

    async fn registered_goal_fixture() -> RegisteredGoalFixture {
        use zeroclaw_goal::accounting::GoalOperationAccountingFactory;
        let (directory, handle, tracker, factory) = registered_goal_resources();
        let store = handle.goal_store().unwrap();
        let identity =
            zeroclaw_goal::GoalExecutionScope::new("pipeline-goal", "pipeline-session", 1).unwrap();
        let task = crate::control_plane::TaskRecord {
            id: identity.task_id().into(),
            kind: crate::control_plane::TaskKind::Goal,
            agent: "main".into(),
            status: zeroclaw_api::task::TaskStatus::Running,
            owner_pid: 1,
            owner_boot_id: "test-boot".into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: Some("matrix:test-room".into()),
            originator_chain: Vec::new(),
            delivered: false,
            idem_key: None,
            principal_id: Some("@test:example.org".into()),
            session_id: Some(identity.session_id().into()),
            execution_epoch: 1,
            started_at: "2026-09-05T00:00:00Z".into(),
            finished_at: None,
        };
        let goal = zeroclaw_goal::GoalTaskRecord {
            task_id: task.id.clone(),
            objective: "finish work".into(),
            ..Default::default()
        };
        assert_eq!(
            store
                .create_or_replace_session_goal(task, goal)
                .await
                .unwrap(),
            zeroclaw_goal::GoalTransitionResult::Applied
        );
        let accountant = factory.for_scope(identity.clone());
        let root = crate::goal_accounting::GoalForegroundContext::new_root(
            identity.clone(),
            handle.clone(),
            tokio_util::sync::CancellationToken::new(),
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        RegisteredGoalFixture {
            _directory: directory,
            root,
            accountant,
            tracker,
            handle,
            identity,
        }
    }

    fn real_delegate_for_server(uri: String) -> Arc<dyn Tool> {
        use std::collections::HashMap;
        let agents = HashMap::from([(
            "worker".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig {
                model_provider: "ollama.local".into(),
                ..Default::default()
            },
        )]);
        let providers = HashMap::from([(
            "ollama".to_string(),
            HashMap::from([(
                "local".to_string(),
                zeroclaw_config::schema::ModelProviderConfig {
                    model: Some("fixture-model".into()),
                    ..Default::default()
                },
            )]),
        )]);
        let options = zeroclaw_providers::ModelProviderRuntimeOptions {
            provider_api_url: Some(uri),
            ..Default::default()
        };
        Arc::new(
            crate::tools::DelegateTool::new_with_options(
                agents,
                None,
                Arc::new(SecurityPolicy::default()),
                options,
            )
            .with_providers_models(providers),
        )
    }

    #[test]
    fn real_delegate_arc_wrappers_preserve_argument_sensitive_child_predicate() {
        let delegate = real_delegate_for_server("http://127.0.0.1:1".into());
        let direct = tools::ArcToolRef(delegate.clone());
        let wrapped = tools::boxed_registry_from_arcs(vec![delegate]);
        for tool in [&direct as &dyn Tool, wrapped[0].as_ref()] {
            for args in [
                serde_json::json!({}),
                serde_json::json!({"action":"delegate"}),
                serde_json::json!({"action":"{{step[0].result}}"}),
                serde_json::json!({"action":3}),
            ] {
                assert!(tool.may_invoke_agent_child(&args));
            }
            for action in [
                "check_result",
                "list_results",
                "cancel_task",
                "await_sessions",
            ] {
                assert!(!tool.may_invoke_agent_child(&serde_json::json!({"action":action})));
            }
        }
    }

    struct GoalPipelineProbe {
        accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
        handle: crate::control_plane::ControlPlaneHandle,
        deadline: tokio::time::Instant,
        calls: Arc<AtomicUsize>,
    }

    struct GoalParallelContextProbe {
        name: &'static str,
        accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
        handle: crate::control_plane::ControlPlaneHandle,
        deadline: tokio::time::Instant,
        barrier: Arc<tokio::sync::Barrier>,
        in_flight: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
        tokens: Arc<std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>>,
    }

    zeroclaw_api::mock_tool_attribution!(GoalParallelContextProbe);

    impl GoalParallelContextProbe {
        fn observe(&self) -> anyhow::Result<()> {
            let context =
                crate::goal_accounting::GoalForegroundContext::current_for_child()?.unwrap();
            anyhow::ensure!(
                context.depth() == 0
                    && context.scope_identity().task_id() == "pipeline-goal"
                    && context.scope_identity().session_id() == "pipeline-session"
                    && context.scope_identity().execution_epoch() == 1,
                "wrong parallel Goal identity"
            );
            anyhow::ensure!(
                Arc::ptr_eq(&context.control_plane().store, &self.handle.store),
                "wrong parallel Goal store"
            );
            anyhow::ensure!(
                context.deadline() == self.deadline && !context.token().is_cancelled(),
                "wrong parallel Goal lifetime"
            );
            anyhow::ensure!(
                zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(|active| active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &self.accountant)))
                    .unwrap_or(false),
                "wrong parallel Goal accountant"
            );
            let api = zeroclaw_api::tool::ToolExecutionContext::current()
                .ok_or_else(|| anyhow::Error::msg("missing API context"))?;
            anyhow::ensure!(api.is_goal_foreground(), "missing API Goal participation");
            let api_token = api
                .cancellation_token()
                .ok_or_else(|| anyhow::Error::msg("missing API cancellation token"))?;
            let mut tokens = self.tokens.lock().unwrap();
            tokens.push(context.token().clone());
            tokens.push(api_token.clone());
            Ok(())
        }
    }

    #[async_trait]
    impl Tool for GoalParallelContextProbe {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "Observe child-free parallel Goal context."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.observe()?;
            let active = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            self.barrier.wait().await;
            self.observe()?;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: self.name.to_string().into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn registered_goal_child_free_parallel_steps_overlap_and_keep_real_context() {
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        // Each retained token is tested in a fresh run: cancelling any one
        // must cancel the root and every other observed token, not merely a child.
        for token_index in 0..8 {
            let fixture = registered_goal_fixture().await;
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let active = Arc::new(AtomicUsize::new(0));
            let maximum = Arc::new(AtomicUsize::new(0));
            let tokens = Arc::new(std::sync::Mutex::new(Vec::new()));
            let tools: Vec<Arc<dyn Tool>> = ["parallel_one", "parallel_two"]
                .into_iter()
                .map(|name| {
                    Arc::new(GoalParallelContextProbe {
                        name,
                        accountant: fixture.accountant.clone(),
                        handle: fixture.handle.clone(),
                        deadline: fixture.root.deadline(),
                        barrier: barrier.clone(),
                        in_flight: active.clone(),
                        maximum: maximum.clone(),
                        tokens: tokens.clone(),
                    }) as Arc<dyn Tool>
                })
                .collect();
            let assembled =
                assemble_real_children_pipeline(tools, Arc::new(AtomicUsize::new(0)), &[]).await;
            let pipeline = assembled
                .registry
                .iter()
                .find(|tool| tool.name() == tools::PipelineTool::NAME)
                .unwrap();
            let mode = if token_index % 2 == 0 { "all" } else { "last" };
            let output = GOAL_OPERATION_ACCOUNTING.scope(Some(fixture.accountant.clone()), fixture.root.scope(async {
                let result = tokio::time::timeout(std::time::Duration::from_secs(5), pipeline.execute(serde_json::json!({
                    "parallel": true, "result": mode, "steps": [
                        {"tool":"parallel_one","args":{}}, {"tool":"parallel_two","args":{}}
                    ]
                }))).await.expect("child-free steps must overlap").unwrap();
                assert_eq!(crate::goal_accounting::GoalForegroundContext::current().unwrap().depth(), 0);
                assert!(zeroclaw_api::tool::ToolExecutionContext::current().unwrap().is_goal_foreground());
                assert!(GOAL_OPERATION_ACCOUNTING.try_with(|active| Arc::ptr_eq(active.as_ref().unwrap(), &fixture.accountant)).unwrap());
                result
            })).await;
            assert!(output.success, "{:?}", output.error);
            if mode == "all" {
                let results: serde_json::Value =
                    serde_json::from_str(output.output.as_str()).unwrap();
                assert_eq!(results[0]["tool"], "parallel_one");
                assert_eq!(results[1]["tool"], "parallel_two");
            } else {
                assert_eq!(output.output.as_str(), "parallel_two");
            }
            assert_eq!(maximum.load(Ordering::SeqCst), 2);
            assert_eq!(active.load(Ordering::SeqCst), 0);
            assert!(crate::goal_accounting::GoalForegroundContext::current().is_none());
            assert!(zeroclaw_api::tool::ToolExecutionContext::current().is_none());
            assert!(GOAL_OPERATION_ACCOUNTING.try_with(|_| ()).is_err());
            let goal = fixture
                .handle
                .goal_store()
                .unwrap()
                .get_goal_task("pipeline-goal")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                goal.accounting_state,
                zeroclaw_goal::GoalAccountingState::Complete
            );
            assert!(goal.pending_call_id.is_none());
            assert_eq!(
                fixture
                    .tracker
                    .get_strict_usage_totals_for_task_with_pricing("pipeline-goal")
                    .unwrap()
                    .0,
                0
            );
            let observed = tokens.lock().unwrap();
            assert_eq!(observed.len(), 8);
            observed[token_index].cancel();
            assert!(fixture.root.token().is_cancelled());
            assert!(
                observed
                    .iter()
                    .all(tokio_util::sync::CancellationToken::is_cancelled)
            );
        }
    }

    zeroclaw_api::mock_tool_attribution!(GoalPipelineProbe);

    #[async_trait]
    impl Tool for GoalPipelineProbe {
        fn name(&self) -> &str {
            "goal_pipeline_probe"
        }
        fn description(&self) -> &str {
            "Observe an actual child tool's Goal scope."
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let context =
                crate::goal_accounting::GoalForegroundContext::current_for_child()?.unwrap();
            anyhow::ensure!(
                context.depth() == 1
                    && context.scope_identity().task_id() == "pipeline-goal"
                    && context.scope_identity().session_id() == "pipeline-session"
                    && context.scope_identity().execution_epoch() == 1,
                "wrong child Goal identity"
            );
            anyhow::ensure!(
                Arc::ptr_eq(&context.control_plane().store, &self.handle.store),
                "different Goal store"
            );
            anyhow::ensure!(
                context.deadline() <= self.deadline && !context.token().is_cancelled(),
                "wrong child lifetime"
            );
            anyhow::ensure!(
                zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(|active| active
                        .as_ref()
                        .is_some_and(|active| Arc::ptr_eq(active, &self.accountant)))
                    .unwrap_or(false),
                "different Goal accountant"
            );
            anyhow::ensure!(
                zeroclaw_api::tool::ToolExecutionContext::current()
                    .is_some_and(|current| current.is_goal_foreground()
                        && current.cancellation_token().is_some()),
                "missing Goal tool context"
            );
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: "context observed".into(),
                error: None,
            })
        }
    }

    fn real_pipeline_child_config(fixture: &RegisteredGoalFixture, uri: String) -> Config {
        real_pipeline_child_config_in(fixture._directory.path(), uri)
    }

    fn real_pipeline_child_config_in(directory: &std::path::Path, uri: String) -> Config {
        use zeroclaw_config::autonomy::{DelegationMode, DelegationPolicy};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, DelegateTargetConfig, ModelProviderConfig,
            OllamaModelProviderConfig, RiskProfileConfig, RuntimeProfileConfig,
        };
        let mut config = Config {
            data_dir: directory.join("runtime"),
            config_path: directory.join("config.toml"),
            ..Config::default()
        };
        config.providers.models.ollama.insert(
            "local".into(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("fixture-model".into()),
                    uri: Some(uri),
                    pricing: [
                        ("fixture-model.input".into(), 1.0),
                        ("fixture-model.output".into(), 2.0),
                    ]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        config.risk_profiles.insert(
            "fixture".into(),
            RiskProfileConfig {
                delegation_policy: DelegationPolicy {
                    mode: DelegationMode::Allow,
                },
                allowed_tools: vec![
                    "delegate".into(),
                    "spawn_subagent".into(),
                    "goal_pipeline_probe".into(),
                ],
                auto_approve: vec!["goal_pipeline_probe".into()],
                ..Default::default()
            },
        );
        config.runtime_profiles.insert(
            "fixture".into(),
            RuntimeProfileConfig {
                agentic: true,
                max_tool_iterations: 5,
                ..Default::default()
            },
        );
        let common = AliasedAgentConfig {
            model_provider: "ollama.local".into(),
            risk_profile: "fixture".into(),
            runtime_profile: "fixture".into(),
            ..Default::default()
        };
        config.agents.insert(
            "main".into(),
            AliasedAgentConfig {
                delegates: vec![DelegateTargetConfig::bounded("worker")],
                ..common.clone()
            },
        );
        config.agents.insert("worker".into(), common);
        config
    }

    #[tokio::test]
    async fn registered_goal_pipeline_runs_agentic_delegate_then_real_spawn() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        let sequence = Arc::new(AtomicUsize::new(0));
        let next = Arc::clone(&sequence);
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(move |_request: &wiremock::Request| {
                let index = next.fetch_add(1, Ordering::SeqCst);
                let message = if index == 0 {
                    serde_json::json!({"content":null,"tool_calls":[{"id":"probe","type":"function","function":{"name":"goal_pipeline_probe","arguments":"{}"}}]})
                } else {
                    serde_json::json!({"role":"assistant","content": if index == 1 { "agentic finished" } else { "spawn finished" }})
                };
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"choices":[{"message":message}],"usage":{"prompt_tokens":8,"completion_tokens":3}}))
            }).expect(3).mount(&server).await;
        let fixture = registered_goal_fixture().await;
        let config = Arc::new(real_pipeline_child_config(&fixture, server.uri()));
        let security = Arc::new(SecurityPolicy::for_agent(&config, "main").unwrap());
        let probe_calls = Arc::new(AtomicUsize::new(0));
        let probe: Arc<dyn Tool> = Arc::new(GoalPipelineProbe {
            accountant: fixture.accountant.clone(),
            handle: fixture.handle.clone(),
            deadline: fixture.root.deadline(),
            calls: probe_calls.clone(),
        });
        let delegate: Arc<dyn Tool> = Arc::new(
            crate::tools::DelegateTool::new(config.agents.clone(), None, security.clone())
                .with_root_config(config.clone())
                .with_caller_alias("main")
                .with_runtime(Arc::new(crate::platform::NativeRuntime::new()))
                .with_risk_profiles(config.risk_profiles.clone())
                .with_runtime_profiles(config.runtime_profiles.clone())
                .with_parent_tools(Arc::new(parking_lot::RwLock::new(vec![probe]))),
        );
        let spawn: Arc<dyn Tool> = Arc::new(crate::tools::SpawnSubagentTool::new(
            config, "main", security,
        ));
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled =
            assemble_real_children_pipeline(vec![delegate, spawn], effects.clone(), &[]).await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .unwrap();
        let result = GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(fixture.accountant.clone()),
                fixture.root.scope(async {
                    let result = pipeline
                        .execute(serde_json::json!({"result":"all","steps":[
                            {"tool":"delegate","args":{"agent":"worker","prompt":"call probe"}},
                            {"tool":"spawn_subagent","args":{"prompt":"finish"}},
                            {"tool":"shell","args":{}}
                        ]}))
                        .await
                        .unwrap();
                    assert_eq!(
                        crate::goal_accounting::GoalForegroundContext::current()
                            .unwrap()
                            .depth(),
                        0
                    );
                    let rows = fixture.handle.store.list_by_agent("main").await.unwrap();
                    let children: Vec<_> = rows
                        .iter()
                        .filter(|row| row.kind == crate::control_plane::TaskKind::Subagent)
                        .collect();
                    assert_eq!(children.len(), 1);
                    assert_eq!(children[0].parent_id.as_deref(), Some("pipeline-goal"));
                    assert_eq!(children[0].depth, 1);
                    assert_eq!(
                        children[0].status,
                        crate::control_plane::TaskStatus::Completed
                    );
                    assert_eq!(children[0].owner_pid, std::process::id());
                    assert_eq!(children[0].owner_boot_id, fixture.handle.boot_id);
                    result
                }),
            )
            .await;
        assert!(result.success, "{:?}", result.error);
        assert!(
            result.output.contains("agentic finished") && result.output.contains("spawn finished")
        );
        assert_eq!(probe_calls.load(Ordering::SeqCst), 1);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(sequence.load(Ordering::SeqCst), 3);
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing("pipeline-goal")
                .unwrap()
                .0,
            33
        );
        assert!(
            fixture
                .handle
                .goal_store()
                .unwrap()
                .get_goal_task("pipeline-goal")
                .await
                .unwrap()
                .unwrap()
                .pending_call_id
                .is_none()
        );
    }

    async fn registered_goal_pipeline_spawn_cancel_drains(use_alias: bool) {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(30)).set_body_json(serde_json::json!({
                "choices":[{"message":{"role":"assistant","content":"must not finish"}}],"usage":{"prompt_tokens":8,"completion_tokens":3}
            }))).mount(&server).await;
        let fixture = registered_goal_fixture().await;
        let settle_entered = Arc::new(tokio::sync::Notify::new());
        let settle_release = Arc::new(tokio::sync::Notify::new());
        let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
            Arc::new(ScopedHeldSettlement {
                inner: fixture.accountant.clone(),
                entered: settle_entered.clone(),
                release: settle_release.clone(),
            });
        let terminal_entered = Arc::new(tokio::sync::Notify::new());
        let terminal_release = Arc::new(tokio::sync::Notify::new());
        let mut handle = fixture.handle.clone();
        handle.store = Arc::new(ScopedHeldTerminal {
            inner: handle.store.clone(),
            mode: ScopedTerminalMode::Hold {
                entered: terminal_entered.clone(),
                release: terminal_release.clone(),
            },
        });
        let root = crate::goal_accounting::GoalForegroundContext::new_root(
            fixture.identity.clone(),
            handle,
            fixture.root.token().clone(),
            fixture.root.deadline(),
        );
        let config = Arc::new(real_pipeline_child_config(&fixture, server.uri()));
        let security = Arc::new(SecurityPolicy::for_agent(&config, "main").unwrap());
        let spawn: Arc<dyn Tool> = Arc::new(crate::tools::SpawnSubagentTool::new(
            config, "main", security,
        ));
        let skill = Skill {
            name: "ops".into(),
            description: "pipeline alias".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "chain".into(),
                description: "pipeline".into(),
                kind: "builtin".into(),
                command: String::new(),
                args: Default::default(),
                target: Some(tools::PipelineTool::NAME.into()),
                locked_args: [("result".to_string(), "last".to_string())]
                    .into_iter()
                    .collect(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled =
            assemble_real_children_pipeline(vec![spawn], effects.clone(), &[skill]).await;
        let name = if use_alias {
            "ops__chain"
        } else {
            tools::PipelineTool::NAME
        };
        let meta = crate::agent::turn::TurnMeta {
            parent_agent_alias: None,
            agent_alias: Some("main"),
            turn_id: "pipeline-cancel",
            channel_name: "test",
        };
        let dispatch = crate::agent::tool_execution::ToolDispatchContext {
            tools_registry: &assembled.registry,
            activated_tools: None,
            excluded_tools: &[],
            model_switch_callback: None,
        };
        let observer = crate::observability::NoopObserver;
        GOAL_OPERATION_ACCOUNTING.scope(Some(accountant), root.scope(async {
            let execution = crate::agent::tool_execution::execute_one_tool(
                name, serde_json::json!({"result":"all","steps":[
                    {"tool":"spawn_subagent","args":{"prompt":"wait"}},
                    {"tool":"shell","args":{}}
                ]}), None, dispatch, &meta, &observer, Some(root.token()), None, None,
            );
            tokio::pin!(execution);
            tokio::select! {
                result = &mut execution => panic!(
                    "pipeline returned before provider started: {}",
                    match &result {
                        Ok(outcome) => format!(
                            "outcome (success={}, error={:?}, output={:?})",
                            outcome.success,
                            outcome.error_reason.as_deref(),
                            outcome.output
                        ),
                        Err(error) => format!("execution error: {error:#}"),
                    }
                ),
                () = async {
                    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                        while server.received_requests().await.unwrap().is_empty() { tokio::task::yield_now().await; }
                    }).await.expect("spawn provider must start");
                } => {}
            }
            let goal = root.control_plane().goal_store().unwrap().get_goal_task("pipeline-goal").await.unwrap().unwrap();
            assert!(goal.pending_call_id.is_some());
            let rows = root.control_plane().store.list_by_agent("main").await.unwrap();
            let child = rows.iter().find(|row| row.kind == crate::control_plane::TaskKind::Subagent).unwrap();
            let child_id = child.id.clone();
            assert_eq!(child.status, crate::control_plane::TaskStatus::Running);
            root.token().cancel();
            tokio::select! {
                result = &mut execution => panic!(
                    "pipeline returned before settlement release: {}",
                    match &result {
                        Ok(outcome) => format!(
                            "outcome (success={}, error={:?}, output={:?})",
                            outcome.success,
                            outcome.error_reason.as_deref(),
                            outcome.output
                        ),
                        Err(error) => format!("execution error: {error:#}"),
                    }
                ),
                result = tokio::time::timeout(std::time::Duration::from_secs(10), settle_entered.notified()) => { result.expect("settlement must start"); }
            }
            assert!(root.control_plane().goal_store().unwrap().get_goal_task("pipeline-goal").await.unwrap().unwrap().pending_call_id.is_some());
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            settle_release.notify_one();
            tokio::select! {
                result = &mut execution => panic!(
                    "pipeline returned before row cleanup release: {}",
                    match &result {
                        Ok(outcome) => format!(
                            "outcome (success={}, error={:?}, output={:?})",
                            outcome.success,
                            outcome.error_reason.as_deref(),
                            outcome.output
                        ),
                        Err(error) => format!("execution error: {error:#}"),
                    }
                ),
                result = tokio::time::timeout(std::time::Duration::from_secs(10), terminal_entered.notified()) => { result.expect("terminal cleanup must start"); }
            }
            assert!(root.control_plane().goal_store().unwrap().get_goal_task("pipeline-goal").await.unwrap().unwrap().pending_call_id.is_none());
            assert_eq!(root.control_plane().store.get_snapshot(&child_id).await.unwrap().unwrap().task.status, crate::control_plane::TaskStatus::Running);
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            terminal_release.notify_one();
            let outcome = execution.await.unwrap();
            let error = outcome.terminal_error.unwrap();
            assert!(error.downcast_ref::<zeroclaw_api::tool::GoalChildOperationError>().is_some());
            assert!(error.downcast_ref::<crate::agent::loop_::ToolLoopCancelled>().is_some());
            assert_eq!(error.downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>().unwrap().accounting_state(), zeroclaw_goal::GoalAccountingState::OutcomeUnknown);
            let row = root.control_plane().store.get_snapshot(&child_id).await.unwrap().unwrap();
            assert_eq!(row.task.status, crate::control_plane::TaskStatus::Cancelled);
            assert_eq!(row.task.parent_id.as_deref(), Some("pipeline-goal"));
            assert_eq!(crate::goal_accounting::GoalForegroundContext::current().unwrap().depth(), 0);
        })).await;
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn registered_goal_pipeline_spawn_cancel_waits_for_settlement_and_row() {
        registered_goal_pipeline_spawn_cancel_drains(false).await;
    }

    #[tokio::test]
    async fn registered_goal_pipeline_skill_alias_spawn_cancel_waits_for_settlement_and_row() {
        registered_goal_pipeline_spawn_cancel_drains(true).await;
    }

    #[tokio::test]
    async fn registered_goal_pipeline_real_delegate_charges_eleven_tokens() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "fixture response"}}],
                "usage": {"prompt_tokens": 8, "completion_tokens": 3}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled = assemble_real_delegate_pipeline(
            real_delegate_for_server(server.uri()),
            effects.clone(),
            &[],
        )
        .await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .unwrap();
        let result =
            GOAL_OPERATION_ACCOUNTING
                .scope(
                    Some(fixture.accountant.clone()),
                    fixture.root.scope(async {
                        let result = pipeline.execute(serde_json::json!({"result":"all", "steps":[
                {"tool":"delegate","args":{"agent":"worker","prompt":"fixture task"}},
                {"tool":"shell","args":{"text":"{{step[0].result}}"}}
            ]})).await.unwrap();
                        assert_eq!(
                            crate::goal_accounting::GoalForegroundContext::current()
                                .unwrap()
                                .depth(),
                            0
                        );
                        result
                    }),
                )
                .await;
        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("fixture response"));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            11
        );
        let reopened = zeroclaw_config::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            fixture._directory.path(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            11
        );
        let goal = fixture
            .handle
            .goal_store()
            .unwrap()
            .get_goal_task(fixture.identity.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        );
        assert!(goal.pending_call_id.is_none());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(request["model"], "fixture-model");
        assert!(crate::goal_accounting::GoalForegroundContext::current().is_none());
    }

    #[tokio::test]
    async fn registered_goal_pipeline_allows_two_successive_root_children() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "fixture response"}}],
                "usage": {"prompt_tokens": 8, "completion_tokens": 3}
            })))
            .expect(2)
            .mount(&server)
            .await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled =
            assemble_real_delegate_pipeline(real_delegate_for_server(server.uri()), effects, &[])
                .await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .unwrap();
        let result = GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(fixture.accountant.clone()),
                fixture.root.scope(async {
                    let result = pipeline
                        .execute(serde_json::json!({"steps":[
                            {"tool":"delegate","args":{"agent":"worker","prompt":"first"}},
                            {"tool":"delegate","args":{"agent":"worker","prompt":"second"}}
                        ]}))
                        .await
                        .unwrap();
                    assert_eq!(
                        crate::goal_accounting::GoalForegroundContext::current()
                            .unwrap()
                            .depth(),
                        0
                    );
                    result
                }),
            )
            .await;
        assert!(result.success, "{:?}", result.error);
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            22
        );
        let goal = fixture
            .handle
            .goal_store()
            .unwrap()
            .get_goal_task(fixture.identity.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        );
        assert!(goal.pending_call_id.is_none());
    }

    #[tokio::test]
    async fn registered_goal_parallel_child_refuses_before_any_sibling_effect() {
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = wiremock::MockServer::start().await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled = assemble_real_delegate_pipeline(
            real_delegate_for_server(server.uri()),
            effects.clone(),
            &[],
        )
        .await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .unwrap();
        for (child_args, single) in [
            (serde_json::json!({"agent":"worker","prompt":"work"}), false),
            (
                serde_json::json!({"action":"delegate","agent":"worker","prompt":"work"}),
                true,
            ),
            (serde_json::json!({}), true),
            (serde_json::json!({"action":"{{step[0].result}}"}), false),
        ] {
            let steps = if single {
                serde_json::json!([{"tool":"delegate","args":child_args}])
            } else {
                serde_json::json!([{"tool":"shell","args":{}},{"tool":"delegate","args":child_args}])
            };
            let result = GOAL_OPERATION_ACCOUNTING
                .scope(
                    Some(fixture.accountant.clone()),
                    fixture.root.scope(
                        pipeline.execute(serde_json::json!({"parallel":true,"steps":steps})),
                    ),
                )
                .await
                .unwrap();
            assert!(!result.success);
            assert!(
                result
                    .error
                    .unwrap()
                    .contains("Goal Mode permits only synchronous foreground child execution.")
            );
        }
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert!(server.received_requests().await.unwrap().is_empty());
        let goal = fixture
            .handle
            .goal_store()
            .unwrap()
            .get_goal_task(fixture.identity.task_id())
            .await
            .unwrap()
            .unwrap();
        assert!(goal.pending_call_id.is_none());
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            0
        );
    }

    #[tokio::test]
    async fn registered_goal_parallel_spawn_and_locked_skill_child_refuse_before_effects() {
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = wiremock::MockServer::start().await;
        let fixture = registered_goal_fixture().await;
        let config = Arc::new(real_pipeline_child_config(&fixture, server.uri()));
        let security = Arc::new(SecurityPolicy::for_agent(&config, "main").unwrap());
        let spawn: Arc<dyn Tool> = Arc::new(crate::tools::SpawnSubagentTool::new(
            config,
            "main",
            security.clone(),
        ));
        let locked_child = SkillTool {
            name: "create".into(),
            description: "Lock delegate creation rather than caller management arguments".into(),
            kind: "builtin".into(),
            command: String::new(),
            args: Default::default(),
            target: Some("delegate".into()),
            locked_args: [("action".to_string(), "delegate".to_string())]
                .into_iter()
                .collect(),
            timeout_secs: None,
        };
        // Skill aliases are created after the production pipeline snapshot.
        // Register this real alias as an explicit input to this fixture's
        // snapshot; do not add a new self-registration path to production.
        let alias: Arc<dyn Tool> = Arc::new(crate::tools::skill_tool::SkillBuiltinTool::new(
            "ops",
            &locked_child,
            real_delegate_for_server(server.uri()),
            locked_child.locked_args.clone(),
        ));
        let skill = Skill {
            name: "ops".into(),
            description: "Registered pipeline alias".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "chain".into(),
                description: "Run the same registered pipeline".into(),
                kind: "builtin".into(),
                command: String::new(),
                args: Default::default(),
                target: Some(tools::PipelineTool::NAME.into()),
                locked_args: [("result".to_string(), "last".to_string())]
                    .into_iter()
                    .collect(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled =
            assemble_real_children_pipeline(vec![spawn, alias], effects.clone(), &[skill]).await;
        for outer in [tools::PipelineTool::NAME, "ops__chain"] {
            let pipeline = assembled
                .registry
                .iter()
                .find(|tool| tool.name() == outer)
                .unwrap();
            for (child, args) in [
                ("spawn_subagent", serde_json::json!({"prompt":"work"})),
                (
                    "ops__create",
                    serde_json::json!({"action":"list_results","agent":"worker","prompt":"work"}),
                ),
            ] {
                for ordinary_first in [false, true] {
                    let steps = if ordinary_first {
                        serde_json::json!([{"tool":"shell","args":{}},{"tool":child,"args":args}])
                    } else {
                        serde_json::json!([{"tool":child,"args":args}])
                    };
                    let request = serde_json::json!({"parallel":true,"result":"all","steps":steps});
                    assert!(pipeline.may_invoke_agent_child(&request));
                    let result = GOAL_OPERATION_ACCOUNTING
                        .scope(
                            Some(fixture.accountant.clone()),
                            fixture.root.scope(pipeline.execute(request)),
                        )
                        .await
                        .unwrap();
                    assert!(!result.success);
                    assert_eq!(result.error.as_deref(), Some(format!(
                        "Step {} ({child}) failed: Goal Mode permits only synchronous foreground child execution.",
                        usize::from(ordinary_first),
                    ).as_str()));
                    assert_eq!(effects.load(Ordering::SeqCst), 0);
                    assert!(!security.tracker.is_limited_for_current(1));
                    assert!(server.received_requests().await.unwrap().is_empty());
                    let rows = fixture.handle.store.list_by_agent("main").await.unwrap();
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0].id, fixture.identity.task_id());
                }
            }
        }
        let goal = fixture
            .handle
            .goal_store()
            .unwrap()
            .get_goal_task(fixture.identity.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::Complete
        );
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            0
        );
        assert!(crate::goal_accounting::GoalForegroundContext::current().is_none());
        assert!(zeroclaw_api::tool::ToolExecutionContext::current().is_none());
    }

    #[tokio::test]
    async fn registered_goal_pipeline_rejects_recursive_delegate_at_child_depth() {
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = wiremock::MockServer::start().await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled = assemble_real_delegate_pipeline(
            real_delegate_for_server(server.uri()),
            effects.clone(),
            &[],
        )
        .await;
        let pipeline = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == tools::PipelineTool::NAME)
            .unwrap();
        let child = fixture.root.child();
        let result = GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(fixture.accountant.clone()),
                child.scope(pipeline.execute(serde_json::json!({"steps":[
                    {"tool":"delegate","args":{"agent":"worker","prompt":"recursive"}},
                    {"tool":"shell","args":{}}
                ]}))),
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains(&crate::i18n::get_required_cli_string(
                    "goal-mode-child-recursion-disallowed"
                ))
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn registered_goal_pipeline_preserves_real_delegate_failure_at_dispatch() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled = assemble_real_delegate_pipeline(
            real_delegate_for_server(server.uri()),
            effects.clone(),
            &[],
        )
        .await;
        let meta = crate::agent::turn::TurnMeta {
            parent_agent_alias: None,
            agent_alias: Some("main"),
            turn_id: "pipeline-test",
            channel_name: "test",
        };
        let dispatch = crate::agent::tool_execution::ToolDispatchContext {
            tools_registry: &assembled.registry,
            activated_tools: None,
            excluded_tools: &[],
            model_switch_callback: None,
        };
        let outcome = GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(fixture.accountant.clone()),
                fixture
                    .root
                    .scope(crate::agent::tool_execution::execute_one_tool(
                        tools::PipelineTool::NAME,
                        serde_json::json!({"result":"last","steps":[
                            {"tool":"delegate","args":{"agent":"worker","prompt":"fixture task"}},
                            {"tool":"shell","args":{}}
                        ]}),
                        None,
                        dispatch,
                        &meta,
                        &crate::observability::NoopObserver,
                        None,
                        None,
                        None,
                    )),
            )
            .await
            .unwrap();
        assert!(!outcome.success);
        let error = outcome.terminal_error.unwrap();
        assert!(error.is::<zeroclaw_api::tool::GoalChildOperationError>());
        let accounting = error
            .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
            .unwrap();
        assert_eq!(
            accounting.accounting_state(),
            zeroclaw_goal::GoalAccountingState::OutcomeUnknown
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert!(!server.received_requests().await.unwrap().is_empty());
        let goal = fixture
            .handle
            .goal_store()
            .unwrap()
            .get_goal_task(fixture.identity.task_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.accounting_state,
            zeroclaw_goal::GoalAccountingState::OutcomeUnknown
        );
        assert!(goal.pending_call_id.is_none());
    }

    #[tokio::test]
    async fn registered_goal_skill_aliases_use_locked_args_and_real_pipeline() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": "alias response"}}],
                "usage": {"prompt_tokens": 8, "completion_tokens": 3}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let fixture = registered_goal_fixture().await;
        let effects = Arc::new(AtomicUsize::new(0));
        let skill_tool = |name: &str, target: &str, key: &str, value: &str| SkillTool {
            name: name.into(),
            description: "Goal alias".into(),
            kind: "builtin".into(),
            command: String::new(),
            args: Default::default(),
            target: Some(target.into()),
            locked_args: [(key.to_string(), value.to_string())].into_iter().collect(),
            timeout_secs: None,
        };
        let skill = Skill {
            name: "ops".into(),
            description: "Goal alias fixture".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: Vec::new(),
            tools: vec![
                skill_tool("chain", tools::PipelineTool::NAME, "result", "last"),
                skill_tool("manage", "delegate", "action", "list_results"),
                skill_tool("create", "delegate", "action", "delegate"),
            ],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let assembled = assemble_real_delegate_pipeline(
            real_delegate_for_server(server.uri()),
            effects.clone(),
            &[skill],
        )
        .await;
        let find = |name: &str| {
            assembled
                .registry
                .iter()
                .find(|tool| tool.name() == name)
                .unwrap()
        };
        assert!(
            !find("ops__manage").may_invoke_agent_child(&serde_json::json!({"action":"delegate"}))
        );
        assert!(
            find("ops__create")
                .may_invoke_agent_child(&serde_json::json!({"action":"list_results"}))
        );
        let chain = find("ops__chain");
        let args = serde_json::json!({"result":"all","steps":[
            {"tool":"delegate","args":{"agent":"worker","prompt":"fixture task"}},
            {"tool":"shell","args":{}}
        ]});
        assert!(chain.may_invoke_agent_child(&args));
        let result = GOAL_OPERATION_ACCOUNTING
            .scope(
                Some(fixture.accountant.clone()),
                fixture.root.scope(chain.execute(args)),
            )
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.output.as_str(), "ran");
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .tracker
                .get_strict_usage_totals_for_task_with_pricing(fixture.identity.task_id())
                .unwrap()
                .0,
            11
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum RegisteredPipelineFailure {
        Missing,
        Invalid,
        TerminalCommit,
    }

    #[derive(Default)]
    struct RegisteredPipelineEngineObservations {
        parent_calls: AtomicUsize,
        verifier_calls: AtomicUsize,
        append_calls: AtomicUsize,
        host_errors: AtomicUsize,
        hooks: std::sync::Mutex<Vec<String>>,
        history: std::sync::Mutex<Vec<zeroclaw_providers::ChatMessage>>,
        native_chain: std::sync::Mutex<Option<String>>,
        accountant: std::sync::Mutex<
            Option<std::sync::Weak<dyn zeroclaw_goal::accounting::GoalOperationAccounting>>,
        >,
        accounting_contexts: std::sync::Mutex<Vec<(&'static str, u32)>>,
        root_deadline: std::sync::Mutex<Option<tokio::time::Instant>>,
    }

    struct RegisteredPipelineAccountingFactory {
        inner: Arc<crate::goal_accounting::RuntimeGoalOperationAccountingFactory>,
        handle: crate::control_plane::ControlPlaneHandle,
        observations: Arc<RegisteredPipelineEngineObservations>,
    }

    impl zeroclaw_goal::accounting::GoalOperationAccountingFactory
        for RegisteredPipelineAccountingFactory
    {
        fn for_scope(
            &self,
            scope: zeroclaw_goal::GoalExecutionScope,
        ) -> Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> {
            let accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting> =
                Arc::new(RegisteredPipelineAccountant {
                    inner: zeroclaw_goal::accounting::GoalOperationAccountingFactory::for_scope(
                        self.inner.as_ref(),
                        scope.clone(),
                    ),
                    scope,
                    handle: self.handle.clone(),
                    observations: self.observations.clone(),
                });
            *self.observations.accountant.lock().unwrap() = Some(Arc::downgrade(&accountant));
            accountant
        }
    }

    struct RegisteredPipelineAccountant {
        inner: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
        scope: zeroclaw_goal::GoalExecutionScope,
        handle: crate::control_plane::ControlPlaneHandle,
        observations: Arc<RegisteredPipelineEngineObservations>,
    }

    impl RegisteredPipelineAccountant {
        fn observe_context(&self, boundary: &'static str) {
            let root = crate::goal_accounting::GoalForegroundContext::current_for_child()
                .unwrap()
                .unwrap();
            assert_eq!(root.scope_identity().task_id(), self.scope.task_id());
            assert_eq!(root.scope_identity().session_id(), self.scope.session_id());
            assert_eq!(
                root.scope_identity().execution_epoch(),
                self.scope.execution_epoch()
            );
            assert!(root.depth() <= 1);
            assert!(root.deadline() <= self.observations.root_deadline.lock().unwrap().unwrap());
            assert!(!root.token().is_cancelled());
            assert!(Arc::ptr_eq(&root.control_plane().store, &self.handle.store));
            let expected = self
                .observations
                .accountant
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap();
            assert!(
                zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(|active| Arc::ptr_eq(active.as_ref().unwrap(), &expected))
                    .unwrap()
            );
            let api = zeroclaw_api::tool::ToolExecutionContext::current().unwrap();
            assert!(api.is_goal_foreground());
            assert!(api.cancellation_token().is_some());
            self.observations
                .accounting_contexts
                .lock()
                .unwrap()
                .push((boundary, root.depth()));
        }
    }

    #[async_trait]
    impl zeroclaw_goal::accounting::GoalOperationAccounting for RegisteredPipelineAccountant {
        async fn admit(
            &self,
            request: zeroclaw_goal::accounting::GoalOperationRequest,
        ) -> anyhow::Result<()> {
            self.observe_context("admit");
            self.inner.admit(request).await
        }
        async fn settle(
            &self,
            settlement: zeroclaw_goal::accounting::GoalOperationSettlement,
        ) -> anyhow::Result<()> {
            self.observe_context("settle");
            self.inner.settle(settlement).await
        }
        async fn pricing_complete(&self) -> anyhow::Result<bool> {
            self.inner.pricing_complete().await
        }
    }

    struct RegisteredPipelineHooks(Arc<RegisteredPipelineEngineObservations>);

    #[async_trait]
    impl crate::hooks::HookHandler for RegisteredPipelineHooks {
        fn name(&self) -> &str {
            "registered-pipeline-parent-hooks"
        }

        async fn before_tool_call_with_context(
            &self,
            context: &crate::hooks::ToolCallHookContext,
            tool: String,
            args: serde_json::Value,
        ) -> crate::hooks::HookResult<(String, serde_json::Value)> {
            self.0
                .hooks
                .lock()
                .unwrap()
                .push(format!("before:{tool}:{}", context.invocation_id()));
            crate::hooks::HookResult::Continue((tool, args))
        }

        async fn on_after_tool_call_with_context(
            &self,
            context: &crate::hooks::ToolCallHookContext,
            tool: &str,
            result: &ToolResult,
            _: std::time::Duration,
        ) {
            assert!(!result.success, "only the failed outer composite completes");
            self.0
                .hooks
                .lock()
                .unwrap()
                .push(format!("after:{tool}:{}", context.invocation_id()));
        }

        async fn on_tool_call_abandoned(
            &self,
            context: &crate::hooks::ToolCallHookContext,
            tool: &str,
        ) {
            self.0
                .hooks
                .lock()
                .unwrap()
                .push(format!("abandoned:{tool}:{}", context.invocation_id()));
        }
    }

    struct RegisteredPipelineParentProvider {
        observations: Arc<RegisteredPipelineEngineObservations>,
        outer: &'static str,
        child: &'static str,
        root: crate::goal_accounting::GoalForegroundContext,
        accountant: Arc<dyn zeroclaw_goal::accounting::GoalOperationAccounting>,
    }

    impl zeroclaw_api::attribution::Attributable for RegisteredPipelineParentProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "registered-pipeline-parent"
        }
    }

    #[async_trait]
    impl zeroclaw_providers::ModelProvider for RegisteredPipelineParentProvider {
        fn supports_native_tools(&self) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            _: Option<&str>,
            _: &str,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("registered parent must use the ordinary structured request path")
        }

        async fn chat(
            &self,
            _: zeroclaw_providers::ChatRequest<'_>,
            _: &str,
            _: Option<f64>,
        ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
            anyhow::ensure!(
                self.observations
                    .parent_calls
                    .fetch_add(1, Ordering::SeqCst)
                    == 0,
                "no parent model or summary call may follow the failed child"
            );
            let root = crate::goal_accounting::GoalForegroundContext::current_for_child()?.unwrap();
            assert_eq!(root.depth(), 0);
            assert_eq!(
                root.scope_identity().task_id(),
                self.root.scope_identity().task_id()
            );
            assert_eq!(
                root.scope_identity().session_id(),
                self.root.scope_identity().session_id()
            );
            assert_eq!(
                root.scope_identity().execution_epoch(),
                self.root.scope_identity().execution_epoch()
            );
            assert_eq!(root.deadline(), self.root.deadline());
            assert!(Arc::ptr_eq(
                &root.control_plane().store,
                &self.root.control_plane().store
            ));
            assert!(
                zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                    .try_with(|active| { Arc::ptr_eq(active.as_ref().unwrap(), &self.accountant) })
                    .unwrap()
            );
            assert!(
                zeroclaw_api::tool::ToolExecutionContext::current()
                    .unwrap()
                    .is_goal_foreground()
            );
            let child_args = if self.child == "delegate" {
                serde_json::json!({"agent":"worker","prompt":"return fixture evidence"})
            } else {
                serde_json::json!({"prompt":"return fixture evidence"})
            };
            Ok(zeroclaw_providers::ChatResponse {
                text: None,
                tool_calls: vec![
                    zeroclaw_providers::ToolCall {
                        id: "registered-outer".into(),
                        name: self.outer.into(),
                        arguments: serde_json::json!({"result":"all","steps":[
                            {"tool":self.child,"args":child_args},
                            {"tool":"shell","args":{"text":"internal tail must not run"}}
                        ]})
                        .to_string(),
                        extra_content: None,
                    },
                    zeroclaw_providers::ToolCall {
                        id: "registered-parent-tail".into(),
                        name: "shell".into(),
                        arguments: serde_json::json!({"text":"parent sibling must not run"})
                            .to_string(),
                        extra_content: None,
                    },
                ],
                usage: Some(zeroclaw_api::model_provider::TokenUsage {
                    input_tokens: Some(5),
                    output_tokens: Some(2),
                    ..Default::default()
                }),
                reasoning_content: None,
            })
        }
    }

    #[derive(Clone)]
    struct RegisteredPipelineEngineDriver {
        key: zeroclaw_goal::GoalSessionKey,
        handle: crate::control_plane::ControlPlaneHandle,
        config: Arc<Config>,
        registry: Arc<ScopedToolRegistry>,
        observations: Arc<RegisteredPipelineEngineObservations>,
        outer: &'static str,
        child: &'static str,
    }

    #[async_trait]
    impl zeroclaw_goal::GoalSessionDriver for RegisteredPipelineEngineDriver {
        fn session_key(&self) -> &zeroclaw_goal::GoalSessionKey {
            &self.key
        }

        async fn bind(
            &self,
            ingress: &zeroclaw_goal::GoalIngressContext,
        ) -> anyhow::Result<zeroclaw_goal::GoalSessionLease> {
            assert_eq!(ingress.session_key(), &self.key);
            Ok(zeroclaw_goal::GoalSessionLease::new(
                zeroclaw_goal::GoalSessionBinding::new(self.key.clone()),
                (),
            ))
        }

        async fn acquire_execution(
            &self,
            ingress: &zeroclaw_goal::GoalIngressContext,
            scope: &zeroclaw_goal::GoalExecutionScope,
            _: &zeroclaw_goal::checkpoint::GoalParentTurnCheckpoint,
        ) -> anyhow::Result<Box<dyn zeroclaw_goal::GoalSessionExecutionLease>> {
            assert_eq!(ingress.session_key(), &self.key);
            assert_eq!(scope.session_id(), self.key.durable_id());
            Ok(Box::new(self.clone()))
        }
    }

    #[async_trait]
    impl zeroclaw_goal::GoalSessionExecutionLease for RegisteredPipelineEngineDriver {
        fn session_key(&self) -> &zeroclaw_goal::GoalSessionKey {
            &self.key
        }
        fn canonical_history(&self) -> anyhow::Result<Vec<zeroclaw_providers::ChatMessage>> {
            Ok(vec![zeroclaw_providers::ChatMessage::user(
                "canonical fixture request",
            )])
        }

        async fn run_parent_turn(
            &mut self,
            operation: &zeroclaw_goal::GoalOperationScope,
            turn: zeroclaw_goal::GoalParentTurn,
        ) -> anyhow::Result<zeroclaw_goal::GoalParentTurnResult> {
            use crate::agent::loop_::{
                LoopKnobs, ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess,
                ResolvedRuntimeKnobs, ToolLoop,
            };
            let root = crate::goal_accounting::GoalForegroundContext::new_root(
                operation.execution().clone(),
                self.handle.clone(),
                tokio_util::sync::CancellationToken::new(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            );
            *self.observations.root_deadline.lock().unwrap() = Some(root.deadline());
            let accountant = zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                .try_with(|active| active.clone())
                .unwrap()
                .unwrap();
            let provider = RegisteredPipelineParentProvider {
                observations: self.observations.clone(),
                outer: self.outer,
                child: self.child,
                root: root.clone(),
                accountant,
            };
            let input = zeroclaw_goal::control::goal_parent_turn_input(&turn);
            let mut history =
                input.goal_start_transcript("registered parent system", turn.working_history)?;
            let mut hooks = crate::hooks::HookRunner::new();
            hooks.register(Box::new(RegisteredPipelineHooks(self.observations.clone())));
            let approval = crate::approval::ApprovalManager::for_non_interactive(
                &zeroclaw_config::schema::RiskProfileConfig {
                    auto_approve: vec![self.outer.into(), "shell".into()],
                    ..Default::default()
                },
            );
            let security = SecurityPolicy::for_agent(&self.config, "main").unwrap();
            let observer = crate::observability::NoopObserver;
            let pacing = zeroclaw_config::schema::PacingConfig::default();
            let multimodal = zeroclaw_config::schema::MultimodalConfig::default();
            let knobs = LoopKnobs::default();
            let mut breadcrumb = false;
            let mut injected = None;
            let future: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + '_>> = Box::pin(
                crate::agent::loop_::run_tool_call_loop(ToolLoop {
                    exec: ResolvedAgentExecution::resolve(
                        ResolvedModelAccess { model_provider: &provider, provider_name: "ollama.local", model: "fixture-model", dispatch_model: "fixture-model", temperature: None },
                        ResolvedIo { tools_registry: &self.registry, observer: &observer, silent: true, approval: Some(&approval), security: Some(&security), multimodal_config: &multimodal, config: Some(&self.config), hooks: Some(&hooks), activated_tools: None, model_switch_callback: None, receipt_generator: None },
                        ResolvedRuntimeKnobs { max_tool_iterations: 5, excluded_tools: &[], dedup_exempt_tools: &[], pacing: &pacing, strict_tool_parsing: false, parallel_tools: false, max_tool_result_chars: 0,
                            context_limits: zeroclaw_config::schema::ResolvedContextLimits { model_context_window: zeroclaw_config::schema::UNCONFIGURED_CONTEXT_WINDOW_FALLBACK, context_token_budget: 0, model_context_window_source: zeroclaw_config::schema::ModelContextWindowSource::CompatibilityFallback },
                            context_limits_resolver: None, knobs: &knobs }),
                    history: &mut history, history_has_trim_breadcrumb: &mut breadcrumb, injected_memory_preamble: &mut injected,
                    channel_name: "test", channel_reply_target: None, cancellation_token: None, on_delta: None, shared_budget: None, channel: None, collected_receipts: None,
                    event_tx: None, steering: None, new_messages_out: None, image_cache: None,
                    ingress: zeroclaw_api::ingress::IngressContext::sub_turn(), memory: None,
                    agent_alias: Some("main"), parent_agent_alias: None, turn_id: "registered-parent", served_route_sink: None, sop_reassembly: None,
                })
            );
            let result = root.scope(future).await;
            *self.observations.history.lock().unwrap() = history.clone();
            assert!(crate::goal_accounting::GoalForegroundContext::current().is_none());
            let error = result.expect_err("real child failure must stop the ordinary parent loop");
            assert!(
                error.is::<zeroclaw_api::tool::GoalChildOperationError>(),
                "{error:#}"
            );
            assert!(
                format!("{error:#}").contains(&format!("Pipeline step 0 ({}) failed", self.child)),
                "{error:#}"
            );
            // This fixture records the host-owned native attempt, not G5
            // transport rendering or successful delivery to a person.
            self.observations.host_errors.fetch_add(1, Ordering::SeqCst);
            *self.observations.native_chain.lock().unwrap() = Some(format!("{error:#}"));
            Err(error)
        }

        async fn run_verifier(
            &mut self,
            _: &zeroclaw_goal::GoalOperationScope,
            _: zeroclaw_goal::GoalVerifierTurn,
        ) -> anyhow::Result<zeroclaw_goal::GoalVerifierResult> {
            self.observations
                .verifier_calls
                .fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("verifier must not follow a real pipeline child failure")
        }
        async fn append_parent_candidate(&mut self, _: String) -> anyhow::Result<()> {
            self.observations
                .append_calls
                .fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("candidate append must not follow a real pipeline child failure")
        }
        async fn publish_goal_notice(
            &mut self,
            _: zeroclaw_goal::GoalExecutionNotice,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    async fn registered_goal_engine_fixture(point: RegisteredPipelineFailure, outer: &'static str) {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        use zeroclaw_goal::{
            GoalHostSettings, GoalHostSettingsSource, GoalIngressContext, GoalIngressPrincipal,
            GoalRuntime, GoalSessionKey,
        };
        let server = MockServer::start().await;
        let mut response = serde_json::json!({"choices":[{"message":{"role":"assistant","content":"actual child evidence"}}],"usage":{"prompt_tokens":8,"completion_tokens":3}});
        match point {
            RegisteredPipelineFailure::Missing => {
                response.as_object_mut().unwrap().remove("usage");
            }
            RegisteredPipelineFailure::Invalid => {
                response["usage"]["prompt_tokens_details"] = serde_json::json!({"cached_tokens":9});
            }
            RegisteredPipelineFailure::TerminalCommit => {}
        }
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
        // No seeded task: normal admission creates this fixture's sole task
        // and request. Existing seeded registered-child tests keep their path.
        let (directory, mut handle, tracker, factory) = registered_goal_resources();
        let terminal_attempts = Arc::new(AtomicUsize::new(0));
        if matches!(point, RegisteredPipelineFailure::TerminalCommit) {
            handle.store = Arc::new(ScopedHeldTerminal {
                inner: handle.store.clone(),
                mode: ScopedTerminalMode::Fail {
                    attempts: terminal_attempts.clone(),
                },
            });
        }
        let mut config = real_pipeline_child_config_in(directory.path(), server.uri());
        config.goal.enabled = true;
        config.goal.default_token_limit = Some(1000);
        config.goal.default_cost_limit_usd = None;
        config.goal.verifier.model_provider = "ollama.local".into();
        config.goal.verifier.model = Some("fixture-model".into());
        config
            .risk_profiles
            .get_mut("fixture")
            .unwrap()
            .allowed_tools
            .extend([outer.into(), "shell".into()]);
        config.validate_goal_config().unwrap();
        let config = Arc::new(config);
        let child = if matches!(point, RegisteredPipelineFailure::TerminalCommit) {
            "spawn_subagent"
        } else {
            "delegate"
        };
        let actual_child: Arc<dyn Tool> = if child == "delegate" {
            real_delegate_for_server(server.uri())
        } else {
            Arc::new(crate::tools::SpawnSubagentTool::new(
                config.clone(),
                "main",
                Arc::new(SecurityPolicy::for_agent(&config, "main").unwrap()),
            ))
        };
        let skill = Skill {
            name: "ops".into(),
            description: "Actual parent pipeline alias".into(),
            description_localizations: Default::default(),
            version: "1.0.0".into(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "chain".into(),
                description: "Keep intermediate child evidence private".into(),
                kind: "builtin".into(),
                command: String::new(),
                args: Default::default(),
                target: Some(tools::PipelineTool::NAME.into()),
                locked_args: [("result".into(), "last".into())].into_iter().collect(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let effects = Arc::new(AtomicUsize::new(0));
        let assembled =
            assemble_real_children_pipeline(vec![actual_child], effects.clone(), &[skill]).await;
        let observations = Arc::new(RegisteredPipelineEngineObservations::default());
        let key = GoalSessionKey::matrix("matrix_registered_pipeline_engine").unwrap();
        let driver = Arc::new(RegisteredPipelineEngineDriver {
            key: key.clone(),
            handle: handle.clone(),
            config: config.clone(),
            registry: Arc::new(assembled.registry),
            observations: observations.clone(),
            outer,
            child,
        });
        let ingress = GoalIngressContext::trusted(
            key.clone(),
            "main",
            "matrix:fixture",
            GoalIngressPrincipal::Matrix {
                raw_mxid: "@fixture:example.org".into(),
            },
        )
        .unwrap();
        let runtime = GoalRuntime::new(handle.goal_store().unwrap());
        let settings =
            GoalHostSettings::from_config(&config, std::process::id(), handle.boot_id.clone())
                .unwrap();
        let source = GoalHostSettingsSource::new(
            zeroclaw_config::live::LiveConfig::new(config.as_ref().clone()).handle(),
            std::process::id(),
            handle.boot_id.clone(),
        )
        .unwrap();
        let request = runtime
            .submit(
                &settings,
                ingress,
                driver,
                zeroclaw_commands::goal::GoalCommand::Start {
                    budget: zeroclaw_commands::goal::GoalBudgetSelection::Defaults,
                    objective: "exercise the actual registered child failure".into(),
                },
            )
            .await
            .unwrap()
            .into_parts()
            .1
            .unwrap();
        let scope = request.scope().clone();
        let engine = runtime.execution_engine(Arc::new(RegisteredPipelineAccountingFactory {
            inner: factory,
            handle: handle.clone(),
            observations: observations.clone(),
        }));
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.run(
                &source,
                request,
                zeroclaw_goal::checkpoint::GoalParentTurnCheckpoint::new(),
            ),
        )
        .await
        .expect("real registered child failure must settle")
        .unwrap_err();
        assert!(
            error.is::<zeroclaw_api::tool::GoalChildOperationError>(),
            "{error:#}"
        );
        let native_chain = observations.native_chain.lock().unwrap().clone().unwrap();
        assert!(
            native_chain.starts_with(&format!("Pipeline step 0 ({child}) failed: ")),
            "{native_chain}"
        );
        match point {
            RegisteredPipelineFailure::Missing => {
                assert!(native_chain.contains("missing usage"), "{native_chain}")
            }
            RegisteredPipelineFailure::Invalid => assert!(
                native_chain.contains("CachedInputExceedsInput"),
                "{native_chain}"
            ),
            RegisteredPipelineFailure::TerminalCommit => assert_eq!(
                native_chain
                    .matches("registered pipeline child terminal storage failure")
                    .count(),
                1
            ),
        }
        match point {
            RegisteredPipelineFailure::Missing | RegisteredPipelineFailure::Invalid => {
                assert_eq!(format!("{error:#}"), native_chain);
            }
            RegisteredPipelineFailure::TerminalCommit => assert_eq!(
                format!("{error:#}"),
                format!("Goal parent operation failed: {native_chain}")
            ),
        }
        let (state, tokens, rows, reason) = match point {
            RegisteredPipelineFailure::Missing => (
                zeroclaw_goal::GoalAccountingState::Missing,
                7,
                1,
                "accounting_missing_or_invalid",
            ),
            // The nine cached-input tokens prove a larger input lower bound
            // than the inconsistent reported eight: retain 9 + 3, unpriced.
            RegisteredPipelineFailure::Invalid => (
                zeroclaw_goal::GoalAccountingState::Invalid,
                19,
                2,
                "accounting_missing_or_invalid",
            ),
            RegisteredPipelineFailure::TerminalCommit => (
                zeroclaw_goal::GoalAccountingState::Complete,
                18,
                2,
                "parent_operation_failed",
            ),
        };
        if matches!(point, RegisteredPipelineFailure::TerminalCommit) {
            assert!(error.is::<ScopedChildTerminalFailure>(), "{error:#}");
            assert!(!error.is::<zeroclaw_goal::accounting::GoalAccountingFailure>());
            assert_eq!(terminal_attempts.load(Ordering::SeqCst), 1);
        } else {
            let accounting = error
                .downcast_ref::<zeroclaw_goal::accounting::GoalAccountingFailure>()
                .unwrap();
            assert_eq!(accounting.accounting_state(), state);
            assert!(!accounting.has_pending_operation());
            assert!(!error.is::<ScopedChildTerminalFailure>());
            assert_eq!(terminal_attempts.load(Ordering::SeqCst), 0);
        }
        let store = handle.goal_store().unwrap();
        let task = store
            .current_goal_for_session(scope.session_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(task.id, scope.task_id());
        assert_eq!(task.status, crate::control_plane::TaskStatus::Failed);
        assert_eq!(task.execution_epoch, scope.execution_epoch() + 1);
        assert_eq!(
            store
                .terminal_reason_for_session_goal(scope.task_id(), scope.session_id())
                .await
                .unwrap()
                .as_deref(),
            Some(reason)
        );
        let goal = store.get_goal_task(scope.task_id()).await.unwrap().unwrap();
        assert_eq!(goal.accounting_state, state);
        assert!(goal.pending_call_id.is_none());
        assert!(goal.pending_call_epoch.is_none());
        let totals = tracker
            .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
            .unwrap();
        assert_eq!(totals.0, tokens);
        assert_eq!(
            totals.2,
            !matches!(point, RegisteredPipelineFailure::Invalid)
        );
        assert_eq!(
            tracker
                .get_summary_for_task(scope.task_id())
                .unwrap()
                .request_count,
            rows
        );
        let ledger: Vec<crate::cost::types::CostRecord> =
            std::fs::read_to_string(directory.path().join("state/costs.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert_eq!(ledger.len(), rows);
        assert!(
            ledger
                .iter()
                .all(|row| row.task_id.as_deref() == Some(scope.task_id())
                    && row.provider_ref.as_deref() == Some("ollama.local")
                    && row.usage.model == "fixture-model")
        );
        assert_eq!(ledger[0].usage.input_tokens, 5);
        assert_eq!(ledger[0].usage.output_tokens, 2);
        if matches!(point, RegisteredPipelineFailure::Invalid) {
            assert_eq!(ledger[1].usage.input_tokens, 9);
            assert_eq!(ledger[1].usage.output_tokens, 3);
            assert_eq!(ledger[1].usage.unpriced_tokens, 12);
            assert!(!ledger[1].usage.pricing_available);
        }
        let reopened = zeroclaw_config::cost::CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: false,
                ..Default::default()
            },
            directory.path(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .get_strict_usage_totals_for_task_with_pricing(scope.task_id())
                .unwrap()
                .0,
            tokens
        );
        assert_eq!(
            reopened
                .get_summary_for_task(scope.task_id())
                .unwrap()
                .request_count,
            rows
        );
        let children = handle.store.list_by_agent("main").await.unwrap();
        let children: Vec<_> = children
            .iter()
            .filter(|row| row.kind == crate::control_plane::TaskKind::Subagent)
            .collect();
        if matches!(point, RegisteredPipelineFailure::TerminalCommit) {
            let [child] = children.as_slice() else {
                panic!("one actual spawned child must exist");
            };
            assert_eq!(child.status, crate::control_plane::TaskStatus::Running);
            assert_eq!(child.parent_id.as_deref(), Some(scope.task_id()));
            assert_eq!(child.depth, 1);
            assert_eq!(child.owner_pid, std::process::id());
            assert_eq!(child.owner_boot_id, handle.boot_id);
        } else {
            assert!(children.is_empty());
        }
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(observations.parent_calls.load(Ordering::SeqCst), 1);
        assert_eq!(observations.host_errors.load(Ordering::SeqCst), 1);
        assert_eq!(observations.verifier_calls.load(Ordering::SeqCst), 0);
        assert_eq!(observations.append_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *observations.accounting_contexts.lock().unwrap(),
            [("admit", 0), ("settle", 0), ("admit", 1), ("settle", 1),]
        );
        assert_eq!(
            *observations.hooks.lock().unwrap(),
            [
                format!("before:{outer}:registered-parent:0:0"),
                "before:shell:registered-parent:0:1".into(),
                format!("after:{outer}:registered-parent:0:0"),
                "abandoned:shell:registered-parent:0:1".into(),
            ]
        );
        {
            let history = observations.history.lock().unwrap();
            let encoded = serde_json::to_value(&*history).unwrap().to_string();
            for id in ["registered-outer", "registered-parent-tail"] {
                assert_eq!(
                    encoded.matches(id).count(),
                    2,
                    "one ordinary call/result pair for {id}"
                );
            }
            let completed = history
                .iter()
                .find(|row| row.role == "tool" && row.content.contains("registered-outer"))
                .unwrap();
            assert!(
                completed
                    .content
                    .contains(&format!("Pipeline step 0 ({child}) failed"))
            );
            assert!(
                !completed.content.contains("actual child evidence"),
                "result=last cannot leak successful intermediate text into the error result"
            );
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert!(crate::goal_accounting::GoalForegroundContext::current().is_none());
        assert!(zeroclaw_api::tool::ToolExecutionContext::current().is_none());
        assert!(
            zeroclaw_goal::accounting::GOAL_OPERATION_ACCOUNTING
                .try_with(|_| ())
                .is_err()
        );
    }

    #[tokio::test]
    async fn registered_goal_pipeline_missing_usage_reaches_parent_hooks_history_and_engine() {
        for outer in [tools::PipelineTool::NAME, "ops__chain"] {
            registered_goal_engine_fixture(RegisteredPipelineFailure::Missing, outer).await;
        }
    }
    #[tokio::test]
    async fn registered_goal_pipeline_invalid_usage_reaches_parent_hooks_history_and_engine() {
        for outer in [tools::PipelineTool::NAME, "ops__chain"] {
            registered_goal_engine_fixture(RegisteredPipelineFailure::Invalid, outer).await;
        }
    }
    #[tokio::test]
    async fn registered_goal_pipeline_real_terminal_failure_reaches_parent_hooks_history_and_engine()
     {
        for outer in [tools::PipelineTool::NAME, "ops__chain"] {
            registered_goal_engine_fixture(RegisteredPipelineFailure::TerminalCommit, outer).await;
        }
    }

    async fn assemble_pipeline(
        security: Arc<SecurityPolicy>,
        skills: &[Skill],
        calls: Arc<AtomicUsize>,
        caller_allowed: Option<&[String]>,
    ) -> ScopedAssembled {
        let mut config = Config::default();
        config.pipeline.enabled = true;
        config
            .tools
            .optional
            .push(tools::PipelineTool::NAME.to_string());
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
        config
            .tools
            .optional
            .push(tools::PipelineTool::NAME.to_string());
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

    fn builtin_deferral_config(enabled: bool) -> Config {
        use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

        let mut config = Config::default();
        config.runtime_profiles.insert(
            "catalog".into(),
            RuntimeProfileConfig {
                deferred_builtin_tools: enabled,
                ..RuntimeProfileConfig::default()
            },
        );
        config.agents.insert(
            "default".into(),
            AliasedAgentConfig {
                runtime_profile: "catalog".into(),
                ..AliasedAgentConfig::default()
            },
        );
        config
    }

    async fn assemble_builtin_catalog(
        config: &Config,
        security: &Arc<SecurityPolicy>,
        built: AllToolsResult,
        caller_allowed: Option<&[String]>,
        listing: bool,
    ) -> ScopedAssembled {
        ScopedToolRegistry::assemble(ScopedAssembly {
            config,
            agent_alias: "default",
            security,
            built,
            skills: &[],
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: false,
            list_deferred_mcp_specs: listing,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await
    }

    fn search_schemas(result: &ToolResult) -> Vec<serde_json::Value> {
        result
            .output
            .lines()
            .filter_map(|line| line.strip_prefix("<function>")?.strip_suffix("</function>"))
            .map(|schema| serde_json::from_str(schema).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn builtin_deferral_preserves_core_schemas_and_executable_registry() {
        let config = builtin_deferral_config(true);
        let security = Arc::new(SecurityPolicy {
            excluded_tools: Some(vec!["denied".into()]),
            ..SecurityPolicy::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let assembled = assemble_builtin_catalog(
            &config,
            &security,
            built_with_counting_tools(
                Arc::clone(&calls),
                &[
                    "shell",
                    "file_read",
                    "file_write",
                    "file_edit",
                    "memory_recall",
                    "memory_store",
                    "memory_export",
                    "calendar",
                    "denied",
                ],
            ),
            None,
            false,
        )
        .await;
        assert_eq!(
            assembled.registry.hidden_builtin_names(),
            HashSet::from(["memory_export".to_string(), "calendar".to_string()])
        );
        assert_eq!(assembled.registry.len(), 9);
        assert!(
            assembled
                .registry
                .iter()
                .all(|tool| tool.name() != "denied")
        );
        let calendar = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == "calendar")
            .unwrap();
        assert!(
            calendar
                .execute(serde_json::json!({}))
                .await
                .unwrap()
                .success
        );
        let expected_schema = serde_json::to_value(calendar.spec()).unwrap();
        let search = assembled.tool_search_handle.as_ref().unwrap();
        let denied = search
            .execute(serde_json::json!({"query": "select:denied"}))
            .await
            .unwrap();
        assert!(search_schemas(&denied).is_empty());
        let selected = search
            .execute(serde_json::json!({"query": "select:calendar"}))
            .await
            .unwrap();
        assert_eq!(search_schemas(&selected), vec![expected_schema]);
        assert_eq!(
            assembled.registry.hidden_builtin_names(),
            HashSet::from(["memory_export".to_string()])
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let activated = assembled.activated_handle.as_ref().unwrap().lock().unwrap();
        assert!(activated.get("calendar").is_none());
        assert!(activated.tool_specs().is_empty());
    }

    #[tokio::test]
    async fn builtin_schemas_stay_eager_without_opt_in_or_discovery() {
        let cases = [
            (false, SecurityPolicy::default(), None),
            (
                true,
                SecurityPolicy {
                    excluded_tools: Some(vec!["tool_search".into()]),
                    ..SecurityPolicy::default()
                },
                None,
            ),
            (
                true,
                SecurityPolicy {
                    allowed_tools: Some(vec!["shell".into(), "calendar".into()]),
                    ..SecurityPolicy::default()
                },
                None,
            ),
            (
                true,
                SecurityPolicy::default(),
                Some(vec!["shell".to_string(), "calendar".to_string()]),
            ),
        ];
        for (enabled, policy, caller_allowed) in cases {
            let config = builtin_deferral_config(enabled);
            let assembled = assemble_builtin_catalog(
                &config,
                &Arc::new(policy),
                built_with(vec![
                    Box::new(MockTool("shell")),
                    Box::new(MockTool("calendar")),
                ]),
                caller_allowed.as_deref(),
                false,
            )
            .await;
            assert!(assembled.registry.hidden_builtin_names().is_empty());
            assert_eq!(assembled.registry.len(), 2);
            assert!(assembled.tool_search_handle.is_none());
            assert!(assembled.activated_handle.is_none());
        }
    }

    #[tokio::test]
    async fn listing_assembly_keeps_builtin_schemas_eager() {
        let config = builtin_deferral_config(true);
        let assembled = assemble_builtin_catalog(
            &config,
            &Arc::new(SecurityPolicy::default()),
            built_with(vec![
                Box::new(MockTool("shell")),
                Box::new(MockTool("calendar")),
            ]),
            None,
            true,
        )
        .await;
        assert!(assembled.registry.hidden_builtin_names().is_empty());
        assert_eq!(assembled.registry.len(), 2);
        assert!(assembled.tool_search_handle.is_none());
        assert!(assembled.activated_handle.is_none());
    }

    #[tokio::test]
    async fn builtin_deferral_prunes_removed_metadata_and_selection() {
        let config = builtin_deferral_config(true);
        let mut assembled = assemble_builtin_catalog(
            &config,
            &Arc::new(SecurityPolicy::default()),
            built_with(vec![
                Box::new(MockTool("shell")),
                Box::new(MockTool("calendar")),
                Box::new(MockTool("weather")),
            ]),
            None,
            false,
        )
        .await;
        let search = Arc::clone(assembled.tool_search_handle.as_ref().unwrap());
        search
            .execute(serde_json::json!({"query": "select:calendar"}))
            .await
            .unwrap();
        assembled.registry.retain(|tool| tool.name() != "calendar");
        assert_eq!(
            assembled.registry.hidden_builtin_names(),
            HashSet::from(["weather".to_string()])
        );
        assert!(!search.deferred_prompt_section().contains("calendar"));
        let removed = search
            .execute(serde_json::json!({"query": "select:calendar"}))
            .await
            .unwrap();
        assert!(search_schemas(&removed).is_empty());

        search
            .execute(serde_json::json!({"query": "select:weather"}))
            .await
            .unwrap();
        let principal_allowed = vec!["shell".to_string(), "tool_search".to_string()];
        search.narrow_to_caller(&principal_allowed);
        assert!(search.deferred_prompt_section().is_empty());
        let removed = search
            .execute(serde_json::json!({"query": "select:weather"}))
            .await
            .unwrap();
        assert!(search_schemas(&removed).is_empty());
        assembled
            .registry
            .retain(|tool| principal_allowed.iter().any(|name| name == tool.name()));
        assert!(assembled.registry.hidden_builtin_names().is_empty());
        assert!(
            !assembled
                .activated_handle
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .has_deferred_builtin_schemas()
        );
    }

    #[tokio::test]
    async fn removing_builtin_search_restores_surviving_eager_schemas() {
        let config = builtin_deferral_config(true);
        let mut assembled = assemble_builtin_catalog(
            &config,
            &Arc::new(SecurityPolicy::default()),
            built_with(vec![
                Box::new(MockTool("shell")),
                Box::new(MockTool("calendar")),
            ]),
            None,
            false,
        )
        .await;
        assert!(
            assembled
                .registry
                .hidden_builtin_names()
                .contains("calendar")
        );
        assembled
            .registry
            .retain(|tool| tool.name() != "tool_search");
        assert_eq!(assembled.registry.len(), 2);
        assert!(assembled.registry.hidden_builtin_names().is_empty());
        assert!(
            assembled
                .tool_search_handle
                .as_ref()
                .unwrap()
                .deferred_prompt_section()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deferred_memory_selection_uses_rebound_backend() {
        use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};
        use zeroclaw_tools::memory_export::MemoryExportTool;

        let original_dir = tempfile::TempDir::new().unwrap();
        let current_dir = tempfile::TempDir::new().unwrap();
        let original: Arc<dyn Memory> =
            Arc::new(SqliteMemory::new("original", original_dir.path()).unwrap());
        let current: Arc<dyn Memory> =
            Arc::new(SqliteMemory::new("current", current_dir.path()).unwrap());
        original
            .store("shared", "original-backend", MemoryCategory::Core, None)
            .await
            .unwrap();
        current
            .store("private", "current-backend", MemoryCategory::Core, None)
            .await
            .unwrap();
        let config = builtin_deferral_config(true);
        let security = Arc::new(SecurityPolicy::default());
        let mut assembled = assemble_builtin_catalog(
            &config,
            &security,
            built_with(vec![Box::new(MemoryExportTool::new(Arc::clone(&original)))]),
            None,
            false,
        )
        .await;
        assert!(
            assembled
                .registry
                .hidden_builtin_names()
                .contains("memory_export")
        );
        let selected = assembled
            .tool_search_handle
            .as_ref()
            .unwrap()
            .execute(serde_json::json!({"query": "select:memory_export"}))
            .await
            .unwrap();
        assert_eq!(search_schemas(&selected)[0]["name"], "memory_export");
        assert!(assembled.registry.hidden_builtin_names().is_empty());
        assembled
            .registry
            .rebind_memory_tools(Arc::clone(&current), Arc::clone(&security));
        assert!(assembled.registry.hidden_builtin_names().is_empty());
        assert!(
            assembled
                .activated_handle
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .get("memory_export")
                .is_none()
        );
        let result = assembled.registry[0]
            .execute(serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.success);
        let entries: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 1);
        assert_eq!(entries[0]["content"], "current-backend");
        assert!(!result.output.contains("original-backend"));
    }

    #[tokio::test]
    async fn scoped_assembly_threads_configured_nat64_prefixes_to_skill_http() {
        let mut config = Config::default();
        config.security.nat64_prefixes = vec!["2001:4860:4860::/96".to_string()];
        let skill = Skill {
            name: "net".to_string(),
            description: "network skill".to_string(),
            description_localizations: Default::default(),
            version: "1".to_string(),
            author: None,
            tags: Vec::new(),
            tools: vec![SkillTool {
                name: "fetch".to_string(),
                description: "fetch".to_string(),
                kind: "http".to_string(),
                command: "https://[2001:4860:4860::a00:1]/".to_string(),
                args: Default::default(),
                target: None,
                locked_args: Default::default(),
                timeout_secs: None,
            }],
            prompts: Vec::new(),
            slash_options: Vec::new(),
            always: false,
            location: None,
        };
        let security = Arc::new(SecurityPolicy::default());
        let assembled = ScopedToolRegistry::assemble(ScopedAssembly {
            config: &config,
            agent_alias: "default",
            security: &security,
            built: built_with(Vec::new()),
            skills: std::slice::from_ref(&skill),
            runtime: Arc::new(crate::platform::NativeRuntime::new()),
            caller_allowed: None,
            connect_mcp: false,
            connect_peripherals: false,
            exclude_memory: false,
            acp_delivery: true,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: false,
            mcp_registry: None,
        })
        .await;
        let tool = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == "net__fetch")
            .expect("HTTP skill must be registered");
        let result = tool.execute(serde_json::json!({})).await.unwrap();
        assert!(!result.success);
        assert_eq!(
            result.error.as_deref(),
            Some("HTTP destination rejected by network policy")
        );
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
            registry: ScopedToolRegistry(Vec::new(), None),
            delegate_handle: None,
            ask_user_handle: None,
            reaction_handle: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            poll_handle: None,
            escalate_handle: None,
            channel_room_handle: None,
            deferred_section: deferred.to_string(),
            pinned_section: pinned.to_string(),
            pinned_blocks: Vec::new(),
            activated_handle: None,
            tool_search_handle: None,
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
