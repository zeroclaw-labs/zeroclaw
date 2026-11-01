//! Gateway history bridge. Pairing remains live transport authority, never a
//! persisted credential or an invented user identity.
use crate::AppState;
use std::sync::Arc;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::{
    ContextSnapshot, EngineTurnInput, SessionExecutionPort,
};
use zeroclaw_api::session_events::*;
use zeroclaw_kernel::session::engine::EngineSessionAuthority;
use zeroclaw_kernel::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_kernel::session::{EffectIntent, PermissionResolver, SessionError};
use zeroclaw_runtime::live_config_authority::{AgentExecutionCapability, AgentHistoryAuthority};

/// Verified transport proof stays in process memory. Neither credential nor
/// credential digest is included in durable input or execution evidence.
#[derive(Clone)]
pub(crate) enum IngressProof {
    Webhook {
        subject: Option<String>,
        secret_hash: Option<String>,
    },
    Paired {
        subject: Option<String>,
    },
    #[cfg(any(
        feature = "channel-linq",
        feature = "channel-nextcloud",
        feature = "channel-whatsapp-cloud"
    ))]
    Channel {
        kind: String,
        alias: String,
        sender: String,
        platform_sender: Option<String>,
    },
}
impl IngressProof {
    pub(crate) fn recovery_route(&self) -> serde_json::Value {
        match self {
            Self::Paired { .. } => serde_json::json!({"version":1,"kind":"paired"}),
            Self::Webhook { .. } => serde_json::json!({"version":1,"kind":"webhook"}),
            #[cfg(any(
                feature = "channel-linq",
                feature = "channel-nextcloud",
                feature = "channel-whatsapp-cloud"
            ))]
            Self::Channel {
                platform_sender, ..
            } => serde_json::json!({
                "version":1,"kind":"channel","platform_sender":platform_sender
            }),
        }
    }

    fn validates_recovery(&self, input: &EngineTurnInput) -> bool {
        if input.reply_route.as_ref() != Some(&self.recovery_route()) {
            return false;
        }
        match self {
            Self::Paired { .. } | Self::Webhook { .. } => {
                input.ingress.transport == zeroclaw_api::ingress::Transport::Gateway
                    && input.ingress.origin == zeroclaw_api::ingress::TurnOrigin::Interactive
                    && input.ingress.internal_principal.is_none()
            }
            #[cfg(any(
                feature = "channel-linq",
                feature = "channel-nextcloud",
                feature = "channel-whatsapp-cloud"
            ))]
            Self::Channel {
                kind,
                alias,
                sender,
                ..
            } => {
                input.ingress.transport
                    == (zeroclaw_api::ingress::Transport::Channel {
                        kind: kind.clone(),
                        alias: alias.clone(),
                    })
                    && input.ingress.sender.as_ref() == Some(sender)
                    && input.ingress.origin == zeroclaw_api::ingress::TurnOrigin::Channel
                    && input.ingress.internal_principal.is_none()
            }
        }
    }
}

fn recovery_prompt(
    input: &EngineTurnInput,
) -> anyhow::Result<zeroclaw_api::model_provider::ChatMessage> {
    use zeroclaw_api::session_events::capture::EngineInputPart;
    let mut text = Vec::new();
    let mut attachments = Vec::new();
    for part in &input.parts {
        match part {
            EngineInputPart::Text(value) => text.push(value.as_str()),
            EngineInputPart::Opaque { .. } => {}
            EngineInputPart::File(file) => {
                anyhow::ensure!(
                    file.registration["source"] == "gateway_attachment"
                        && file.representation.codec == "session.attachment-reference/v1",
                    "gateway_recovery_file_unsupported"
                );
                attachments.push(serde_json::from_slice(&file.representation.body)?);
            }
        }
    }
    Ok(
        zeroclaw_api::model_provider::ChatMessage::user(text.join("\n"))
            .with_attachments(attachments),
    )
}

/// Register explicit media references separately from the user's literal text.
pub(crate) fn with_attachments(
    mut input: EngineTurnInput,
    attachments: &[zeroclaw_api::media::AttachmentReference],
) -> anyhow::Result<EngineTurnInput> {
    use zeroclaw_api::session_events::{
        PayloadRecord,
        capture::{EngineFileInput, EngineInputPart},
    };
    for reference in attachments {
        input.parts.push(EngineInputPart::File(EngineFileInput {
            file_id: uuid::Uuid::new_v4().to_string(),
            registration: serde_json::json!({"source":"gateway_attachment"}),
            representation: PayloadRecord::json("session.attachment-reference/v1", reference)?,
            policy_evidence: serde_json::json!({"source":"authenticated_gateway"}),
        }));
    }
    Ok(input)
}

