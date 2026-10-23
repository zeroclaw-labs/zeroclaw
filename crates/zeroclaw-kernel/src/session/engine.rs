//! Narrow bridge from admitted session commands to an existing engine future.
//! Hosts supply current authority and execution evidence; this
//! bridge creates no capability factory or second model/tool loop.
use super::execution::{TurnExecutorPort, UserTurnCommand};
use super::query::{PayloadSelector, SessionAccess, SessionAccessPolicy, SessionQuery};
use super::*;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::{
    ContextSnapshot, ContextTransformIntent, EngineFileInput, EngineTurnFuture, EngineTurnInput,
    ModelChangeDisposition, ModelOverrideSelection, ProviderAdmissionPort, SessionExecutionPort,
};
use zeroclaw_api::session_events::*;

/// The host binds protocol identity and current config at use time. No bearer,
/// Config snapshot or historical grant is stored on the execution handle.
pub trait EngineSessionAuthority: SessionAccessPolicy + PermissionResolver {
    /// Immutable ownership resolved by the entrance's canonical session
    /// admission. It is not the administrator who happens to resume a session.
    fn session_owner(&self, principal: &PrincipalId) -> Option<String> {
        Some(principal.0.clone())
    }
    fn execution_binding(&self, input: &EngineTurnInput) -> Result<PayloadRecord, SessionError>;
    fn maintenance_binding(&self) -> Result<PayloadRecord, SessionError>;
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String>;
}

/// Application-owned family composition. Hosts supply authenticated scope and
/// current authority; kernel never constructs a physical backend.
#[async_trait::async_trait]
pub trait EngineSessionSource: Send + Sync {
    async fn stream_id(&self, family: &str) -> Result<SessionStreamId, ContractError>;
    /// Routing metadata for the trusted authorization resolver. A transport
    /// must authorize before disclosing this descriptor to a caller.
    async fn describe(&self, family: String) -> anyhow::Result<Option<FamilyDescriptor>>;
    async fn families(&self) -> anyhow::Result<Vec<String>>;
    /// Trusted host retention command. Closing retains the append-only audit
    /// trail; it never erases payloads or expires a running execution.
    async fn archive_stale(&self, _prefix: &str, _before_millis: i64) -> anyhow::Result<usize> {
        anyhow::bail!("session_retention_unsupported")
    }
    async fn open(
        &self,
        family: String,
        alias: String,
        principal: PrincipalId,
        authority: Arc<dyn EngineSessionAuthority>,
        initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>>;
    /// Host-only admission. System scope must pass a policy that explicitly
    /// authorizes this component; the default never grants execution.
    async fn open_system(
        &self,
        _family: String,
        _alias: String,
        _component: String,
        _authority: Arc<dyn EngineSessionAuthority>,
        _initial: ContextSnapshot,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        anyhow::bail!("system_session_source_unsupported")
    }
    /// Resolve an existing native family without admission, recovery or write
    /// ownership. Read-only callers cannot create a session by querying it.
    async fn query(
        &self,
        family: String,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
    ) -> anyhow::Result<Option<FamilyQuery>>;
}

#[derive(Clone, Debug)]
pub struct FamilyDescriptor {
    pub stream: SessionStreamId,
    pub owning_agent: String,
    pub owner: Option<String>,
}

pub struct FamilyQuery {
    pub stream: SessionStreamId,
    pub query: SessionQuery,
}

#[async_trait::async_trait]
impl views::SessionHistoryReadPort for EngineSession {
    async fn current(&self) -> anyhow::Result<views::SessionSummary> {
        Ok(self.query().summary(self.stream.clone()).await?)
    }

