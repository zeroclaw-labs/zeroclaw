//! Append-only session event contract.
//!
//! The event stream is the canonical history.  Message lists, turn status and
//! timelines are projections and must not become a second writer.  This module
//! deliberately contains no storage or runtime dependencies: SQLite and JSONL
//! implementations live in `zeroclaw-infra` and the admission/coordinator
//! lives in `zeroclaw-kernel`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
pub mod capture;
pub mod catalog;
pub mod codec;
pub mod context_read;
mod index;
pub mod lineage;
pub mod payloads;
pub mod projection;
mod provider_observation;
pub mod rules;
pub mod search;
pub mod transcript;
pub mod views;
pub use codec::*;
pub use provider_observation::order_provider_segments;
use sha2::{Digest as ShaDigest, Sha256};
use std::fmt;
use uuid::Uuid;

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, ContractError> {
                let value = value.into();
                validate_id(&value)?;
                Ok(Self(value))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

opaque_id!(SessionId);
opaque_id!(SessionStreamId);
opaque_id!(BranchId);
opaque_id!(TurnId);
opaque_id!(InvocationId);
opaque_id!(EventId);
opaque_id!(MessageId);
opaque_id!(PayloadId);

/// Maximum native append-batch and event-page byte budget.
pub const MAX_PAGE_BYTES: usize = 64 * 1024 * 1024;

/// A stream-local durable cursor. Sequence zero denotes the empty prefix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCursor {
    pub stream_id: SessionStreamId,
    pub sequence: u64,
}

impl EventCursor {
    #[must_use]
    pub fn beginning(stream_id: SessionStreamId) -> Self {
        Self {
            stream_id,
            sequence: 0,
        }
    }
}

