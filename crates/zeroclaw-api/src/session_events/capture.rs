//! Narrow in-process capture ports. Adapters cannot choose a stream, actor or
//! execution grant. Scope is injected by the admitted execution owner.
use super::*;
use crate::model_provider::ChatMessage;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Durable capture failed or an effect became Unknown. Reliability layers must
/// stop all automatic retries/fallback until the session owner resolves it.
#[derive(Debug, thiserror::Error)]
#[error("session_capture_stopped")]
pub struct SessionCaptureStopped;

/// Preserve the stop classification across std Error and boxed stream bridges.
/// An anyhow context alone supports anyhow downcasts, but its marker is hidden
/// from a subsequent std Error source traversal.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
struct CaptureStopCause {
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

pub fn capture_stopped(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(CaptureStopCause {
        source: error.reallocate_into_boxed_dyn_error_without_backtrace(),
    })
    .context(SessionCaptureStopped)
}

pub fn is_capture_stopped(error: &anyhow::Error) -> bool {
    error.is::<SessionCaptureStopped>()
        || error
            .chain()
            .any(|cause| cause.is::<CaptureStopCause>() || cause.is::<SessionCaptureStopped>())
}

#[derive(Debug)]
struct SharedCaptureFailure(Arc<anyhow::Error>);

impl std::fmt::Display for SharedCaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for SharedCaptureFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref().as_ref())
    }
}

/// A transport callback and its original waiter both observe the same capture
/// failure. Share its source chain rather than flattening it into an error string.
pub fn share_capture_failure(error: anyhow::Error) -> (anyhow::Error, anyhow::Error) {
    let source = Arc::new(capture_stopped(error));
    (
        anyhow::Error::new(SharedCaptureFailure(Arc::clone(&source))),
        anyhow::Error::new(SharedCaptureFailure(source)),
    )
}

/// Marker an adapter may attach when it can prove that delivery did not pass
/// the declared completion boundary. Bare transport errors remain ambiguous:
/// the remote side may have accepted the message before the connection failed.
#[derive(Debug, thiserror::Error)]
#[error("delivery failed before completion boundary")]
pub struct DeliveryDefiniteFailure;

pub fn delivery_definitely_failed(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(DeliveryDefiniteFailure).context(error)
}

pub fn is_delivery_definitely_failed(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<DeliveryDefiniteFailure>())
}

#[derive(Clone, Debug)]
pub struct ProviderDispatchIntent {
    /// Frozen requested route and previous result from the same logical call.
    pub requested_route: Option<Value>,
    pub previous_attempt_event_id: Option<EventId>,
    pub provider_ref: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Value>,
    pub temperature: Option<f64>,
    /// Immutable fact from this dispatch request, not a live config snapshot.
    pub thinking: Option<crate::model_provider::NativeThinkingParams>,
    pub tool_round_policy: crate::model_provider::ToolRoundPolicy,
}

/// Evidence created by the runtime's actual per-call route decision. Model
/// override identity and summary purpose are resolved from canonical events.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteSource {
    Configured,
    Routing,
    Vision,
    BeforeCallHook,
}

tokio::task_local! {
    static PROVIDER_ROUTE_SOURCE: ProviderRouteSource;
}

pub async fn scope_provider_route_source<F: Future>(
    source: ProviderRouteSource,
    future: F,
) -> F::Output {
    PROVIDER_ROUTE_SOURCE.scope(source, future).await
}

pub fn current_provider_route_source() -> Option<ProviderRouteSource> {
    PROVIDER_ROUTE_SOURCE.try_with(|source| *source).ok()
}

#[async_trait::async_trait]
pub trait ProviderAdmissionPort: Send + Sync {
    async fn admit(
        &self,
        intent: ProviderDispatchIntent,
    ) -> anyhow::Result<Arc<dyn ProviderObservationPort>>;

    async fn begin_child(
        &self,
        _intent: ChildDispatchIntent,
    ) -> anyhow::Result<Arc<dyn ChildExecutionPort>> {
        anyhow::bail!("session_child_capture_unsupported")
    }

