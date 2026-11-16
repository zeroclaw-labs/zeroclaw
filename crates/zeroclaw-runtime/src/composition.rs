//! Runtime composition contract: the capabilities an embedder supplies.
//!
//! The agent, turn-loop, owned-execution, delegate and cron entry points take
//! these capabilities through their `*_with_capabilities` forms. Their older
//! signatures remain as adapters that build a config-backed set from the
//! config they were given and delegate, so existing callers keep today's
//! construction until they move.
//! `docs/book/src/architecture/runtime-composition.md` sets out the ownership
//! and lifetime rules and the order in which entry points move onto this
//! contract.
//!
//! The runtime asks for capabilities through *sources* rather than receiving
//! finished instances, because it resolves them per agent, per configured
//! provider reference, and per config generation. A source decides how to
//! build or cache what it returns; the runtime decides when to ask, which
//! agent it is asking for, and which resolved security policy applies.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_api::observability_traits::Observer;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

use crate::tools::AllToolsResult;

mod config_backed;
#[cfg(all(test, unix))]
mod supplied_only_traps;

/// The config-backed sources the compatibility adapters use, for an
/// application layer that composes the same recipe.
pub mod defaults {
    pub use super::config_backed::{
        ConfigMemory, ConfigProviders, NoOutboundChannels, NoSuppliedTools, native_compat_registry,
    };
}

/// Every capability the runtime obtains from outside itself, for one config
/// generation.
///
/// Immutable once built. A live config apply that changes capability wiring
/// builds a new value for the new generation instead of mutating this one, so
/// a turn that started under one generation finishes with the capabilities it
/// started with.
#[derive(Clone)]
pub struct RuntimeCapabilities {
    /// Model providers, resolved per agent and provider reference.
    pub providers: Arc<dyn ProviderSource>,
    /// Memory backends, resolved per agent.
    pub memory: Arc<dyn MemorySource>,
    /// Tool registries, built per agent against the policy the runtime resolved.
    pub tools: Arc<dyn ToolSource>,
    /// Outbound delivery channels, looked up by configured channel alias.
    pub channels: Arc<dyn ChannelSource>,
    /// The process-level observer for this generation.
    pub observer: Arc<dyn Observer>,
}

/// Capabilities bound to the principal an entry point resolved for.
///
/// This is what an operation started from inside a turn inherits: a
/// delegate target, a spawned sub-agent, a peer agent's turn, or a
/// cross-agent SOP step. Carrying the pair keeps such an operation on the
/// entry point's sources and identity instead of falling back to
/// config-backed sources and no principal.
#[derive(Clone)]
pub struct BoundCapabilities {
    /// The capabilities the originating entry point was given.
    pub capabilities: RuntimeCapabilities,
    /// The principal the originating entry point resolved for, if any.
    pub principal: Option<PrincipalId>,
}

/// Where a tool that starts child operations reads the capabilities it
/// inherits. Empty until the entry point that owns the tool's registry binds
/// it; while empty the tool keeps its pre-capability behavior.
pub type CapabilitySlot = Arc<std::sync::OnceLock<BoundCapabilities>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistryConstruction {
    SuppliedOnly,
    NativeCompat,
}

pub(crate) struct PreparedRegistry {
    pub(crate) built: crate::tools::AllToolsResult,
    pub(crate) construction: RegistryConstruction,
    pub(crate) supplied_tail: std::ops::Range<usize>,
}

impl RuntimeCapabilities {
    /// The capabilities today's `create_*` factories produce for `config`.
    ///
    /// Backs the compatibility adapters: an old entry point builds this and
    /// calls its `*_with_capabilities` form, so its construction is unchanged.
    /// Built per call, as the adapters' callers built their capabilities
    /// before; the application layer's own set replaces it as callers move.
    pub(crate) fn config_backed(config: &Config) -> Self {
        config_backed::capabilities(config)
    }

    /// The config-backed sources ([`defaults`]) with `observer`: the set the
    /// compatibility adapters build, for an application layer that owns the
    /// observer's lifetime. Every provider, memory and tool request resolves
    /// exactly as it does through an adapter.
    pub fn config_backed_with_observer(observer: Arc<dyn Observer>) -> Self {
        config_backed::capabilities_with_observer(observer)
    }

    /// The config-backed set with a no-op observer, for an adapter whose
    /// path builds providers, memory or tools but never created an observer.
    /// Building the configured observer there would add exporter setup that
    /// path never performed.
    pub(crate) fn config_backed_unobserved() -> Self {
        config_backed::unobserved_capabilities()
    }

    /// Ask the provider source for `request` and hand back the owned handle
    /// the agent and turn loop hold.
    pub(crate) fn model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Box<dyn ModelProvider>> {
        let provider = self.providers.model_provider(request)?;
        Ok(Box::new(provider))
    }

    /// Ask the memory source for `agent_alias`'s store in `config`.
    pub(crate) async fn agent_memory(
        &self,
        config: &Config,
        agent_alias: &str,
    ) -> anyhow::Result<Arc<dyn Memory>> {
        self.memory
            .memory(&MemoryRequest {
                config,
                agent_alias,
            })
            .await
    }

