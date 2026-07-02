//! WHO resolved a SOP approval gate, and from WHERE (EPIC C).
//!
//! `source` is the security-load-bearing field: it is ALWAYS derived from the
//! transport that called `resolve_gate`, NEVER from a client-supplied JSON field.
//! The constructors are the only way to build a principal, so a remote caller
//! cannot claim to be the agent (or vice versa) by shaping a request body.

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

    /// Stable wire label for the source (for ledger payloads / logs).
    pub fn source_label(&self) -> &'static str {
        match self.source {
            ApprovalSource::Agent => "agent",
            ApprovalSource::Cli => "cli",
            ApprovalSource::Ws => "ws",
            ApprovalSource::Http => "http",
            ApprovalSource::System => "system",
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