    async fn list(&self, limit: usize) -> anyhow::Result<Vec<views::SessionSummary>> {
        let current = self.query().summary(self.stream.clone()).await?;
        let current_key = current.family_key().map(str::to_owned);
        let mut summaries = vec![current];
        if let (Some(source), EventActor::Principal { principal_id }) = (&self.source, &self.actor)
        {
            for family in source.families().await? {
                if Some(&family) == current_key.as_ref() {
                    continue;
                }
                let stream = source.stream_id(&family).await?;
                match SessionAccessPolicy::authorize_actor(
                    self.authority.as_ref(),
                    &self.actor,
                    &stream,
                    SessionAccess::Read,
                ) {
                    Ok(()) => {}
                    Err(SessionError::PermissionDenied(_)) => continue,
                    Err(error) => return Err(error.into()),
                }
                if let Some(native) = source
                    .query(
                        family,
                        PrincipalId(principal_id.clone()),
                        self.authority.clone(),
                    )
                    .await?
                {
                    summaries.push(native.query.summary(native.stream).await?);
                }
            }
        }
        summaries.retain(|summary| !summary.closed);
        summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_activity));
        summaries.truncate(limit);
        for summary in &summaries {
            SessionAccessPolicy::authorize_actor(
                self.authority.as_ref(),
                &self.actor,
                &summary.head.stream_id,
                SessionAccess::Read,
            )?;
        }
        Ok(summaries)
    }

    async fn history(
        &self,
        family: &str,
        limit: usize,
    ) -> anyhow::Result<Option<views::HistoricalMessagePage>> {
        let summary = self.query().summary(self.stream.clone()).await?;
        if summary.family_key() == Some(family)
            || family == self.stream.as_str()
            || summary.family_key() == Some(format!("gw_{family}").as_str())
        {
            return Ok(Some(
                self.query()
                    .message_page(self.stream.clone(), None, limit)
                    .await?,
            ));
        }
        if let (Some(source), EventActor::Principal { principal_id }) = (&self.source, &self.actor)
        {
            let stream = source.stream_id(family).await?;
            match SessionAccessPolicy::authorize_actor(
                self.authority.as_ref(),
                &self.actor,
                &stream,
                SessionAccess::Read,
            ) {
                Ok(()) => {}
                Err(SessionError::PermissionDenied(_)) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
            if let Some(native) = source
                .query(
                    family.into(),
                    PrincipalId(principal_id.clone()),
                    self.authority.clone(),
                )
                .await?
            {
                return Ok(Some(
                    native
                        .query
                        .message_page(native.stream, None, limit)
                        .await?,
                ));
            }
        }
        Ok(None)
    }
}