/// The caller owns the transport's session queue. Rebuild each engine from
/// durable input, then refresh context before the newly arrived request.
pub(crate) async fn recover_pending(
    state: &AppState,
    family: &str,
    port: &Arc<dyn SessionExecutionPort>,
    agent: &mut zeroclaw_runtime::agent::Agent,
    proof: &IngressProof,
    cancellation: Option<tokio_util::sync::CancellationToken>,
) -> anyhow::Result<()> {
    let (alias, provider, model) = agent.attribution_fields();
    for mut input in port.pending_inputs(provider, model).await? {
        if input.agent_alias.as_deref() != Some(&alias) || !proof.validates_recovery(&input) {
            port.reject_pending_input(&input.turn_id, "gateway_recovery_route_unavailable")
                .await?;
            continue;
        }
        let prompt = recovery_prompt(&input)?;
        restore(port.as_ref(), agent).await?;
        let (_, provider, model) = agent.attribution_fields();
        input.provider_ref = provider;
        input.model = model;
        let tracking = state.cost_tracker.as_ref().map(|tracker| {
            zeroclaw_runtime::agent::cost::tool_loop_cost_tracking_context_from_tracker(
                &state.config.read(),
                &alias,
                tracker.clone(),
            )
        });
        let engine = async {
            if let Some(token) = &cancellation {
                let (events, mut receiver) = tokio::sync::mpsc::channel(64);
                let turn = agent.turn_streamed(&prompt, events, Some(token.clone()));
                let (result, ()) =
                    tokio::join!(turn, async { while receiver.recv().await.is_some() {} });
                result.map(|(response, _)| response)
            } else {
                agent.turn(&prompt).await
            }
        };
        let turn = input.turn_id.clone();
        let result = zeroclaw_runtime::agent::loop_::scope_session_key(
            Some(family.into()),
            zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                tracking,
                zeroclaw_api::session_events::capture::scope_session_execution(
                    port.clone(),
                    port.execute(input, Box::pin(engine)),
                ),
            ),
        )
        .await;
        if let Err(error) = result {
            // HTTP and WebSocket can share a family while retaining distinct
            // admission actors. Only a still-queued input conflict is safe to
            // reject; a conflict after execution must retain its failure.
            let key = format!("engine-input:{turn}");
            let admission_changed = error.chain().any(|cause| matches!(
                cause.downcast_ref::<SessionError>(),
                Some(SessionError::Append(AppendError::IdempotencyConflict(conflict))) if conflict == &key
            ));
            if admission_changed {
                let (_, provider, model) = agent.attribution_fields();
                if port
                    .pending_inputs(provider, model)
                    .await?
                    .iter()
                    .any(|input| input.turn_id == turn)
                {
                    port.reject_pending_input(&turn, "gateway_recovery_admission_changed")
                        .await?;
                    continue;
                }
            }
            return Err(error);
        }
    }
    restore(port.as_ref(), agent).await
}
tokio::task_local! { static INGRESS_PROOF: IngressProof; }
pub(crate) async fn scope_ingress<F: std::future::Future>(
    proof: IngressProof,
    future: F,
) -> F::Output {
    INGRESS_PROOF.scope(proof, future).await
}
pub(crate) fn current_ingress() -> anyhow::Result<IngressProof> {
    INGRESS_PROOF
        .try_with(Clone::clone)
        .map_err(|_| anyhow::Error::msg("native_gateway_ingress_proof_missing"))
}
pub(crate) fn webhook_proof(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> anyhow::Result<IngressProof> {
    Ok(IngressProof::Webhook {
        subject: header_subject(state, headers)?,
        secret_hash: headers
            .get("X-Webhook-Secret")
            .and_then(|value| value.to_str().ok())
            .map(crate::hash_webhook_secret),
    })
}
struct IngressAuthority {
    base: Arc<dyn EngineSessionAuthority>,
    proof: IngressProof,
    live: zeroclaw_config::live::LiveConfigHandle,
    pairing: Arc<zeroclaw_config::pairing::PairingGuard>,
    root_alias: String,
}
impl IngressAuthority {
    fn current(&self) -> Result<(), SessionError> {
        let config = self.live.read();
        match &self.proof {
            IngressProof::Paired { subject } => {
                if self.pairing.require_pairing()
                    && !subject
                        .as_deref()
                        .is_some_and(|hash| self.pairing.token_hash_is_paired(hash))
                {
                    return Err(SessionError::PermissionDenied(
                        "gateway authentication revoked".into(),
                    ));
                }
            }
            IngressProof::Webhook {
                subject,
                secret_hash,
            } => {
                if (self.pairing.require_pairing()
                    && !subject
                        .as_deref()
                        .is_some_and(|hash| self.pairing.token_hash_is_paired(hash)))
                    || config
                        .gateway
                        .webhook_secret
                        .as_deref()
                        .map(str::trim)
                        .filter(|secret| !secret.is_empty())
                        .map(crate::hash_webhook_secret)
                        .is_some_and(|current| secret_hash.as_ref() != Some(&current))
                {
                    return Err(SessionError::PermissionDenied(
                        "gateway webhook authentication revoked".into(),
                    ));
                }
            }
            #[cfg(any(
                feature = "channel-linq",
                feature = "channel-nextcloud",
                feature = "channel-whatsapp-cloud"
            ))]
            IngressProof::Channel {
                kind,
                alias,
                sender,
                platform_sender,
            } => {
                let channel = format!("{kind}.{alias}");
                let peers = if matches!(kind.as_str(), "lark" | "feishu") {
                    config.channel_external_peers_for(&["lark", "feishu"], alias)
                } else {
                    config.channel_external_peers(kind, alias)
                };
                let ids = [sender.as_str(), platform_sender.as_deref().unwrap_or("")];
                if config.agent_for_channel(&channel) != Some(self.root_alias.as_str())
                    || !zeroclaw_config::schema::peer_policy_admits(&peers, &ids, |a, b| a == b)
                {
                    return Err(SessionError::PermissionDenied(
                        "gateway channel admission revoked".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}
impl SessionAccessPolicy for IngressAuthority {
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
impl PermissionResolver for IngressAuthority {
    fn tool_authority_evidence(
        &self,
        stream: &SessionStreamId,
        name: &str,
    ) -> Result<serde_json::Value, SessionError> {
        self.current()?;
        self.base.tool_authority_evidence(stream, name)
    }
    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_context(stream, branch)
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        self.current()?;
        PermissionResolver::authorize(self.base.as_ref(), intent)
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
            proof: self.proof.clone(),
            live: self.live.clone(),
            pairing: self.pairing.clone(),
            root_alias: self.root_alias.clone(),
        }))
    }
}
impl EngineSessionAuthority for IngressAuthority {
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
pub(crate) async fn open_ingress(
    state: &AppState,
    family: &str,
    alias: &str,
    proof: IngressProof,
) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
    let source = state
        .native_sessions
        .as_ref()
        .ok_or_else(|| anyhow::Error::msg("native_session_source_missing"))?;
    let component = "gateway_ingress";
    let origin = match proof {
        IngressProof::Webhook { .. } | IngressProof::Paired { .. } => {
            zeroclaw_api::ingress::TurnOrigin::Interactive
        }
        #[cfg(any(
            feature = "channel-linq",
            feature = "channel-nextcloud",
            feature = "channel-whatsapp-cloud"
        ))]
        IngressProof::Channel { .. } => zeroclaw_api::ingress::TurnOrigin::Channel,
    };
    let capability =
        AgentExecutionCapability::from_parts(state.config.clone(), state.agent_lifecycle.clone());
    let authority = Arc::new(IngressAuthority {
        base: Arc::new(AgentHistoryAuthority {
            admission: capability.admit(alias)?.into(),
            actor: EventActor::System {
                component: component.into(),
            },
            stream: source.stream_id(family).await?,
            origin,
            parent_alias: None,
        }),
        proof,
        live: state.config.clone(),
        pairing: state.pairing.clone(),
        root_alias: alias.into(),
    });
    authority.current()?;
    source
        .open_system(
            family.into(),
            alias.into(),
            component.into(),
            authority,
            ContextSnapshot {
                messages: vec![],
                history_has_trim_breadcrumb: Some(false),
            },
        )
        .await
}

