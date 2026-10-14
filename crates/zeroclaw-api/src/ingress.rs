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

/// The runtime-owned principal that initiated an internally driven turn —
/// who caused the turn to exist, distinct from the executing agent identity
/// the turn runs under. Stamped once at dispatch from the runtime's own
/// resolved state (job config, canonical agent alias, runtime task id),
/// never from message content or tool arguments, and immutable for the
/// turn's lifetime. Persisted records carry it verbatim as at-time-of-action
/// fact. Internal principals never appear in `peer_groups` and are never
/// consulted by peer-membership checks — identity and delivery permission
/// are separate axes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalPrincipal {
    /// A scheduled cron job. `job_id` is the stable store id; `job_name`
    /// the operator-facing name, if the job has one.
    Cron {
        job_id: String,
        job_name: Option<String>,
    },
    /// A peer-agent dispatch. `sender_alias` is the sending agent's
    /// canonical alias — the initiator only; the recipient turn executes
    /// under the recipient's own alias.
    PeerAgent { sender_alias: String },
    /// A daemon-driven task (heartbeat pipeline, SOP step driver). `task`
    /// is a runtime-owned identifier, never model-authored text.
    Daemon { task: String },
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
    /// The internal principal that initiated the turn, when the dispatching
    /// surface stamps one (see [`InternalPrincipal`]). `None` for external
    /// turns, for internal paths that have no principal contract yet, and
    /// for envelopes serialized before this field existed (serde-defaults
    /// fail-closed to absent).
    #[serde(default)]
    pub internal_principal: Option<InternalPrincipal>,
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
            internal_principal: None,
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

    /// Envelope for a turn whose origin and (optional) internal principal
    /// are both threaded in from the entry point. With `None` this is
    /// exactly [`IngressContext::from_origin`].
    #[must_use]
    pub fn from_parts(origin: TurnOrigin, internal_principal: Option<InternalPrincipal>) -> Self {
        Self {
            internal_principal,
            ..Self::phase1(origin)
        }
    }

    /// Envelope for a scheduled cron job turn stamped with its initiating
    /// job. The principal is resolved from the job's stored config at
    /// dispatch and is immutable for the turn's lifetime.
    #[must_use]
    pub fn cron_job(job_id: impl Into<String>, job_name: Option<String>) -> Self {
        Self::from_parts(
            TurnOrigin::Cron,
            Some(InternalPrincipal::Cron {
                job_id: job_id.into(),
                job_name,
            }),
        )
    }

    /// Envelope for a peer-agent dispatch stamped with the sending agent's
    /// canonical alias — the initiator; the recipient turn still executes
    /// under the recipient's own alias.
    #[must_use]
    pub fn peer_agent(sender_alias: impl Into<String>) -> Self {
        Self::from_parts(
            TurnOrigin::AgentDirect,
            Some(InternalPrincipal::PeerAgent {
                sender_alias: sender_alias.into(),
            }),
        )
    }

    /// Envelope for a daemon-driven turn stamped with a runtime-owned task
    /// identifier (never model-authored text).
    #[must_use]
    pub fn daemon_task(task: impl Into<String>) -> Self {
        Self::from_parts(
            TurnOrigin::Daemon,
            Some(InternalPrincipal::Daemon { task: task.into() }),
        )
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
            assert!(ctx.internal_principal.is_none());
        }
    }

    #[test]
    fn principal_constructors_stamp_origin_and_principal() {
        let cron = IngressContext::cron_job("job-1", Some("nightly".to_string()));
        assert_eq!(cron.origin, TurnOrigin::Cron);
        assert_eq!(
            cron.internal_principal,
            Some(InternalPrincipal::Cron {
                job_id: "job-1".to_string(),
                job_name: Some("nightly".to_string()),
            })
        );
        // Everything but origin and principal matches the plain envelope.
        assert_eq!(
            IngressContext {
                internal_principal: None,
                ..cron
            },
            IngressContext::cron()
        );

        let peer = IngressContext::peer_agent("front");
        assert_eq!(peer.origin, TurnOrigin::AgentDirect);
        assert_eq!(
            peer.internal_principal,
            Some(InternalPrincipal::PeerAgent {
                sender_alias: "front".to_string(),
            })
        );

        let daemon = IngressContext::daemon_task("heartbeat:decision");
        assert_eq!(daemon.origin, TurnOrigin::Daemon);
        assert_eq!(
            daemon.internal_principal,
            Some(InternalPrincipal::Daemon {
                task: "heartbeat:decision".to_string(),
            })
        );
    }

    #[test]
    fn from_parts_without_principal_matches_from_origin() {
        for origin in [
            TurnOrigin::Interactive,
            TurnOrigin::Channel,
            TurnOrigin::Cron,
            TurnOrigin::Daemon,
            TurnOrigin::AgentDirect,
            TurnOrigin::SubTurn,
        ] {
            assert_eq!(
                IngressContext::from_parts(origin, None),
                IngressContext::from_origin(origin)
            );
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

    #[test]
    fn legacy_envelope_without_principal_deserializes_absent() {
        // An envelope serialized before the internal_principal field existed
        // must come back with no principal, not error.
        let legacy = serde_json::json!({
            "message_id": null,
            "source_class": "internal",
            "sender": null,
            "transport": "internal",
            "trust": "trusted",
            "origin": "cron",
        });
        let ctx: IngressContext = serde_json::from_value(legacy).unwrap();
        assert_eq!(ctx.origin, TurnOrigin::Cron);
        assert!(ctx.internal_principal.is_none());
    }

    #[test]
    fn principal_serializes_snake_case_and_round_trips() {
        let cases = [
            (
                IngressContext::cron_job("j1", None),
                serde_json::json!({"cron": {"job_id": "j1", "job_name": null}}),
            ),
            (
                IngressContext::peer_agent("front"),
                serde_json::json!({"peer_agent": {"sender_alias": "front"}}),
            ),
            (
                IngressContext::daemon_task("sop:r1:step:2"),
                serde_json::json!({"daemon": {"task": "sop:r1:step:2"}}),
            ),
        ];
        for (ctx, expected_principal) in cases {
            let v = serde_json::to_value(&ctx).unwrap();
            assert_eq!(v["internal_principal"], expected_principal);
            let back: IngressContext = serde_json::from_value(v).unwrap();
            assert_eq!(back, ctx);
        }
    }
}
