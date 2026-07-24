//! WHO resolved a SOP approval gate, and from WHERE (EPIC C).

use serde::{Deserialize, Serialize};

/// The transport a gate resolution arrived on. The agent tool, the loopback CLI,
/// the gateway WebSocket frame, the gateway HTTP route, or the daemon timeout tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    /// The in-agent `sop_approve` tool (the self-satisfiable path).
    Agent,
    /// `zeroclaw sop approve <id>` over loopback HTTP to the daemon.
    Cli,
    /// Gateway WebSocket `approval_response` frame.
    Ws,
    /// Gateway `POST /admin/sop/approve` route.
    Http,
    /// The timeout tick (escalate/cancel). Not an approval, a transition.
    System,
    /// A chat-channel gate answer (a SOP-gate button click or a
    /// `<choice> <reference>` text reply), attributed to the channel user who
    /// answered. Constructed only by the orchestrator's channel-gate intercept.
    Channel,
}

/// WHO resolved a gate and from WHERE. Recorded into the append-only ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPrincipal {
    pub source: ApprovalSource,
    /// Best-effort identity: pairing subject / bearer principal / agent alias /
    /// OS user for the loopback CLI. `None` for the system tick. Recorded, not trusted.
    pub identity: Option<String>,
    /// The back-channel that answered (WS connection id, "cli", agent name).
    pub channel: Option<String>,
}

impl ApprovalPrincipal {
    /// The in-agent tool. Constructed inside `sop_approve` ONLY, so the agent can
    /// never claim another source.
    pub fn agent(agent_alias: &str) -> Self {
        Self {
            source: ApprovalSource::Agent,
            identity: Some(agent_alias.to_string()),
            channel: Some(agent_alias.to_string()),
        }
    }

    /// The loopback CLI. Constructed in the gateway handler AFTER `require_localhost`.
    pub fn cli(os_user: Option<String>) -> Self {
        Self {
            source: ApprovalSource::Cli,
            identity: os_user,
            channel: Some("cli".to_string()),
        }
    }

    /// The gateway WebSocket. Constructed from the resolved connection only.
    pub fn ws(conn_id: String, subject: Option<String>) -> Self {
        Self {
            source: ApprovalSource::Ws,
            identity: subject,
            channel: Some(conn_id),
        }
    }

    /// The gateway HTTP route. Constructed from the resolved pairing subject only.
    pub fn http(subject: Option<String>) -> Self {
        Self {
            source: ApprovalSource::Http,
            identity: subject,
            channel: Some("http".to_string()),
        }
    }

    /// A chat-channel gate answer. `channel_key` is the answering channel
    /// (`discord.gnosis`), `user` the platform user id that clicked/replied.
    pub fn channel(channel_key: String, user: Option<String>) -> Self {
        Self {
            source: ApprovalSource::Channel,
            identity: user,
            channel: Some(channel_key),
        }
    }

    /// The daemon timeout tick.
    pub fn system() -> Self {
        Self {
            source: ApprovalSource::System,
            identity: None,
            channel: None,
        }
    }

    /// True when this principal is a DIFFERENT principal than the running agent.
    /// `OutOfBandRequired` mode requires this to clear a gate.
    pub fn is_out_of_band(&self) -> bool {
        self.source != ApprovalSource::Agent
    }

    /// True for the synthetic timeout principal. A `system` approval is metered as
    /// a timeout auto-approval rather than a human approval (matches the ledger
    /// `source == "system"` reconstruction in `rebuild_from_persistence`).
    pub fn is_system(&self) -> bool {
        self.source == ApprovalSource::System
    }

    /// Ledger actor string: prefer the identity, fall back to the source label.
    pub fn actor_label(&self) -> String {
        self.identity
            .clone()
            .unwrap_or_else(|| self.source_label().to_string())
    }