#[derive(Clone)]
pub struct EngineSession {
    pub(super) owner: Arc<SessionCoordinator>,
    pub(super) stream: SessionStreamId,
    pub(super) branch: BranchId,
    pub(super) actor: EventActor,
    pub(super) authority: Arc<dyn EngineSessionAuthority>,
    source: Option<Arc<dyn EngineSessionSource>>,
}
impl EngineSession {
    pub(super) fn is_active(&self) -> bool {
        ACTIVE_ENGINE_SESSION
            .try_with(|(stream, branch)| stream == &self.stream && branch == &self.branch)
            .unwrap_or(false)
    }
    pub(super) fn active_turn(&self) -> Result<Option<TurnId>, ContractError> {
        if self.is_active() {
            zeroclaw_api::session_events::capture::current_engine_turn_id()
                .map(|id| TurnId::parse(&id))
                .transpose()
        } else {
            Ok(None)
        }
    }
    pub fn with_source(mut self, source: Arc<dyn EngineSessionSource>) -> Self {
        self.source = Some(source);
        self
    }
    pub fn new(
        owner: Arc<SessionCoordinator>,
        stream: SessionStreamId,
        branch: BranchId,
        principal: PrincipalId,
        authority: Arc<dyn EngineSessionAuthority>,
    ) -> Self {
        Self::with_actor(
            owner,
            stream,
            branch,
            EventActor::Principal {
                principal_id: principal.0,
            },
            authority,
        )
    }
    pub(super) async fn input_command(
        &self,
        input: EngineTurnInput,
    ) -> anyhow::Result<UserTurnCommand> {
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Write,
        )?;
        let turn = TurnId::parse(&input.turn_id)?;
        let ingress_key = format!("engine-input:{turn}");
        let binding = self.authority.execution_binding(&input)?;
        let parts = input.parts;
        let mut ingress = serde_json::to_value(&input.ingress)?;
        if let Some(workspace) = &input.execution_workspace {
            ingress["execution_workspace"] = serde_json::to_value(workspace)?;
        }
        let mut ingress = PayloadRecord::json("session.ingress/v1", &ingress)?;
        ingress.payload_id = PayloadId::parse(format!("ingress-{turn}"))?;
        let route = input
            .reply_route
            .map(|route| -> anyhow::Result<PayloadRecord> {
                let mut payload = PayloadRecord::json("session.route/v1", &route)?;
                payload.payload_id = PayloadId::parse(format!("reply-route-{turn}"))?;
                Ok(payload)
            })
            .transpose()?;
        let reply_route_ref = route.as_ref().map(StoredPayloadRef::from);
        let mut input_payloads = vec![ingress.clone(), binding.clone()];
        input_payloads.extend(route);
        let context = self.latest_context().await?;
        Ok(UserTurnCommand {
            stream_id: self.stream.clone(),
            branch_id: self.branch.clone(),
            turn_id: turn.clone(),
            message_id: MessageId::parse(format!("message-{turn}"))?,
            ingress_key,
            parts,
            ingress_ref: StoredPayloadRef::from(&ingress),
            reply_route_ref,
            execution_binding_ref: StoredPayloadRef::from(&binding),
            context_event_id: context.event_id,
            input_payloads,
        })
    }
    pub fn with_actor(
        owner: Arc<SessionCoordinator>,
        stream: SessionStreamId,
        branch: BranchId,
        actor: EventActor,
        authority: Arc<dyn EngineSessionAuthority>,
    ) -> Self {
        Self {
            owner,
            stream,
            branch,
            actor,
            authority,
            source: None,
        }
    }
    pub fn query(&self) -> SessionQuery {
        self.owner
            .query_actor(self.actor.clone(), self.authority.clone())
            .on_branch(self.branch.clone())
    }
    pub(super) fn context_query(&self) -> SessionQuery {
        self.owner.query_actor(
            self.actor.clone(),
            Arc::new(EngineContextReader {
                authority: self.authority.clone(),
                branch: self.branch.clone(),
            }),
        )
    }
    pub(super) async fn latest_context(&self) -> anyhow::Result<SessionEvent> {
        self.authority
            .authorize_context(&self.stream, &self.branch)?;
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let event = self
            .owner
            .blocking(move || owner.branch_context(&stream, &branch))
            .await?
            .ok_or_else(|| anyhow::Error::msg("engine committed context projection missing"))?;
        self.authority
            .authorize_context(&self.stream, &self.branch)?;
        Ok(event)
    }
}