    /// Bind the selected registry to these capabilities and to
    /// `principal`, before the registry reaches `ScopedToolRegistry::assemble`.
    ///
    /// Every tool in the registry that starts child operations (the delegate,
    /// `spawn_subagent`, `send_message_to_peer`) runs them through these
    /// capabilities and for this principal from now on, so a child does not
    /// step outside the entry point's providers or lose its identity.
    ///
    /// The tool source's tools join `tools` only, so the agent's
    /// `allowed_tools` and `excluded_tools` filter and the caller's selector
    /// apply to them exactly as to runtime tools. They stay out of
    /// `unfiltered_tool_arcs`, which skill elevation resolves against, so a
    /// skill cannot raise a source tool past that filter. A source tool whose
    /// name is already registered is dropped, and the first tool keeps the
    /// name: in native compatibility mode that is the native recipe's tool,
    /// and in both modes an earlier source tool wins over a later one. The
    /// surviving source vector, including an empty one, goes through the
    /// ordinary narrowing gates.
    pub(crate) fn prepare_registry(
        &self,
        mut built: crate::tools::AllToolsResult,
        construction: RegistryConstruction,
        request: &ToolRequest<'_>,
        principal: Option<&PrincipalId>,
    ) -> anyhow::Result<PreparedRegistry> {
        for slot in &built.capability_slots {
            let _ = slot.set(BoundCapabilities {
                capabilities: self.clone(),
                principal: principal.cloned(),
            });
        }
        let supplied = self.tools.tools(request)?;
        let start = built.tools.len();
        for tool in supplied {
            if let Some(index) = built
                .tools
                .iter()
                .position(|existing| existing.name() == tool.name())
            {
                let kept = if index < start {
                    "runtime tool"
                } else {
                    "earlier source tool"
                };
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "agent": request.agent_alias,
                            "tool": tool.name(),
                            "kept": kept,
                        })),
                    "tool source supplied a tool whose name is already registered; keeping the first"
                );
                continue;
            }
            built.tools.push(tool);
        }
        let supplied_tail = start..built.tools.len();
        Ok(PreparedRegistry {
            built,
            construction,
            supplied_tail,
        })
    }
}

/// What the runtime is asking a [`ProviderSource`] for.
pub struct ProviderRequest<'a> {
    /// The config generation the request belongs to.
    pub config: &'a Config,
    /// The agent the provider serves.
    pub agent_alias: &'a str,
    /// `None` resolves the agent's configured provider. `Some` names an
    /// explicit provider reference, as a model switch or a delegate target does.
    pub provider_ref: Option<&'a str>,
    /// The model the provider serves when a turn names none. `None` uses the
    /// model configured on the resolved provider entry. The runtime passes the
    /// model it resolved, so a source never has to repeat that precedence.
    pub model: Option<&'a str>,
    /// The principal the provider is resolved for, when the entry point knows
    /// one. `None` means the caller has no resolved principal (a local CLI run,
    /// or an entry point that has not moved onto principal-aware routing). A
    /// source may use it to select credentials or quotas; it grants nothing.
    pub principal: Option<&'a PrincipalId>,
}

/// Supplies model providers.
pub trait ProviderSource: Send + Sync {
    /// Return the provider for `request`. A source may cache and share
    /// providers; the caller holds the returned handle only for the session or
    /// turn that asked for it.
    fn model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>>;

    /// Return the provider an agent switches to mid-session. `provider_ref`
    /// and `model` name the switch target, and `principal` is the one the
    /// agent was built for.
    ///
    /// Defaults to [`Self::model_provider`]. Override it only when a switch
    /// selects credentials or options differently from starting a session on
    /// the same reference, as the runtime's config-backed source does to keep
    /// the agent's existing switch behavior.
    fn switched_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        self.model_provider(request)
    }

    /// Return the provider that serves an image-bearing turn when the turn's
    /// provider lacks vision and `[multimodal] vision_model_provider` names a
    /// vision route. `provider_ref` is that route, `model` the vision model the
    /// runtime resolved, and `principal` the one the turn was built for.
    ///
    /// Defaults to [`Self::model_provider`]. A refusal ends the turn's vision
    /// routing; the runtime does not retry through config. Override it only
    /// when a vision route resolves differently from starting a session on the
    /// same reference, as the runtime's config-backed source does to keep the
    /// existing vision-route construction.
    fn vision_model_provider(
        &self,
        request: &ProviderRequest<'_>,
    ) -> anyhow::Result<Arc<dyn ModelProvider>> {
        self.model_provider(request)
    }
}

/// What the runtime is asking a [`MemorySource`] for.
pub struct MemoryRequest<'a> {
    /// The config generation the request belongs to.
    pub config: &'a Config,
    /// The agent whose memory is requested.
    pub agent_alias: &'a str,
}

/// Supplies memory backends.
///
/// Asynchronous because opening a store can touch disk or the network.
#[async_trait]
pub trait MemorySource: Send + Sync {
    /// Return the memory backend for `request`. Two requests for the same
    /// agent in one generation must reach the same underlying store.
    async fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>>;
}