/// Hex encoded SHA-256 digest used for chain and payload integrity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    #[must_use]
    pub fn zero() -> Self {
        Self("0".repeat(64))
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(hex_encode(&Sha256::digest(bytes)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Digest {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl Default for Digest {
    fn default() -> Self {
        Self::zero()
    }
}

/// Timestamp represented as Unix milliseconds to keep the API independent of
/// a date/time implementation.
pub type Timestamp = i64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EventActor {
    Principal {
        principal_id: String,
    },
    /// Authenticated platform subject, without an inferred canonical principal
    /// or any authority granted by this historical attribution.
    ChannelResponder {
        channel: String,
        responder_id: String,
    },
    System {
        component: String,
    },
}

impl Default for EventActor {
    fn default() -> Self {
        Self::System {
            component: "unknown".to_owned(),
        }
    }
}

/// Supported semantic event tags.  The payload is validated as a typed object
/// by the coordinator; keeping the tag separate lets readers skip a future tag
/// without losing its raw canonical bytes in either backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted,
    SessionMetadataChanged,
    BranchCreated,
    TurnAccepted,
    ControlInputRecorded,
    CancellationRequested,
    ElicitationRequested,
    ElicitationResolved,
    IngressDecisionRecorded,
    TurnStarted,
    UserContentRecorded,
    FileRegistered,
    FileAttached,
    FileUsed,
    ContextTransformStarted,
    ContextTransformFailed,
    ContextTransformCancelled,
    ContextProjectionCreated,
    ProviderRequestPrepared,
    ProviderAttemptStarted,
    ProviderResponseProgressObserved,
    ProviderAttemptCompleted,
    ProviderAttemptFailed,
    ToolCallProposed,
    PolicyDecisionRecorded,
    ApprovalRequested,
    ApprovalResolved,
    ApprovalConsumed,
    ApprovalInvalidated,
    ApprovalReplyRejected,
    ToolEffectStarted,
    ToolEffectProgressObserved,
    ToolEffectCompleted,
    ToolEffectFailed,
    InvocationOutcomeUnknown,
    InvocationCancelled,
    RecoveryObserved,
    OutcomeReconciled,
    AuthorityRevisionObserved,
    SessionModelOverrideChanged,
    DeliveryStarted,
    DeliveryCompleted,
    DeliveryFailed,
    ChildInvocationRequested,
    DerivedSessionStarted,
    ChildInvocationCompleted,
    ChildInvocationRejected,
    ChildCancellationRequested,
    ChildResultConsumed,
    AssistantContentRecorded,
    TurnCommitted,
    TurnCancelled,
    TurnOutcomeUnknown,
    ContentSuperseded,
    PayloadErasureRequested,
    PayloadRedacted,
    LegacyHistoryImported,
    SessionClosed,
    #[serde(other)]
    /// A decoded view of an unknown tag.  The backend retains the original
    /// line/envelope bytes; this variant is never accepted for a write.
    Unknown,
}

impl EventKind {
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    #[must_use]
    pub fn requires_invocation(&self) -> bool {
        matches!(
            self,
            Self::ProviderRequestPrepared
                | Self::ProviderAttemptStarted
                | Self::ProviderResponseProgressObserved
                | Self::ProviderAttemptCompleted
                | Self::ProviderAttemptFailed
                | Self::ToolCallProposed
                | Self::ApprovalConsumed
                | Self::ToolEffectStarted
                | Self::ToolEffectProgressObserved
                | Self::ToolEffectCompleted
                | Self::ToolEffectFailed
                | Self::InvocationOutcomeUnknown
                | Self::InvocationCancelled
                | Self::DeliveryStarted
                | Self::DeliveryCompleted
                | Self::DeliveryFailed
                | Self::ChildInvocationRequested
                | Self::ChildInvocationCompleted
                | Self::ChildInvocationRejected
                | Self::ChildCancellationRequested
                | Self::ChildResultConsumed
        )
    }
}

/// Versioned, object-shaped event payload.  Concrete coordinators construct
/// these objects from their typed command/event variants; the store never
/// accepts arbitrary event tags or mutates fields.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventBody {
    pub codec_version: u32,
    #[serde(flatten)]
    pub data: EventData,
}
impl<'de> Deserialize<'de> for EventBody {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let version = value
            .get("codec_version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(|| serde::de::Error::custom("invalid body codec version"))?;
        let kind: EventKind = serde_json::from_value(
            value
                .get("kind")
                .cloned()
                .ok_or_else(|| serde::de::Error::custom("missing event kind"))?,
        )
        .map_err(serde::de::Error::custom)?;
        // Serde's adjacent-tagged `other` unit variant rejects a future tag
        // carrying an object in `fields`. Inspect the tag first so future
        // bodies are skipped without weakening validation of known bodies.
        let data = if version == 1 && !kind.is_unknown() {
            serde_json::from_value(value).map_err(serde::de::Error::custom)?
        } else {
            EventData::Unknown
        };
        Ok(Self {
            codec_version: version,
            data,
        })
    }
}
impl EventBody {
    pub fn kind(&self) -> EventKind {
        self.data.kind()
    }
    pub fn fields(&self) -> Value {
        serde_json::to_value(&self.data)
            .ok()
            .and_then(|value| value.get("fields").cloned())
            .unwrap_or(Value::Null)
    }
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.codec_version != 1 {
            return Err(ContractError::InvalidBody(
                "unsupported body codec version".to_owned(),
            ));
        }
        if self.kind().is_unknown() {
            return Err(ContractError::InvalidBody(
                "unknown event kinds cannot be written".to_owned(),
            ));
        }
        for reference in self.data.stored_refs()? {
            validate_id(reference.payload_id.as_str())?;
            if reference.codec.is_empty() || !valid_digest(&reference.body_digest) {
                return Err(ContractError::InvalidPayload(
                    "invalid stored payload reference".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

/// Canonical event envelope.  `sequence`, `committed_at`, and `previous_hash`
/// are assigned by the backend and must be zero/empty on an append request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub schema_version: u32,
    pub event_id: EventId,
    pub stream_id: SessionStreamId,
    pub sequence: u64,
    pub branch_id: Option<BranchId>,
    pub turn_id: Option<TurnId>,
    pub causation_id: Option<EventId>,
    pub invocation_id: Option<InvocationId>,
    pub occurred_at: Timestamp,
    pub committed_at: Timestamp,
    pub actor: EventActor,
    pub previous_hash: Digest,
    pub body: EventBody,
}

impl SessionEvent {
    /// Static envelope checks for committed reads. Unknown body codecs/tags
    /// retain their position and original bytes without guessing semantics.
    pub fn validate_recorded(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(ContractError::UnsupportedSchema(self.schema_version));
        }
        if self.sequence == 0
            || self.committed_at <= 0
            || self.occurred_at < 0
            || !valid_digest(&self.previous_hash)
        {
            return Err(ContractError::InvalidEnvelope(
                "invalid committed envelope".into(),
            ));
        }
        if !self.body.kind().is_unknown() {
            self.body.validate()?;
            if self.body.kind().requires_invocation() && self.invocation_id.is_none() {
                return Err(ContractError::InvalidEnvelope(
                    "committed invocation missing identity".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn typed(
        stream_id: SessionStreamId,
        data: EventData,
        actor: EventActor,
    ) -> Result<Self, ContractError> {
        let event = Self {
            schema_version: 1,
            event_id: EventId::new(),
            stream_id,
            sequence: 0,
            branch_id: None,
            turn_id: None,
            causation_id: None,
            invocation_id: None,
            occurred_at: now_millis(),
            committed_at: 0,
            actor,
            previous_hash: Digest::zero(),
            body: EventBody {
                codec_version: 1,
                data,
            },
        };
        event.body.validate()?;
        Ok(event)
    }

    pub fn new(
        stream_id: SessionStreamId,
        kind: EventKind,
        fields: Value,
        actor: EventActor,
    ) -> Result<Self, ContractError> {
        let event = Self {
            schema_version: 1,
            event_id: EventId::new(),
            stream_id,
            sequence: 0,
            branch_id: None,
            turn_id: None,
            causation_id: None,
            invocation_id: None,
            occurred_at: now_millis(),
            committed_at: 0,
            actor,
            previous_hash: Digest::zero(),
            body: EventBody {
                codec_version: 1,
                data: serde_json::from_value(serde_json::json!({"kind": kind, "fields": fields}))
                    .map_err(|error| ContractError::InvalidBody(error.to_string()))?,
            },
        };
        // Callers may fill envelope-owned references such as invocation_id,
        // turn_id, and branch_id immediately after construction. Full
        // admission validation runs when the batch is appended.
        event.body.validate()?;
        Ok(event)
    }

    pub fn validate_for_append(&self) -> Result<(), ContractError> {
        if self.schema_version != 1 {
            return Err(ContractError::UnsupportedSchema(self.schema_version));
        }
        if self.sequence != 0 || self.committed_at != 0 || self.previous_hash != Digest::zero() {
            return Err(ContractError::InvalidEnvelope(
                "backend-owned sequence, commit time, or chain hash was supplied".to_owned(),
            ));
        }
        validate_id(self.event_id.as_str())?;
        validate_id(self.stream_id.as_str())?;
        for id in [
            self.branch_id.as_ref().map(BranchId::as_str),
            self.turn_id.as_ref().map(TurnId::as_str),
            self.invocation_id.as_ref().map(InvocationId::as_str),
            self.causation_id.as_ref().map(EventId::as_str),
        ]
        .into_iter()
        .flatten()
        {
            validate_id(id)?;
        }
        if self.occurred_at < 0 {
            return Err(ContractError::InvalidEnvelope(
                "negative observation timestamp".into(),
            ));
        }
        let actor = match &self.actor {
            EventActor::Principal { principal_id } => principal_id,
            EventActor::ChannelResponder {
                channel,
                responder_id,
            } => {
                if channel.trim().is_empty() || channel.len() > 2048 {
                    return Err(ContractError::InvalidEnvelope(
                        "invalid channel attribution".into(),
                    ));
                }
                responder_id
            }
            EventActor::System { component } => component,
        };
        if actor.trim().is_empty() || actor.len() > 2048 {
            return Err(ContractError::InvalidEnvelope(
                "invalid actor attribution".into(),
            ));
        }
        if self.event_id.as_str().is_empty() || self.stream_id.as_str().is_empty() {
            return Err(ContractError::InvalidEnvelope(
                "event_id and stream_id are required".to_owned(),
            ));
        }
        if self.body.kind().requires_invocation() && self.invocation_id.is_none() {
            return Err(ContractError::InvalidEnvelope(format!(
                "event kind {:?} requires invocation_id",
                self.body.kind()
            )));
        }
        self.body.validate()
    }

    /// Canonical bytes used for request commitments and chain hashing.  The
    /// serde struct field order is fixed by this source type and is covered by
    /// backend conformance fixtures.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ContractError> {
        canonical_json(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRecord {
    pub payload_id: PayloadId,
    pub codec: String,
    pub body: Vec<u8>,
    pub body_digest: Digest,
    pub byte_len: u64,
}

impl PayloadRecord {
    pub fn json(codec: &str, value: &impl Serialize) -> Result<Self, ContractError> {
        Self::new(PayloadId::new(), codec.to_owned(), canonical_json(value)?)
    }
    pub fn new(payload_id: PayloadId, codec: String, body: Vec<u8>) -> Result<Self, ContractError> {
        if codec.trim().is_empty() {
            return Err(ContractError::InvalidPayload("codec is empty".to_owned()));
        }
        let byte_len = u64::try_from(body.len())
            .map_err(|_| ContractError::InvalidPayload("payload is too large".to_owned()))?;
        Ok(Self {
            payload_id,
            codec,
            body_digest: Digest::from_bytes(&body),
            body,
            byte_len,
        })
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        validate_id(self.payload_id.as_str())?;
        if self.codec.trim().is_empty() {
            return Err(ContractError::InvalidPayload("codec is empty".to_owned()));
        }
        let actual_len = u64::try_from(self.body.len())
            .map_err(|_| ContractError::InvalidPayload("payload is too large".to_owned()))?;
        if actual_len != self.byte_len || Digest::from_bytes(&self.body) != self.body_digest {
            return Err(ContractError::PayloadConflict(self.payload_id.to_string()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppendRequest {
    pub stream_id: SessionStreamId,
    pub expected_head: EventCursor,
    pub idempotency_key: String,
    pub events: Vec<SessionEvent>,
    #[serde(default)]
    pub payloads: Vec<PayloadRecord>,
    #[serde(default)]
    pub erasure_reference_commitment: Option<Digest>,
}

impl AppendRequest {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.validate_events(&self.events)
    }

    fn validate_events(&self, events: &[SessionEvent]) -> Result<(), ContractError> {
        if events.len() > 10_000 || self.payloads.len() > 10_000 {
            return Err(ContractError::InvalidRequest(
                "batch item limit exceeded".to_owned(),
            ));
        }
        let mut bytes = 0usize;
        for event in events {
            bytes = bytes
                .checked_add(event.canonical_bytes()?.len())
                .ok_or_else(|| ContractError::InvalidRequest("batch size overflow".to_owned()))?;
        }
        for payload in &self.payloads {
            bytes = bytes
                .checked_add(payload.body.len())
                .ok_or_else(|| ContractError::InvalidRequest("batch size overflow".to_owned()))?;
        }
        if bytes > MAX_PAGE_BYTES {
            return Err(ContractError::InvalidRequest(
                "batch byte limit exceeded".to_owned(),
            ));
        }
        if self.idempotency_key.trim().is_empty() || self.idempotency_key.len() > 512 {
            return Err(ContractError::InvalidRequest(
                "idempotency_key is empty".to_owned(),
            ));
        }
        if self.expected_head.stream_id != self.stream_id {
            return Err(ContractError::InvalidRequest(
                "expected_head belongs to another stream".to_owned(),
            ));
        }
        if events.is_empty() {
            return Err(ContractError::InvalidRequest(
                "empty batches are not appendable".to_owned(),
            ));
        }
        let mut event_ids = std::collections::HashSet::new();
        for event in events {
            event.validate_for_append()?;
            if event.stream_id != self.stream_id {
                return Err(ContractError::InvalidEnvelope(
                    "event belongs to another stream".to_owned(),
                ));
            }
            if !event_ids.insert(event.event_id.clone()) {
                return Err(ContractError::InvalidRequest(
                    "event_id is duplicated in batch".to_owned(),
                ));
            }
        }
        let mut payload_ids = std::collections::HashSet::new();
        for payload in &self.payloads {
            payload.validate()?;
            if !payload_ids.insert(payload.payload_id.clone()) {
                return Err(ContractError::InvalidRequest(
                    "payload_id is duplicated in batch".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub fn commitment(&self) -> Result<Digest, ContractError> {
        // Validate bytes against their digest before binding only metadata.
        // Serializing binary bodies as JSON number arrays would materialize
        // one serde Value per byte and defeat bounded payload memory.
        #[derive(Serialize)]
        struct Commitment<'a> {
            stream_id: &'a SessionStreamId,
            idempotency_key: &'a str,
            events: Vec<SessionEvent>,
            payloads: Vec<StoredPayloadRef>,
        }
        let mut events = self.events.clone();
        for event in &mut events {
            event.sequence = 0;
            event.committed_at = 0;
            event.previous_hash = Digest::zero();
        }
        self.validate_events(&events)?;
        Ok(Digest::from_bytes(&canonical_json(&Commitment {
            stream_id: &self.stream_id,
            idempotency_key: &self.idempotency_key,
            events,
            payloads: self.payloads.iter().map(StoredPayloadRef::from).collect(),
        })?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendReceipt {
    pub first: EventCursor,
    pub last: EventCursor,
    pub event_ids: Vec<EventId>,
    pub head_hash: Digest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub stream_id: SessionStreamId,
    pub after: EventCursor,
    pub through: EventCursor,
    pub events: Vec<SessionEvent>,
    /// Original canonical envelope bytes, retained for forward-compatible
    /// Unknown/Other decoding and export.
    pub raw_events: Vec<Vec<u8>>,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadRead {
    Present(PayloadRecord),
    ErasurePending,
    Redacted,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid envelope: {0}")]
    InvalidEnvelope(String),
    #[error("invalid body: {0}")]
    InvalidBody(String),
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    #[error("payload conflict: {0}")]
    PayloadConflict(String),
    #[error("unsupported schema version: {0}")]
    UnsupportedSchema(u32),
    #[error("encoding failed: {0}")]
    Encoding(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("head conflict: expected {expected}, current {current}")]
    HeadConflict { expected: u64, current: u64 },
    #[error("idempotency conflict for key {0}")]
    IdempotencyConflict(String),
    #[error("invalid state transition: {0}")]
    InvalidTransition(String),
    #[error("invalid event reference: {0}")]
    InvalidReference(String),
    #[error("writer fenced: {0}")]
    WriterFenced(String),
    #[error("reference set changed while validating: {0}")]
    ReferenceConflict(String),
    #[error("unresolved reference reachability: {0}")]
    UnresolvedReachability(String),
    #[error("unsupported schema: {0}")]
    UnsupportedSchema(String),
    #[error("storage unavailable: {0}")]
    StorageUnavailable(String),
    #[error("commit outcome is unknown: {0}")]
    CommitOutcomeUnknown(String),
    #[error("corrupt storage: {0}")]
    Corrupt(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("unsupported schema: {0}")]
    UnsupportedSchema(String),
    #[error("history gap: {0}")]
    Gap(String),
    #[error("corrupt storage: {0}")]
    Corrupt(String),
    #[error("payload missing: {0}")]
    PayloadMissing(String),
    #[error("original payload was erased: {0}")]
    OriginalPayloadErased(String),
    #[error("access denied: {0}")]
    AccessDenied(String),
    #[error("payload too large for requested page")]
    TooLarge,
    #[error("storage unavailable: {0}")]
    StorageUnavailable(String),
}

/// The storage boundary. Implementations must be append-only and must return
/// the same receipt for a retry with the same commitment.
pub trait SessionEventStore: Send + Sync {
    fn append_batch(&self, request: AppendRequest) -> Result<AppendReceipt, AppendError>;
    /// Resolve immutable event relationships from their canonical source.
    /// This internal lookup does not authorize a viewer or expose a writer.
    fn read_event(&self, event_id: &EventId) -> Result<Option<SessionEvent>, ReadError>;
    /// A disposable lookup over immutable events, never an independent
    /// context/status owner. Backends may index these canonical coordinates.
    fn read_latest_event(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: Option<&TurnId>,
        kind: &EventKind,
    ) -> Result<Option<SessionEvent>, ReadError> {
        let mut through = self.head(stream)?;
        while through.sequence > 0 {
            let after = EventCursor {
                stream_id: stream.clone(),
                sequence: through.sequence.saturating_sub(256),
            };
            let page = self.read_page(stream, &after, Some(&through), 256, MAX_PAGE_BYTES)?;
            if page.has_more {
                // A byte-bounded page may not reach the requested upper bound.
                // Continue forward inside this window before searching backward.
                let mut cursor = after.clone();
                let mut latest = None;
                loop {
                    let page =
                        self.read_page(stream, &cursor, Some(&through), 256, MAX_PAGE_BYTES)?;
                    for event in page.events {
                        cursor.sequence = event.sequence;
                        if event.branch_id.as_ref() == Some(branch)
                            && turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn))
                            && event.body.kind() == *kind
                        {
                            latest = Some(event);
                        }
                    }
                    if !page.has_more {
                        break;
                    }
                }
                if latest.is_some() {
                    return Ok(latest);
                }
            } else if let Some(event) = page.events.into_iter().rev().find(|event| {
                event.branch_id.as_ref() == Some(branch)
                    && turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn))
                    && event.body.kind() == *kind
            }) {
                return Ok(Some(event));
            }
            through = after;
        }
        Ok(None)
    }
    fn read_page(
        &self,
        stream_id: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<EventPage, ReadError>;
    fn head(&self, stream_id: &SessionStreamId) -> Result<EventCursor, ReadError>;
    fn lookup_receipt(
        &self,
        stream: &SessionStreamId,
        key: &str,
    ) -> Result<Option<AppendReceipt>, ReadError>;
    fn read_payload(&self, payload_id: &PayloadId) -> Result<PayloadRead, ReadError> {
        self.read_payload_bounded(payload_id, MAX_PAGE_BYTES)
    }
    /// Bound allocation before reading bytes. Latest erasure facts override any
    /// historic cursor; this internal port never grants viewer access by ID.
    fn read_payload_bounded(
        &self,
        payload_id: &PayloadId,
        max_bytes: usize,
    ) -> Result<PayloadRead, ReadError>;
    /// Freeze all scopes reaching the leaves under the backend snapshot lock.
    fn erasure_scope(
        &self,
        targets: &[StoredPayloadRef],
    ) -> Result<(Vec<SessionStreamId>, Digest), AppendError>;
}

fn valid_digest(digest: &Digest) -> bool {
    digest.as_str().len() == 64
        && digest
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn validate_id(value: &str) -> Result<(), ContractError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || matches!(value, "." | "..")
    {
        return Err(ContractError::InvalidRequest(
            "identifier must be non-empty, <=256 bytes, and path-free".to_owned(),
        ));
    }
    Ok(())
}

fn now_millis() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Protocol encoding: recursively sorted object keys, serde JSON integer and
/// string representations, no whitespace. Binary payloads are hashed as bytes.
pub fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>, ContractError> {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.sort_keys();
                for child in object.values_mut() {
                    sort(child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    sort(child);
                }
            }
            _ => {}
        }
    }
    let mut value =
        serde_json::to_value(value).map_err(|error| ContractError::Encoding(error.to_string()))?;
    sort(&mut value);
    serde_json::to_vec(&value).map_err(|error| ContractError::Encoding(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_digest_and_length_are_derived() {
        let payload = PayloadRecord::new(PayloadId::new(), "text".to_owned(), b"hello".to_vec())
            .expect("valid payload");
        assert_eq!(payload.byte_len, 5);
        payload.validate().expect("digest is valid");
    }

    #[test]
    fn invocation_events_require_invocation_id() {
        let stream = SessionStreamId::new();
        let event = SessionEvent::new(
            stream,
            EventKind::ProviderAttemptStarted,
            serde_json::json!({"prepared_event_id":"prepared"}),
            EventActor::default(),
        )
        .expect("construction can be completed before invocation binding");
        assert!(event.validate_for_append().is_err());
    }

    #[test]
    fn request_commitment_binds_verified_payload_metadata_and_rejects_forged_bytes() {
        let stream = SessionStreamId::new();
        let payload =
            PayloadRecord::new(PayloadId::new(), "session.bytes/v1".into(), vec![1; 1024])
                .expect("payload");
        let event = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "fixture".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::default(),
        )
        .expect("root");
        let request = AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor::beginning(stream),
            idempotency_key: "fixture".into(),
            events: vec![event],
            payloads: vec![payload],
            erasure_reference_commitment: None,
        };
        let original = request.commitment().expect("commitment");
        let mut forged = request.clone();
        forged.payloads[0].body[0] = 2;
        assert!(matches!(
            forged.commitment(),
            Err(ContractError::PayloadConflict(_))
        ));
        let mut changed = request.clone();
        changed.payloads[0] = PayloadRecord::new(
            changed.payloads[0].payload_id.clone(),
            "session.bytes/v1".into(),
            vec![2; 1024],
        )
        .expect("changed");
        assert_ne!(original, changed.commitment().expect("changed commitment"));
        changed = request;
        changed.payloads[0].codec = "session.text/v1".into();
        assert_ne!(original, changed.commitment().expect("codec commitment"));
    }

    #[test]
    fn request_commitment_excludes_backend_fields() {
        let stream = SessionStreamId::new();
        let mut event = SessionEvent::new(
            stream.clone(),
            EventKind::SessionStarted,
            serde_json::json!({"session_id":"s","owning_agent":"test","capture_mode":"native"}),
            EventActor::default(),
        )
        .expect("event");
        let request = AppendRequest {
            stream_id: stream.clone(),
            expected_head: EventCursor::beginning(stream),
            idempotency_key: "input-1".to_owned(),
            events: vec![event.clone()],
            payloads: Vec::new(),
            erasure_reference_commitment: None,
        };
        let first = request.commitment().expect("commitment");
        event.sequence = 99;
        event.committed_at = 42;
        event.previous_hash = Digest::from_bytes(b"different");
        let changed = AppendRequest {
            events: vec![event],
            ..request
        };
        assert_eq!(first, changed.commitment().expect("commitment"));
    }
}
