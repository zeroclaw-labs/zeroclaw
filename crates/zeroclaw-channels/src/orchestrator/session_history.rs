//! Native history bridge for an already admitted channel lane. Platform peer
//! identities remain ingress evidence and never become RPC principals.
use super::*;
use serde_json::Value;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::{capture::*, *};
use zeroclaw_kernel::session::engine::EngineSessionAuthority;
use zeroclaw_kernel::session::query::{SessionAccess, SessionAccessPolicy};
use zeroclaw_kernel::session::{EffectIntent, PermissionResolver, SessionError};

#[derive(serde::Serialize, serde::Deserialize)]
struct ChannelReplyRoute {
    version: u32,
    /// With a stable platform ID, ingress owns that ID and this retains the
    /// original display identity used for channel history and peer matching.
    display_sender: Option<String>,
    empty_alias: bool,
    reply_target: String,
    timestamp: u64,
    thread_ts: Option<String>,
    interruption_scope_id: Option<String>,
    subject: Option<String>,
    references: Vec<String>,
    shared_history: bool,
    explicitly_addressed: bool,
    voice_origin: bool,
}

fn message_identity(msg: &ChannelMessage) -> anyhow::Result<Digest> {
    let identity = canonical_json(&serde_json::json!({
        "source":"channel_ingress","channel":msg.channel,"alias":msg.channel_alias,
        "sender":msg.sender,"message_id":msg.id,
    }))?;
    Ok(Digest::from_bytes(&identity))
}

struct ChannelAuthority {
    ctx: Arc<ChannelRuntimeContext>,
    message: ChannelMessage,
    stream: SessionStreamId,
    agent: String,
    child: bool,
}
impl ChannelAuthority {
    fn current(&self) -> Result<(), SessionError> {
        let config = self.ctx.live_config.read();
        let channel = channel_key_for_message(&self.message);
        if config.agent_for_channel(&channel) != Some(self.ctx.agent_alias.as_str())
            || config.agent(&self.agent).is_none_or(|agent| !agent.enabled)
        {
            return Err(SessionError::PermissionDenied(
                "channel agent binding revoked".into(),
            ));
        }
        let alias = self
            .message
            .channel_alias
            .as_deref()
            .unwrap_or(&self.message.channel);
        if self.child
            && self.ctx.agent_alias.as_str() != self.agent
            && config
                .delegate_target_mode(self.ctx.agent_alias.as_str(), &self.agent)
                .is_none()
        {
            return Err(SessionError::PermissionDenied(
                "channel child target revoked".into(),
            ));
        }
        let peers = if matches!(self.message.channel.as_str(), "lark" | "feishu") {
            config.channel_external_peers_for(&["lark", "feishu"], alias)
        } else {
            config.channel_external_peers(&self.message.channel, alias)
        };
        if !matches!(self.message.channel.as_str(), "cli" | "filesystem") {
            let ids = [
                self.message.sender.as_str(),
                self.message.platform_sender_id.as_deref().unwrap_or(""),
            ];
            let matcher = |left: &str, right: &str| match self.message.channel.as_str() {
                "telegram" => {
                    left.trim().trim_start_matches('@') == right.trim().trim_start_matches('@')
                }
                "irc" | "matrix" => left.eq_ignore_ascii_case(right),
                _ => left == right,
            };
            if !zeroclaw_config::schema::peer_policy_admits(&peers, &ids, matcher) {
                return Err(SessionError::PermissionDenied(
                    "channel sender policy revoked".into(),
                ));
            }
        }
        Ok(())
    }
}
impl SessionAccessPolicy for ChannelAuthority {
    fn authorize(
        &self,
        _: &PrincipalId,
        _: &SessionStreamId,
        _: SessionAccess,
    ) -> Result<(), SessionError> {
        Err(SessionError::PermissionDenied(
            "channel host is not a principal".into(),
        ))
    }
    fn authorize_actor(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        if stream != &self.stream
            || !matches!(actor,EventActor::System {component} if component=="channel_ingress")
            || matches!(access, SessionAccess::Erase)
        {
            return Err(SessionError::PermissionDenied(
                "channel history scope mismatch".into(),
            ));
        }
        self.current()
    }
}
impl PermissionResolver for ChannelAuthority {
    fn authorize_context(
        &self,
        stream: &SessionStreamId,
        _: &BranchId,
    ) -> Result<(), SessionError> {
        if stream != &self.stream {
            return Err(SessionError::PermissionDenied(
                "channel context scope mismatch".into(),
            ));
        }
        self.current()
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError> {
        PermissionResolver::authorize_context(
            self,
            &intent.stream_id,
            intent
                .event
                .branch_id
                .as_ref()
                .ok_or(SessionError::InvocationMismatch)?,
        )
    }
    fn authorize_tool(
        &self,
        intent: &EffectIntent,
        name: &str,
        _: &Value,
    ) -> Result<(), SessionError> {
        PermissionResolver::authorize(self, intent)?;
        let config = self.ctx.live_config.read();
        let profile = config
            .risk_profile_for_agent(&self.agent)
            .ok_or_else(|| SessionError::PermissionDenied("channel risk profile missing".into()))?;
        if profile
            .effective_allowed_tools()
            .is_some_and(|tools| !tools.iter().any(|tool| tool == name))
        {
            return Err(SessionError::PermissionDenied(
                "current channel tool policy denies action".into(),
            ));
        }
        Ok(())
    }
    fn child_authority(
        &self,
        target: &str,
        stream: &SessionStreamId,
    ) -> Result<Arc<dyn EngineSessionAuthority>, SessionError> {
        self.current()?;
        let config = self.ctx.live_config.read();
        if target != self.agent && config.delegate_target_mode(&self.agent, target).is_none() {
            return Err(SessionError::PermissionDenied(
                "channel child target not reachable".into(),
            ));
        }
        Ok(Arc::new(Self {
            ctx: self.ctx.clone(),
            message: self.message.clone(),
            stream: stream.clone(),
            agent: target.into(),
            child: true,
        }))
    }
}
impl EngineSessionAuthority for ChannelAuthority {
    fn execution_binding(&self, input: &EngineTurnInput) -> Result<PayloadRecord, SessionError> {
        if input.agent_alias.as_deref() != Some(self.agent.as_str())
            || (self.child && input.ingress.origin != zeroclaw_api::ingress::TurnOrigin::SubTurn)
        {
            return Err(SessionError::PermissionDenied(
                "channel engine binding changed".into(),
            ));
        }
        self.current()?;
        Ok(PayloadRecord::json("session.execution-binding/v1",&serde_json::json!({
            "config_revision":self.ctx.live_config.revision(),"requested_provider_ref":input.provider_ref,
            "requested_model":input.model,"purpose":"channel_turn",
        })).map_err(AppendError::from)?)
    }
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError> {
        self.current()?;
        Ok(PayloadRecord::json("session.execution-binding/v1",&serde_json::json!({
            "config_revision":self.ctx.live_config.revision(),"purpose":"channel_context_maintenance",
        })).map_err(AppendError::from)?)
    }
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        Some(zeroclaw_runtime::agent::Agent::session_execution_diagnostic(error))
    }
}