/// What the runtime is asking a [`ToolSource`] for.
///
/// Every field that carries authority is resolved by the runtime before the
/// request is made. Native sources are trusted implementations required to
/// build tools against the supplied policy and runtime adapter. These inputs
/// do not mechanically confine in-process code: a source can return arbitrary
/// native tools. Runtime admission, policy filtering, and per-call gating and
/// approval remain independent requirements.
pub struct ToolRequest<'a> {
    /// The config generation the request belongs to.
    pub config: &'a Arc<Config>,
    /// The agent the registry is for.
    pub agent_alias: &'a str,
    /// The security policy the runtime resolved for this agent. Constructing a
    /// tool against it does not authorize any call: the runtime still gates
    /// and approves every invocation after construction.
    pub security: &'a Arc<SecurityPolicy>,
    /// The execution environment the runtime selected for this agent.
    pub runtime: &'a Arc<dyn RuntimeAdapter>,
    /// The agent's memory as resolved for this build. During bounded
    /// delegation's selection it is the parent registry's memory handle, not
    /// an opened target store, so selection may only compare its identity.
    /// Principal routing of the session's memory happens after construction
    /// and does not reach tools a source builds on this handle.
    pub memory: &'a Arc<dyn Memory>,
}

/// What the runtime hands a native recipe when it builds an eager registry.
///
/// The fields are the caller's own handles for this build, not copies; a
/// recipe may let the tools it returns keep the ones they need, but must not
/// keep them anywhere else. The struct is `#[non_exhaustive]`: the runtime
/// builds it, and new inputs can be added without breaking existing recipes.
#[non_exhaustive]
pub struct NativeRecipeInputs<'a> {
    /// The same request the source's selection and `tools()` receive.
    pub request: &'a ToolRequest<'a>,
    /// The workspace directory the native tools work in.
    pub workspace_dir: &'a std::path::Path,
    /// The canvas store, for entry points that have one.
    pub canvas_store: Option<crate::tools::CanvasStore>,
    /// Whether the build is for a spawned subagent.
    pub is_subagent_caller: bool,
    /// Environment the caller forwards to native shell tools.
    pub forwarded_environment: Option<crate::tools::ForwardedEnvironment>,
    /// The SOP engine the entry point built or was given, if SOPs are on.
    pub sop_engine: Option<Arc<std::sync::Mutex<crate::sop::SopEngine>>>,
    /// The SOP audit log that goes with `sop_engine`.
    pub sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,
    /// The live config handle, for tools that read the current generation.
    pub live_config: Option<zeroclaw_config::live::LiveConfigHandle>,
    /// The caller's execution capability, for tools that start nested work.
    pub execution_capability: Option<crate::live_config_authority::AgentExecutionCapability>,
    /// A read view of ACP sessions, for entry points that serve ACP.
    pub acp_sessions: Option<crate::tools::AcpSessionReadView>,
    /// The caller's tool ceiling for this build, when the entry point runs
    /// under one, or `None` for a root entry point with no caller. The
    /// scheduler, SOP and relay tools persist or launch work that outlives the
    /// turn, so they need the ceiling's value and not only the filtered list.
    /// Crate-private: only the runtime's own entry points set it. A recipe
    /// that forwards `inputs` to [`defaults::native_compat_registry`] carries it
    /// without seeing it; a recipe in another crate cannot read it, so one that
    /// builds the scheduler, SOP or relay tools itself builds them without a
    /// ceiling.
    pub(crate) caller_ceiling: Option<crate::tools::caller_ceiling::CallerCeiling>,
}

/// A native compatibility recipe: builds the native registry for one eager
/// build. [`defaults::native_compat_registry`] is the runtime's own recipe; a
/// source can wrap it or own a closure that keeps per-instance state.
pub type NativeRegistryRecipe =
    dyn Fn(NativeRecipeInputs<'_>) -> anyhow::Result<AllToolsResult> + Send + Sync + 'static;

/// Supplies tool registries.
pub trait ToolSource: Send + Sync {
    /// Chooses how the registry for `request` is built. Called once per build.
    ///
    /// Return `Ok(None)` (the default) to make [`ToolSource::tools`] the whole
    /// registry, even when it is empty. Return `Ok(Some(recipe))` to have the
    /// runtime build the native registry with `recipe` and add this source's
    /// tools to it. An error here, or from the recipe, ends the build.
    ///
    /// This must not do I/O or use `request.memory` beyond comparing identity.
    /// Bounded delegation calls only this method, never the recipe or
    /// `tools()`, so a refusal that must also cover bounded delegation belongs
    /// here. The returned recipe may borrow from the source, not from `request`.
    fn native_recipe<'s>(
        &'s self,
        _request: &ToolRequest<'_>,
    ) -> anyhow::Result<Option<&'s NativeRegistryRecipe>> {
        Ok(None)
    }

    /// The tools this source supplies for `request`.
    ///
    /// Without a native recipe they are the whole registry; with one they are
    /// added to the native tools, and a source tool whose name a native tool
    /// already has is dropped. If two of these tools share a name, the first
    /// is kept. Every dropped tool is logged. Either way the agent's policy,
    /// the caller's allowlist and the memory/delivery context filters still
    /// apply.
    ///
    /// The tools are built before any principal routing of the session's
    /// memory, so a source memory tool built on `request.memory` keeps using
    /// that handle; routing applies only to the runtime's native memory tools.
    fn tools(&self, request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>>;
}

/// Supplies outbound delivery channels.
///
/// It covers replies routed by alias, scheduled delivery, approval prompts and
/// the CLI session channel (`cli`). The CLI keeps its existing stdin reader;
/// this source does not introduce an inbound turn transport.
pub trait ChannelSource: Send + Sync {
    /// Return the channel configured under `alias`, if it is running in this
    /// generation.
    fn channel(&self, alias: &str) -> Option<Arc<dyn Channel>>;
}

