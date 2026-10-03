//! Runtime composition contract: the capabilities an embedder supplies.
//!
//! **Skeleton only.** Nothing in the runtime consumes these types yet, and no
//! construction has moved. `docs/book/src/architecture/runtime-composition.md`
//! sets out the ownership and lifetime rules and the order in which entry
//! points move onto this contract.
//!
//! The runtime asks for capabilities through *sources* rather than receiving
//! finished instances, because it resolves them per agent, per configured
//! provider reference, and per config generation. A source decides how to
//! build or cache what it returns; the runtime decides when to ask, which
//! agent it is asking for, and which resolved security policy applies.

use std::sync::Arc;

use zeroclaw_api::channel::Channel;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_api::model_provider::ModelProvider;
use zeroclaw_api::observability_traits::Observer;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_api::tool::Tool;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::Config;

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

/// What the runtime is asking a [`ProviderSource`] for.
pub struct ProviderRequest<'a> {
    /// The config generation the request belongs to.
    pub config: &'a Config,
    /// The agent the provider serves.
    pub agent_alias: &'a str,
    /// `None` resolves the agent's configured provider. `Some` names an
    /// explicit provider reference, as a model switch or a delegate target does.
    pub provider_ref: Option<&'a str>,
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
}

/// What the runtime is asking a [`MemorySource`] for.
pub struct MemoryRequest<'a> {
    /// The config generation the request belongs to.
    pub config: &'a Config,
    /// The agent whose memory is requested.
    pub agent_alias: &'a str,
}

/// Supplies memory backends.
pub trait MemorySource: Send + Sync {
    /// Return the memory backend for `request`. Two requests for the same
    /// agent in one generation must reach the same underlying store.
    fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>>;
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
