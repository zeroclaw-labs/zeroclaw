//! Peer-inbox history admission resolves the existing peer relation at every
//! use. The event stream does not create a new peer/delegation permission.
use super::*;
use zeroclaw_api::{
    principal::PrincipalId,
    session_events::{capture::*, *},
};
use zeroclaw_kernel::session::{
    EffectIntent, PermissionResolver, SessionError,
    engine::{EngineSessionAuthority, EngineSessionSource},
    query::{SessionAccess, SessionAccessPolicy},
};

struct PeerAuthority {
    base: Arc<dyn EngineSessionAuthority>,
    capability: AgentExecutionCapability,
    sender: String,
    recipient: String,
    channel: String,
}
impl PeerAuthority {
    fn current(&self) -> Result<(), SessionError> {
        let handle = self.capability.config_handle();
        let config = handle.read();
        let peers = resolve_peer_set(&config, &self.sender);
        let fallback = self.channel.split_once('.').map(|(kind, _)| kind);
        if config.agent(&self.sender).is_none_or(|agent| {
            !agent.enabled
                || !agent
                    .channels
                    .iter()
                    .any(|channel| *channel == self.channel)
        }) || config
            .agent(&self.recipient)
            .is_none_or(|agent| !agent.enabled)
            || (!peers.is_known_peer(&self.channel, &self.recipient)
                && !fallback.is_some_and(|kind| peers.is_known_peer(kind, &self.recipient)))
        {
            return Err(SessionError::PermissionDenied(
                "peer inbox route revoked".into(),
            ));
        }
        Ok(())
    }
}
impl SessionAccessPolicy for PeerAuthority {
    fn authorize(
        &self,
        principal: &PrincipalId,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        self.current()?;
        SessionAccessPolicy::authorize(self.base.as_ref(), principal, stream, access)
    }
    fn authorize_actor(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_actor(actor, stream, access)
    }
}
impl PermissionResolver for PeerAuthority {
    fn tool_authority_evidence(
        &self,
        stream: &SessionStreamId,
        name: &str,
    ) -> Result<serde_json::Value, SessionError> {
        self.current()?;
        self.base.tool_authority_evidence(stream, name)
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        self.current()?;
        PermissionResolver::authorize(self.base.as_ref(), intent)
    }
    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_context(stream, branch)
    }
    fn authorize_tool(
        &self,
        intent: &EffectIntent,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_tool(intent, name, args)
    }
    fn child_authority(
        &self,
        target: &str,
        stream: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        self.current()?;
        Ok(Arc::new(Self {
            base: self.base.child_authority(target, stream)?,
            capability: self.capability.clone(),
            sender: self.sender.clone(),
            recipient: self.recipient.clone(),
            channel: self.channel.clone(),
        }))
    }
}
impl EngineSessionAuthority for PeerAuthority {
    fn execution_binding(&self, input: &EngineTurnInput) -> Result<PayloadRecord, SessionError> {
        self.current()?;
        self.base.execution_binding(input)
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        self.current()?;
        self.base.maintenance_binding()
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        self.base.diagnostic(error)
    }
}

pub(super) async fn open(
    source: Arc<dyn EngineSessionSource>,
    admission: AgentExecutionAdmission,
    task: &str,
    sender: &str,
    recipient: &str,
    channel: &str,
) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
    let family = format!("peer-inbox:{task}");
    let capability = admission.capability();
    let base = Arc::new(crate::live_config_authority::AgentHistoryAuthority {
        admission: admission.into(),
        actor: EventActor::System {
            component: "peer_inbox".into(),
        },
        stream: source.stream_id(&family).await?,
        origin: zeroclaw_api::ingress::TurnOrigin::AgentDirect,
        parent_alias: None,
    });
    let authority = Arc::new(PeerAuthority {
        base,
        capability,
        sender: sender.into(),
        recipient: recipient.into(),
        channel: channel.into(),
    });
    source
        .open_system(
            family,
            recipient.into(),
            "peer_inbox".into(),
            authority,
            ContextSnapshot {
                messages: vec![],
                history_has_trim_breadcrumb: Some(false),
            },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::{
        multi_agent::{AgentAlias, PeerGroupConfig},
        schema::AliasedAgentConfig,
    };

    #[test]
    fn peer_tool_evidence_tracks_live_policy_and_route_revocation() {
        let mut config = Config::default();
        config
            .risk_profiles
            .insert("fixture".into(), Default::default());
        for alias in ["sender", "recipient"] {
            config.agents.insert(
                alias.into(),
                AliasedAgentConfig {
                    channels: vec!["telegram.prod".into()],
                    risk_profile: "fixture".into(),
                    ..Default::default()
                },
            );
        }
        config.peer_groups.insert(
            "fixture".into(),
            PeerGroupConfig {
                channel: "telegram".into(),
                agents: vec![AgentAlias::new("sender"), AgentAlias::new("recipient")],
                ..Default::default()
            },
        );
        let live = crate::LiveConfigAuthority::new(config.clone());
        let capability = live.execution_capability();
        let stream = SessionStreamId::new();
        let peer = PeerAuthority {
            base: Arc::new(crate::live_config_authority::AgentHistoryAuthority {
                admission: capability.admit("recipient").expect("admission").into(),
                actor: EventActor::System {
                    component: "peer_inbox".into(),
                },
                stream: stream.clone(),
                origin: zeroclaw_api::ingress::TurnOrigin::AgentDirect,
                parent_alias: None,
            }),
            capability,
            sender: "sender".into(),
            recipient: "recipient".into(),
            channel: "telegram.prod".into(),
        };
        let before = peer
            .tool_authority_evidence(&stream, "shell")
            .expect("evidence");
        assert_eq!(before["source"], "live_tool_admission");
        config
            .risk_profiles
            .get_mut("fixture")
            .expect("profile")
            .always_ask = vec!["shell".into()];
        let revision = live.publish_for_test(config.clone());
        let after = peer
            .tool_authority_evidence(&stream, "shell")
            .expect("updated evidence");
        assert_eq!(
            after["config_revision"],
            serde_json::to_value(revision).expect("revision")
        );
        assert_ne!(after["config_revision"], before["config_revision"]);
        assert_eq!(
            after["matched_rules"]["always_ask"],
            serde_json::json!(["shell"])
        );
        assert!(
            peer.tool_authority_evidence(&SessionStreamId::new(), "shell")
                .is_err()
        );
        config.peer_groups.clear();
        live.publish_for_test(config);
        assert!(peer.tool_authority_evidence(&stream, "shell").is_err());
    }
}