/// Recording sources for tests that construct entry points from supplied
/// capabilities.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use parking_lot::Mutex;

    /// What one provider request carried, owned so a test can inspect it.
    #[derive(Clone, Debug, PartialEq)]
    pub(crate) struct SeenProviderRequest {
        pub(crate) agent_alias: String,
        pub(crate) provider_ref: Option<String>,
        pub(crate) model: Option<String>,
        pub(crate) principal: Option<PrincipalId>,
    }

    /// Records every request and serves [`StubProvider`]. Session requests
    /// land in `seen`, model-switch requests in `switches`.
    #[derive(Default)]
    pub(crate) struct RecordingProviders {
        pub(crate) seen: Mutex<Vec<SeenProviderRequest>>,
        pub(crate) switches: Mutex<Vec<SeenProviderRequest>>,
        /// For each switch, the model the request's config configures on the
        /// switch target, which shows which config generation the switch saw.
        pub(crate) switch_target_models: Mutex<Vec<Option<String>>>,
    }

    impl SeenProviderRequest {
        fn from_request(request: &ProviderRequest<'_>) -> Self {
            Self {
                agent_alias: request.agent_alias.to_string(),
                provider_ref: request.provider_ref.map(str::to_string),
                model: request.model.map(str::to_string),
                principal: request.principal.cloned(),
            }
        }
    }

    impl ProviderSource for RecordingProviders {
        fn model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            self.seen
                .lock()
                .push(SeenProviderRequest::from_request(request));
            Ok(Arc::new(StubProvider))
        }

        fn switched_model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            self.switches
                .lock()
                .push(SeenProviderRequest::from_request(request));
            let target_model = request
                .provider_ref
                .and_then(|provider_ref| provider_ref.split_once('.'))
                .and_then(|(family, alias)| request.config.providers.models.find(family, alias))
                .and_then(|entry| entry.model.clone());
            self.switch_target_models.lock().push(target_model);
            Ok(Arc::new(StubProvider))
        }
    }

    pub(crate) const STUB_REPLY: &str = "stub provider reply";

    /// Answers every chat with [`STUB_REPLY`] and no tool calls.
    pub(crate) struct StubProvider;

    #[async_trait]
    impl ModelProvider for StubProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(STUB_REPLY.into())
        }

        async fn chat(
            &self,
            _request: zeroclaw_api::model_provider::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
            Ok(zeroclaw_api::model_provider::ChatResponse {
                text: Some(STUB_REPLY.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for StubProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "stub"
        }
    }

    /// Records which agents asked and serves a no-op store.
    #[derive(Default)]
    pub(crate) struct RecordingMemory {
        pub(crate) agents: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl MemorySource for RecordingMemory {
        async fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>> {
            self.agents.lock().push(request.agent_alias.to_string());
            Ok(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
        }
    }

    /// Serves [`StubProvider`] only to a request that names a principal, as
    /// a source that scopes credentials by principal would; an anonymous
    /// request is refused. Records every principal it served.
    #[derive(Default)]
    pub(crate) struct PrincipalRequiredProviders {
        pub(crate) served: Mutex<Vec<PrincipalId>>,
    }

    impl ProviderSource for PrincipalRequiredProviders {
        fn model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            let Some(principal) = request.principal else {
                anyhow::bail!("this source serves only a named principal");
            };
            self.served.lock().push(principal.clone());
            Ok(Arc::new(StubProvider))
        }
    }

    /// Refuses every provider request, naming the agent it refused.
    pub(crate) struct RefusingProviders;

    pub(crate) const REFUSAL: &str = "the supplied provider source refuses agent";

    impl ProviderSource for RefusingProviders {
        fn model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            anyhow::bail!("{REFUSAL} {}", request.agent_alias)
        }
    }

    pub(crate) struct NoTools;

    impl ToolSource for NoTools {
        fn tools(&self, _request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
            Ok(Vec::new())
        }
    }

    // The peripheral trap serves the Unix-only construction-trap tests and
    // the plugin late-name test.

    /// Description of every tool the peripheral trap factory builds.
    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    pub(crate) const PERIPHERAL_TRAP_TOOL: &str = "native peripheral trap tool";

    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    static PERIPHERAL_TRAP_LOADS: std::sync::LazyLock<
        Mutex<std::collections::HashMap<String, usize>>,
    > = std::sync::LazyLock::new(Default::default);

    /// Registers the process-wide peripheral factory once. It counts each load
    /// of a board named `native-trap-*` and, for a board named
    /// `native-trap-tool-<name>`, builds one tool called `<name>`. Other
    /// boards get nothing, so tests that do not use trap boards see no change.
    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    pub(crate) fn register_peripheral_trap() {
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| {
            crate::agent::loop_::register_peripheral_tools_fn(Box::new(|config| {
                let mut tools: Vec<Box<dyn Tool>> = Vec::new();
                for board in &config.boards {
                    if board.board.starts_with("native-trap-") {
                        *PERIPHERAL_TRAP_LOADS
                            .lock()
                            .entry(board.board.clone())
                            .or_default() += 1;
                    }
                    if let Some(name) = board.board.strip_prefix("native-trap-tool-") {
                        tools.push(Box::new(PeripheralTrapTool(name.to_string())));
                    }
                }
                Box::pin(async move { Ok(tools) })
            }));
        });
    }

    /// How many times the peripheral factory loaded `board`.
    #[cfg(unix)]
    pub(crate) fn peripheral_trap_loads(board: &str) -> usize {
        PERIPHERAL_TRAP_LOADS
            .lock()
            .get(board)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    struct PeripheralTrapTool(String);

    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    impl zeroclaw_api::attribution::Attributable for PeripheralTrapTool {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            &self.0
        }
    }

    #[cfg(any(unix, feature = "plugins-wasm-cranelift"))]
    #[async_trait]
    impl Tool for PeripheralTrapTool {
        fn name(&self) -> &str {
            &self.0
        }
        fn description(&self) -> &str {
            PERIPHERAL_TRAP_TOOL
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<zeroclaw_api::tool::ToolResult> {
            Ok(zeroclaw_api::tool::ToolResult {
                success: true,
                output: PERIPHERAL_TRAP_TOOL.into(),
                error: None,
            })
        }
    }

    pub(crate) struct NoChannels;

    impl ChannelSource for NoChannels {
        fn channel(&self, _alias: &str) -> Option<Arc<dyn Channel>> {
            None
        }
    }

    /// Recording providers and memory, the given tool source, no channels,
    /// and a no-op observer.
    pub(crate) fn recording_capabilities(
        providers: Arc<RecordingProviders>,
        memory: Arc<RecordingMemory>,
        tools: Arc<dyn ToolSource>,
    ) -> RuntimeCapabilities {
        RuntimeCapabilities {
            providers,
            memory,
            tools,
            channels: Arc::new(NoChannels),
            observer: Arc::new(crate::observability::NoopObserver),
        }
    }

    /// Capabilities around `providers`, with no-op memory, tools and channels.
    pub(crate) fn capabilities_with_providers(
        providers: Arc<dyn ProviderSource>,
    ) -> RuntimeCapabilities {
        RuntimeCapabilities {
            providers,
            memory: Arc::new(RecordingMemory::default()),
            tools: Arc::new(NoTools),
            channels: Arc::new(NoChannels),
            observer: Arc::new(crate::observability::NoopObserver),
        }
    }

    /// A live TCP endpoint that counts every connection it accepts and then
    /// drops it. Configured as an ambient provider route, it observes the
    /// effect itself: any request built from config instead of the supplied
    /// source must connect here first.
    pub(crate) struct CountingEndpoint {
        pub(crate) url: String,
        pub(crate) connections: Arc<std::sync::atomic::AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl CountingEndpoint {
        pub(crate) fn connections(&self) -> usize {
            self.connections.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for CountingEndpoint {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    pub(crate) async fn counting_endpoint() -> CountingEndpoint {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the counting endpoint");
        let address = listener.local_addr().expect("counting endpoint address");
        let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&connections);
        let task = zeroclaw_spawn::spawn!(async move {
            while let Ok((socket, _)) = listener.accept().await {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                drop(socket);
            }
        });
        CountingEndpoint {
            url: format!("http://{address}/v1"),
            connections,
            task,
        }
    }

    pub(crate) const VISION_REFUSAL: &str = "this source does not serve the vision route";
    pub(crate) const VISION_REPLY: &str = "vision stub reply";

    /// A tiny valid PNG as an image marker, for image-bearing turns.
    pub(crate) const IMAGE_TURN: &str = "describe this image [IMAGE:data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC]";

    /// Serves session starts with the text-only [`StubProvider`], records
    /// every vision-route request, and either refuses the route or serves it
    /// with [`VisionStubProvider`].
    #[derive(Default)]
    pub(crate) struct VisionRouteProviders {
        pub(crate) refuse_vision: bool,
        pub(crate) seen: Mutex<Vec<SeenProviderRequest>>,
        pub(crate) vision: Mutex<Vec<SeenProviderRequest>>,
    }

    impl VisionRouteProviders {
        pub(crate) fn refusing() -> Self {
            Self {
                refuse_vision: true,
                ..Self::default()
            }
        }
    }

    impl ProviderSource for VisionRouteProviders {
        fn model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            self.seen
                .lock()
                .push(SeenProviderRequest::from_request(request));
            Ok(Arc::new(StubProvider))
        }

        fn vision_model_provider(
            &self,
            request: &ProviderRequest<'_>,
        ) -> anyhow::Result<Arc<dyn ModelProvider>> {
            self.vision
                .lock()
                .push(SeenProviderRequest::from_request(request));
            if self.refuse_vision {
                anyhow::bail!(VISION_REFUSAL);
            }
            Ok(Arc::new(VisionStubProvider))
        }
    }

    /// A vision-capable provider that answers every request with
    /// [`VISION_REPLY`].
    pub(crate) struct VisionStubProvider;

    #[async_trait]
    impl ModelProvider for VisionStubProvider {
        fn supports_vision(&self) -> bool {
            true
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(VISION_REPLY.into())
        }

        async fn chat(
            &self,
            _request: zeroclaw_api::model_provider::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
            Ok(zeroclaw_api::model_provider::ChatResponse {
                text: Some(VISION_REPLY.into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for VisionStubProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }

        fn alias(&self) -> &str {
            "vision-stub"
        }
    }
}

#[cfg(test)]
mod binding_ratchet_tests {
    /// No production turn loop in the runtime drops the capability binding.
    ///
    /// Every `ToolLoop` the runtime builds outside tests passes an explicit
    /// binding expression, so a nested or forked loop cannot quietly resolve
    /// its own provider changes (the vision route for an image) from config
    /// inside a supplied-capability turn. A new production loop that truly has
    /// no binding must say so here, reviewed, rather than pass `None` by
    /// default.
    #[test]
    fn no_production_tool_loop_drops_the_capability_binding() {
        const ALLOWED: &[&str] = &[];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let test_start =
            regex::Regex::new(r"(?m)^#\[cfg\(test\)\]\s*\n\s*(pub(\(crate\))? )?mod \w+")
                .expect("test-module pattern compiles");
        let mut offenders = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("source directory is readable") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(&root)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                // Test-only modules (declared `#[cfg(test)]` by their parent).
                if relative.ends_with("/tests.rs")
                    || relative == "agent/safety_net.rs"
                    || relative == "agent/parity.rs"
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("source is readable");
                let production = test_start
                    .find(&source)
                    .map_or(source.as_str(), |test_module| {
                        &source[..test_module.start()]
                    });
                for (index, line) in production.lines().enumerate() {
                    let location = format!("{relative}:{}", index + 1);
                    if line.contains("capability_binding: None")
                        && !ALLOWED.contains(&location.as_str())
                    {
                        offenders.push(location);
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "production loops drop the capability binding: {offenders:?}"
        );
    }

    /// The tools that start child work through a capability slot. Their
    /// slot is empty when built and only an entry point's binding fills it,
    /// so an instance built anywhere but the registry factory or the
    /// factories a bounded hop calls to rebuild them for its target runs its
    /// child work on config-backed sources.
    const CHILD_STARTING_TOOLS: &str = "SpawnSubagentTool|SendMessageToPeerTool|CronRunTool";

    /// The production sites allowed to call one of their constructors, as
    /// (file under `src/`, enclosing function, type, number of calls, why).
    const ALLOWED_CHILD_STARTING_CONSTRUCTIONS: &[(&str, &str, &str, usize, &str)] = &[
        (
            "tools/mod.rs",
            "all_tools_with_runtime_on_thread",
            "CronRunTool",
            1,
            "the native registry, which collects the slot for prepare_registry",
        ),
        (
            "tools/mod.rs",
            "all_tools_with_runtime_on_thread",
            "SpawnSubagentTool",
            1,
            "the native registry, which collects the slot for prepare_registry",
        ),
        (
            "tools/mod.rs",
            "all_tools_with_runtime_on_thread",
            "SendMessageToPeerTool",
            1,
            "the native registry, which collects the slot for prepare_registry",
        ),
        (
            "tools/mod.rs",
            "cron_run_tool",
            "CronRunTool",
            1,
            "the factory a bounded hop calls to rebuild the tool for its target",
        ),
        (
            "tools/mod.rs",
            "spawn_subagent_tool",
            "SpawnSubagentTool",
            1,
            "the factory a bounded hop calls to rebuild the tool for its target",
        ),
        (
            "tools/mod.rs",
            "send_message_to_peer_tool",
            "SendMessageToPeerTool",
            1,
            "the factory a bounded hop calls to rebuild the tool for its target",
        ),
    ];

    /// One call of a child-starting tool's constructor in production code.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ConstructorHit {
        file: String,
        line: usize,
        function: String,
        type_name: String,
        text: String,
    }

    /// Matches `Type::new*`, also when rustfmt splits the path or the call
    /// across lines, and when the constructor is passed as a function.
    fn constructor_pattern() -> regex::Regex {
        regex::Regex::new(&format!(r"\b({CHILD_STARTING_TOOLS})\s*::\s*(new\w*)\b"))
            .expect("constructor pattern compiles")
    }

    /// `#[cfg(test)]` (or `#[cfg(all(.., test, ..))]`), any further outer
    /// attributes, then a module item. Group 1 is the module name, group 2 the
    /// `{` of an inline module or the `;` of an out-of-line one.
    fn test_module_pattern() -> regex::Regex {
        regex::Regex::new(
            r#"(?m)^[ \t]*#\[cfg\((?:test|all\([^)\]]*\btest\b[^)\]]*\))\)\][ \t]*\n(?:[ \t]*#\[[^\n]*\][ \t]*\n)*[ \t]*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*([{;])"#,
        )
        .expect("test-module pattern compiles")
    }

    /// The byte just past the `}` that closes the block opened at `open`,
    /// skipping comments, string and character literals.
    fn block_end(source: &str, open: usize) -> usize {
        let chars: Vec<(usize, char)> = source[open..]
            .char_indices()
            .map(|(offset, ch)| (open + offset, ch))
            .collect();
        let at = |index: usize| chars.get(index).map(|(_, ch)| *ch);
        let mut depth = 0usize;
        let mut index = 0;
        while index < chars.len() {
            let ch = chars[index].1;
            match ch {
                '/' if at(index + 1) == Some('/') => {
                    while index < chars.len() && chars[index].1 != '\n' {
                        index += 1;
                    }
                    continue;
                }
                '/' if at(index + 1) == Some('*') => {
                    let mut nested = 0usize;
                    while index < chars.len() {
                        if chars[index].1 == '/' && at(index + 1) == Some('*') {
                            nested += 1;
                            index += 2;
                        } else if chars[index].1 == '*' && at(index + 1) == Some('/') {
                            nested -= 1;
                            index += 2;
                            if nested == 0 {
                                break;
                            }
                        } else {
                            index += 1;
                        }
                    }
                    continue;
                }
                'r' if !index
                    .checked_sub(1)
                    .and_then(at)
                    .is_some_and(|before| before.is_alphanumeric() || before == '_')
                    || index
                        .checked_sub(1)
                        .and_then(at)
                        .is_some_and(|before| before == 'b') =>
                {
                    let mut hashes = 0usize;
                    while at(index + 1 + hashes) == Some('#') {
                        hashes += 1;
                    }
                    if at(index + 1 + hashes) != Some('"') {
                        index += 1;
                        continue;
                    }
                    index += 2 + hashes;
                    while index < chars.len() {
                        if chars[index].1 == '"' && (1..=hashes).all(|n| at(index + n) == Some('#'))
                        {
                            index += 1 + hashes;
                            break;
                        }
                        index += 1;
                    }
                    continue;
                }
                '"' => {
                    index += 1;
                    while index < chars.len() && chars[index].1 != '"' {
                        if chars[index].1 == '\\' {
                            index += 1;
                        }
                        index += 1;
                    }
                }
                '\'' => {
                    if at(index + 1) == Some('\\') {
                        // Past the quote, the backslash and the escaped
                        // character, which may itself be a quote.
                        index += 3;
                        while index < chars.len() && chars[index].1 != '\'' {
                            index += 1;
                        }
                    } else if at(index + 2) == Some('\'') {
                        index += 2;
                    }
                }
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return chars[index].0 + 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        source.len()
    }

    /// An out-of-line test module: its name and its `#[path]`, if any.
    type OutOfLineModule = (String, Option<String>);

    /// The byte ranges of `source` inside inline test modules, and its
    /// out-of-line test modules.
    fn test_parts(source: &str) -> (Vec<std::ops::Range<usize>>, Vec<OutOfLineModule>) {
        let path_attribute =
            regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).expect("path pattern compiles");
        let mut inline = Vec::new();
        let mut out_of_line = Vec::new();
        for module in test_module_pattern().captures_iter(source) {
            let whole = module.get(0).expect("whole match");
            let terminator = module.get(2).expect("terminator");
            if terminator.as_str() == "{" {
                inline.push(whole.start()..block_end(source, terminator.start()));
            } else {
                let path = path_attribute
                    .captures(whole.as_str())
                    .map(|path| path[1].to_string());
                out_of_line.push((module[1].to_string(), path));
            }
        }
        (inline, out_of_line)
    }

    /// Every `.rs` file under `src/` as (path relative to `src/`, source).
    fn runtime_sources() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("source directory is readable") {
                let path = entry.expect("directory entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                    let relative = path
                        .strip_prefix(&root)
                        .expect("under src")
                        .to_string_lossy()
                        .replace('\\', "/");
                    let source = std::fs::read_to_string(&path).expect("source is readable");
                    sources.push((relative, source));
                }
            }
        }
        sources.sort();
        sources
    }

    /// Files that only exist under a test configuration, found from the
    /// out-of-line test modules their parents declare. A directory module
    /// is returned as its directory with a trailing `/`.
    fn test_only_files(sources: &[(String, String)]) -> Vec<String> {
        let mut files = Vec::new();
        for (relative, source) in sources {
            let (directory, file) = relative.rsplit_once('/').unwrap_or(("", relative));
            let join = |name: &str| {
                if directory.is_empty() {
                    name.to_string()
                } else {
                    format!("{directory}/{name}")
                }
            };
            let module_directory = if matches!(file, "mod.rs" | "lib.rs" | "main.rs") {
                join("")
            } else {
                join(&format!("{}/", file.trim_end_matches(".rs")))
            };
            for (name, path) in test_parts(source).1 {
                match path {
                    Some(path) => files.push(join(&path)),
                    None => {
                        let base = format!("{}{name}", module_directory.trim_start_matches('/'));
                        files.push(format!("{base}.rs"));
                        files.push(format!("{base}/"));
                    }
                }
            }
        }
        files
    }

    /// The constructor calls in `source` outside its inline test modules.
    fn production_constructor_hits(file: &str, source: &str) -> Vec<ConstructorHit> {
        let (test_ranges, _) = test_parts(source);
        let function = regex::Regex::new(r"\bfn\s+(\w+)").expect("function pattern compiles");
        let mut hits = Vec::new();
        for hit in constructor_pattern().captures_iter(source) {
            let whole = hit.get(0).expect("whole match");
            if test_ranges
                .iter()
                .any(|range| range.contains(&whole.start()))
            {
                continue;
            }
            let line_start = source[..whole.start()].rfind('\n').map_or(0, |at| at + 1);
            if source[line_start..whole.start()]
                .trim_start()
                .starts_with("//")
            {
                continue;
            }
            hits.push(ConstructorHit {
                file: file.to_string(),
                line: source[..whole.start()].matches('\n').count() + 1,
                function: function
                    .captures_iter(&source[..whole.start()])
                    .last()
                    .map_or_else(String::new, |found| found[1].to_string()),
                type_name: hit[1].to_string(),
                text: whole.as_str().split_whitespace().collect(),
            });
        }
        hits
    }

    /// Every production construction of a child-starting tool in this crate.
    fn all_production_constructor_hits() -> Vec<ConstructorHit> {
        let sources = runtime_sources();
        let test_only = test_only_files(&sources);
        sources
            .iter()
            .filter(|(relative, _)| {
                !test_only.iter().any(|excluded| {
                    relative == excluded
                        || (excluded.ends_with('/') && relative.starts_with(excluded.as_str()))
                })
            })
            .flat_map(|(relative, source)| production_constructor_hits(relative, source))
            .collect()
    }

    /// No production code builds `spawn_subagent`, `send_message_to_peer` or
    /// `cron_run` outside the registry factory and the rebuild factories.
    ///
    /// A new site would hand out a tool whose capability slot nothing fills.
    /// A site that truly has to build one goes through a factory, or is added
    /// to the allowed list with its reason.
    #[test]
    fn child_starting_tools_are_built_only_by_the_registry_factories() {
        let hits = all_production_constructor_hits();
        let mut offenders = Vec::new();
        for &(file, function, type_name, count, _why) in ALLOWED_CHILD_STARTING_CONSTRUCTIONS {
            let found = hits
                .iter()
                .filter(|hit| {
                    hit.file == file && hit.function == function && hit.type_name == type_name
                })
                .count();
            if found != count {
                offenders.push(format!(
                    "{file} in fn {function}: {found} construction(s) of {type_name}, {count} allowed"
                ));
            }
        }
        for hit in &hits {
            let allowed = ALLOWED_CHILD_STARTING_CONSTRUCTIONS.iter().any(
                |&(file, function, type_name, _, _)| {
                    hit.file == file && hit.function == function && hit.type_name == type_name
                },
            );
            if !allowed {
                offenders.push(format!(
                    "{}:{} in fn {}: {}",
                    hit.file, hit.line, hit.function, hit.text
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "child-starting tools built outside the registry factories: {offenders:?}"
        );
    }

    /// The scan finds what it is meant to find, so an empty result above
    /// means there is nothing, not that the pattern missed it.
    #[test]
    fn child_starting_constructor_scan_finds_known_sites() {
        // A synthetic file: a test module BEFORE production code (a scan
        // that cut at the first test module would miss what follows), a
        // brace inside a string and a character literal in that module,
        // calls split across lines, a comment, and a test-only call.
        let synthetic = concat!(
            "#[cfg(test)]\n",
            "mod early {\n",
            "    const OPEN: &str = \"{\";\n",
            "    const CLOSE: char = '}';\n",
            "    fn t() { let _ = CronRunTool::new(a, b, c); }\n",
            "}\n",
            "\n",
            "fn build_split() {\n",
            "    let tool = crate::tools::SpawnSubagentTool\n",
            "        ::new(\n",
            "            config,\n",
            "        );\n",
            "    let peer = SendMessageToPeerTool::new_with_live_config_and_capability(\n",
            "        config, alias, None, None,\n",
            "    );\n",
            "    // CronRunTool::new_with_runtime(..) in a comment is not a call.\n",
            "}\n",
            "\n",
            "#[cfg(all(test, unix))]\n",
            "mod late {\n",
            "    fn t() { let _ = CronRunTool::new_with_runtime(a, b, c, d, None); }\n",
            "}\n",
            "\n",
            "fn build_after_tests() {\n",
            "    let _ = CronRunTool::new_with_runtime_and_capability(a, b, c, d, e, f);\n",
            "}\n",
        );
        let found: Vec<(usize, String, String)> =
            production_constructor_hits("synthetic.rs", synthetic)
                .into_iter()
                .map(|hit| (hit.line, hit.function, hit.text))
                .collect();
        assert_eq!(
            found,
            vec![
                (
                    9,
                    "build_split".to_string(),
                    "SpawnSubagentTool::new".to_string()
                ),
                (
                    13,
                    "build_split".to_string(),
                    "SendMessageToPeerTool::new_with_live_config_and_capability".to_string()
                ),
                (
                    25,
                    "build_after_tests".to_string(),
                    "CronRunTool::new_with_runtime_and_capability".to_string()
                ),
            ]
        );

        // The real tree: the six allowed sites in `tools/mod.rs` are found
        // with their enclosing functions, and nothing else there.
        let sources = runtime_sources();
        let (_, tools_mod) = sources
            .iter()
            .find(|(relative, _)| relative == "tools/mod.rs")
            .expect("tools/mod.rs is scanned");
        let mut in_tools_mod: Vec<(String, String)> =
            production_constructor_hits("tools/mod.rs", tools_mod)
                .into_iter()
                .map(|hit| (hit.function, hit.type_name))
                .collect();
        in_tools_mod.sort();
        let mut expected: Vec<(String, String)> = ALLOWED_CHILD_STARTING_CONSTRUCTIONS
            .iter()
            .map(|&(_, function, type_name, _, _)| (function.to_string(), type_name.to_string()))
            .collect();
        expected.sort();
        assert_eq!(in_tools_mod, expected);

        // Test modules are excluded: `spawn_subagent.rs` constructs the tool
        // only in its tests, and those calls are matched before exclusion.
        let (_, spawn_subagent) = sources
            .iter()
            .find(|(relative, _)| relative == "tools/spawn_subagent.rs")
            .expect("tools/spawn_subagent.rs is scanned");
        assert!(constructor_pattern().find_iter(spawn_subagent).count() > 0);
        assert!(production_constructor_hits("tools/spawn_subagent.rs", spawn_subagent).is_empty());

        // Test-only files declared out of line are recognized, through a
        // plain module, `#[path]`, and `cfg(all(test, ..))`.
        let test_only = test_only_files(&sources);
        for known in [
            "agent/tests.rs",
            "agent/safety_net.rs",
            "agent/parity.rs",
            "composition/supplied_only_traps.rs",
        ] {
            assert!(
                test_only.iter().any(|file| file == known),
                "{known} is not recognized as test-only: {test_only:?}"
            );
        }
    }
}