pub(super) async fn open(
    ctx: Arc<ChannelRuntimeContext>,
    msg: &ChannelMessage,
    family: &str,
) -> anyhow::Result<Option<Arc<dyn SessionExecutionPort>>> {
    let Some(source) = &ctx.native_sessions else {
        return Ok(None);
    };
    let authority = Arc::new(ChannelAuthority {
        ctx: ctx.clone(),
        message: msg.clone(),
        stream: source.stream_id(family).await?,
        agent: ctx.agent_alias.to_string(),
        child: false,
    });
    let initial = ContextSnapshot {
        messages: vec![],
        history_has_trim_breadcrumb: Some(false),
    };
    Ok(Some(
        source
            .open_system(
                family.into(),
                ctx.agent_alias.to_string(),
                "channel_ingress".into(),
                authority,
                initial,
            )
            .await?,
    ))
}

/// The ingress registration creates attachment identity until the shared file
/// owner supplies FileId. It uses the immutable transport message/part position,
/// never a hash of private bytes or a mutable filename.
pub(super) fn input(
    ctx: &ChannelRuntimeContext,
    msg: &ChannelMessage,
    user_text: String,
    provider_ref: String,
    model: String,
) -> anyhow::Result<EngineTurnInput> {
    let identity = message_identity(msg)?;
    let mut files = Vec::new();
    for (index, file) in msg.attachments.iter().enumerate() {
        let file_id = format!("channel-file-{}-{index}", identity.as_str());
        let representation = PayloadRecord::new(
            PayloadId::parse(format!("representation-{file_id}"))?,
            "session.bytes/v1".into(),
            file.data.clone(),
        )?;
        files.push(EngineFileInput {
            file_id,
            registration:serde_json::json!({"source":"channel_ingress_registration","filename":file.file_name,
                "mime_type":file.mime_type,"rendered_marker":file.marker,"byte_len":file.data.len()}),
            representation,
            policy_evidence:serde_json::json!({"source":"admitted_channel_lane","channel":msg.channel,"alias":msg.channel_alias}),
        });
    }
    let reply_route = ChannelReplyRoute {
        version: 1,
        display_sender: msg
            .platform_sender_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .map(|_| msg.sender.clone()),
        empty_alias: msg.channel_alias.as_deref() == Some(""),
        reply_target: msg.reply_target.clone(),
        timestamp: msg.timestamp,
        thread_ts: msg.thread_ts.clone(),
        interruption_scope_id: msg.interruption_scope_id.clone(),
        subject: msg.subject.clone(),
        references: msg.references.clone(),
        shared_history: msg.conversation_scope
            == zeroclaw_api::channel::ChannelConversationScope::ReplyTarget,
        explicitly_addressed: msg.explicitly_addressed,
        voice_origin: msg.voice_origin,
    };
    Ok(EngineTurnInput {
        execution_workspace: None,
        reply_route: Some(serde_json::to_value(reply_route)?),
        turn_id: format!("channel-turn-{}", identity.as_str()),
        agent_alias: Some(ctx.agent_alias.to_string()),
        ingress: channel_ingress_context(msg),
        parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
            user_text, files,
        ),
        provider_ref,
        model,
    })
}