struct GatewayAuthority {
    base: Arc<dyn EngineSessionAuthority>,
    pairing: Arc<zeroclaw_config::pairing::PairingGuard>,
    subject: Option<String>,
}
impl GatewayAuthority {
    fn current(&self) -> Result<(), SessionError> {
        verify_history_subject(&self.pairing, self.subject.as_deref())
    }
}

fn verify_history_subject(
    pairing: &zeroclaw_config::pairing::PairingGuard,
    subject: Option<&str>,
) -> Result<(), SessionError> {
    if pairing.require_pairing() && !subject.is_some_and(|hash| pairing.token_hash_is_paired(hash))
    {
        return Err(SessionError::PermissionDenied(
            "gateway authentication revoked".into(),
        ));
    }
    Ok(())
}

fn history_access_denied(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        matches!(
            source.downcast_ref::<SessionError>(),
            Some(SessionError::PermissionDenied(_))
        ) || matches!(
            source.downcast_ref::<ReadError>(),
            Some(ReadError::AccessDenied(_))
        )
    })
}
impl SessionAccessPolicy for GatewayAuthority {
    fn authorize_reasoning(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_reasoning(actor, stream)
    }
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
impl PermissionResolver for GatewayAuthority {
    fn tool_authority_evidence(
        &self,
        stream: &SessionStreamId,
        name: &str,
    ) -> Result<serde_json::Value, SessionError> {
        self.current()?;
        self.base.tool_authority_evidence(stream, name)
    }
    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_context(stream, branch)
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        self.current()?;
        PermissionResolver::authorize(self.base.as_ref(), intent)
    }
    fn authorize_tool(
        &self,
        intent: &EffectIntent,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<(), SessionError> {
        self.current()?;
        self.base.authorize_tool(intent, name, arguments)
    }
    fn child_authority(
        &self,
        target: &str,
        stream: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        self.current()?;
        Ok(Arc::new(Self {
            base: self.base.child_authority(target, stream)?,
            pairing: self.pairing.clone(),
            subject: self.subject.clone(),
        }))
    }
}
impl EngineSessionAuthority for GatewayAuthority {
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
pub(crate) fn authority(
    state: &AppState,
    stream: SessionStreamId,
    alias: &str,
    subject: Option<&str>,
) -> anyhow::Result<Arc<dyn EngineSessionAuthority>> {
    let capability =
        AgentExecutionCapability::from_parts(state.config.clone(), state.agent_lifecycle.clone());
    let authority = Arc::new(GatewayAuthority {
        base: Arc::new(AgentHistoryAuthority {
            admission: capability
                .admit(alias)
                .map_err(|error| SessionError::PermissionDenied(error.to_string()))?
                .into(),
            actor: EventActor::Principal {
                principal_id: PrincipalId::shared_operator().0,
            },
            stream,
            origin: zeroclaw_api::ingress::TurnOrigin::Interactive,
            parent_alias: None,
        }),
        pairing: state.pairing.clone(),
        subject: subject.map(str::to_owned),
    });
    authority.current()?;
    Ok(authority)
}
pub(crate) fn header_subject(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> anyhow::Result<Option<String>> {
    if !state.pairing.require_pairing() {
        return Ok(None);
    }
    state
        .pairing
        .authenticate_and_hash(crate::api::extract_bearer_token(headers).unwrap_or(""))
        .map(Some)
        .ok_or_else(|| anyhow::Error::msg("gateway authentication revoked"))
}
pub(crate) async fn open(
    state: &AppState,
    family: &str,
    alias: &str,
    subject: Option<&str>,
) -> anyhow::Result<Option<Arc<dyn SessionExecutionPort>>> {
    let Some(source) = &state.native_sessions else {
        return Ok(None);
    };
    let authority = authority(state, source.stream_id(family).await?, alias, subject)?;
    Ok(Some(
        source
            .open(
                family.into(),
                alias.into(),
                PrincipalId::shared_operator(),
                authority,
                ContextSnapshot {
                    messages: vec![],
                    history_has_trim_breadcrumb: Some(false),
                },
            )
            .await?,
    ))
}
pub(crate) async fn query(
    state: &AppState,
    family: &str,
    subject: Option<&str>,
) -> anyhow::Result<Option<zeroclaw_kernel::session::engine::FamilyQuery>> {
    let Some(source) = &state.native_sessions else {
        return Ok(None);
    };
    let Some(descriptor) = source.describe(family.into()).await? else {
        return Ok(None);
    };
    let authority = authority(state, descriptor.stream, &descriptor.owning_agent, subject)?;
    source
        .query(family.into(), PrincipalId::shared_operator(), authority)
        .await
}
pub(crate) fn input(
    turn_id: String,
    alias: String,
    text: String,
    provider_ref: String,
    model: String,
) -> EngineTurnInput {
    let mut ingress = zeroclaw_api::ingress::IngressContext::interactive();
    ingress.transport = zeroclaw_api::ingress::Transport::Gateway;
    ingress.source_class = zeroclaw_api::ingress::SourceClass::External;
    EngineTurnInput {
        execution_workspace: None,
        reply_route: None,
        turn_id,
        agent_alias: Some(alias),
        ingress,
        parts: vec![zeroclaw_api::session_events::capture::EngineInputPart::Text(text)],
        provider_ref,
        model,
    }
}
pub(crate) async fn restore(
    port: &dyn SessionExecutionPort,
    agent: &mut zeroclaw_runtime::agent::Agent,
) -> anyhow::Result<()> {
    if let Some(selection) = port.restore_model_override().await? {
        agent.restore_session_model_selection(selection)?;
    }
    if let Some(context) = port.restore_context().await? {
        agent.clear_history();
        agent.set_history_has_trim_breadcrumb(context.history_has_trim_breadcrumb.unwrap_or(false));
        agent.seed_conversation_history(context.messages);
        port.replace_context(
            ContextSnapshot {
                messages: (agent.history()).to_vec(),
                history_has_trim_breadcrumb: Some(agent.history_has_trim_breadcrumb()),
            },
            "gateway_restore_trim",
        )
        .await?;
    }
    Ok(())
}

/// Resolve exact existing families before applying the display-id namespace.
pub(crate) async fn family(state: &AppState, id: &str) -> anyhow::Result<String> {
    let source = state
        .native_sessions
        .as_ref()
        .ok_or_else(|| anyhow::Error::msg("native_session_source_missing"))?;
    if source.describe(id.into()).await?.is_some() {
        return Ok(id.into());
    }
    Ok(if id.starts_with("gw_") {
        id.into()
    } else {
        format!("gw_{id}")
    })
}
pub(crate) async fn command(
    state: &AppState,
    family: &str,
    subject: Option<&str>,
) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
    let source = state
        .native_sessions
        .as_ref()
        .ok_or_else(|| anyhow::Error::msg("native_session_source_missing"))?;
    let descriptor = source
        .describe(family.into())
        .await?
        .ok_or_else(|| anyhow::Error::msg("session_not_found"))?;
    // Commands address the exact history selected by the caller. Only a new
    // execution may follow a TTL continuation.
    let authority = authority(state, descriptor.stream, &descriptor.owning_agent, subject)?;
    source
        .open(
            family.into(),
            descriptor.owning_agent,
            PrincipalId::shared_operator(),
            authority,
            ContextSnapshot {
                messages: vec![],
                history_has_trim_breadcrumb: Some(false),
            },
        )
        .await
}
pub(crate) async fn list(
    state: &AppState,
    subject: Option<&str>,
    running_only: bool,
) -> anyhow::Result<serde_json::Value> {
    verify_history_subject(&state.pairing, subject)?;
    let source = state
        .native_sessions
        .as_ref()
        .ok_or_else(|| anyhow::Error::msg("native_session_source_missing"))?;
    let mut sessions = Vec::new();
    for family in source.families().await? {
        let native = match query(state, &family, subject).await {
            Ok(Some(native)) => native,
            Ok(None) => continue,
            Err(error) if history_access_denied(&error) => continue,
            Err(error) => return Err(error),
        };
        let summary = native.query.summary(native.stream.clone()).await?;
        if summary.closed || (running_only && !summary.running) {
            continue;
        }
        let timestamp =
            |millis| chrono::DateTime::from_timestamp_millis(millis).map(|time| time.to_rfc3339());
        sessions.push(serde_json::json!({"session_id":family.strip_prefix("gw_").unwrap_or(&family),"session_key":family,"name":summary.title().map(str::to_owned),"agent_alias":summary.owning_agent,"channel_id":summary.channel_id().map(str::to_owned),"message_count":summary.message_count,"created_at":timestamp(summary.created_at),"last_activity":timestamp(summary.last_activity),"source_cursor":summary.head}));
    }
    verify_history_subject(&state.pairing, subject)?;
    Ok(serde_json::json!({"sessions":sessions}))
}
pub(crate) fn response(result: anyhow::Result<serde_json::Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => {
            use axum::http::StatusCode;
            let denied = history_access_denied(&error);
            let status = if denied {
                StatusCode::FORBIDDEN
            } else if error.to_string() == "session_not_found" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status,axum::Json(serde_json::json!({"error":zeroclaw_runtime::agent::Agent::session_execution_diagnostic(&error)}))).into_response()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use zeroclaw_kernel::session::SessionCoordinator;
    use zeroclaw_kernel::session::engine::{
        EngineSession, EngineSessionSource, FamilyDescriptor, FamilyQuery,
    };

    #[test]
    fn gateway_and_ingress_tool_evidence_tracks_live_policy_and_authentication() {
        let mut config = zeroclaw_config::schema::Config::default();
        config
            .risk_profiles
            .insert("fixture".into(), Default::default());
        config.agents.insert(
            "web".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                risk_profile: "fixture".into(),
                ..Default::default()
            },
        );
        let mut state = crate::api::tests::test_state(config.clone());
        state.pairing = Arc::new(zeroclaw_config::pairing::PairingGuard::new(
            true,
            &["fixture-bearer".into()],
            Default::default(),
        ));
        let subject = state
            .pairing
            .authenticate_and_hash("fixture-bearer")
            .expect("authenticate");
        let stream = SessionStreamId::new();
        let gateway = authority(&state, stream.clone(), "web", Some(&subject)).expect("authority");
        let ingress = IngressAuthority {
            base: gateway.clone(),
            proof: IngressProof::Paired {
                subject: Some(subject.clone()),
            },
            live: state.config.clone(),
            pairing: state.pairing.clone(),
            root_alias: "web".into(),
        };
        let evidence = gateway
            .tool_authority_evidence(&stream, "shell")
            .expect("gateway evidence");
        assert_eq!(evidence["source"], "live_tool_admission");
        let before = ingress
            .tool_authority_evidence(&stream, "shell")
            .expect("ingress evidence");
        assert_eq!(before["source"], "live_tool_admission");
        config
            .risk_profiles
            .get_mut("fixture")
            .expect("profile")
            .always_ask = vec!["shell".into()];
        let revision = state.config_authority.publish_for_test(config);
        for evidence in [
            gateway
                .tool_authority_evidence(&stream, "shell")
                .expect("updated gateway"),
            ingress
                .tool_authority_evidence(&stream, "shell")
                .expect("updated ingress"),
        ] {
            assert_eq!(
                evidence["config_revision"],
                serde_json::to_value(revision).expect("revision")
            );
            assert_ne!(evidence["config_revision"], before["config_revision"]);
            assert_eq!(
                evidence["matched_rules"]["always_ask"],
                serde_json::json!(["shell"])
            );
        }
        assert!(
            gateway
                .tool_authority_evidence(&SessionStreamId::new(), "shell")
                .is_err()
        );
        assert!(state.pairing.revoke_token_hash(&subject));
        assert!(gateway.tool_authority_evidence(&stream, "shell").is_err());
        assert!(ingress.tool_authority_evidence(&stream, "shell").is_err());
    }