struct EngineExecutor<'a> {
    future: Option<EngineTurnFuture<'a>>,
    output: Option<String>,
    error: Option<Arc<anyhow::Error>>,
    authority: Arc<dyn EngineSessionAuthority>,
}
#[derive(Debug)]
struct SharedEngineError(Arc<anyhow::Error>);
impl std::fmt::Display for SharedEngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}
impl std::error::Error for SharedEngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}
#[async_trait::async_trait]
impl TurnExecutorPort for EngineExecutor<'_> {
    fn diagnostic(&self, error: &anyhow::Error) -> Option<String> {
        self.authority.diagnostic(error)
    }
    async fn execute(
        &mut self,
        _: &str,
        _: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String> {
        let future = self
            .future
            .take()
            .ok_or_else(|| anyhow::Error::msg("engine future already consumed"))?;
        match future.await {
            Ok(output) => {
                self.output = Some(output.clone());
                Ok(output)
            }
            Err(error) => {
                let error = Arc::new(error);
                self.error = Some(Arc::clone(&error));
                Err(SharedEngineError(error).into())
            }
        }
    }
}
#[async_trait::async_trait]
impl SessionExecutionPort for EngineSession {
    fn history_reader(&self) -> Option<Arc<dyn views::SessionHistoryReadPort>> {
        Some(Arc::new(self.clone()))
    }
    async fn pending_inputs(
        &self,
        provider_ref: String,
        model: String,
    ) -> anyhow::Result<Vec<EngineTurnInput>> {
        self.recoverable_inputs(provider_ref, model).await
    }
    async fn reject_pending_input(&self, turn_id: &str, reason_key: &str) -> anyhow::Result<()> {
        self.cancel_requeued_input(turn_id, reason_key, None).await
    }
    async fn fork_branch(
        &self,
        branch: BranchId,
        cursor: EventCursor,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        Ok(Arc::new(self.fork(branch, cursor).await?))
    }
    async fn hide_content(&self, target: EventId, reason: &str) -> anyhow::Result<()> {
        EngineSession::hide_content(self, target, reason).await
    }
    async fn replace_content(&self, target: EventId, replacement: EventId) -> anyhow::Result<()> {
        EngineSession::replace_content(self, target, replacement).await
    }
    async fn record_file(&self, file: EngineFileInput, operation: &str) -> anyhow::Result<()> {
        self.control_file(file, operation).await
    }
    async fn request_elicitation(
        &self,
        request_id: &str,
        prompt: &str,
        schema: &serde_json::Value,
        expires_at: Timestamp,
    ) -> anyhow::Result<()> {
        self.control_elicitation_requested(request_id, prompt, schema, expires_at)
            .await
    }
    async fn resolve_elicitation(
        &self,
        request_id: &str,
        resolution: &serde_json::Value,
    ) -> anyhow::Result<()> {
        self.control_elicitation_resolved(request_id, resolution)
            .await
    }
    async fn deliver(
        &self,
        intent: zeroclaw_api::session_events::capture::DeliveryIntent,
        send: zeroclaw_api::session_events::capture::DeliveryFuture<'_>,
    ) -> anyhow::Result<serde_json::Value> {
        self.deliver_observed(intent, send)
            .await
            .map_err(zeroclaw_api::session_events::capture::capture_stopped)
    }
    async fn record_context_input(
        &self,
        input: EngineTurnInput,
        context_text: &str,
        reason_key: &str,
    ) -> anyhow::Result<MessageId> {
        self.control_context_input(input, context_text, reason_key)
            .await
    }
    async fn record_control(&self, value: serde_json::Value) -> anyhow::Result<()> {
        self.control_evidence(value).await
    }
    async fn record_approval_reply_rejected(
        &self,
        request_id: Option<String>,
        classification: String,
        evidence: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.control_approval_reply_rejected(request_id, classification, evidence)
            .await
    }
    async fn close(&self, reason_key: &str) -> anyhow::Result<()> {
        self.control_close(reason_key).await
    }
    async fn rename(&self, title: &str) -> anyhow::Result<()> {
        self.control_rename(title).await
    }
    async fn observe_metadata(
        &self,
        value: zeroclaw_api::session_events::capture::SessionMetadataObservation,
    ) -> anyhow::Result<()> {
        self.control_metadata(value).await
    }
    async fn append_assistant(&self, text: &str) -> anyhow::Result<MessageId> {
        self.control_append(text).await
    }
    async fn observe_model_change(
        &self,
        selection: ModelOverrideSelection,
        outcome: ModelChangeDisposition,
    ) -> anyhow::Result<()> {
        self.control_model_change(selection, outcome).await
    }
    async fn request_cancellation(
        &self,
        turn: Option<&str>,
        incarnation: &str,
        reason_key: &str,
    ) -> anyhow::Result<()> {
        self.control_cancellation(turn, incarnation, reason_key)
            .await
    }

    async fn restore_model_override(&self) -> anyhow::Result<Option<ModelOverrideSelection>> {
        Ok(self.query().model_selection(self.stream.clone()).await?)
    }

    async fn restore_context(&self) -> anyhow::Result<Option<ContextSnapshot>> {
        let event = self.latest_context().await?;
        self.read_context_snapshot(event).await
    }

