//! Agent Card provider — abstracts the discovery-card source.
//!
//! The host supplies an `AgentCard` ([`StaticAgentCard`]) or implements a custom
//! producer (e.g. a ZeroClaw gateway generating it from config/identity/skills).
//! Mirrors the Google `a2a-rs` `AgentCardProducer`: the protocol crate is

use std::sync::Arc;

use crate::a2a::a2a_wire::AgentCard;

/// Agent Card provider.
pub trait AgentCardProducer: Send + Sync {
    /// Returns the Agent Card this server publishes.
    fn card(&self) -> AgentCard;
}

/// Static Agent Card — the host provides one fixed card.
pub struct StaticAgentCard {
    card: AgentCard,
}

impl StaticAgentCard {
    #[must_use]
    pub fn new(card: AgentCard) -> Self {
        Self { card }
    }
}

impl AgentCardProducer for StaticAgentCard {
    fn card(&self) -> AgentCard {
        self.card.clone()
    }
}

/// Convenience: build an `Arc<dyn AgentCardProducer>` from an `AgentCard`.
#[must_use]
pub fn static_card_producer(card: AgentCard) -> Arc<dyn AgentCardProducer> {
    Arc::new(StaticAgentCard::new(card))
}