    #[test]
    fn gateway_recovery_route_does_not_persist_credentials() {
        let proof = IngressProof::Webhook {
            subject: Some("private-subject".into()),
            secret_hash: Some("private-secret".into()),
        };
        let route = proof.recovery_route();
        assert_eq!(route, serde_json::json!({"version":1,"kind":"webhook"}));
        let mut accepted = input(
            "turn".into(),
            "web".into(),
            "before".into(),
            "provider".into(),
            "model".into(),
        );
        accepted.reply_route = Some(route);
        accepted.parts.push(
            zeroclaw_api::session_events::capture::EngineInputPart::Opaque {
                type_name: "future.part".into(),
                body: serde_json::json!({"url":"https://example.invalid/no-fetch"}),
            },
        );
        accepted
            .parts
            .push(zeroclaw_api::session_events::capture::EngineInputPart::Text("after".into()));
        assert!(proof.validates_recovery(&accepted));
        assert!(!IngressProof::Paired { subject: None }.validates_recovery(&accepted));
        assert_eq!(recovery_prompt(&accepted).unwrap().content, "before\nafter");
    }

    #[test]
    fn gateway_recovery_preserves_typed_media_and_literal_text_separately() {
        use zeroclaw_api::media::{AttachmentKind, AttachmentReference};
        let references = vec![AttachmentReference {
            kind: AttachmentKind::Image,
            target: "/workspace/webui_uploads/photo.png".into(),
        }];
        let accepted = with_attachments(
            input(
                "turn".into(),
                "web".into(),
                "literal [IMAGE:/example]".into(),
                "provider".into(),
                "model".into(),
            ),
            &references,
        )
        .unwrap();
        let recovered = recovery_prompt(&accepted).unwrap();
        assert_eq!(recovered.content, "literal [IMAGE:/example]");
        assert_eq!(recovered.attachments, references);
    }