pub(super) fn recover_message(
    ctx: &ChannelRuntimeContext,
    input: &EngineTurnInput,
    family: &str,
) -> anyhow::Result<ChannelMessage> {
    anyhow::ensure!(
        input.agent_alias.as_deref() == Some(ctx.agent_alias.as_str()),
        "channel_recovery_agent_mismatch"
    );
    let route: ChannelReplyRoute =
        serde_json::from_value(input.reply_route.clone().ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_attrs(serde_json::json!({
                        "error_key": "channel_recovery_route_missing",
                    })),
                "Channel recovery route is missing"
            );
            anyhow::Error::msg("channel_recovery_route_missing")
        })?)?;
    anyhow::ensure!(
        route.version == 1,
        "channel_recovery_route_version_unsupported"
    );
    let zeroclaw_api::ingress::Transport::Channel { kind, alias } = &input.ingress.transport else {
        anyhow::bail!("channel_recovery_transport_mismatch");
    };
    let sender = input.ingress.sender.clone().unwrap_or_default();
    let mut message = ChannelMessage {
        id: input.ingress.message_id.clone().unwrap_or_default(),
        platform_sender_id: route.display_sender.as_ref().map(|_| sender.clone()),
        sender: route.display_sender.unwrap_or(sender),
        reply_target: route.reply_target,
        channel: kind.clone(),
        channel_alias: (route.empty_alias || !alias.is_empty()).then(|| alias.clone()),
        timestamp: route.timestamp,
        thread_ts: route.thread_ts,
        interruption_scope_id: route.interruption_scope_id,
        subject: route.subject,
        references: route.references,
        conversation_scope: if route.shared_history {
            zeroclaw_api::channel::ChannelConversationScope::ReplyTarget
        } else {
            zeroclaw_api::channel::ChannelConversationScope::Sender
        },
        explicitly_addressed: route.explicitly_addressed,
        voice_origin: route.voice_origin,
        ..Default::default()
    };
    anyhow::ensure!(
        channel_ingress_context(&message) == input.ingress,
        "channel_recovery_identity_mismatch"
    );
    anyhow::ensure!(
        runtime_conversation_history_key(ctx, &message) == family,
        "channel_recovery_family_mismatch"
    );
    anyhow::ensure!(
        input.turn_id == format!("channel-turn-{}", message_identity(&message)?.as_str()),
        "channel_recovery_turn_mismatch"
    );

    #[derive(serde::Deserialize)]
    struct FileRegistration {
        source: String,
        filename: String,
        mime_type: Option<String>,
        rendered_marker: Option<zeroclaw_api::media::RenderedMarker>,
        byte_len: usize,
    }
    for part in &input.parts {
        match part {
            EngineInputPart::Text(text) => message.content.push_str(text),
            EngineInputPart::File(file) => {
                let registration: FileRegistration =
                    serde_json::from_value(file.registration.clone())?;
                anyhow::ensure!(
                    registration.source == "channel_ingress_registration"
                        && registration.byte_len == file.representation.body.len(),
                    "channel_recovery_file_metadata_mismatch"
                );
                message
                    .attachments
                    .push(zeroclaw_api::media::MediaAttachment {
                        file_name: registration.filename,
                        data: file.representation.body.clone(),
                        mime_type: registration.mime_type,
                        marker: registration.rendered_marker,
                    });
            }
            EngineInputPart::Opaque { .. } => {}
        }
    }
    Ok(message)
}

