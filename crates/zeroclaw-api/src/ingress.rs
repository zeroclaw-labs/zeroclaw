//! Universal ingress policy contract types (RFC phase 1).

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOrigin {
    /// An interactive operator session (CLI chat loop or one-shot run).
    Interactive,
    /// A turn dispatched by the channel orchestrator for a channel peer.
    Channel,
    /// A scheduled cron job turn.
    Cron,
    /// A daemon-initiated turn (heartbeat task pipeline).
    Daemon,
    /// A direct embedded `Agent::turn` call (library/API consumer).
    AgentDirect,
    /// A nested sub-turn inside a parent turn (delegate subagent, safety
    /// net, skills review). Fail-closed default: sub-turns never receive
    /// origin-gated behavior such as context injection, so an unstamped
    /// or legacy envelope behaves like a sub-turn.
    #[default]
    SubTurn,
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
/// engine. See the module docs.
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
    /// Who initiated the turn (see [`TurnOrigin`]). Serde-defaults to
    /// [`TurnOrigin::SubTurn`] so envelopes serialized before this field
    /// existed deserialize fail-closed (no origin-gated behavior).
    #[serde(default)]
    pub origin: TurnOrigin,
}

impl IngressContext {
    fn phase1(origin: TurnOrigin) -> Self {
        Self {
            message_id: None,
            source_class: SourceClass::Internal,
            sender: None,
            transport: Transport::Internal,
            trust: TrustClass::Trusted,
            origin,
        }
    }

    /// Envelope for an interactive operator turn (CLI chat loop, one-shot run).
    #[must_use]
    pub fn interactive() -> Self {
        Self::phase1(TurnOrigin::Interactive)
    }

    /// Envelope for a turn the channel orchestrator dispatches for a channel
    /// peer. Keeps the placeholder source/transport/trust; the real
    /// channel transport identity is not stamped at the edge yet.
    #[must_use]
    pub fn channel() -> Self {
        Self::phase1(TurnOrigin::Channel)
    }

    /// Envelope for a scheduled cron job turn.
    #[must_use]
    pub fn cron() -> Self {
        Self::phase1(TurnOrigin::Cron)
    }

    /// Envelope for a daemon-initiated turn (heartbeat task pipeline).
    #[must_use]
    pub fn daemon() -> Self {
        Self::phase1(TurnOrigin::Daemon)
    }

    /// Envelope for a direct embedded `Agent::turn` call.
    #[must_use]
    pub fn agent_direct() -> Self {
        Self::phase1(TurnOrigin::AgentDirect)
    }

    /// Envelope for a nested sub-turn inside a parent turn (delegate
    /// subagent, safety net, skills review). Sub-turns never receive
    /// origin-gated behavior such as context injection.
    #[must_use]
    pub fn sub_turn() -> Self {
        Self::phase1(TurnOrigin::SubTurn)
    }

    /// Envelope for a turn whose origin is threaded in from the entry
    /// point (e.g. `agent::run` / `process_message`, whose one body serves
    /// several distinct entries: CLI, cron, daemon, subagent spawn).
    /// Equivalent to the per-origin constructors above.
    #[must_use]
    pub fn from_origin(origin: TurnOrigin) -> Self {
        Self::phase1(origin)
    }
}

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
    fn sub_turn_envelope_is_internal_and_trusted() {
        let ctx = IngressContext::sub_turn();
        assert_eq!(ctx.source_class, SourceClass::Internal);
        assert_eq!(ctx.trust, TrustClass::Trusted);
        assert_eq!(ctx.transport, Transport::Internal);
        assert!(ctx.sender.is_none());
        assert!(ctx.message_id.is_none());
        assert_eq!(ctx.origin, TurnOrigin::SubTurn);
    }

    #[test]
    fn from_origin_matches_the_named_constructor() {
        assert_eq!(
            IngressContext::from_origin(TurnOrigin::Cron),
            IngressContext::cron()
        );
        assert_eq!(
            IngressContext::from_origin(TurnOrigin::SubTurn),
            IngressContext::sub_turn()
        );
    }

    #[test]
    fn per_origin_constructors_vary_only_the_origin() {
        let cases = [
            (IngressContext::interactive(), TurnOrigin::Interactive),
            (IngressContext::channel(), TurnOrigin::Channel),
            (IngressContext::cron(), TurnOrigin::Cron),
            (IngressContext::daemon(), TurnOrigin::Daemon),
            (IngressContext::agent_direct(), TurnOrigin::AgentDirect),
            (IngressContext::sub_turn(), TurnOrigin::SubTurn),
        ];
        for (ctx, origin) in cases {
            assert_eq!(ctx.origin, origin);
            assert_eq!(ctx.source_class, SourceClass::Internal);
            assert_eq!(ctx.trust, TrustClass::Trusted);
            assert_eq!(ctx.transport, Transport::Internal);
            assert!(ctx.sender.is_none());
            assert!(ctx.message_id.is_none());
        }
    }

    #[test]
    fn legacy_envelope_without_origin_deserializes_fail_closed() {
        // An envelope serialized before the origin field existed must come
        // back as SubTurn (no origin-gated behavior), not error.
        let legacy = serde_json::json!({
            "message_id": null,
            "source_class": "internal",
            "sender": null,
            "transport": "internal",
            "trust": "trusted",
        });
        let ctx: IngressContext = serde_json::from_value(legacy).unwrap();
        assert_eq!(ctx.origin, TurnOrigin::SubTurn);
    }

    #[test]
    fn origin_serializes_snake_case() {
        let ctx = IngressContext::agent_direct();
        let v = serde_json::to_value(&ctx).unwrap();
        assert_eq!(v["origin"], "agent_direct");
        let back: IngressContext = serde_json::from_value(v).unwrap();
        assert_eq!(back, ctx);
    }
}