    /// Record an authorized task result when it is actually delivered to the
    /// current turn. TaskRegistry continues to own task state and visibility.
    async fn consume_child_result(&self, _task_id: &str, _value: &Value) -> anyhow::Result<()> {
        anyhow::bail!("session_child_consumption_unsupported")
    }

    /// The host records the outcome of a control action before dispatching
    /// under the new selection. Prepared attempts remain immutable.
    async fn observe_model_change(
        &self,
        _selection: ModelOverrideSelection,
        _outcome: ModelChangeDisposition,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_model_change_capture_unsupported")
    }

    /// Local context mutation belongs to the same admitted session. Simple
    /// fixture/embedding ports may omit maintenance capture; the native
    /// coordinator implements it and propagates all persistence failures.
    async fn begin_context_transform(
        &self,
        _intent: ContextTransformIntent,
    ) -> anyhow::Result<Option<Arc<dyn ContextTransformPort>>> {
        Ok(None)
    }
}

#[derive(Clone, Debug)]
pub struct ChildDispatchIntent {
    /// The real host execution relationship, never model-authored metadata.
    pub relation: String,
    pub target_agent: String,
    pub prompt: String,
    pub execution_mode: ChildExecutionMode,
    /// Correlation only. TaskRegistry remains the task/liveness authority.
    pub task_id: Option<String>,
}