    /// The voter SOURCE for quorum distinctness. HTTP and WS collapse to a single
    /// `gateway` source because they authenticate via the SAME PairingGuard token -
    /// the derived subject (token hash) is identical whichever transport carries it,
    /// so one credential presented over HTTP and over WS is ONE voter, not two, and
    /// cannot inflate a quorum. Agent/CLI keep distinct sources (a CLI OS-user is a
    /// genuinely different actor from a paired gateway device).
    fn voter_source(&self) -> &'static str {
        match self.source {
            ApprovalSource::Http | ApprovalSource::Ws => "gateway",
            _ => self.source_label(),
        }
    }

    /// Quorum-distinctness key. Qualified by the *voter source* (see
    /// `Self::voter_source`) so a different identity on a different source stays
    /// distinct, while the same paired credential over HTTP+WS counts once. Channel
    /// voters are additionally namespaced by channel key so same-looking sender ids
    /// from different channels/aliases cannot satisfy each other's quorum. An
    /// identity-less principal keys to its source (and channel, for channel answers)
    /// so anonymous votes from one source count once and cannot inflate a quorum.
    pub fn voter_key(&self) -> String {
        if self.source == ApprovalSource::Channel {
            return match (self.channel.as_deref(), self.identity.as_deref()) {
                (Some(channel), Some(id)) => format!("channel:{channel}:{id}"),
                (Some(channel), None) => format!("channel:{channel}"),
                (None, Some(id)) => format!("channel:{id}"),
                (None, None) => "channel".to_string(),
            };
        }
        match &self.identity {
            Some(id) => format!("{}:{}", self.voter_source(), id),
            None => self.voter_source().to_string(),
        }
    }

    /// Stable wire label for the source (for ledger payloads / logs).
    pub fn source_label(&self) -> &'static str {
        match self.source {
            ApprovalSource::Agent => "agent",
            ApprovalSource::Cli => "cli",
            ApprovalSource::Ws => "ws",
            ApprovalSource::Http => "http",
            ApprovalSource::System => "system",
            ApprovalSource::Channel => "channel",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_pin_their_source() {
        assert_eq!(ApprovalPrincipal::agent("a").source, ApprovalSource::Agent);
        assert_eq!(ApprovalPrincipal::cli(None).source, ApprovalSource::Cli);
        assert_eq!(
            ApprovalPrincipal::ws("c1".into(), None).source,
            ApprovalSource::Ws
        );
        assert_eq!(ApprovalPrincipal::http(None).source, ApprovalSource::Http);
        assert_eq!(ApprovalPrincipal::system().source, ApprovalSource::System);
    }

    #[test]
    fn is_out_of_band_true_for_all_but_agent() {
        assert!(!ApprovalPrincipal::agent("a").is_out_of_band());
        assert!(ApprovalPrincipal::cli(None).is_out_of_band());
        assert!(ApprovalPrincipal::ws("c".into(), None).is_out_of_band());
        assert!(ApprovalPrincipal::http(None).is_out_of_band());
        assert!(ApprovalPrincipal::system().is_out_of_band());
    }

    #[test]
    fn actor_label_prefers_identity_then_source() {
        assert_eq!(
            ApprovalPrincipal::agent("deploy-bot").actor_label(),
            "deploy-bot"
        );
        assert_eq!(ApprovalPrincipal::cli(None).actor_label(), "cli");
        assert_eq!(ApprovalPrincipal::system().actor_label(), "system");
    }

    #[test]
    fn voter_key_collapses_gateway_transports_but_keeps_sources_distinct() {
        // HTTP and WS share the paired credential -> one canonical gateway voter.
        assert_eq!(
            ApprovalPrincipal::http(Some("sub".into())).voter_key(),
            ApprovalPrincipal::ws("conn".into(), Some("sub".into())).voter_key(),
            "the same subject over HTTP and WS is one voter"
        );
        assert_eq!(
            ApprovalPrincipal::http(Some("sub".into())).voter_key(),
            "gateway:sub"
        );
        // A CLI actor with the same name is a genuinely distinct voter.
        assert_ne!(
            ApprovalPrincipal::cli(Some("sub".into())).voter_key(),
            ApprovalPrincipal::http(Some("sub".into())).voter_key(),
            "a CLI actor is distinct from a gateway subject"
        );
        // Different subjects on the gateway stay distinct.
        assert_ne!(
            ApprovalPrincipal::http(Some("a".into())).voter_key(),
            ApprovalPrincipal::http(Some("b".into())).voter_key()
        );
    }

    #[test]
    fn voter_key_namespaces_channel_principals_by_channel_key() {
        assert_eq!(
            ApprovalPrincipal::channel("discord.ops".into(), Some("123".into())).voter_key(),
            "channel:discord.ops:123"
        );
        assert_ne!(
            ApprovalPrincipal::channel("discord.ops".into(), Some("123".into())).voter_key(),
            ApprovalPrincipal::channel("slack.ops".into(), Some("123".into())).voter_key(),
            "same-looking sender ids on different channels must be distinct voters"
        );
    }

    #[test]
    fn principal_source_not_client_settable() {
        // The security invariant: a principal's source comes from a constructor
        // (the transport), not from deserializing a client body. We CAN deserialize
        // the wire shape (for ledger round-trips), but production handlers build
        // principals via the constructors, never `serde::from` a request body.
        let json = r#"{"source":"http","identity":"attacker","channel":"x"}"#;
        let p: ApprovalPrincipal = serde_json::from_str(json).unwrap();
        assert_eq!(p.source, ApprovalSource::Http);
        // Constructors remain the only path used by handlers; this test documents
        // that the wire form exists for persistence, not for trust decisions.
    }
}
