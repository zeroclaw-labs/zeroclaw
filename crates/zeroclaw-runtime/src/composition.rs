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

mod config_backed;

/// The config-backed sources the compatibility adapters use, for an
/// application layer that composes the same recipe.
pub mod defaults {
    pub use super::config_backed::{
        ConfigMemory, ConfigProviders, NoOutboundChannels, NoSuppliedTools,
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

    /// Bind a registry the runtime built to these capabilities, before the
    /// registry reaches `ScopedToolRegistry::assemble`.
    ///
    /// The registry's delegate tool, if any, resolves delegated targets
    /// through these capabilities from now on, so a delegated sub-agent does
    /// not step outside the entry point's providers.
    ///
    /// The tool source's tools join `tools` only, so the agent's
    /// `allowed_tools` and `excluded_tools` filter and the caller's selector
    /// apply to them exactly as to runtime tools. They stay out of
    /// `unfiltered_tool_arcs`, which skill elevation resolves against, so a
    /// skill cannot raise a source tool past that filter. A source tool whose
    /// name the runtime already registered is dropped: the runtime's own tool
    /// keeps the name.
    pub(crate) fn bind_registry(
        &self,
        built: &mut crate::tools::AllToolsResult,
        request: &ToolRequest<'_>,
    ) -> anyhow::Result<()> {
        if let Some(slot) = built.delegate_capabilities.as_ref() {
            let _ = slot.set(self.clone());
        }
        let supplied = self.tools.tools(request)?;
        for tool in supplied {
            if built
                .tools
                .iter()
                .any(|existing| existing.name() == tool.name())
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "agent": request.agent_alias,
                            "tool": tool.name(),
                        })),
                    "tool source supplied a tool whose name the runtime already registered; keeping the runtime tool"
                );
                continue;
            }
            built.tools.push(tool);
        }
        Ok(())
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
/// request is made. A source builds tools against these values and has no way
/// to substitute its own.
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
    /// The agent's memory, as returned by the generation's [`MemorySource`].
    pub memory: &'a Arc<dyn Memory>,
}

/// Supplies tool registries.
pub trait ToolSource: Send + Sync {
    /// Return the tools for `request`. The runtime may add its own core tools
    /// and remove any tool the resolved policy excludes; it never adds a tool
    /// the policy forbids because a source returned it.
    fn tools(&self, request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>>;
}

/// Supplies outbound delivery channels.
///
/// Inbound turns do not arrive through this trait. It covers only the channels
/// the runtime sends through: replies routed by alias, scheduled delivery, and
/// approval prompts.
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

    pub(crate) struct NoTools;

    impl ToolSource for NoTools {
        fn tools(&self, _request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>> {
            Ok(Vec::new())
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
}
