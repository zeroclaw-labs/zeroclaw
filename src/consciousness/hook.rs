//! HookHandler glue that runs the consciousness orchestrator once per inbound
//! message when `config.consciousness.enabled` is true.
//!
//! Mirrors the conscience-gate wiring (`src/conscience/hook.rs`): the runtime
//! crate cannot depend on this binary-local module, so `main.rs` registers a
//! process-global factory and the runtime builds one hook per Agent.
//!
//!     main.rs → consciousness::register_hook_factory()
//!         → zeroclaw_runtime::hooks::registry::register_factory(...)
//!     Agent::run → HookRunner.register(ConsciousnessHook { orchestrator })
//!     on_message_received → orchestrator.tick()  (observe-only)
//!
//! The hook is observe-only: it ticks the perceive→debate→decide→act→reflect
//! cycle and records the resulting coherence/proposal counts, but never
//! cancels or rewrites the inbound message.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_config::schema::Config;
use zeroclaw_config::x0_extensions::ConsciousnessConfig as ConsciousnessSettings;
use zeroclaw_runtime::hooks::{HookHandler, HookResult};

use crate::consciousness::agents::build_all_agents;
use crate::consciousness::{ConsciousnessConfig as OrchConfig, ConsciousnessOrchestrator};
use crate::continuity::{ContinuityGuard, DriftLimits};
use crate::cosmic::{
    AgentPool, AgentRole, CausalGraph, ConsolidationEngine, Constitution, CosmicMemoryGraph,
    CounterfactualEngine, DriftDetector, EmotionalModulator, FreeEnergyState, GlobalWorkspace,
    IntegrationMeter, NormativeEngine, PolicyEngine, SelfModel, SubsystemId, WorldModel,
};

/// Per-Agent hook that ticks the consciousness orchestrator once per inbound
/// message. The orchestrator lives behind an async mutex because `tick()`
/// takes `&mut self`; it persists across messages so coherence accumulates
/// over the lifetime of the Agent.
pub struct ConsciousnessHook {
    orchestrator: tokio::sync::Mutex<ConsciousnessOrchestrator>,
    /// The most recent turn's rendered deliberation, injected as a prompt
    /// prefix by `before_prompt_build`. `None` until the first tick produces
    /// a non-empty narrative. Stored separately from the tick so the loop's
    /// per-iteration prompt builds reuse one deliberation per turn rather
    /// than re-ticking the orchestrator on every tool round.
    last_deliberation: tokio::sync::Mutex<Option<String>>,
}

impl ConsciousnessHook {
    pub fn new(settings: &ConsciousnessSettings) -> Self {
        Self {
            orchestrator: tokio::sync::Mutex::new(build_orchestrator(settings)),
            last_deliberation: tokio::sync::Mutex::new(None),
        }
    }
}

/// Build a ready-to-tick orchestrator: map the operator-facing settings onto
/// the orchestrator's config, wire the cosmic subsystems, and register the
/// core agent panel. Subsystem capacities mirror the integration-test setup.
fn build_orchestrator(settings: &ConsciousnessSettings) -> ConsciousnessOrchestrator {
    let oc = OrchConfig {
        bus_capacity: settings.bus_capacity,
        debate_rounds: settings.debate_rounds,
        approval_threshold: settings.approval_threshold,
        ..OrchConfig::default()
    };
    let mut orchestrator = ConsciousnessOrchestrator::new(oc);

    let mut workspace = GlobalWorkspace::new(0.2, 5, 100);
    workspace.register_subsystem(SubsystemId::Memory, 0.9);
    workspace.register_subsystem(SubsystemId::FreeEnergy, 0.8);
    workspace.register_subsystem(SubsystemId::Causality, 0.7);
    workspace.register_subsystem(SubsystemId::SelfModel, 0.6);
    workspace.register_subsystem(SubsystemId::WorldModel, 0.5);

    let mut agent_pool = AgentPool::new(8, 100);
    agent_pool.register_agent("primary", AgentRole::Primary);

    // Order matches `build_all_agents`: workspace, agent_pool, continuity,
    // graph, consolidation, world_model, counterfactual, policy, free_energy,
    // causal, modulator, normative, constitution, self_model, drift, integration.
    let agents = build_all_agents(
        Arc::new(Mutex::new(workspace)),
        Arc::new(Mutex::new(agent_pool)),
        Arc::new(Mutex::new(ContinuityGuard::new(DriftLimits::default()))),
        Arc::new(Mutex::new(CosmicMemoryGraph::new(1000))),
        Arc::new(Mutex::new(ConsolidationEngine::new(0.8))),
        Arc::new(Mutex::new(WorldModel::new(100))),
        Arc::new(Mutex::new(CounterfactualEngine::new(10, 10))),
        Arc::new(Mutex::new(PolicyEngine::new(10))),
        Arc::new(Mutex::new(FreeEnergyState::new(100))),
        Arc::new(Mutex::new(CausalGraph::new(100))),
        Arc::new(Mutex::new(EmotionalModulator::new())),
        Arc::new(Mutex::new(NormativeEngine::new(100, 100))),
        Arc::new(Mutex::new(Constitution::new())),
        Arc::new(Mutex::new(SelfModel::new(100))),
        Arc::new(Mutex::new(DriftDetector::new(50, 0.1))),
        Arc::new(Mutex::new(IntegrationMeter::new())),
    );
    for agent in agents {
        orchestrator.register_agent(agent);
    }
    orchestrator
}

