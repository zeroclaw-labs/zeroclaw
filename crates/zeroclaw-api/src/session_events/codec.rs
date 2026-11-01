//! Version-one semantic bodies and registered payload codecs.
use super::*;
use std::marker::PhantomData;

/// A frozen content/structure reference. The backend verifies all metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPayloadRef {
    pub payload_id: PayloadId,
    pub codec: String,
}
impl From<&PayloadRecord> for StoredPayloadRef {
    fn from(record: &PayloadRecord) -> Self {
        Self {
            payload_id: record.payload_id.clone(),
            codec: record.codec.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TypedPayloadRef<T> {
    pub stored: StoredPayloadRef,
    #[serde(skip)]
    pub marker: PhantomData<T>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderBlockRef {
    pub result_event_id: EventId,
    pub block_index: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRange {
    pub stream_id: SessionStreamId,
    pub from_seq: u64,
    pub to_seq: u64,
    pub disposition: RangeDisposition,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeDisposition {
    Included,
    Summarized,
    Omitted,
    Superseded,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetRef {
    Session { stream_id: SessionStreamId },
    Branch { branch_id: BranchId },
    Turn { turn_id: TurnId },
    Invocation { invocation_id: InvocationId },
    ContextTransform { started_event_id: EventId },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelResolution {
    Applied {
        previous_selection_event_id: Option<EventId>,
    },
    AppliedForTurn,
    Rejected {
        reason_key: String,
        evidence_ref: Option<StoredPayloadRef>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Supersession {
    Replaced { replacement_event_id: EventId },
    Hidden { reason_key: String },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reconciliation {
    Completed {
        result_ref: Option<StoredPayloadRef>,
    },
    Failed {
        error_ref: StoredPayloadRef,
    },
    Cancelled,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryDisposition {
    Requeued,
    Rescheduled {
        execution_binding_ref: StoredPayloadRef,
    },
    Interrupted,
    ClassifiedUnknown,
    FinalizedFromEvidence,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Native,
    LegacyMessageOnly,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPurpose {
    Main,
    Summary,
    ContextRepair,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildExecutionMode {
    Sync,
    Background,
    ParallelMember,
}

// Named enum fields are public as part of the variant; no independent kind can
// be paired with another body's fields.
macro_rules! event_data {
    ($($name:ident { $($field:ident : $type:ty),* $(,)? }),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(tag="kind", content="fields", rename_all="snake_case")]
        pub enum EventData { $( $name { $( $field : $type ),* }, )* #[serde(skip)] Unknown { raw_body: Value } }
        impl EventData {
            pub fn kind(&self) -> EventKind { match self { $(Self::$name { .. } => EventKind::$name,)* Self::Unknown { .. } => EventKind::Unknown } }
        }
    }
}
event_data! {
    SessionStarted { session_id: SessionId, owning_agent: String, capture_mode: CaptureMode },
    SessionMetadataChanged { field: String, new_value_ref: StoredPayloadRef },
    BranchCreated { parent_branch_id: BranchId, parent_cursor: EventCursor },
    TurnAccepted { input_ref: StoredPayloadRef, ingress_ref: StoredPayloadRef, reply_route_ref: Option<StoredPayloadRef> },
    ControlInputRecorded { control_ref: StoredPayloadRef, expected_target_ref: TargetRef, ingress_ref: StoredPayloadRef },
    CancellationRequested { target_ref: TargetRef, expected_incarnation: String, reason_key: String },
    ElicitationRequested { request_id: String, prompt_ref: StoredPayloadRef, response_schema_ref: StoredPayloadRef, expires_at: Timestamp, session_incarnation: String },
    ElicitationResolved { request_id: String, resolution_ref: StoredPayloadRef },
    IngressDecisionRecorded { subject_event_id: EventId, decision: String, reason_key: String, policy_evidence_ref: StoredPayloadRef },
    TurnStarted { accepted_event_id: EventId, execution_binding_ref: StoredPayloadRef },
    UserContentRecorded { accepted_event_id: EventId, content_ref: StoredPayloadRef },
    FileRegistered { file_id: String, registration_ref: StoredPayloadRef },
    FileAttached { file_id: String, content_event_id: EventId, part_index: usize, attach_role: String },
    FileUsed { file_id: String, operation: String, representation_ref: StoredPayloadRef, policy_evidence_ref: StoredPayloadRef },
    ContextTransformStarted { input_projection_event_id: EventId, transform_spec_ref: StoredPayloadRef, execution_binding_ref: StoredPayloadRef },
    ContextTransformFailed { started_event_id: EventId, error_ref: StoredPayloadRef },
    ContextTransformCancelled { started_event_id: EventId, reason_key: String },
    ContextProjectionCreated { source_ranges: Vec<SourceRange>, summary_ref: Option<StoredPayloadRef>, budget: u64, reason_key: String, algorithm_version: String, result_ref: StoredPayloadRef, summary_source_refs: Vec<ProviderBlockRef>, transform_started_event_id: Option<EventId> },
    ProviderRequestPrepared { context_event_id: EventId, request_ref: StoredPayloadRef, route_attribution_ref: StoredPayloadRef, purpose: ProviderPurpose },
    ProviderAttemptStarted { prepared_event_id: EventId, previous_attempt_event_id: Option<EventId>, idempotency_ref: Option<StoredPayloadRef> },
    ProviderResponseProgressObserved { observation_ref: StoredPayloadRef },
    ProviderAttemptCompleted { response_ref: StoredPayloadRef, served_attribution_ref: Option<StoredPayloadRef>, last_progress_event_id: Option<EventId> },
    ProviderAttemptFailed { error_ref: StoredPayloadRef, partial_response_ref: Option<StoredPayloadRef>, served_attribution_ref: Option<StoredPayloadRef>, last_progress_event_id: Option<EventId> },
    ToolCallProposed { provider_result_event_id: EventId, block_index: usize },
    PolicyDecisionRecorded { proposed_event_id: EventId, final_action_ref: StoredPayloadRef, decision: String, action_fingerprint: Digest, matched_rule_refs: Vec<String>, authority_evidence_ref: StoredPayloadRef },
    ApprovalRequested { request_id: String, policy_event_id: EventId, display_ref: StoredPayloadRef, trusted_route_ref: StoredPayloadRef, expires_at: Option<Timestamp>, session_incarnation: String },
    ApprovalResolved { request_id: String, resolution_ref: StoredPayloadRef },
    ApprovalConsumed { confirmation_id: String },
    ApprovalInvalidated { request_id: String, confirmation_id: Option<String>, reason_key: String, evidence_ref: Option<StoredPayloadRef> },
    ApprovalReplyRejected { request_id: Option<String>, subject_reply_event_id: Option<EventId>, classification: String, evidence_ref: Option<StoredPayloadRef> },
    ToolEffectStarted { policy_event_id: EventId, confirmation_event_id: Option<EventId>, execution_ref: StoredPayloadRef },
    ToolEffectProgressObserved { observation_ref: StoredPayloadRef },
    ToolEffectCompleted { result_ref: StoredPayloadRef, raw_result_ref: Option<StoredPayloadRef>, transformation_ref: Option<StoredPayloadRef>, receipt_ref: Option<StoredPayloadRef> },
    ToolEffectFailed { error_ref: StoredPayloadRef, partial_result_ref: Option<StoredPayloadRef>, receipt_ref: Option<StoredPayloadRef> },
    InvocationOutcomeUnknown { last_observed_event_id: EventId, reason_key: String, observed_partial_ref: Option<StoredPayloadRef>, error_ref: Option<StoredPayloadRef> },
    InvocationCancelled { cancellation_event_id: EventId, verification_evidence_ref: StoredPayloadRef, observed_partial_ref: Option<StoredPayloadRef> },
    RecoveryObserved { prior_execution_ref: Option<StoredPayloadRef>, through_cursor: EventCursor, disposition: RecoveryDisposition, evidence_ref: StoredPayloadRef },
    OutcomeReconciled { unknown_event_id: EventId, resolution: Reconciliation, verification_evidence_ref: StoredPayloadRef },
    AuthorityRevisionObserved { revision_evidence_ref: StoredPayloadRef, previous_observation_event_id: Option<EventId>, change_record_ref: Option<StoredPayloadRef>, affected_scope_ref: TargetRef, reason_key: String },
    SessionModelOverrideChanged { resolution: ModelResolution },
    DeliveryStarted { authorized_intent_ref: StoredPayloadRef },
    DeliveryCompleted { started_event_id: EventId, verified_receipt_ref: StoredPayloadRef },
    DeliveryFailed { started_event_id: EventId, error_ref: StoredPayloadRef },
    ChildInvocationRequested { relation: String, execution_mode: ChildExecutionMode, target_agent_ref: StoredPayloadRef, input_projection_ref: StoredPayloadRef, policy_evidence_ref: StoredPayloadRef, task_id: Option<String> },
    DerivedSessionStarted { execution_turn_id: Option<TurnId> },
    ChildInvocationCompleted { child_terminal_cursor: EventCursor, consumed_result_ref: Option<StoredPayloadRef> },
    ChildInvocationRejected { requested_event_id: EventId, reason_key: String, policy_evidence_ref: Option<StoredPayloadRef> },
    ChildCancellationRequested { requested_event_id: EventId, reason_key: String, target_scope_ref: TargetRef },
    ChildResultConsumed { child_outcome_event_id: EventId, consumed_result_ref: StoredPayloadRef, consumer_event_id: EventId },
    AssistantContentRecorded { content_ref: StoredPayloadRef, provenance_refs: Vec<ProviderBlockRef> },
    TurnCommitted { completion_ref: StoredPayloadRef },
    TurnCancelled { reason_key: String, unresolved_invocation_ids: Vec<InvocationId> },
    TurnOutcomeUnknown { invocation_ids: Vec<InvocationId>, reason_key: String, observed_partial_ref: Option<StoredPayloadRef> },
    ContentSuperseded { target_event_ids: Vec<EventId>, disposition: Supersession },
    PayloadErasureRequested { targets: Vec<StoredPayloadRef>, reason_category: String },
    PayloadRedacted { erasure_requested_event_id: EventId },
    LegacyHistoryImported { source_kind: String, import_receipt_ref: StoredPayloadRef, completeness: String, known_gaps: Vec<String> },
    SessionClosed { reason_key: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildTerminalEvidence {
    pub child_terminal_cursor: EventCursor,
    pub terminal_event_id: EventId,
}

/// Read-side state derived from the current erasure chain, never persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableContentPart {
    pub part_index: usize,
    pub disposition: super::payloads::PayloadDisposition,
}

impl EventData {
    pub fn stored_refs(&self) -> Result<Vec<StoredPayloadRef>, ContractError> {
        if matches!(self, Self::Unknown { .. }) {
            // Unknown events are retained for readers. Their semantics cannot
            // authorize writes or erasure; migration follows the raw envelope.
            return Ok(Vec::new());
        }
        fn collect(value: &Value, out: &mut Vec<StoredPayloadRef>) -> Result<(), ContractError> {
            match value {
                Value::Object(object) if object.contains_key("payload_id") => {
                    out.push(
                        serde_json::from_value(value.clone())
                            .map_err(|error| ContractError::InvalidBody(error.to_string()))?,
                    );
                }
                Value::Object(object) => {
                    for child in object.values() {
                        collect(child, out)?;
                    }
                }
                Value::Array(items) => {
                    for child in items {
                        collect(child, out)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let mut refs = Vec::new();
        collect(
            &serde_json::to_value(self)
                .map_err(|error| ContractError::Encoding(error.to_string()))?,
            &mut refs,
        )?;
        Ok(refs)
    }
}

/// An ordered reference to the sole message creation source. Reused context
/// items retain this identity; new summaries/edits create a new source and ID.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRef {
    pub message_id: MessageId,
    pub source: ContextMessageSource,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextMessageSource {
    Content {
        content_event_id: EventId,
        content_ref: StoredPayloadRef,
    },
    Conversation {
        payload_ref: StoredPayloadRef,
        item_index: usize,
    },
}

/// Structural identity survives content-leaf erasure. File identifiers are
/// supplied by the file owner; capture never infers them from text paths.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContentPayload {
    pub message_id: MessageId,
    pub parts: Vec<RecordedContentPart>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedContentPart {
    Text {
        text_ref: StoredPayloadRef,
    },
    File {
        file_id: String,
    },
    Opaque {
        type_name: String,
        body_ref: StoredPayloadRef,
    },
    #[serde(other)]
    Other,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseBlock {
    Text {
        text_ref: StoredPayloadRef,
    },
    Reasoning {
        representation_kind: String,
        payload_ref: StoredPayloadRef,
        completion: ContentCompletion,
        replay_requirement: String,
    },
    ToolCall {
        tool_call_ref: StoredPayloadRef,
    },
    Media {
        media_descriptor_ref: StoredPayloadRef,
        received: MediaReceived,
    },
    Other {
        type_name: String,
        /// Historical wire name; content is retained unchanged, without credential filtering.
        classified_body_ref: StoredPayloadRef,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentCompletion {
    Complete,
    Partial,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaReceived {
    Captured {
        representation_ref: StoredPayloadRef,
    },
    ReferenceOnly {
        locator_ref: StoredPayloadRef,
        expiry: Option<Timestamp>,
    },
    Partial {
        representation_ref: StoredPayloadRef,
        completion_evidence: StoredPayloadRef,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponsePayload {
    pub blocks: Vec<ResponseBlock>,
    pub completion: ContentCompletion,
    pub finish_evidence_ref: Option<StoredPayloadRef>,
    pub usage_ref: Option<StoredPayloadRef>,
    pub reasoning_absence: Option<String>,
}
impl ResponseBlock {
    /// Canonical stored representation used for component byte offsets.
    pub fn component(&self) -> (&str, &StoredPayloadRef) {
        match self {
            Self::Text { text_ref } => ("text", text_ref),
            Self::Reasoning {
                payload_ref,
                completion,
                ..
            } => (
                if *completion == ContentCompletion::Complete {
                    "reasoning_finalized"
                } else {
                    "reasoning"
                },
                payload_ref,
            ),
            Self::ToolCall { tool_call_ref } => ("tool_call", tool_call_ref),
            Self::Media { received, .. } => match received {
                MediaReceived::Captured { representation_ref }
                | MediaReceived::Partial {
                    representation_ref, ..
                } => ("media", representation_ref),
                MediaReceived::ReferenceOnly { locator_ref, .. } => ("locator", locator_ref),
            },
            Self::Other {
                classified_body_ref,
                ..
            } => ("opaque", classified_body_ref),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderObservation {
    ProtocolSegmentObserved {
        position: crate::model_provider::ProviderContentPosition,
        component: String,
        byte_offset: u64,
        segment_ref: StoredPayloadRef,
    },
    BlockOpened {
        block_index: usize,
        descriptor_ref: StoredPayloadRef,
    },
    BytesAppended {
        block_index: usize,
        component: String,
        byte_offset: u64,
        bytes_ref: StoredPayloadRef,
    },
    BlockFinalized {
        block_index: usize,
        finalized_block_ref: StoredPayloadRef,
    },
    MetadataObserved {
        metadata_ref: StoredPayloadRef,
    },
    ProviderToolActivityObserved {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_call_id: Option<String>,
        activity_ref: StoredPayloadRef,
    },
    DiagnosticObserved {
        error_ref: StoredPayloadRef,
    },
    CancellationObserved {
        request_event_id: EventId,
        stage: DiagnosticStage,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderObservationPayload {
    pub operations: Vec<ProviderObservation>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolObservation {
    ProcessStarted {
        origin: ObservationOrigin,
        execution_evidence_ref: StoredPayloadRef,
    },
    OutputChunk {
        origin: ObservationOrigin,
        stream: OutputStream,
        byte_offset: u64,
        bytes_ref: StoredPayloadRef,
    },
    CheckpointObserved {
        origin: ObservationOrigin,
        checkpoint_ref: StoredPayloadRef,
    },
    CleanupStarted {
        origin: ObservationOrigin,
        target_ref: StoredPayloadRef,
        reason_evidence_ref: StoredPayloadRef,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolObservationPayload {
    pub operations: Vec<ToolObservation>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    HostAdapter,
    RegisteredTool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Prepare,
    Connect,
    Dispatch,
    AwaitResponse,
    ConsumeStream,
    Decode,
    Capture,
    Commit,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderDiagnostic {
    pub error_key: String,
    pub stage: DiagnosticStage,
    pub detail: DiagnosticDetail,
    pub protocol_evidence: Vec<ProtocolEvidence>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticDetail {
    Available { detail_ref: StoredPayloadRef },
    Unavailable { reason_key: String },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolEvidence {
    Http {
        status_code: u16,
        provider_error_code: Option<String>,
        provider_request_id: Option<String>,
        retry_after: Option<String>,
    },
    Transport {
        code: String,
    },
    StreamTermination {
        evidence: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalResolution {
    /// No prompt was available; execution remains subject to shell policy.
    ChannelUnsupported {
        confirmation_id: String,
        verification_evidence_ref: StoredPayloadRef,
    },
    Approved {
        confirmation_id: String,
        validity: String,
        verification_evidence_ref: StoredPayloadRef,
    },
    Denied {
        reason_key: String,
        verification_evidence_ref: StoredPayloadRef,
    },
}

/// Immutable semantic decision observed at the authenticated reply boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReplyDecision {
    ApproveOnce,
    ApproveSession,
    Deny,
    Replace,
}

impl From<&crate::channel::ChannelApprovalResponse> for ApprovalReplyDecision {
    fn from(response: &crate::channel::ChannelApprovalResponse) -> Self {
        use crate::channel::ChannelApprovalResponse;
        match response {
            ChannelApprovalResponse::Approve => Self::ApproveOnce,
            ChannelApprovalResponse::AlwaysApprove => Self::ApproveSession,
            ChannelApprovalResponse::Deny => Self::Deny,
            ChannelApprovalResponse::DenyWithEdit { .. } => Self::Replace,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalReplyPayload {
    pub decision: ApprovalReplyDecision,
    /// Classified, erasable replacement text. Older replies may omit it;
    /// newly observed replacements must always retain this content reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_ref: Option<StoredPayloadRef>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnCompletion {
    Success {
        result_event_ids: Vec<EventId>,
    },
    Failed {
        error_ref: StoredPayloadRef,
        observed_result_event_ids: Vec<EventId>,
    },
}