    // Test composition uses the real coordinator and both physical stores.
    // Only application family routing is reduced to one fixture family.
    pub(crate) struct Source {
        pub owner: Arc<SessionCoordinator>,
        pub stream: SessionStreamId,
        branch: BranchId,
        family: String,
    }
    impl Source {
        pub(crate) fn new(directory: &std::path::Path, backend: &str, family: &str) -> Arc<Self> {
            let owner = Arc::new(SessionCoordinator::new(
                zeroclaw_infra::session_events::open_event_store(directory, backend)
                    .expect("store"),
            ));
            let stream = SessionStreamId::new();
            let branch = BranchId::new();
            let mut root = SessionEvent::typed(
                stream.clone(),
                EventData::SessionStarted {
                    session_id: SessionId::new(),
                    owning_agent: "web".into(),
                    capture_mode: CaptureMode::Native,
                },
                EventActor::Principal {
                    principal_id: PrincipalId::shared_operator().0,
                },
            )
            .expect("root");
            root.branch_id = Some(branch.clone());
            let context = PayloadRecord::json(
                "session.context/v1",
                &serde_json::json!({"messages":[],"history_has_trim_breadcrumb":false}),
            )
            .expect("context");
            let mut projection = SessionEvent::typed(
                stream.clone(),
                EventData::ContextProjectionCreated {
                    source_ranges: vec![],
                    summary_ref: None,
                    budget: 0,
                    reason_key: "initial".into(),
                    algorithm_version: "fixture/v1".into(),
                    result_ref: StoredPayloadRef::from(&context),
                    summary_source_refs: vec![],
                    transform_started_event_id: None,
                },
                EventActor::default(),
            )
            .expect("projection");
            projection.branch_id = Some(branch.clone());
            owner
                .append(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: EventCursor::beginning(stream.clone()),
                    idempotency_key: "fixture-root".into(),
                    events: vec![root, projection],
                    payloads: vec![context],
                    erasure_reference_commitment: None,
                })
                .expect("append");
            Arc::new(Self {
                owner,
                stream,
                branch,
                family: family.into(),
            })
        }
    }
    #[async_trait::async_trait]
    impl EngineSessionSource for Source {
        async fn stream_id(&self, family: &str) -> Result<SessionStreamId, ContractError> {
            assert_eq!(family, self.family);
            Ok(self.stream.clone())
        }
        async fn families(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec![self.family.clone()])
        }
        async fn describe(&self, family: String) -> anyhow::Result<Option<FamilyDescriptor>> {
            Ok((family == self.family).then(|| FamilyDescriptor {
                stream: self.stream.clone(),
                owning_agent: "web".into(),
                owner: Some(PrincipalId::shared_operator().0),
            }))
        }
        async fn open(
            &self,
            family: String,
            alias: String,
            principal: PrincipalId,
            authority: Arc<dyn EngineSessionAuthority>,
            _initial: ContextSnapshot,
        ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
            anyhow::ensure!(
                family == self.family && alias == "web",
                "fixture_scope_mismatch"
            );
            Ok(Arc::new(EngineSession::new(
                self.owner.clone(),
                self.stream.clone(),
                self.branch.clone(),
                principal,
                authority,
            )))
        }
        async fn open_system(
            &self,
            family: String,
            alias: String,
            component: String,
            authority: Arc<dyn EngineSessionAuthority>,
            _initial: ContextSnapshot,
        ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
            anyhow::ensure!(
                family == self.family && alias == "web",
                "fixture_scope_mismatch"
            );
            Ok(Arc::new(EngineSession::with_actor(
                self.owner.clone(),
                self.stream.clone(),
                self.branch.clone(),
                EventActor::System { component },
                authority,
            )))
        }
        async fn query(
            &self,
            family: String,
            principal: PrincipalId,
            policy: Arc<dyn SessionAccessPolicy>,
        ) -> anyhow::Result<Option<FamilyQuery>> {
            if family != self.family {
                return Ok(None);
            }
            policy.authorize(&principal, &self.stream, SessionAccess::Read)?;
            Ok(Some(FamilyQuery {
                stream: self.stream.clone(),
                query: self.owner.query(principal, policy),
            }))
        }
    }
    #[tokio::test]
    async fn native_gateway_commands_and_http_queries_share_events_and_live_auth() {
        use axum::{
            extract::{Path, State},
            http::HeaderMap,
            response::IntoResponse,
        };
        use http_body_util::BodyExt;
        for backend in ["sqlite", "jsonl"] {
            let directory = tempfile::tempdir().expect("directory");
            let mut config = zeroclaw_config::schema::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            };
            config
                .risk_profiles
                .insert("fixture".into(), Default::default());
            config.agents.insert(
                "web".into(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    risk_profile: "fixture".into(),
                    ..Default::default()
                },
            );
            let mut state = crate::api::tests::test_state(config);
            let source = Source::new(directory.path(), backend, "gw_native-fixture");
            state.native_sessions = Some(source.clone());
            let port = open(&state, "gw_native-fixture", "web", None)
                .await
                .expect("open")
                .expect("port");
            port.append_assistant("historical notice")
                .await
                .expect("append");
            port.append_assistant("retained notice")
                .await
                .expect("append second");
            port.rename("native title").await.expect("rename");
            let response = crate::api::handle_api_session_messages(
                State(state.clone()),
                HeaderMap::new(),
                Path("native-fixture".into()),
            )
            .await
            .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let value: serde_json::Value = serde_json::from_slice(
                &response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes(),
            )
            .expect("json");
            assert_eq!(value["messages"][0]["content"], "historical notice");
            assert!(value["messages"][0]["message_id"].is_string());
            assert!(value["messages"][0]["created_at"].is_string());
            assert_eq!(
                list(&state, None, false).await.expect("list")["sessions"][0]["name"],
                "native title"
            );
            let policy = Arc::new(UnrestrictedFixture);
            let query = source
                .owner
                .query(PrincipalId::shared_operator(), policy.clone());
            let page = query
                .read_page(
                    source.stream.clone(),
                    EventCursor::beginning(source.stream.clone()),
                    None,
                    100,
                )
                .await
                .expect("events");
            let message = page
                .events
                .iter()
                .find(|event| event.body.kind() == EventKind::AssistantContentRecorded)
                .expect("message");
            let PayloadRead::Present(leaf) = query
                .read_payload(
                    source.stream.clone(),
                    zeroclaw_kernel::session::query::PayloadSelector {
                        event_sequence: message.sequence,
                        reference_path: vec![0, 0],
                    },
                )
                .await
                .expect("leaf")
            else {
                panic!("present")
            };
            let intent = SessionEvent::typed(
                source.stream.clone(),
                EventData::PayloadErasureRequested {
                    targets: vec![StoredPayloadRef::from(&leaf)],
                    reason_category: "operator_requested".into(),
                },
                EventActor::Principal {
                    principal_id: PrincipalId::shared_operator().0,
                },
            )
            .expect("intent");
            source
                .owner
                .request_erasure(
                    PrincipalId::shared_operator(),
                    policy,
                    AppendRequest {
                        stream_id: source.stream.clone(),
                        expected_head: page.through,
                        idempotency_key: "erase-gateway-message".into(),
                        events: vec![intent],
                        payloads: vec![],
                        erasure_reference_commitment: None,
                    },
                )
                .await
                .expect("erase");
            let response = crate::api::handle_api_session_messages(
                State(state.clone()),
                HeaderMap::new(),
                Path("native-fixture".into()),
            )
            .await
            .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            let erased: serde_json::Value = serde_json::from_slice(
                &response
                    .into_body()
                    .collect()
                    .await
                    .expect("body")
                    .to_bytes(),
            )
            .expect("json");
            assert_eq!(
                erased["messages"][0]["message_id"],
                value["messages"][0]["message_id"]
            );
            assert_eq!(erased["messages"][0]["content"], "");
            assert_eq!(
                erased["messages"][0]["unavailable_parts"][0]["disposition"],
                "redacted"
            );
            assert_eq!(erased["messages"][1]["content"], "retained notice");
            state.pairing = Arc::new(zeroclaw_config::pairing::PairingGuard::new(
                true,
                &["fixture-bearer".into()],
                Default::default(),
            ));
            let hash = state
                .pairing
                .authenticate_and_hash("fixture-bearer")
                .expect("fixture authentication");
            let bound = open(&state, "gw_native-fixture", "web", Some(&hash))
                .await
                .expect("authenticated open")
                .expect("port");
            assert!(
                open(&state, "gw_native-fixture", "web", None)
                    .await
                    .is_err()
            );
            assert!(state.pairing.revoke_token_hash(&hash));
            assert!(bound.append_assistant("revoked action").await.is_err());
            assert!(bound.restore_context().await.is_err());
            let response = crate::api::handle_api_session_messages(
                State(state.clone()),
                HeaderMap::new(),
                Path("native-fixture".into()),
            )
            .await
            .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
            // The paired proof is ephemeral; history records no bearer or hash.
            let query = source.owner.query(
                PrincipalId::shared_operator(),
                Arc::new(UnrestrictedFixture),
            );
            let page = query
                .read_page(
                    source.stream.clone(),
                    EventCursor::beginning(source.stream.clone()),
                    None,
                    256,
                )
                .await
                .expect("events");
            let serialized =
                String::from_utf8(canonical_json(&page.events).expect("codec")).expect("utf8");
            assert!(!serialized.contains("fixture-bearer"));
            assert!(!serialized.contains(&hash));
        }
    }
    #[tokio::test]
    async fn native_http_mutations_resolve_ids_preserve_history_and_cancel_exact_session() {
        use axum::{
            Json,
            extract::{Path, State},
            http::{HeaderMap, StatusCode},
            response::IntoResponse,
        };
        use tokio_util::sync::CancellationToken;
        for backend in ["sqlite", "jsonl"] {
            for family in ["gw_team_alpha", "gw_team.alpha"] {
                let directory = tempfile::tempdir().expect("directory");
                let mut config = zeroclaw_config::schema::Config {
                    data_dir: directory.path().into(),
                    ..Default::default()
                };
                config
                    .risk_profiles
                    .insert("fixture".into(), Default::default());
                config.agents.insert(
                    "web".into(),
                    zeroclaw_config::schema::AliasedAgentConfig {
                        risk_profile: "fixture".into(),
                        ..Default::default()
                    },
                );
                let mut state = crate::api::tests::test_state(config);
                let source = Source::new(directory.path(), backend, family);
                state.native_sessions = Some(source.clone());
                let port = open(&state, family, "web", None)
                    .await
                    .expect("open")
                    .expect("port");
                port.append_assistant("original").await.expect("seed");
                let display = family.strip_prefix("gw_").expect("display");
                let mut rx = state.event_tx.subscribe();
                for id in [display, family] {
                    let response = crate::api::handle_api_session_message_post(
                        State(state.clone()),
                        HeaderMap::new(),
                        Path(id.into()),
                        Json(
                            serde_json::from_value(
                                serde_json::json!({"content":"raw API content"}),
                            )
                            .expect("body"),
                        ),
                    )
                    .await
                    .into_response();
                    assert_eq!(response.status(), StatusCode::OK);
                    let event = rx.try_recv().expect("broadcast");
                    assert_eq!(event["session_id"], display);
                    assert_eq!(event["content"], "raw API content");
                    let rename = crate::api::handle_api_session_rename(
                        State(state.clone()),
                        HeaderMap::new(),
                        Path(id.into()),
                        Json(serde_json::json!({"name":"renamed"})),
                    )
                    .await
                    .into_response();
                    assert_eq!(rename.status(), StatusCode::OK);
                    let status = crate::api::handle_api_session_state(
                        State(state.clone()),
                        HeaderMap::new(),
                        Path(id.into()),
                    )
                    .await
                    .into_response();
                    assert_eq!(status.status(), StatusCode::OK);
                }
                let empty = crate::api::handle_api_session_message_post(
                    State(state.clone()),
                    HeaderMap::new(),
                    Path(display.into()),
                    Json(serde_json::from_value(serde_json::json!({"content":" "})).expect("body")),
                )
                .await
                .into_response();
                assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
                let missing = crate::api::handle_api_session_message_post(
                    State(state.clone()),
                    HeaderMap::new(),
                    Path("missing".into()),
                    Json(
                        serde_json::from_value(serde_json::json!({"content":"must not create"}))
                            .expect("body"),
                    ),
                )
                .await
                .into_response();
                assert_eq!(missing.status(), StatusCode::NOT_FOUND);
                let active = CancellationToken::new();
                let other = CancellationToken::new();
                {
                    let mut tokens = state.cancel_tokens.lock().expect("tokens");
                    tokens.insert(family.into(), Arc::new(active.clone()));
                    tokens.insert(format!("{family}-other"), Arc::new(other.clone()));
                }
                let deletion = crate::api::handle_api_session_delete(
                    State(state.clone()),
                    HeaderMap::new(),
                    Path(display.into()),
                )
                .await
                .into_response();
                assert_eq!(deletion.status(), StatusCode::OK);
                assert!(active.is_cancelled());
                assert!(!other.is_cancelled());
                let query = source.owner.query(
                    PrincipalId::shared_operator(),
                    Arc::new(UnrestrictedFixture),
                );
                assert_eq!(
                    query
                        .messages(source.stream.clone())
                        .await
                        .expect("retained history")
                        .len(),
                    3
                );
                assert!(
                    query
                        .summary(source.stream.clone())
                        .await
                        .expect("summary")
                        .closed
                );
            }
        }
    }

    #[tokio::test]
    async fn native_http_delivery_records_body_production_only_when_polled() {
        use http_body_util::BodyExt;
        for backend in ["sqlite", "jsonl"] {
            let directory = tempfile::tempdir().expect("directory");
            let mut config = zeroclaw_config::schema::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            };
            config
                .risk_profiles
                .insert("fixture".into(), Default::default());
            config.agents.insert(
                "web".into(),
                zeroclaw_config::schema::AliasedAgentConfig {
                    risk_profile: "fixture".into(),
                    ..Default::default()
                },
            );
            let mut state = crate::api::tests::test_state(config);
            let source = Source::new(directory.path(), backend, "gw_body-fixture");
            state.native_sessions = Some(source.clone());
            let port = open(&state, "gw_body-fixture", "web", None)
                .await
                .expect("open")
                .expect("port");
            let turn = TurnId::new().to_string();
            port.execute(
                input(
                    turn.clone(),
                    "web".into(),
                    "body fixture".into(),
                    "fixture".into(),
                    "fixture".into(),
                ),
                Box::pin(async { Ok("answer".into()) }),
            )
            .await
            .expect("turn");
            let coordinate = DeliveryCoordinate { port, turn };
            let query = source.owner.query(
                PrincipalId::shared_operator(),
                Arc::new(UnrestrictedFixture),
            );
            let head = query
                .read_page(
                    source.stream.clone(),
                    EventCursor::beginning(source.stream.clone()),
                    None,
                    256,
                )
                .await
                .expect("before")
                .through;
            drop(json_response(
                serde_json::json!({"answer":"discarded"}),
                Some(coordinate.clone()),
                "webhook_http",
            ));
            assert!(
                query
                    .read_page(source.stream.clone(), head.clone(), None, 256)
                    .await
                    .expect("dropped body")
                    .events
                    .is_empty()
            );
            let response = json_response(
                serde_json::json!({"answer":"produced"}),
                Some(coordinate),
                "webhook_http",
            );
            let bytes = response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")["answer"],
                "produced"
            );
            let page = source
                .owner
                .query(
                    PrincipalId::shared_operator(),
                    Arc::new(UnrestrictedFixture),
                )
                .read_page(source.stream.clone(), head, None, 256)
                .await
                .expect("events");
            assert_eq!(page.events.len(), 2);
            assert_eq!(page.events[0].body.kind(), EventKind::DeliveryStarted);
            assert_eq!(page.events[1].body.kind(), EventKind::DeliveryCompleted);
        }
    }
    struct UnrestrictedFixture;
    impl SessionAccessPolicy for UnrestrictedFixture {
        fn authorize(
            &self,
            _principal: &PrincipalId,
            _stream: &SessionStreamId,
            _access: SessionAccess,
        ) -> Result<(), SessionError> {
            Ok(())
        }
    }
}