    async fn summarize_context(
        &self,
        purpose: ProviderPurpose,
        summary: zeroclaw_api::session_events::capture::ContextSummaryExecutor<'_>,
    ) -> anyhow::Result<ContextSnapshot> {
        self.summarize_frozen_context(purpose, summary).await
    }
    async fn replace_context(
        &self,
        context: ContextSnapshot,
        reason_key: &str,
    ) -> anyhow::Result<()> {
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Write,
        )?;
        if let Some(current) = self.restore_context().await?
            && canonical_json(&current)? == canonical_json(&context)?
        {
            return Ok(());
        }
        // The active engine already holds this stream's foreground lane.
        // Use its bound transform port rather than taking that lane again.
        // A port belonging to another stream must use ordinary maintenance
        // admission, even when invoked from inside an unrelated turn.
        if self.is_active() {
            let port = zeroclaw_api::session_events::capture::current_provider_admission()
                .ok_or_else(|| anyhow::Error::msg("active engine capture missing"))?;
            let transform = port
                .begin_context_transform(ContextTransformIntent {
                    reason_key: reason_key.into(),
                    algorithm_version: "engine-context/v1".into(),
                    budget: 0,
                })
                .await?
                .ok_or_else(|| anyhow::Error::msg("active engine transform missing"))?;
            return transform.complete_snapshot(context).await;
        }
        let input = self.latest_context().await?;
        let binding = self.authority.maintenance_binding()?;
        self.owner
            .replace_context(
                super::ContextMaintenanceCommand {
                    stream: self.stream.clone(),
                    branch: self.branch.clone(),
                    input_projection: input.event_id,
                    execution_binding: binding,
                    intent: ContextTransformIntent {
                        reason_key: reason_key.into(),
                        algorithm_version: "engine-context/v1".into(),
                        budget: 0,
                    },
                },
                context,
                self.authority.clone(),
            )
            .await?;
        Ok(())
    }
    async fn execute(
        &self,
        input: EngineTurnInput,
        engine: EngineTurnFuture<'_>,
    ) -> anyhow::Result<String> {
        let command = match self.input_command(input.clone()).await {
            Ok(command) => command,
            Err(error) => {
                if matches!(
                    error.downcast_ref::<SessionError>(),
                    Some(SessionError::PermissionDenied(_))
                ) {
                    self.cancel_requeued_input(
                        &input.turn_id,
                        "recovered_input_permission_denied",
                        Some(input.clone()),
                    )
                    .await?;
                }
                return Err(error);
            }
        };
        let mut executor = EngineExecutor {
            future: Some(engine),
            output: None,
            error: None,
            authority: Arc::clone(&self.authority),
        };
        let execution = zeroclaw_api::session_events::capture::scope_session_execution(
            Arc::new(self.clone()),
            self.owner.execute_turn(
                command,
                self.actor.clone(),
                self.authority.clone(),
                self.authority.clone(),
                &mut executor,
            ),
        );
        let rejected_input = input.clone();
        let execution = ACTIVE_ENGINE_SESSION.scope(
            (self.stream.clone(), self.branch.clone()),
            zeroclaw_api::session_events::capture::scope_engine_input(input, execution),
        );
        let receipt = match &self.source {
            Some(source) => scope_engine_session_source(source.clone(), execution).await,
            None => execution.await,
        };
        if matches!(&receipt, Err(SessionError::PermissionDenied(_))) {
            self.cancel_requeued_input(
                &rejected_input.turn_id,
                "recovered_input_permission_denied",
                Some(rejected_input.clone()),
            )
            .await?;
        }
        let receipt = receipt.map_err(|error| {
            zeroclaw_api::session_events::capture::capture_stopped(anyhow::Error::new(error))
        })?;
        let terminal_id = receipt
            .event_ids
            .last()
            .cloned()
            .ok_or_else(|| anyhow::Error::msg("turn outcome receipt empty"))?;
        let owner = self.owner.clone();
        let unknown =
            self.owner
                .blocking(move || {
                    let terminal = owner.store.read_event(&terminal_id)?.ok_or_else(|| {
                        SessionError::StoreUnhealthy("turn outcome missing".into())
                    })?;
                    Ok(terminal.body.kind() == EventKind::TurnOutcomeUnknown)
                })
                .await?;
        if unknown {
            let error = executor
                .error
                .take()
                .map(|error| anyhow::Error::new(SharedEngineError(error)))
                .unwrap_or_else(|| anyhow::Error::msg("recorded_turn_outcome_unknown"));
            return Err(zeroclaw_api::session_events::capture::capture_stopped(
                error,
            ));
        }
        if let Some(error) = executor.error {
            return Err(SharedEngineError(error).into());
        }
        if let Some(output) = executor.output {
            return Ok(output);
        }
        // Retry reads the committed result through the authorized query. It
        // does not poll the supplied engine future or invoke a capability.
        let query = self
            .owner
            .query_actor(self.actor.clone(), self.authority.clone())
            .on_branch(self.branch.clone());
        let page = query
            .read_page(
                self.stream.clone(),
                EventCursor {
                    stream_id: self.stream.clone(),
                    sequence: receipt.first.sequence - 1,
                },
                Some(receipt.last),
                256,
                4 * 1024 * 1024,
            )
            .await?;
        let terminal = page
            .events
            .last()
            .ok_or_else(|| anyhow::Error::msg("turn terminal missing"))?;
        let PayloadRead::Present(completion) = query
            .read_payload(
                self.stream.clone(),
                PayloadSelector {
                    event_sequence: terminal.sequence,
                    reference_path: vec![0],
                },
                4 * 1024 * 1024,
            )
            .await?
        else {
            anyhow::bail!("turn completion unavailable");
        };
        let TurnCompletion::Success { result_event_ids } =
            serde_json::from_slice(&completion.body)?
        else {
            anyhow::bail!("recorded turn has no successful result");
        };
        let id = result_event_ids
            .last()
            .ok_or_else(|| anyhow::Error::msg("turn result missing"))?
            .clone();
        let owner = self.owner.clone();
        let content_event =
            self.owner
                .blocking(move || {
                    owner.store.read_event(&id)?.ok_or_else(|| {
                        SessionError::StaleTarget("turn result event missing".into())
                    })
                })
                .await?;
        let PayloadRead::Present(content) = query
            .read_payload(
                self.stream.clone(),
                PayloadSelector {
                    event_sequence: content_event.sequence,
                    reference_path: vec![0],
                },
                4 * 1024 * 1024,
            )
            .await?
        else {
            anyhow::bail!("recorded assistant content unavailable");
        };
        let content: ContentPayload = serde_json::from_slice(&content.body)?;
        let mut text = String::new();
        for (index, part) in content.parts.iter().enumerate() {
            if matches!(part, RecordedContentPart::Text { .. }) {
                let PayloadRead::Present(leaf) = query
                    .read_payload(
                        self.stream.clone(),
                        PayloadSelector {
                            event_sequence: content_event.sequence,
                            reference_path: vec![0, index],
                        },
                        4 * 1024 * 1024,
                    )
                    .await?
                else {
                    anyhow::bail!("recorded assistant text unavailable");
                };
                text.push_str(std::str::from_utf8(&leaf.body)?);
            }
        }
        Ok(text)
    }
}

