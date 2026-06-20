//! Universal ingress policy contract types (RFC #6971, phase 1).
//!
//! Layer: api (kernel ABI). Pure types, no logic, no IO. These describe the
//! envelope that every inbound turn carries into the unified turn engine and
//! the disposition the SOP policy layer returns for it.
//!
//! The entry layer (channel orchestrator, gateway, ACP, RPC, cron/SOP/subagent)
//! stamps an [`IngressContext`] and threads it into `run_tool_call_loop`; the
//! engine consults the policy front door (`ingress_policy`, in
//! `zeroclaw-runtime`) which returns an [`IngressDecision`]. The default
//! disposition is [`IngressDecision::Loop`] — run the agent, today's behavior.
//!
//! This module does NOT decide policy (that is the runtime front door) and does
//! NOT stamp real identity (phase 2). Phase 1 ships these shapes and the
//! always-on threading; only `Loop` is reachable under the default policy.

use serde::{Deserialize, Serialize};

/// Whether an inbound turn originates outside the agent (a transport peer) or
/// from an internal driver (cron, an SOP step, a subagent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceClass {
    /// A message from a transport peer (channel user, webhook caller, …).
    External,
    /// An internally driven turn (cron, SOP step, subagent).
    Internal,
}

/// The transport an inbound turn arrived on. Real per-transport stamping is
/// phase 2; phase 1 stamps [`Transport::Internal`] everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// A messaging channel — `kind` is the channel type (e.g. `"github"`),
    /// `alias` the configured channel alias.
    Channel { kind: String, alias: String },
    /// The HTTP/WebSocket gateway (REST/WS turn).
    Gateway,
    /// Agent Client Protocol (local IDE bridge).
    Acp,
    /// RPC socket turn (zerocode path).
    Rpc,
    /// An internally driven turn with no external transport.
    Internal,
}

/// Trust class resolved for the turn's sender. Minimal for phase 1; peer-group
/// resolution (the real source) is phase 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustClass {
    /// Sender is in a trusted peer group (or the turn is internally driven).
    Trusted,
    /// Sender is untrusted — external text to be treated as data, not
    /// instructions, when the policy says so.
    Untrusted,
}

/// Untrusted-data framing instructions for an [`IngressDecision::Annotate`]
/// disposition. Minimal placeholder for phase 1; the framing fields are
/// fleshed out when `Annotate` becomes reachable (phase 3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UntrustedFraming {}

/// The envelope stamped by the entry layer; travels with the turn into the
/// engine. See the module docs and RFC #6971 §3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressContext {
    /// Stable inbound id (e.g. a `ChannelMessage.id`) — provenance + audit
    /// handle. `None` for id-less internal turns.
    pub message_id: Option<String>,
    /// Whether the turn is external or internally driven.
    pub source_class: SourceClass,
    /// Platform user id / principal of the sender, if any.
    pub sender: Option<String>,
    /// The transport the turn arrived on.
    pub transport: Transport,
    /// The resolved trust class of the sender.
    pub trust: TrustClass,
}

impl IngressContext {
    /// The envelope for an internally driven, trusted turn (cron, SOP step,
    /// subagent, or any phase-1 caller that has not yet stamped real identity).
    ///
    /// `source_class = Internal`, `trust = Trusted`, `transport = Internal`,
    /// `sender = None`, `message_id = None`. Passing the layer with this
    /// envelope dispositions to [`IngressDecision::Loop`] under the default
    /// policy, so behavior is identical to a turn that never had an envelope.
    #[must_use]
    pub fn internal() -> Self {
        Self {
            message_id: None,
            source_class: SourceClass::Internal,
            sender: None,
            transport: Transport::Internal,
            trust: TrustClass::Trusted,
        }
    }
}

/// The SOP policy layer's disposition for one inbound turn (RFC #6971 §3/§5).
///
/// `Loop` is the default and the only disposition reachable under the default
/// policy in phase 1. The other arms exist so the engine match is exhaustive
/// and future-ready; they become reachable when phase 3 wires non-`Loop`
/// policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IngressDecision {
    /// DEFAULT — run the agent. Free: allocates no SOP run, does no IO.
    Loop,
    /// Wrap the message as untrusted data with the given framing, then loop.
    Annotate { framing: UntrustedFraming },
    /// Hand the turn to a managed SOP run (HITL).
    Gate { sop: String },
    /// Refuse the turn; audit-logged.
    Drop { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_envelope_is_internal_and_trusted() {
        let ctx = IngressContext::internal();
        assert_eq!(ctx.source_class, SourceClass::Internal);
        assert_eq!(ctx.trust, TrustClass::Trusted);
        assert_eq!(ctx.transport, Transport::Internal);
        assert!(ctx.sender.is_none());
        assert!(ctx.message_id.is_none());
    }
}