fn classify_delivery_error(error: anyhow::Error) -> anyhow::Error {
    if error
        .chain()
        .any(|cause| cause.is::<zeroclaw_api::channel::FinalizeUndelivered>())
    {
        delivery_definitely_failed(error)
    } else {
        error
    }
}

pub(super) async fn deliver<F: std::future::Future<Output = anyhow::Result<()>> + Send>(
    native: Option<&Arc<dyn SessionExecutionPort>>,
    turn: &str,
    channel: &str,
    target: &str,
    content: &str,
    send: F,
) -> anyhow::Result<()> {
    let Some(port) = native else {
        return send.await;
    };
    port.deliver(
        DeliveryIntent {
            turn_id: Some(turn.into()),
            transport: channel.into(),
            target: target.into(),
            content: content.into(),
            completion_boundary: "channel_adapter_acceptance".into(),
        },
        Box::pin(async {
            send.await.map_err(classify_delivery_error)?;
            Ok(serde_json::json!({"boundary":"channel_adapter_acceptance"}))
        }),
    )
    .await?;
    Ok(())
}

/// Capture host presentation effects at the adapter call boundary. Spawned
/// draft workers explicitly transfer this immutable execution coordinate.
#[derive(Clone)]
pub(super) struct DeliveryScope {
    port: Arc<dyn SessionExecutionPort>,
    turn: Option<String>,
}
tokio::task_local! {
    static DELIVERY_SCOPE: DeliveryScope;
}
impl DeliveryScope {
    pub(super) fn current() -> Option<Self> {
        if let Ok(scope) = DELIVERY_SCOPE.try_with(Clone::clone) {
            return Some(scope);
        }
        Some(Self {
            port: current_session_execution()?,
            turn: zeroclaw_api::session_events::capture::current_engine_turn_id(),
        })
    }
    pub(super) async fn run<F: std::future::Future>(scope: Option<Self>, future: F) -> F::Output {
        match scope {
            Some(scope) => DELIVERY_SCOPE.scope(scope, future).await,
            None => future.await,
        }
    }
    pub(super) async fn observe<T: serde::Serialize + Send>(
        scope: Option<&Self>,
        operation: &str,
        channel: &str,
        target: &str,
        content: &str,
        effect: impl std::future::Future<Output = anyhow::Result<T>> + Send,
    ) -> anyhow::Result<T> {
        let Some(scope) = scope else {
            return effect.await;
        };
        let mut output = None;
        scope.port.deliver(DeliveryIntent {
            turn_id: scope.turn.clone(), transport: channel.into(), target: target.into(),
            content: content.into(), completion_boundary: format!("{operation}:channel_adapter_acceptance"),
        }, Box::pin(async {
            let value = effect.await.map_err(classify_delivery_error)?;
            let receipt = serde_json::json!({"operation": operation, "adapter_result": serde_json::to_value(&value)?});
            output = Some(value);
            Ok(receipt)
        })).await?;
        output.ok_or_else(|| anyhow::Error::msg("channel_delivery_result_missing"))
    }
}

pub(super) async fn observe_delivery<T: serde::Serialize + Send>(
    operation: &str,
    channel: &str,
    target: &str,
    content: &str,
    effect: impl std::future::Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    DeliveryScope::observe(
        DeliveryScope::current().as_ref(),
        operation,
        channel,
        target,
        content,
        effect,
    )
    .await
}

pub(super) async fn send(channel: &dyn Channel, message: &SendMessage) -> anyhow::Result<()> {
    observe_delivery(
        "message_send",
        channel.name(),
        &message.recipient,
        &message.content,
        channel.send(message),
    )
    .await
}

pub(super) async fn cancel_draft(
    channel: &dyn Channel,
    target: &str,
    draft: &str,
) -> anyhow::Result<()> {
    observe_delivery(
        "draft_cancel",
        channel.name(),
        target,
        draft,
        channel.cancel_draft(target, draft),
    )
    .await
}