#[async_trait::async_trait]
pub trait ChildExecutionPort: Send + Sync {
    /// Own the full child turn and settle the parent from its committed
    /// terminal cursor. Hosts transfer this bound port explicitly into spawn.
    async fn execute(&self, engine: EngineTurnFuture<'_>) -> anyhow::Result<String>;
    async fn request_cancellation(&self, _reason_key: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_child_cancellation_unsupported")
    }
    /// The host calls this only after dropping and joining its execution
    /// future. It records uncertainty; it never asserts remote cancellation.
    async fn interrupted(&self, _reason_key: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_child_interruption_capture_unsupported")
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ModelOverrideSelection {
    /// Resolve the current configured base route and keep automatic routing.
    Inherit { thinking: Option<String> },
    /// An explicit base route, even when effort routing remains automatic.
    Set {
        provider_ref: String,
        model: String,
        thinking: Option<String>,
        #[serde(default)]
        route_mode: ModelRouteMode,
    },
    /// Legacy spelling of `Inherit { thinking: None }`.
    Clear,
}

/// The selected route's operator control mode, independent of who later
/// switches its model. Legacy persisted selections kept the route pinned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouteMode {
    #[default]
    Manual,
    Automatic,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelChangeDisposition {
    Applied,
    AppliedForTurn,
    Rejected {
        reason_key: String,
        diagnostic: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct ContextTransformIntent {
    pub reason_key: String,
    pub algorithm_version: String,
    pub budget: u64,
}

/// A historical engine projection, including explicit breadcrumb provenance.
/// This is context output, never a snapshot of live policy or configuration.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ContextSnapshot {
    pub messages: Vec<crate::model_provider::ConversationMessage>,
    pub history_has_trim_breadcrumb: Option<bool>,
}

pub type ContextSummaryFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;

/// Invoked once under the foreground lane with the kernel-assembled summary
/// conversation. The executor converts messages and calls the model; context
/// retention, prompt policy and summary validation remain kernel-owned.
pub type ContextSummaryExecutor<'a> = Box<
    dyn FnOnce(Vec<crate::model_provider::ConversationMessage>) -> ContextSummaryFuture<'a>
        + Send
        + 'a,
>;

/// Host-owned metadata observed during execution. This cannot change session
/// identity, ownership or permissions.
#[derive(Clone, Debug)]
pub enum SessionMetadataObservation {
    ContextUsage {
        input_tokens: Option<u64>,
    },
    Plan {
        entries: Vec<crate::plan::PlanEntry>,
    },
}

impl SessionMetadataObservation {
    pub fn from_turn_event(event: &crate::agent::TurnEvent) -> Option<Self> {
        match event {
            crate::agent::TurnEvent::Usage {
                input_tokens,
                accepted: true,
                ..
            } => Some(Self::ContextUsage {
                input_tokens: *input_tokens,
            }),
            crate::agent::TurnEvent::Plan { entries } => Some(Self::Plan {
                entries: entries.clone(),
            }),
            _ => None,
        }
    }
}

impl ContextSnapshot {
    /// Compatibility view for executors whose existing input is flat chat.
    /// The stored context keeps the complete native conversation separately
    /// from this disposable projection.
    pub fn chat_projection(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .filter_map(|message| match message {
                crate::model_provider::ConversationMessage::Chat(chat) => Some(chat.clone()),
                _ => None,
            })
            .collect()
    }
}

#[async_trait::async_trait]
pub trait ContextTransformPort: Send + Sync {
    async fn complete(&self, messages: Vec<ChatMessage>) -> anyhow::Result<()>;
    async fn complete_snapshot(&self, context: ContextSnapshot) -> anyhow::Result<()> {
        self.complete(context.chat_projection()).await
    }
    async fn complete_summary(
        &self,
        _context: ContextSnapshot,
        _summary: String,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_summary_installation_unsupported")
    }
    async fn failed(&self, diagnostic: String) -> anyhow::Result<()>;
    async fn cancelled(&self, reason_key: String) -> anyhow::Result<()>;
    fn provider_capture(
        &self,
        purpose: ProviderPurpose,
    ) -> anyhow::Result<Arc<dyn ProviderAdmissionPort>>;
}

#[async_trait::async_trait]
pub trait ProviderObservationPort: Send + Sync {
    fn started_event_id(&self) -> EventId;
    /// Frozen canonical structures and leaves. Implementations validate that
    /// the data is an observation of the already-bound provider invocation.
    async fn observe(
        &self,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt>;
}

tokio::task_local! {
    static ADMISSION: Arc<dyn ProviderAdmissionPort>;
}

pub async fn scope_provider_capture<F: Future>(
    port: Arc<dyn ProviderAdmissionPort>,
    future: F,
) -> F::Output {
    ADMISSION.scope(port, future).await
}

pub fn current_provider_admission() -> Option<Arc<dyn ProviderAdmissionPort>> {
    ADMISSION.try_with(Arc::clone).ok()
}

/// The provider call identity and the action after schema recovery and host
/// hooks/defaults are distinct observations. Neither is an authorization grant.
#[derive(Clone, Debug)]
pub struct ToolPreparationIntent {
    pub proposed_name: String,
    pub provider_call_id: Option<String>,
    pub name: String,
    pub arguments: Value,
    pub approval_requirement: String,
    pub trusted_route: Value,
}

#[async_trait::async_trait]
pub trait ToolAdmissionPort: Send + Sync {
    async fn prepare(
        &self,
        intent: ToolPreparationIntent,
    ) -> anyhow::Result<Arc<dyn PreparedToolPort>>;
}

/// Deadline supplied by the approval owner, never inferred from preparation
/// time. `None` means no absolute deadline is known here; the response owner's
/// existing timeout, current authority and one-use consumption still apply.
#[derive(Clone, Debug, Default)]
pub struct ApprovalRequestIntent {
    pub expires_at: Option<Timestamp>,
}

#[async_trait::async_trait]
pub trait PreparedToolPort: Send + Sync {
    /// The host excludes its internal one-use approval grant from the semantic
    /// action. All hook/default mutations must match the committed action.
    async fn verify_action(&self, name: &str, arguments: &Value) -> anyhow::Result<()>;
    async fn request_approval(&self, intent: ApprovalRequestIntent) -> anyhow::Result<()>;
    /// Observe the authenticated reply through the original pending
    /// registration, before waking its waiter. This observation grants no
    /// permission; resolution links it from committed history.
    async fn record_approval_reply(
        &self,
        _actor: EventActor,
        _response: crate::channel::ChannelApprovalResponse,
        _evidence: Value,
    ) -> anyhow::Result<EventId> {
        anyhow::bail!("prepared_approval_reply_capture_unsupported")
    }
    /// Source/evidence must describe the actual reply, including absence of an
    /// operator reply. Persist before applying an Always decision in memory.
    async fn resolve_approval(
        &self,
        response: crate::channel::ChannelApprovalResponse,
        reason: &str,
        reply_event_id: Option<EventId>,
        evidence: Value,
    ) -> anyhow::Result<()>;
    /// Record one refused reply against this prepared approval without
    /// resolving or consuming it. Each call represents a distinct observation.
    async fn record_approval_reply_rejected(
        &self,
        _classification: String,
        _evidence: Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("prepared_approval_rejection_capture_unsupported")
    }
    async fn invalidate(&self, reason: &str) -> anyhow::Result<()>;
    async fn start(&self) -> anyhow::Result<Arc<dyn ToolObservationPort>>;
}

#[async_trait::async_trait]
pub trait ToolObservationPort: Send + Sync {
    fn started_event_id(&self) -> EventId;
    /// Resolve terminal state from canonical history; no local outcome cache.
    async fn has_outcome(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
    /// The process owner supplies real stop/reap evidence. Future drop alone
    /// must never call this boundary.
    async fn cancel_verified(
        &self,
        _reason_key: &str,
        _evidence: Value,
    ) -> anyhow::Result<AppendReceipt> {
        anyhow::bail!("tool_verified_cancellation_unsupported")
    }
    async fn observe(
        &self,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt>;
}

tokio::task_local! {
    static TOOL_ADMISSION: Arc<dyn ToolAdmissionPort>;
    static TOOL_PREPARATION: Arc<dyn PreparedToolPort>;
    // Frozen prepared handles for this execution batch, not a persistent
    // registry. Each call gets the handle minted for its unique host call id.
    static TOOL_BATCH: Vec<(String, Arc<dyn PreparedToolPort>)>;
    static TOOL_OBSERVATION: Arc<dyn ToolObservationPort>;
}
pub async fn scope_tool_capture<F: Future>(
    port: Arc<dyn ToolAdmissionPort>,
    future: F,
) -> F::Output {
    TOOL_ADMISSION.scope(port, future).await
}
pub fn current_tool_admission() -> Option<Arc<dyn ToolAdmissionPort>> {
    TOOL_ADMISSION.try_with(Arc::clone).ok()
}
pub async fn scope_tool_preparation<F: Future>(
    port: Arc<dyn PreparedToolPort>,
    future: F,
) -> F::Output {
    TOOL_PREPARATION.scope(port, future).await
}
pub fn current_tool_preparation() -> Option<Arc<dyn PreparedToolPort>> {
    TOOL_PREPARATION.try_with(Arc::clone).ok()
}
pub async fn scope_tool_batch<F: Future>(
    ports: Vec<(String, Arc<dyn PreparedToolPort>)>,
    future: F,
) -> F::Output {
    TOOL_BATCH.scope(ports, future).await
}
pub fn prepared_tool_for_call(id: &str) -> Option<Arc<dyn PreparedToolPort>> {
    TOOL_BATCH
        .try_with(|ports| {
            ports
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, port)| Arc::clone(port))
        })
        .ok()
        .flatten()
}
pub async fn scope_tool_observation<F: Future>(
    port: Arc<dyn ToolObservationPort>,
    future: F,
) -> F::Output {
    TOOL_OBSERVATION.scope(port, future).await
}
pub fn current_tool_observation() -> Option<Arc<dyn ToolObservationPort>> {
    TOOL_OBSERVATION.try_with(Arc::clone).ok()
}

/// Observations supplied by the existing engine at its turn boundary. The
/// composition root binds identity, stream and current authorization; none of
/// those may be chosen by the engine or inferred from user text.
#[derive(Clone, Debug)]
pub struct EngineTurnInput {
    /// Actual admitted execution directory supplied by the host, when it
    /// overrides the configured workspace. Audit evidence only, never a grant.
    pub execution_workspace: Option<std::path::PathBuf>,
    pub turn_id: String,
    pub agent_alias: Option<String>,
    pub ingress: crate::ingress::IngressContext,
    /// Trusted entrance routing facts needed to rebuild an interrupted queued
    /// dispatch. Content and attachments remain owned by `parts`.
    pub reply_route: Option<Value>,
    /// Canonical ingress order, including file-only messages. Text views for
    /// legacy executors are derived from these parts, never stored alongside.
    pub parts: Vec<EngineInputPart>,
    pub provider_ref: String,
    pub model: String,
}

#[derive(Clone, Debug)]
pub enum EngineInputPart {
    Text(String),
    File(EngineFileInput),
    /// Preserve unsupported protocol content at its original position. A
    /// legacy text executor skips its semantics without losing the evidence.
    Opaque {
        type_name: String,
        body: Value,
    },
}

impl EngineInputPart {
    /// Adapt entrances whose protocol defines text followed by attachments.
    pub fn text_and_files(text: String, files: Vec<EngineFileInput>) -> Vec<Self> {
        std::iter::once(Self::Text(text))
            .chain(files.into_iter().map(Self::File))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct EngineFileInput {
    pub file_id: String,
    pub registration: Value,
    pub representation: PayloadRecord,
    pub policy_evidence: Value,
}

pub type EngineTurnFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send + 'a>>;
pub type DeliveryFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct DeliveryIntent {
    pub turn_id: Option<String>,
    pub transport: String,
    pub target: String,
    pub content: String,
    /// For example platform acceptance or local socket write. It never
    /// implies that a remote viewer read the message.
    pub completion_boundary: String,
}

#[async_trait::async_trait]
pub trait SessionExecutionPort: Send + Sync {
    fn history_reader(&self) -> Option<Arc<dyn super::views::SessionHistoryReadPort>> {
        None
    }
    /// Inputs requeued after verified owner loss, ordered by their original
    /// acceptance. The host supplies the currently resolved provider route and
    /// rebuilds the engine from each returned input before admitting new work.
    /// Legally erased inputs are cancelled under current write authority.
    async fn pending_inputs(
        &self,
        _provider_ref: String,
        _model: String,
    ) -> anyhow::Result<Vec<EngineTurnInput>> {
        Ok(Vec::new())
    }
    /// Settle a verified, effect-free recovered input that the host cannot
    /// readmit. Requires current session write authority; never stops live work.
    async fn reject_pending_input(&self, _turn_id: &str, _reason_key: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_pending_rejection_unsupported")
    }
    /// Register bytes under the identity already allocated by the file owner.
    /// This observes an admitted rendition; it does not grant file access.
    async fn fork_branch(
        &self,
        _branch: BranchId,
        _cursor: EventCursor,
    ) -> anyhow::Result<Arc<dyn SessionExecutionPort>> {
        anyhow::bail!("session_branch_control_unsupported")
    }
    async fn hide_content(&self, _target: EventId, _reason_key: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_content_control_unsupported")
    }
    async fn replace_content(&self, _target: EventId, _replacement: EventId) -> anyhow::Result<()> {
        anyhow::bail!("session_content_control_unsupported")
    }
    async fn record_file(&self, _file: EngineFileInput, _operation: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_file_capture_unsupported")
    }
    async fn request_elicitation(
        &self,
        _request_id: &str,
        _prompt: &str,
        _response_schema: &Value,
        _expires_at: Timestamp,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_elicitation_capture_unsupported")
    }
    async fn resolve_elicitation(
        &self,
        _request_id: &str,
        _resolution: &Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_elicitation_capture_unsupported")
    }
    async fn deliver(
        &self,
        _intent: DeliveryIntent,
        _send: DeliveryFuture<'_>,
    ) -> anyhow::Result<Value> {
        anyhow::bail!("session_delivery_capture_unsupported")
    }
    /// Admit input that updates context without starting an engine or effect,
    /// such as an observed group message. Retry preserves the original message.
    async fn record_context_input(
        &self,
        _input: EngineTurnInput,
        _context_text: &str,
        _reason_key: &str,
    ) -> anyhow::Result<MessageId> {
        anyhow::bail!("session_context_input_capture_unsupported")
    }
    /// Host control evidence, preserved as observed. This never grants
    /// permission or authorizes an external effect.
    async fn record_control(&self, _value: Value) -> anyhow::Result<()> {
        anyhow::bail!("session_control_capture_unsupported")
    }
    /// Record an authenticated transport's approval reply that was rejected
    /// before it could affect the approval state. This is audit-only and never
    /// grants or consumes an approval.
    async fn record_approval_reply_rejected(
        &self,
        _request_id: Option<String>,
        _classification: String,
        _evidence: Value,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_approval_rejection_capture_unsupported")
    }
    /// Close execution while preserving historical facts. Payload erasure is
    /// a separately authorized operation, not implied by closing.
    async fn close(&self, _reason_key: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_close_capture_unsupported")
    }
    async fn rename(&self, _title: &str) -> anyhow::Result<()> {
        anyhow::bail!("session_metadata_capture_unsupported")
    }
    async fn observe_metadata(&self, _value: SessionMetadataObservation) -> anyhow::Result<()> {
        anyhow::bail!("session_metadata_capture_unsupported")
    }
    async fn append_assistant(&self, _text: &str) -> anyhow::Result<MessageId> {
        anyhow::bail!("session_content_command_unsupported")
    }
    async fn observe_model_change(
        &self,
        _selection: ModelOverrideSelection,
        _outcome: ModelChangeDisposition,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_model_control_unsupported")
    }
    async fn request_cancellation(
        &self,
        _turn: Option<&str>,
        _incarnation: &str,
        _reason_key: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_cancellation_command_unsupported")
    }
    async fn restore_model_override(&self) -> anyhow::Result<Option<ModelOverrideSelection>> {
        Ok(None)
    }
    /// A native reader restores the committed context. None means that this
    /// embedding has no persisted context; corruption must propagate as Err.
    async fn restore_context(&self) -> anyhow::Result<Option<ContextSnapshot>> {
        Ok(None)
    }
    /// Freeze input under the foreground lane, capture one maintenance provider
    /// call, and return the committed projection before releasing the lane.
    async fn summarize_context(
        &self,
        _purpose: ProviderPurpose,
        _summary: ContextSummaryExecutor<'_>,
    ) -> anyhow::Result<ContextSnapshot> {
        anyhow::bail!("session_summary_capture_unsupported")
    }
    /// Clear/restore replaces a derived context through a recorded transform.
    async fn replace_context(
        &self,
        _context: ContextSnapshot,
        _reason_key: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("session_context_replacement_unsupported")
    }
    /// Own admission and terminal recording around the one existing engine
    /// future. A duplicate command must return its projection without polling
    /// the future, so effects cannot be replayed by retrying an input.
    async fn execute(
        &self,
        input: EngineTurnInput,
        engine: EngineTurnFuture<'_>,
    ) -> anyhow::Result<String>;
}

tokio::task_local! {
    static SESSION_EXECUTION: Arc<dyn SessionExecutionPort>;
    static ENGINE_INPUT: EngineTurnInput;
}

/// Explicitly transferred by hosts when their existing executor spawns. This
/// is one command's immutable input, never a source of identity or permission.
pub async fn scope_engine_input<F: Future>(input: EngineTurnInput, future: F) -> F::Output {
    ENGINE_INPUT.scope(input, future).await
}

pub fn current_engine_input() -> Option<EngineTurnInput> {
    ENGINE_INPUT.try_with(Clone::clone).ok()
}

pub fn current_engine_turn_id() -> Option<String> {
    ENGINE_INPUT.try_with(|input| input.turn_id.clone()).ok()
}

pub async fn scope_session_execution<F: Future>(
    port: Arc<dyn SessionExecutionPort>,
    future: F,
) -> F::Output {
    SESSION_EXECUTION.scope(port, future).await
}

pub fn current_session_execution() -> Option<Arc<dyn SessionExecutionPort>> {
    SESSION_EXECUTION.try_with(Arc::clone).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_selection_retains_only_thinking_and_accepts_legacy_clear() {
        for thinking in [None, Some("high".to_string())] {
            let selection = ModelOverrideSelection::Inherit {
                thinking: thinking.clone(),
            };
            let value = serde_json::to_value(&selection).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "operation": "inherit",
                    "thinking": thinking,
                })
            );
            assert_eq!(
                serde_json::from_value::<ModelOverrideSelection>(value).unwrap(),
                selection
            );
        }
        assert_eq!(
            serde_json::from_value::<ModelOverrideSelection>(serde_json::json!({
                "operation": "clear"
            }))
            .unwrap(),
            ModelOverrideSelection::Clear,
        );
        assert_eq!(
            serde_json::from_value::<ModelOverrideSelection>(serde_json::json!({
                "operation": "inherit"
            }))
            .unwrap(),
            ModelOverrideSelection::Inherit { thinking: None },
        );
    }

    #[test]
    fn model_route_mode_preserves_legacy_pins_and_round_trips_automatic_selection() {
        let legacy = serde_json::json!({
            "operation": "set",
            "provider_ref": "custom.selected",
            "model": "selected-model",
            "thinking": "high"
        });
        let manual: ModelOverrideSelection = serde_json::from_value(legacy.clone()).unwrap();
        assert!(matches!(
            manual,
            ModelOverrideSelection::Set {
                route_mode: ModelRouteMode::Manual,
                ..
            }
        ));
        let mut automatic = legacy.clone();
        automatic["route_mode"] = serde_json::json!("automatic");
        let selection: ModelOverrideSelection = serde_json::from_value(automatic.clone()).unwrap();
        assert!(matches!(
            selection,
            ModelOverrideSelection::Set {
                route_mode: ModelRouteMode::Automatic,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(selection).unwrap(), automatic);
        let mut invalid = legacy;
        invalid["route_mode"] = serde_json::json!("invalid");
        assert!(serde_json::from_value::<ModelOverrideSelection>(invalid).is_err());
    }

    #[test]
    fn shared_capture_failure_preserves_typed_source_for_callback_and_waiter() {
        let original = std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "fixture commit outcome unknown",
        );
        let (callback, waiter) = share_capture_failure(original.into());
        for error in [callback, waiter] {
            assert!(is_capture_stopped(&error));
            assert!(format!("{error:#}").contains("fixture commit outcome unknown"));
            let source = error
                .chain()
                .find_map(|cause| cause.downcast_ref::<std::io::Error>())
                .unwrap();
            assert_eq!(source.kind(), std::io::ErrorKind::BrokenPipe);
        }
    }

    #[test]
    fn capture_stop_survives_context_and_boxed_stream_error_bridges() {
        let legacy = anyhow::Error::msg("fixture uncertainty").context(SessionCaptureStopped);
        assert!(is_capture_stopped(&legacy));
        let error = capture_stopped(anyhow::Error::msg("fixture unresolved effect"));
        assert!(error.is::<SessionCaptureStopped>());
        let error = anyhow::Error::new(crate::model_provider::StreamError::CaptureStopped(
            error.reallocate_into_boxed_dyn_error_without_backtrace(),
        ))
        .context("stream executor");
        assert!(is_capture_stopped(&error));
        assert!(format!("{error:#}").contains("fixture unresolved effect"));
        assert!(!is_capture_stopped(&anyhow::Error::msg("ordinary error")));
    }

    #[test]
    fn delivery_failure_marker_is_distinct_from_transport_uncertainty() {
        let definite = delivery_definitely_failed(anyhow::Error::msg("rejected before write"));
        assert!(is_delivery_definitely_failed(&definite));
        assert!(!is_delivery_definitely_failed(&anyhow::Error::msg(
            "connection reset"
        )));
    }
}