#[async_trait]
impl HookHandler for ConsciousnessHook {
    fn name(&self) -> &str {
        "consciousness"
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        let result = self.orchestrator.lock().await.tick();
        tracing::debug!(
            target: "consciousness",
            coherence = result.coherence,
            proposals = result.proposals_generated,
            approved = result.proposals_approved,
            vetoed = result.proposals_vetoed,
            debate_rounds = result.debate_rounds_used,
            "consciousness tick"
        );

        // Render the council's current thought into a compact one-line block.
        // Empty narratives produce no injection (e.g. a cold first tick).
        let intention = result.narrative.current_intention.trim();
        let synthesis = result.narrative.synthesis.trim();
        let deliberation = if intention.is_empty() && synthesis.is_empty() {
            None
        } else {
            let body: Vec<&str> = [intention, synthesis]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            Some(format!(
                "[Internal deliberation (coherence {:.2}): {}]",
                result.coherence,
                body.join(" ")
            ))
        };
        *self.last_deliberation.lock().await = deliberation;

        HookResult::Continue(message)
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        // Inject this turn's deliberation as a prompt prefix so the council's
        // reasoning actually shapes the model's response. Observe-only when no
        // deliberation is available (the prompt passes through unchanged).
        match self.last_deliberation.lock().await.as_deref() {
            Some(deliberation) => HookResult::Continue(format!("{deliberation}\n\n{prompt}")),
            None => HookResult::Continue(prompt),
        }
    }
}

/// Install the consciousness hook factory. Every Agent built after this
/// (when `config.consciousness.enabled` is true) gets a ticking orchestrator.
/// Idempotent at the registry layer; call once at startup.
pub fn register_hook_factory() {
    zeroclaw_runtime::hooks::registry::register_factory(Box::new(|cfg: &Config| {
        if cfg.consciousness.enabled {
            vec![Box::new(ConsciousnessHook::new(&cfg.consciousness))]
        } else {
            Vec::new()
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hook_ticks_and_passes_message_through() {
        let settings = ConsciousnessSettings {
            enabled: true,
            ..Default::default()
        };
        let hook = ConsciousnessHook::new(&settings);

        let msg = ChannelMessage {
            content: "hello".into(),
            ..Default::default()
        };
        // Observe-only: the orchestrator ticks but the message is returned
        // unchanged via HookResult::Continue.
        match hook.on_message_received(msg).await {
            HookResult::Continue(out) => assert_eq!(out.content, "hello"),
            HookResult::Cancel(reason) => panic!("expected Continue, got Cancel: {reason}"),
        }

        // The orchestrator persists across messages, so a second tick must
        // also succeed (coherence accumulates rather than re-initialising).
        match hook.on_message_received(ChannelMessage::default()).await {
            HookResult::Continue(_) => {}
            HookResult::Cancel(reason) => panic!("second tick cancelled: {reason}"),
        }

        // before_prompt_build returns the prompt, optionally prefixed with the
        // deliberation block. The original prompt body is always preserved.
        match hook.before_prompt_build("USER PROMPT".to_string()).await {
            HookResult::Continue(p) => assert!(
                p.contains("USER PROMPT"),
                "prompt body must be preserved, got: {p}"
            ),
            HookResult::Cancel(reason) => panic!("prompt build cancelled: {reason}"),
        }
    }
}