// A host's injected source is an implementation handle. It creates no history
// or policy state. Spawn sites must transfer it explicitly into new tasks.
tokio::task_local! {
    static ENGINE_SESSION_SOURCE: Arc<dyn EngineSessionSource>;
    // A task-bound execution coordinate, derived from the admitted command.
    // It never owns history, policy or scheduling state.
    static ACTIVE_ENGINE_SESSION: (SessionStreamId, BranchId);
}
pub async fn scope_engine_session_source<F: std::future::Future>(
    source: Arc<dyn EngineSessionSource>,
    future: F,
) -> F::Output {
    ENGINE_SESSION_SOURCE.scope(source, future).await
}
pub fn current_engine_session_source() -> Option<Arc<dyn EngineSessionSource>> {
    ENGINE_SESSION_SOURCE.try_with(Arc::clone).ok()
}

pub(super) fn decode_context_item(
    row: serde_json::Value,
) -> anyhow::Result<Option<zeroclaw_api::model_provider::ConversationMessage>> {
    Ok(zeroclaw_api::session_events::context_read::decode_context_item(row)?)
}
pub(super) fn decode_context_messages(
    bytes: &[u8],
) -> anyhow::Result<Vec<zeroclaw_api::model_provider::ConversationMessage>> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    Ok(rows
        .into_iter()
        .map(decode_context_item)
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect())
}

// Engine restoration is an internal context use, never a disclosure grant for
// the public query facade. Each read resolves the current execution authority.
struct EngineContextReader {
    authority: Arc<dyn EngineSessionAuthority>,
    branch: BranchId,
}
impl SessionAccessPolicy for EngineContextReader {
    fn authorize(
        &self,
        principal: &PrincipalId,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        SessionAccessPolicy::authorize(self.authority.as_ref(), principal, stream, access)
    }
    fn authorize_actor(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        self.authority.authorize_actor(actor, stream, access)
    }
    fn authorize_reasoning(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
    ) -> Result<(), SessionError> {
        self.authority
            .authorize_actor(actor, stream, SessionAccess::Read)?;
        self.authority.authorize_context(stream, &self.branch)
    }
}