/// Keep only a bound execution coordinate for transport observations. Policy
/// remains live on the port; response text stays on its existing owner.
#[derive(Clone)]
pub(crate) struct DeliveryCoordinate {
    pub port: Arc<dyn SessionExecutionPort>,
    pub turn: String,
}
impl DeliveryCoordinate {
    pub(crate) async fn observe(
        &self,
        transport: &str,
        target: &str,
        content: &str,
        boundary: &str,
        effect: impl std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send,
    ) -> anyhow::Result<()> {
        self.port
            .deliver(
                zeroclaw_api::session_events::capture::DeliveryIntent {
                    turn_id: Some(self.turn.clone()),
                    transport: transport.into(),
                    target: target.into(),
                    content: content.into(),
                    completion_boundary: boundary.into(),
                },
                Box::pin(effect),
            )
            .await?;
        Ok(())
    }
}

/// HTTP handlers can prove body production, not remote acceptance or reading.
/// Commit the boundary when Hyper actually polls the response body. A dropped
/// body never manufactures a successful network acknowledgement.
pub(crate) fn json_response(
    body: serde_json::Value,
    delivery: Option<DeliveryCoordinate>,
    transport: &'static str,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(delivery) = delivery else {
        return axum::Json(body).into_response();
    };
    let encoded = body.to_string();
    let stream =
        futures_util::stream::once(async move {
            delivery.observe(transport,"http_response",&encoded,"http_body_produced",async {
            Ok(serde_json::json!({"boundary":"http_body_produced","bytes":encoded.len()}))
        }).await?;
            Ok::<_, anyhow::Error>(axum::body::Bytes::from(encoded))
        });
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

/// Preserve the bound transport coordinate when a committed engine outcome is
/// an error, without flattening its typed diagnostic chain.
pub(crate) struct TurnDeliveryError {
    pub error: anyhow::Error,
    pub delivery: DeliveryCoordinate,
}
impl std::fmt::Debug for TurnDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnDeliveryError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for TurnDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for TurnDeliveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}
pub(crate) fn failure_delivery(error: &anyhow::Error) -> Option<DeliveryCoordinate> {
    error.chain().find_map(|cause| {
        cause
            .downcast_ref::<TurnDeliveryError>()
            .map(|failure| failure.delivery.clone())
    })
}
