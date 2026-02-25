use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use thiserror::Error;
use uuid::Uuid;

/// Delivery mode for a coordination envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryScope {
    /// Send to exactly one target agent.
    Direct,
    /// Fan out to all registered agents.
    Broadcast,
}

/// Typed payload variants used by agent coordination.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoordinationPayload {
    DelegateTask {
        task_id: String,
        summary: String,
        metadata: Value,
    },
    ContextPatch {
        key: String,
        expected_version: u64,
        value: Value,
    },
    TaskResult {
        task_id: String,
        success: bool,
        output: String,
    },
    Ack {
        acked_message_id: String,
    },
    Control {
        action: String,
        note: Option<String>,
    },
}

/// Message envelope used by coordination protocol traffic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoordinationEnvelope {
    pub id: String,
    pub conversation_id: String,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub from: String,
    pub to: Option<String>,
    pub topic: String,
    pub scope: DeliveryScope,
    pub payload: CoordinationPayload,
}

impl CoordinationEnvelope {
    /// Construct a direct message envelope.
    pub fn new_direct(
        from: impl Into<String>,
        to: impl Into<String>,
        conversation_id: impl Into<String>,
        topic: impl Into<String>,
        payload: CoordinationPayload,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.into(),
            correlation_id: None,
            causation_id: None,
            from: from.into(),
            to: Some(to.into()),
            topic: topic.into(),
            scope: DeliveryScope::Direct,
            payload,
        }
    }

    /// Construct a broadcast envelope.
    pub fn new_broadcast(
        from: impl Into<String>,
        conversation_id: impl Into<String>,
        topic: impl Into<String>,
        payload: CoordinationPayload,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.into(),
            correlation_id: None,
            causation_id: None,
            from: from.into(),
            to: None,
            topic: topic.into(),
            scope: DeliveryScope::Broadcast,
            payload,
        }
    }

    /// Validate transport and payload contract before publishing.
    pub fn validate(&self) -> Result<(), CoordinationError> {
        require_non_empty(&self.id, "id")?;
        require_non_empty(&self.conversation_id, "conversation_id")?;
        require_non_empty(&self.from, "from")?;
        require_non_empty(&self.topic, "topic")?;

        match self.scope {
            DeliveryScope::Direct => {
                let target = self
                    .to
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if target.is_none() {
                    return Err(CoordinationError::MissingTarget {
                        message_id: self.id.clone(),
                    });
                }
            }
            DeliveryScope::Broadcast => {
                if self.to.is_some() {
                    return Err(CoordinationError::BroadcastHasTarget {
                        message_id: self.id.clone(),
                    });
                }
            }
        }

        if let Some(correlation_id) = &self.correlation_id {
            require_non_empty(correlation_id, "correlation_id")?;
        }
        if let Some(causation_id) = &self.causation_id {
            require_non_empty(causation_id, "causation_id")?;
        }

        match &self.payload {
            CoordinationPayload::DelegateTask {
                task_id, summary, ..
            } => {
                require_non_empty(task_id, "task_id")?;
                require_non_empty(summary, "summary")?;
                if self.scope != DeliveryScope::Direct {
                    return Err(CoordinationError::InvalidDeliveryScope {
                        message_id: self.id.clone(),
                        expected: DeliveryScope::Direct,
                        actual: self.scope,
                        payload: "delegate_task".to_string(),
                    });
                }
            }
            CoordinationPayload::ContextPatch { key, .. } => {
                require_non_empty(key, "key")?;
            }
            CoordinationPayload::TaskResult {
                task_id, output, ..
            } => {
                require_non_empty(task_id, "task_id")?;
                require_non_empty(output, "output")?;
                if self
                    .correlation_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    return Err(CoordinationError::MissingCorrelationId {
                        message_id: self.id.clone(),
                    });
                }
            }
            CoordinationPayload::Ack { acked_message_id } => {
                require_non_empty(acked_message_id, "acked_message_id")?;
            }
            CoordinationPayload::Control { action, .. } => {
                require_non_empty(action, "action")?;
            }
        }

        Ok(())
    }
}

fn require_non_empty(value: &str, field: &'static str) -> Result<(), CoordinationError> {
    if value.trim().is_empty() {
        return Err(CoordinationError::EmptyField { field });
    }
    Ok(())
}

/// Errors emitted by the coordination protocol and message bus.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CoordinationError {
    #[error("field `{field}` must not be empty")]
    EmptyField { field: &'static str },
    #[error("message `{message_id}` requires a direct target agent")]
    MissingTarget { message_id: String },
    #[error("broadcast message `{message_id}` cannot set explicit target")]
    BroadcastHasTarget { message_id: String },
    #[error("task result message `{message_id}` requires `correlation_id`")]
    MissingCorrelationId { message_id: String },
    #[error(
        "invalid delivery scope for payload `{payload}` on message `{message_id}`: expected {expected:?}, got {actual:?}"
    )]
    InvalidDeliveryScope {
        message_id: String,
        expected: DeliveryScope,
        actual: DeliveryScope,
        payload: String,
    },
    #[error("duplicate message id `{message_id}`")]
    DuplicateMessageId { message_id: String },
    #[error("unknown target agent `{agent}` for message `{message_id}`")]
    UnknownTarget { agent: String, message_id: String },
    #[error("agent `{agent}` is not registered")]
    UnknownAgent { agent: String },
    #[error("invalid delegate context key `{key}` on message `{message_id}`")]
    InvalidDelegateContextKey { key: String, message_id: String },
    #[error("delegate context key `{key}` requires `correlation_id` on message `{message_id}`")]
    MissingDelegateContextCorrelation { key: String, message_id: String },
    #[error(
        "delegate context key `{key}` correlation mismatch on message `{message_id}`: key has `{key_correlation_id}`, envelope has `{envelope_correlation_id}`"
    )]
    DelegateContextCorrelationMismatch {
        key: String,
        message_id: String,
        key_correlation_id: String,
        envelope_correlation_id: String,
    },
    #[error("context version mismatch for key `{key}`: expected {expected}, actual {actual}")]
    ContextVersionMismatch {
        key: String,
        expected: u64,
        actual: u64,
    },
}

/// Sequenced message emitted by the bus.
#[derive(Debug, Clone)]
pub struct SequencedEnvelope {
    pub sequence: u64,
    pub envelope: CoordinationEnvelope,
}

/// Dead-letter item retained for audit and debugging.
#[derive(Debug, Clone)]
pub struct DeadLetter {
    pub envelope: CoordinationEnvelope,
    pub reason: String,
}

/// Versioned shared context record written through `ContextPatch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedContextEntry {
    pub key: String,
    pub value: Value,
    pub version: u64,
    pub updated_by: String,
    pub last_message_id: String,
}

/// Publish result metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishReceipt {
    pub sequence: u64,
    pub delivered_to: usize,
}

/// Capacity limits used by `InMemoryMessageBus` retention policies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct InMemoryMessageBusLimits {
    pub max_inbox_messages_per_agent: usize,
    pub max_dead_letters: usize,
    pub max_context_entries: usize,
    pub max_seen_message_ids: usize,
}

impl Default for InMemoryMessageBusLimits {
    fn default() -> Self {
        Self {
            max_inbox_messages_per_agent: 256,
            max_dead_letters: 256,
            max_context_entries: 512,
            max_seen_message_ids: 4096,
        }
    }
}

/// Runtime counters for operational visibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct InMemoryMessageBusStats {
    /// Total publish attempts that passed envelope validation.
    pub publish_attempts_total: u64,
    /// Total successful deliveries (fan-out count for broadcast).
    pub deliveries_total: u64,
    /// Number of inbox messages evicted due to inbox capacity limits.
    pub inbox_overflow_evictions_total: u64,
    /// Total dead-letter entries ever recorded.
    pub dead_letters_total: u64,
    /// Number of dead-letter entries evicted due to dead-letter cap.
    pub dead_letter_evictions_total: u64,
    /// Number of shared-context entries evicted due to context capacity limits.
    pub context_evictions_total: u64,
    /// Number of idempotency IDs evicted due to dedupe-window capacity limits.
    pub seen_message_id_evictions_total: u64,
}

#[derive(Debug, Default)]
struct BusState {
    next_sequence: u64,
    seen_message_ids: HashSet<String>,
    seen_message_order: VecDeque<String>,
    inboxes: HashMap<String, VecDeque<SequencedEnvelope>>,
    inbox_correlation_counts: HashMap<String, HashMap<String, usize>>,
    dead_letters: Vec<DeadLetter>,
    dead_letters_by_correlation: HashMap<String, VecDeque<DeadLetter>>,
    context: HashMap<String, SharedContextEntry>,
    context_order: VecDeque<String>,
    delegate_context_order: VecDeque<String>,
    context_order_by_correlation: HashMap<String, VecDeque<String>>,
    delegate_context_order_by_correlation: HashMap<String, VecDeque<String>>,
    context_correlation_by_key: HashMap<String, String>,
    limits: InMemoryMessageBusLimits,
    stats: InMemoryMessageBusStats,
}

impl BusState {
    fn with_limits(mut limits: InMemoryMessageBusLimits) -> Self {
        if limits.max_inbox_messages_per_agent == 0 {
            limits.max_inbox_messages_per_agent = 1;
        }
        if limits.max_dead_letters == 0 {
            limits.max_dead_letters = 1;
        }
        if limits.max_context_entries == 0 {
            limits.max_context_entries = 1;
        }
        if limits.max_seen_message_ids == 0 {
            limits.max_seen_message_ids = 1;
        }

        Self {
            next_sequence: 0,
            seen_message_ids: HashSet::new(),
            seen_message_order: VecDeque::new(),
            inboxes: HashMap::new(),
            inbox_correlation_counts: HashMap::new(),
            dead_letters: Vec::new(),
            dead_letters_by_correlation: HashMap::new(),
            context: HashMap::new(),
            context_order: VecDeque::new(),
            delegate_context_order: VecDeque::new(),
            context_order_by_correlation: HashMap::new(),
            delegate_context_order_by_correlation: HashMap::new(),
            context_correlation_by_key: HashMap::new(),
            limits,
            stats: InMemoryMessageBusStats::default(),
        }
    }
}

/// Deterministic in-memory coordination message bus with:
/// - typed envelope validation
/// - idempotency guard on message id
/// - per-agent ordered delivery
/// - dead-letter retention for invalid/conflicting messages
/// - optimistic-locking context patches
#[derive(Debug, Clone)]
pub struct InMemoryMessageBus {
    inner: Arc<Mutex<BusState>>,
}

impl Default for InMemoryMessageBus {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryMessageBus {
    pub fn new() -> Self {
        Self::with_limits(InMemoryMessageBusLimits::default())
    }

    pub fn with_limits(limits: InMemoryMessageBusLimits) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusState::with_limits(limits))),
        }
    }

    /// Register an agent inbox.
    pub fn register_agent(&self, agent: impl Into<String>) -> Result<(), CoordinationError> {
        let agent = agent.into();
        require_non_empty(&agent, "agent")?;
        let mut state = self.lock_state();
        state.inboxes.entry(agent.clone()).or_default();
        state.inbox_correlation_counts.entry(agent).or_default();
        Ok(())
    }

    /// Remove an existing agent inbox.
    pub fn unregister_agent(&self, agent: &str) -> bool {
        let mut state = self.lock_state();
        let removed = state.inboxes.remove(agent).is_some();
        state.inbox_correlation_counts.remove(agent);
        removed
    }

    /// Publish an envelope to the bus.
    pub fn publish(
        &self,
        envelope: CoordinationEnvelope,
    ) -> Result<PublishReceipt, CoordinationError> {
        if let Err(error) = envelope.validate() {
            self.push_dead_letter(envelope, error.to_string());
            return Err(error);
        }

        let mut state = self.lock_state();
        state.stats.publish_attempts_total += 1;
        if state.seen_message_ids.contains(&envelope.id) {
            let error = CoordinationError::DuplicateMessageId {
                message_id: envelope.id.clone(),
            };
            push_dead_letter_locked(&mut state, envelope, error.to_string());
            return Err(error);
        }
        if state.seen_message_ids.len() >= state.limits.max_seen_message_ids {
            if let Some(evicted_id) = state.seen_message_order.pop_front() {
                if state.seen_message_ids.remove(&evicted_id) {
                    state.stats.seen_message_id_evictions_total += 1;
                }
            }
        }
        state.seen_message_ids.insert(envelope.id.clone());
        state.seen_message_order.push_back(envelope.id.clone());

        if let CoordinationPayload::ContextPatch {
            key,
            expected_version,
            value,
        } = &envelope.payload
        {
            if let Err(error) =
                apply_context_patch_locked(&mut state, &envelope, key, *expected_version, value)
            {
                push_dead_letter_locked(&mut state, envelope, error.to_string());
                return Err(error);
            }
        }

        state.next_sequence += 1;
        let sequence = state.next_sequence;
        let sequenced = SequencedEnvelope {
            sequence,
            envelope: envelope.clone(),
        };

        let delivered_to = match envelope.scope {
            DeliveryScope::Direct => {
                let target = envelope.to.as_deref().expect("validated direct target");
                if !state.inboxes.contains_key(target) {
                    let error = CoordinationError::UnknownTarget {
                        agent: target.to_string(),
                        message_id: envelope.id.clone(),
                    };
                    push_dead_letter_locked(&mut state, envelope, error.to_string());
                    return Err(error);
                }

                let dropped = push_inbox_entry_locked(&mut state, target, sequenced);
                if let Some(dropped) = dropped {
                    state.stats.inbox_overflow_evictions_total += 1;
                    push_dead_letter_locked(
                        &mut state,
                        dropped,
                        format!("inbox overflow: dropped oldest message for agent '{target}'"),
                    );
                }
                1
            }
            DeliveryScope::Broadcast => {
                if state.inboxes.is_empty() {
                    0
                } else {
                    let fanout = state.inboxes.len();
                    let mut dropped_items: Vec<(String, CoordinationEnvelope)> = Vec::new();
                    let agents = state.inboxes.keys().cloned().collect::<Vec<_>>();
                    for agent in &agents {
                        if let Some(dropped) =
                            push_inbox_entry_locked(&mut state, agent, sequenced.clone())
                        {
                            dropped_items.push((agent.clone(), dropped));
                        }
                    }
                    for (agent, dropped) in dropped_items {
                        state.stats.inbox_overflow_evictions_total += 1;
                        push_dead_letter_locked(
                            &mut state,
                            dropped,
                            format!("inbox overflow: dropped oldest message for agent '{agent}'"),
                        );
                    }
                    fanout
                }
            }
        };
        state.stats.deliveries_total += delivered_to as u64;

        Ok(PublishReceipt {
            sequence,
            delivered_to,
        })
    }

    /// Drain up to `max` pending envelopes for an agent inbox.
    /// Use `max = 0` to drain all available messages.
    pub fn drain_for_agent(
        &self,
        agent: &str,
        max: usize,
    ) -> Result<Vec<SequencedEnvelope>, CoordinationError> {
        let mut state = self.lock_state();
        let agent_owned = agent.to_string();
        let inbox_len = state.inboxes.get(agent).map(VecDeque::len).ok_or_else(|| {
            CoordinationError::UnknownAgent {
                agent: agent_owned.clone(),
            }
        })?;

        let drain_count = if max == 0 {
            inbox_len
        } else {
            max.min(inbox_len)
        };
        let mut drained = Vec::with_capacity(drain_count);
        for _ in 0..drain_count {
            let envelope = {
                let inbox = state
                    .inboxes
                    .get_mut(agent)
                    .expect("agent existence should be validated before drain");
                inbox.pop_front()
            };
            if let Some(envelope) = envelope {
                let correlation_counts = state
                    .inbox_correlation_counts
                    .entry(agent_owned.clone())
                    .or_default();
                decrement_correlation_count(correlation_counts, &envelope.envelope);
                drained.push(envelope);
            }
        }
        Ok(drained)
    }

    pub fn pending_for_agent(&self, agent: &str) -> Result<usize, CoordinationError> {
        let state = self.lock_state();
        state
            .inboxes
            .get(agent)
            .map(VecDeque::len)
            .ok_or_else(|| CoordinationError::UnknownAgent {
                agent: agent.to_string(),
            })
    }

    pub fn pending_for_agent_correlation(
        &self,
        agent: &str,
        correlation_id: &str,
    ) -> Result<usize, CoordinationError> {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return Ok(0);
        }

        let state = self.lock_state();
        if !state.inboxes.contains_key(agent) {
            return Err(CoordinationError::UnknownAgent {
                agent: agent.to_string(),
            });
        }

        Ok(state
            .inbox_correlation_counts
            .get(agent)
            .and_then(|counts| counts.get(correlation_id).copied())
            .unwrap_or(0))
    }

    /// Peek up to `max` pending envelopes for an agent without consuming them.
    /// Use `max = 0` to peek the full inbox.
    pub fn peek_for_agent(
        &self,
        agent: &str,
        max: usize,
    ) -> Result<Vec<SequencedEnvelope>, CoordinationError> {
        self.peek_for_agent_with_offset(agent, 0, max)
    }

    /// Peek up to `max` pending envelopes for an agent without consuming them,
    /// with an offset into inbox order (oldest first).
    /// Use `max = 0` to peek all entries after `offset`.
    pub fn peek_for_agent_with_offset(
        &self,
        agent: &str,
        offset: usize,
        max: usize,
    ) -> Result<Vec<SequencedEnvelope>, CoordinationError> {
        let state = self.lock_state();
        let inbox = state
            .inboxes
            .get(agent)
            .ok_or_else(|| CoordinationError::UnknownAgent {
                agent: agent.to_string(),
            })?;

        let available = inbox.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };
        Ok(inbox
            .iter()
            .skip(offset)
            .take(take_count)
            .cloned()
            .collect())
    }

    /// Peek up to `max` pending envelopes matching a correlation ID for an
    /// agent without consuming them, with an offset in match order
    /// (oldest first). Use `max = 0` to peek all matches after `offset`.
    pub fn peek_for_agent_correlation_with_offset(
        &self,
        agent: &str,
        correlation_id: &str,
        offset: usize,
        max: usize,
    ) -> Result<Vec<SequencedEnvelope>, CoordinationError> {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return Ok(Vec::new());
        }

        let state = self.lock_state();
        let inbox = state
            .inboxes
            .get(agent)
            .ok_or_else(|| CoordinationError::UnknownAgent {
                agent: agent.to_string(),
            })?;

        let available = state
            .inbox_correlation_counts
            .get(agent)
            .and_then(|counts| counts.get(correlation_id).copied())
            .unwrap_or(0)
            .saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        Ok(inbox
            .iter()
            .filter(|entry| {
                normalized_non_empty(entry.envelope.correlation_id.as_deref())
                    .is_some_and(|value| value == correlation_id)
            })
            .skip(offset)
            .take(take_count)
            .cloned()
            .collect())
    }

    /// Snapshot registered agents with inboxes.
    pub fn registered_agents(&self) -> Vec<String> {
        let state = self.lock_state();
        let mut agents = state.inboxes.keys().cloned().collect::<Vec<_>>();
        agents.sort();
        agents
    }

    pub fn limits(&self) -> InMemoryMessageBusLimits {
        self.lock_state().limits
    }

    pub fn stats(&self) -> InMemoryMessageBusStats {
        self.lock_state().stats
    }

    pub fn subscriber_count(&self) -> usize {
        self.lock_state().inboxes.len()
    }

    /// Snapshot all shared context entries.
    pub fn context_snapshot(&self) -> HashMap<String, SharedContextEntry> {
        self.lock_state().context.clone()
    }

    /// Snapshot shared context entries in write-recency order (newest first).
    /// Use `max = 0` to return all entries.
    pub fn context_entries_recent(&self, max: usize) -> Vec<(String, SharedContextEntry)> {
        self.context_entries_recent_with_offset(0, max)
    }

    /// Snapshot shared context entries in write-recency order (newest first),
    /// with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn context_entries_recent_with_offset(
        &self,
        offset: usize,
        max: usize,
    ) -> Vec<(String, SharedContextEntry)> {
        let state = self.lock_state();
        let available = state.context_order.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        state
            .context_order
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .filter_map(|key| {
                state
                    .context
                    .get(key)
                    .cloned()
                    .map(|entry| (key.clone(), entry))
            })
            .collect()
    }

    /// Snapshot shared context entries for a correlation ID in write-recency
    /// order (newest first). Use `max = 0` to return all entries.
    pub fn context_entries_recent_for_correlation(
        &self,
        correlation_id: &str,
        max: usize,
    ) -> Vec<(String, SharedContextEntry)> {
        self.context_entries_recent_for_correlation_with_offset(correlation_id, 0, max)
    }

    /// Snapshot shared context entries for a correlation ID in write-recency
    /// order (newest first), with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn context_entries_recent_for_correlation_with_offset(
        &self,
        correlation_id: &str,
        offset: usize,
        max: usize,
    ) -> Vec<(String, SharedContextEntry)> {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return Vec::new();
        }

        let state = self.lock_state();
        let Some(order) = state.context_order_by_correlation.get(correlation_id) else {
            return Vec::new();
        };

        let available = order.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        order
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .filter_map(|key| {
                state
                    .context
                    .get(key)
                    .cloned()
                    .map(|entry| (key.clone(), entry))
            })
            .collect()
    }

    pub fn context_count(&self) -> usize {
        self.lock_state().context.len()
    }

    pub fn context_count_for_correlation(&self, correlation_id: &str) -> usize {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return 0;
        }

        let state = self.lock_state();
        state
            .context_order_by_correlation
            .get(correlation_id)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    /// Snapshot only `delegate/` shared context entries in write-recency order
    /// (newest first), with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn delegate_context_entries_recent_with_offset(
        &self,
        offset: usize,
        max: usize,
    ) -> Vec<(String, SharedContextEntry)> {
        let state = self.lock_state();
        let available = state.delegate_context_order.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        state
            .delegate_context_order
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .filter_map(|key| {
                state
                    .context
                    .get(key)
                    .cloned()
                    .map(|entry| (key.clone(), entry))
            })
            .collect()
    }

    /// Snapshot only `delegate/` shared context entries for a correlation ID
    /// in write-recency order (newest first), with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn delegate_context_entries_recent_for_correlation_with_offset(
        &self,
        correlation_id: &str,
        offset: usize,
        max: usize,
    ) -> Vec<(String, SharedContextEntry)> {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return Vec::new();
        }

        let state = self.lock_state();
        let Some(order) = state
            .delegate_context_order_by_correlation
            .get(correlation_id)
        else {
            return Vec::new();
        };

        let available = order.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        order
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .filter_map(|key| {
                state
                    .context
                    .get(key)
                    .cloned()
                    .map(|entry| (key.clone(), entry))
            })
            .collect()
    }

    pub fn delegate_context_count(&self) -> usize {
        self.lock_state().delegate_context_order.len()
    }

    pub fn delegate_context_count_for_correlation(&self, correlation_id: &str) -> usize {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return 0;
        }

        let state = self.lock_state();
        state
            .delegate_context_order_by_correlation
            .get(correlation_id)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    /// Snapshot dead-letter entries in recency order (newest first),
    /// with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn dead_letters_recent(&self, offset: usize, max: usize) -> Vec<DeadLetter> {
        let state = self.lock_state();
        let available = state.dead_letters.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        state
            .dead_letters
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .cloned()
            .collect()
    }

    /// Snapshot dead-letter entries for a correlation ID in recency order
    /// (newest first), with an offset for pagination.
    /// Use `max = 0` to return all entries after `offset`.
    pub fn dead_letters_recent_for_correlation(
        &self,
        correlation_id: &str,
        offset: usize,
        max: usize,
    ) -> Vec<DeadLetter> {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return Vec::new();
        }

        let state = self.lock_state();
        let Some(entries) = state.dead_letters_by_correlation.get(correlation_id) else {
            return Vec::new();
        };

        let available = entries.len().saturating_sub(offset);
        let take_count = if max == 0 {
            available
        } else {
            max.min(available)
        };

        entries
            .iter()
            .rev()
            .skip(offset)
            .take(take_count)
            .cloned()
            .collect()
    }

    pub fn context_entry(&self, key: &str) -> Option<SharedContextEntry> {
        self.lock_state().context.get(key).cloned()
    }

    pub fn dead_letter_count(&self) -> usize {
        self.lock_state().dead_letters.len()
    }

    pub fn dead_letter_count_for_correlation(&self, correlation_id: &str) -> usize {
        let correlation_id = correlation_id.trim();
        if correlation_id.is_empty() {
            return 0;
        }

        let state = self.lock_state();
        state
            .dead_letters_by_correlation
            .get(correlation_id)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    pub fn dead_letters(&self) -> Vec<DeadLetter> {
        self.lock_state().dead_letters.clone()
    }

    fn push_dead_letter(&self, envelope: CoordinationEnvelope, reason: String) {
        let mut state = self.lock_state();
        push_dead_letter_locked(&mut state, envelope, reason);
    }

    fn lock_state(&self) -> MutexGuard<'_, BusState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn push_inbox_entry_locked(
    state: &mut BusState,
    agent: &str,
    entry: SequencedEnvelope,
) -> Option<CoordinationEnvelope> {
    let max_inbox_messages_per_agent = state.limits.max_inbox_messages_per_agent;
    let (inboxes, correlation_counts_by_agent) =
        (&mut state.inboxes, &mut state.inbox_correlation_counts);
    let inbox = inboxes
        .get_mut(agent)
        .expect("agent existence should be validated before pushing inbox entry");
    let correlation_counts = correlation_counts_by_agent
        .entry(agent.to_string())
        .or_default();

    let dropped = if inbox.len() >= max_inbox_messages_per_agent {
        inbox.pop_front()
    } else {
        None
    };
    if let Some(dropped_entry) = dropped.as_ref() {
        decrement_correlation_count(correlation_counts, &dropped_entry.envelope);
    }

    increment_correlation_count(correlation_counts, &entry.envelope);
    inbox.push_back(entry);
    dropped.map(|value| value.envelope)
}

fn increment_correlation_count(
    counts: &mut HashMap<String, usize>,
    envelope: &CoordinationEnvelope,
) {
    if let Some(correlation_id) = normalized_non_empty(envelope.correlation_id.as_deref()) {
        *counts.entry(correlation_id.to_string()).or_insert(0) += 1;
    }
}

fn decrement_correlation_count(
    counts: &mut HashMap<String, usize>,
    envelope: &CoordinationEnvelope,
) {
    let Some(correlation_id) = normalized_non_empty(envelope.correlation_id.as_deref()) else {
        return;
    };

    let mut remove_key = false;
    if let Some(count) = counts.get_mut(correlation_id) {
        if *count <= 1 {
            remove_key = true;
        } else {
            *count -= 1;
        }
    }
    if remove_key {
        counts.remove(correlation_id);
    }
}

fn push_dead_letter_locked(state: &mut BusState, envelope: CoordinationEnvelope, reason: String) {
    state.stats.dead_letters_total += 1;
    if state.dead_letters.len() >= state.limits.max_dead_letters {
        state.stats.dead_letter_evictions_total += 1;
        if let Some(evicted) = state.dead_letters.first() {
            if let Some(correlation_id) =
                normalized_non_empty(evicted.envelope.correlation_id.as_deref())
            {
                let mut remove_correlation_key = false;
                if let Some(entries) = state.dead_letters_by_correlation.get_mut(correlation_id) {
                    let _ = entries.pop_front();
                    remove_correlation_key = entries.is_empty();
                }
                if remove_correlation_key {
                    state.dead_letters_by_correlation.remove(correlation_id);
                }
            }
        }
        let _ = state.dead_letters.remove(0);
    }

    let dead_letter = DeadLetter { envelope, reason };
    if let Some(correlation_id) =
        normalized_non_empty(dead_letter.envelope.correlation_id.as_deref())
    {
        state
            .dead_letters_by_correlation
            .entry(correlation_id.to_string())
            .or_default()
            .push_back(dead_letter.clone());
    }
    state.dead_letters.push(dead_letter);
}

fn apply_context_patch_locked(
    state: &mut BusState,
    envelope: &CoordinationEnvelope,
    key: &str,
    expected_version: u64,
    value: &Value,
) -> Result<(), CoordinationError> {
    let key_delegate_correlation = if key.starts_with("delegate/") {
        let parsed = parse_delegate_context_correlation_from_key(key).ok_or_else(|| {
            CoordinationError::InvalidDelegateContextKey {
                key: key.to_string(),
                message_id: envelope.id.clone(),
            }
        })?;
        let envelope_correlation = normalized_non_empty(envelope.correlation_id.as_deref())
            .ok_or_else(|| CoordinationError::MissingDelegateContextCorrelation {
                key: key.to_string(),
                message_id: envelope.id.clone(),
            })?;
        if parsed != envelope_correlation {
            return Err(CoordinationError::DelegateContextCorrelationMismatch {
                key: key.to_string(),
                message_id: envelope.id.clone(),
                key_correlation_id: parsed.to_string(),
                envelope_correlation_id: envelope_correlation.to_string(),
            });
        }
        Some(parsed)
    } else {
        None
    };

    let current_version = state.context.get(key).map_or(0, |entry| entry.version);
    if current_version != expected_version {
        return Err(CoordinationError::ContextVersionMismatch {
            key: key.to_string(),
            expected: expected_version,
            actual: current_version,
        });
    }

    let key_owned = key.to_string();
    let key_is_delegate = key_delegate_correlation.is_some();
    let previous_correlation = state.context_correlation_by_key.get(key).cloned();
    let is_new_key = !state.context.contains_key(key);
    if is_new_key && state.context.len() >= state.limits.max_context_entries {
        if let Some(evicted_key) = state.context_order.pop_front() {
            if state.context.remove(&evicted_key).is_some() {
                state.stats.context_evictions_total += 1;
            }
            let evicted_correlation = state.context_correlation_by_key.remove(&evicted_key);
            if let Some(correlation_id) = evicted_correlation.as_deref() {
                remove_key_from_context_correlation_order(state, correlation_id, &evicted_key);
            }
            if evicted_key.starts_with("delegate/") {
                remove_key_from_delegate_context_order(
                    state,
                    &evicted_key,
                    evicted_correlation.as_deref(),
                );
            }
        }
    }

    if !is_new_key {
        if let Some(position) = state
            .context_order
            .iter()
            .position(|existing| existing == key)
        {
            let _ = state.context_order.remove(position);
        }
    }
    state.context_order.push_back(key_owned.clone());

    if let Some(correlation_id) = previous_correlation.as_deref() {
        remove_key_from_context_correlation_order(state, correlation_id, key);
    }
    if key_is_delegate {
        remove_key_from_delegate_context_order(state, key, previous_correlation.as_deref());
        state.delegate_context_order.push_back(key_owned.clone());
    }
    if let Some(correlation_id) = normalized_non_empty(envelope.correlation_id.as_deref()) {
        state
            .context_order_by_correlation
            .entry(correlation_id.to_string())
            .or_default()
            .push_back(key_owned.clone());
        if key_is_delegate {
            state
                .delegate_context_order_by_correlation
                .entry(correlation_id.to_string())
                .or_default()
                .push_back(key_owned.clone());
        }
        state
            .context_correlation_by_key
            .insert(key_owned.clone(), correlation_id.to_string());
    } else {
        state.context_correlation_by_key.remove(&key_owned);
    }

    state.context.insert(
        key_owned.clone(),
        SharedContextEntry {
            key: key_owned,
            value: value.clone(),
            version: current_version + 1,
            updated_by: envelope.from.clone(),
            last_message_id: envelope.id.clone(),
        },
    );

    Ok(())
}

fn remove_key_from_context_correlation_order(
    state: &mut BusState,
    correlation_id: &str,
    key: &str,
) {
    let mut remove_correlation_key = false;
    if let Some(order) = state.context_order_by_correlation.get_mut(correlation_id) {
        if let Some(position) = order.iter().position(|existing| existing == key) {
            let _ = order.remove(position);
        }
        remove_correlation_key = order.is_empty();
    }
    if remove_correlation_key {
        state.context_order_by_correlation.remove(correlation_id);
    }
}

fn remove_key_from_delegate_context_order(
    state: &mut BusState,
    key: &str,
    correlation_id: Option<&str>,
) {
    if let Some(position) = state
        .delegate_context_order
        .iter()
        .position(|existing| existing == key)
    {
        let _ = state.delegate_context_order.remove(position);
    }

    let Some(correlation_id) = correlation_id else {
        return;
    };

    let mut remove_correlation_key = false;
    if let Some(order) = state
        .delegate_context_order_by_correlation
        .get_mut(correlation_id)
    {
        if let Some(position) = order.iter().position(|existing| existing == key) {
            let _ = order.remove(position);
        }
        remove_correlation_key = order.is_empty();
    }
    if remove_correlation_key {
        state
            .delegate_context_order_by_correlation
            .remove(correlation_id);
    }
}

fn normalized_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_delegate_context_correlation_from_key(key: &str) -> Option<&str> {
    let mut parts = key.splitn(3, '/');
    let namespace = parts.next()?;
    if namespace != "delegate" {
        return None;
    }
    let correlation = parts.next()?.trim();
    if correlation.is_empty() {
        return None;
    }
    // Require at least one trailing segment (e.g. delegate/<corr>/state).
    let tail = parts.next()?.trim();
    if tail.is_empty() {
        return None;
    }
    Some(correlation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;
    use tokio::sync::Barrier;

    #[test]
    fn delegate_task_requires_direct_target() {
        let envelope = CoordinationEnvelope {
            id: "msg-1".to_string(),
            conversation_id: "conv-1".to_string(),
            correlation_id: None,
            causation_id: None,
            from: "lead".to_string(),
            to: None,
            topic: "coordination".to_string(),
            scope: DeliveryScope::Direct,
            payload: CoordinationPayload::DelegateTask {
                task_id: "task-1".to_string(),
                summary: "Investigate bug".to_string(),
                metadata: json!({}),
            },
        };

        let error = envelope
            .validate()
            .expect_err("target agent must be required");
        assert_eq!(
            error,
            CoordinationError::MissingTarget {
                message_id: "msg-1".to_string()
            }
        );
    }

    #[test]
    fn task_result_requires_correlation_id() {
        let envelope = CoordinationEnvelope {
            id: "msg-2".to_string(),
            conversation_id: "conv-1".to_string(),
            correlation_id: None,
            causation_id: None,
            from: "worker".to_string(),
            to: Some("lead".to_string()),
            topic: "coordination".to_string(),
            scope: DeliveryScope::Direct,
            payload: CoordinationPayload::TaskResult {
                task_id: "task-1".to_string(),
                success: true,
                output: "done".to_string(),
            },
        };

        let error = envelope
            .validate()
            .expect_err("task result must require correlation");
        assert_eq!(
            error,
            CoordinationError::MissingCorrelationId {
                message_id: "msg-2".to_string()
            }
        );
    }

    #[test]
    fn json_roundtrip_keeps_payload_shape() {
        let mut envelope = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-1",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "task-1".to_string(),
                summary: "Analyze logs".to_string(),
                metadata: json!({"priority": "high"}),
            },
        );
        envelope.correlation_id = Some("corr-1".to_string());

        let encoded = serde_json::to_string(&envelope).expect("serialize envelope");
        let decoded: CoordinationEnvelope =
            serde_json::from_str(&encoded).expect("deserialize envelope");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn duplicate_message_ids_are_rejected_and_dead_lettered() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker").expect("register worker");

        let mut envelope = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-1",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "task-1".to_string(),
                summary: "Investigate".to_string(),
                metadata: json!({}),
            },
        );
        envelope.id = "fixed-id".to_string();

        let first = bus.publish(envelope.clone()).expect("first publish");
        assert_eq!(first.delivered_to, 1);

        let second = bus.publish(envelope).expect_err("duplicate id must fail");
        assert_eq!(
            second,
            CoordinationError::DuplicateMessageId {
                message_id: "fixed-id".to_string()
            }
        );

        let dead_letters = bus.dead_letters();
        assert_eq!(dead_letters.len(), 1);
        assert!(dead_letters[0].reason.contains("duplicate message id"));

        let stats = bus.stats();
        assert_eq!(stats.seen_message_id_evictions_total, 0);
    }

    #[test]
    fn dedupe_window_evicts_old_ids_and_allows_reuse_after_eviction() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 32,
            max_dead_letters: 32,
            max_context_entries: 32,
            max_seen_message_ids: 2,
        });
        bus.register_agent("worker").expect("register worker");

        for message_id in ["msg-0", "msg-1", "msg-2"] {
            let mut envelope = CoordinationEnvelope::new_direct(
                "lead",
                "worker",
                "conv-dedupe-window",
                "coordination",
                CoordinationPayload::DelegateTask {
                    task_id: message_id.to_string(),
                    summary: "Investigate".to_string(),
                    metadata: json!({}),
                },
            );
            envelope.id = message_id.to_string();
            bus.publish(envelope).expect("publish should succeed");
        }

        // `msg-0` has been evicted from dedupe window and can be reused.
        let mut reused = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-dedupe-window",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "msg-0".to_string(),
                summary: "Investigate again".to_string(),
                metadata: json!({}),
            },
        );
        reused.id = "msg-0".to_string();
        bus.publish(reused)
            .expect("reused id should be accepted after eviction");

        // Recent IDs are still protected by dedupe window.
        let mut duplicate_recent = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-dedupe-window",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "msg-2".to_string(),
                summary: "duplicate".to_string(),
                metadata: json!({}),
            },
        );
        duplicate_recent.id = "msg-2".to_string();
        let error = bus
            .publish(duplicate_recent)
            .expect_err("recent duplicate should be rejected");
        assert_eq!(
            error,
            CoordinationError::DuplicateMessageId {
                message_id: "msg-2".to_string()
            }
        );

        let stats = bus.stats();
        assert_eq!(stats.seen_message_id_evictions_total, 2);
    }

    #[test]
    fn context_patch_conflict_goes_to_dead_letter() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("lead").expect("register lead");

        let first_patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-ctx",
            "context",
            CoordinationPayload::ContextPatch {
                key: "task-99/state".to_string(),
                expected_version: 0,
                value: json!({"phase": "started"}),
            },
        );
        bus.publish(first_patch).expect("first patch must succeed");

        let stale_patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-ctx",
            "context",
            CoordinationPayload::ContextPatch {
                key: "task-99/state".to_string(),
                expected_version: 0,
                value: json!({"phase": "stale"}),
            },
        );
        let error = bus
            .publish(stale_patch)
            .expect_err("stale expected_version must fail");
        assert_eq!(
            error,
            CoordinationError::ContextVersionMismatch {
                key: "task-99/state".to_string(),
                expected: 0,
                actual: 1
            }
        );

        let entry = bus
            .context_entry("task-99/state")
            .expect("context entry must exist");
        assert_eq!(entry.version, 1);
        assert_eq!(entry.value, json!({"phase": "started"}));
        assert_eq!(bus.dead_letters().len(), 1);
    }

    #[test]
    fn delegate_context_patch_requires_correlation_id() {
        let bus = InMemoryMessageBus::new();

        let mut patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context-correlation",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch.id = "msg-delegate-corr-required".to_string();
        let error = bus
            .publish(patch)
            .expect_err("delegate context patch without correlation must fail");
        assert_eq!(
            error,
            CoordinationError::MissingDelegateContextCorrelation {
                key: "delegate/corr-a/state".to_string(),
                message_id: "msg-delegate-corr-required".to_string(),
            }
        );
        assert_eq!(bus.dead_letter_count(), 1);
    }

    #[test]
    fn delegate_context_patch_rejects_mismatched_correlation_id() {
        let bus = InMemoryMessageBus::new();

        let mut patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context-correlation-mismatch",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch.id = "msg-delegate-corr-mismatch".to_string();
        patch.correlation_id = Some("corr-b".to_string());
        let error = bus
            .publish(patch)
            .expect_err("delegate context patch with mismatch must fail");
        assert_eq!(
            error,
            CoordinationError::DelegateContextCorrelationMismatch {
                key: "delegate/corr-a/state".to_string(),
                message_id: "msg-delegate-corr-mismatch".to_string(),
                key_correlation_id: "corr-a".to_string(),
                envelope_correlation_id: "corr-b".to_string(),
            }
        );
        assert_eq!(bus.dead_letter_count(), 1);
    }

    #[test]
    fn delegate_context_patch_rejects_invalid_delegate_key_shape() {
        let bus = InMemoryMessageBus::new();

        let mut patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context-key-shape",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch.id = "msg-delegate-key-shape".to_string();
        patch.correlation_id = Some("corr-a".to_string());
        let error = bus
            .publish(patch)
            .expect_err("delegate context patch with invalid key shape must fail");
        assert_eq!(
            error,
            CoordinationError::InvalidDelegateContextKey {
                key: "delegate/corr-a".to_string(),
                message_id: "msg-delegate-key-shape".to_string(),
            }
        );
        assert_eq!(bus.dead_letter_count(), 1);
    }

    #[test]
    fn delegate_context_patch_rejects_empty_tail_segment() {
        let bus = InMemoryMessageBus::new();

        let mut patch = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context-key-tail",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch.id = "msg-delegate-key-tail".to_string();
        patch.correlation_id = Some("corr-a".to_string());
        let error = bus
            .publish(patch)
            .expect_err("delegate context patch with empty tail must fail");
        assert_eq!(
            error,
            CoordinationError::InvalidDelegateContextKey {
                key: "delegate/corr-a/".to_string(),
                message_id: "msg-delegate-key-tail".to_string(),
            }
        );
        assert_eq!(bus.dead_letter_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_publish_keeps_inbox_order() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("lead").expect("register lead");
        bus.register_agent("worker").expect("register worker");

        let total = 32usize;
        let barrier = Arc::new(Barrier::new(total));
        let mut tasks = Vec::with_capacity(total);

        for index in 0..total {
            let bus_clone = bus.clone();
            let barrier_clone = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier_clone.wait().await;
                let mut envelope = CoordinationEnvelope::new_direct(
                    "lead",
                    "worker",
                    "conv-concurrent",
                    "coordination",
                    CoordinationPayload::DelegateTask {
                        task_id: format!("task-{index}"),
                        summary: format!("work-{index}"),
                        metadata: json!({"idx": index}),
                    },
                );
                envelope.id = format!("msg-{index}");
                bus_clone.publish(envelope).expect("publish").sequence
            }));
        }

        let mut published_sequences = Vec::with_capacity(total);
        for handle in tasks {
            published_sequences.push(handle.await.expect("join"));
        }
        assert_eq!(published_sequences.len(), total);

        let drained = bus
            .drain_for_agent("worker", 0)
            .expect("drain worker inbox should succeed");
        assert_eq!(drained.len(), total);

        for pair in drained.windows(2) {
            assert!(pair[0].sequence < pair[1].sequence);
        }

        let mut seen_tasks = HashSet::new();
        for item in drained {
            if let CoordinationPayload::DelegateTask { task_id, .. } = item.envelope.payload {
                seen_tasks.insert(task_id);
            }
        }
        assert_eq!(seen_tasks.len(), total);
    }

    #[test]
    fn multi_agent_delegation_flow_updates_context_and_returns_result() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("lead").expect("register lead");
        bus.register_agent("researcher")
            .expect("register researcher");

        let mut request = CoordinationEnvelope::new_direct(
            "lead",
            "researcher",
            "conv-42",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "task-42".to_string(),
                summary: "Find root cause".to_string(),
                metadata: json!({"priority": "p1"}),
            },
        );
        request.id = "msg-request".to_string();
        request.correlation_id = Some("corr-42".to_string());
        bus.publish(request.clone())
            .expect("request should publish");

        let researcher_inbox = bus
            .drain_for_agent("researcher", 10)
            .expect("researcher drain");
        assert_eq!(researcher_inbox.len(), 1);
        assert_eq!(researcher_inbox[0].envelope.id, "msg-request");

        let mut patch = CoordinationEnvelope::new_broadcast(
            "researcher",
            "conv-42",
            "context",
            CoordinationPayload::ContextPatch {
                key: "task-42/findings".to_string(),
                expected_version: 0,
                value: json!({"summary": "Root cause isolated"}),
            },
        );
        patch.id = "msg-patch".to_string();
        patch.correlation_id = Some("corr-42".to_string());
        patch.causation_id = Some("msg-request".to_string());
        bus.publish(patch).expect("context patch should publish");

        let mut result = CoordinationEnvelope::new_direct(
            "researcher",
            "lead",
            "conv-42",
            "coordination",
            CoordinationPayload::TaskResult {
                task_id: "task-42".to_string(),
                success: true,
                output: "Investigation complete".to_string(),
            },
        );
        result.id = "msg-result".to_string();
        result.correlation_id = Some("corr-42".to_string());
        result.causation_id = Some("msg-request".to_string());
        bus.publish(result).expect("result should publish");

        let lead_inbox = bus.drain_for_agent("lead", 10).expect("lead drain");
        assert_eq!(lead_inbox.len(), 2);
        assert_eq!(lead_inbox[0].envelope.id, "msg-patch");
        assert_eq!(lead_inbox[1].envelope.id, "msg-result");

        let context = bus
            .context_entry("task-42/findings")
            .expect("context must exist");
        assert_eq!(context.version, 1);
        assert_eq!(context.updated_by, "researcher");
        assert_eq!(context.last_message_id, "msg-patch");
        assert_eq!(context.value, json!({"summary": "Root cause isolated"}));
    }

    #[test]
    fn peek_does_not_consume_messages() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker").expect("register worker");

        let mut envelope = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-peek",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "task-1".to_string(),
                summary: "peek test".to_string(),
                metadata: json!({}),
            },
        );
        envelope.id = "msg-peek".to_string();
        bus.publish(envelope).expect("publish");

        let peeked = bus.peek_for_agent("worker", 10).expect("peek");
        assert_eq!(peeked.len(), 1);
        assert_eq!(peeked[0].envelope.id, "msg-peek");

        let pending = bus.pending_for_agent("worker").expect("pending");
        assert_eq!(pending, 1);
    }

    #[test]
    fn correlation_pending_and_peek_paging_follow_inbox_lifecycle() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker").expect("register worker");

        for (message_id, correlation_id) in [
            ("msg-corr-0", "corr-a"),
            ("msg-corr-1", "corr-b"),
            ("msg-corr-2", "corr-a"),
            ("msg-corr-3", "corr-a"),
        ] {
            let mut envelope = CoordinationEnvelope::new_direct(
                "lead",
                "worker",
                "conv-peek-correlation",
                "coordination",
                CoordinationPayload::DelegateTask {
                    task_id: message_id.to_string(),
                    summary: "peek correlation".to_string(),
                    metadata: json!({}),
                },
            );
            envelope.id = message_id.to_string();
            envelope.correlation_id = Some(correlation_id.to_string());
            bus.publish(envelope).expect("publish should succeed");
        }

        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-a")
                .expect("pending corr-a should succeed"),
            3
        );
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-b")
                .expect("pending corr-b should succeed"),
            1
        );

        let page = bus
            .peek_for_agent_correlation_with_offset("worker", "corr-a", 1, 1)
            .expect("peek corr-a page should succeed");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].envelope.id, "msg-corr-2");

        let drained_one = bus
            .drain_for_agent("worker", 1)
            .expect("drain one should succeed");
        assert_eq!(drained_one.len(), 1);
        assert_eq!(drained_one[0].envelope.id, "msg-corr-0");
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-a")
                .expect("pending corr-a should succeed after drain"),
            2
        );
    }

    #[test]
    fn inbox_correlation_counts_stay_consistent_with_overflow_evictions() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 2,
            max_dead_letters: 16,
            max_context_entries: 16,
            max_seen_message_ids: 32,
        });
        bus.register_agent("worker").expect("register worker");

        for (id, corr) in [("m0", "corr-a"), ("m1", "corr-b"), ("m2", "corr-a")] {
            let mut envelope = CoordinationEnvelope::new_direct(
                "lead",
                "worker",
                "conv-overflow-corr",
                "coordination",
                CoordinationPayload::DelegateTask {
                    task_id: id.to_string(),
                    summary: "overflow".to_string(),
                    metadata: json!({}),
                },
            );
            envelope.id = id.to_string();
            envelope.correlation_id = Some(corr.to_string());
            bus.publish(envelope).expect("publish should succeed");
        }

        // m0 (corr-a) should be evicted by inbox overflow.
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-a")
                .expect("corr-a pending should work"),
            1
        );
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-b")
                .expect("corr-b pending should work"),
            1
        );

        let corr_a_page = bus
            .peek_for_agent_correlation_with_offset("worker", "corr-a", 0, 10)
            .expect("corr-a peek should work");
        assert_eq!(corr_a_page.len(), 1);
        assert_eq!(corr_a_page[0].envelope.id, "m2");

        let corr_b_page = bus
            .peek_for_agent_correlation_with_offset("worker", "corr-b", 0, 10)
            .expect("corr-b peek should work");
        assert_eq!(corr_b_page.len(), 1);
        assert_eq!(corr_b_page[0].envelope.id, "m1");
    }

    #[test]
    fn correlation_peek_normalizes_whitespace_in_message_correlation_id() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker").expect("register worker");

        let mut envelope = CoordinationEnvelope::new_direct(
            "lead",
            "worker",
            "conv-corr-normalize",
            "coordination",
            CoordinationPayload::DelegateTask {
                task_id: "task-1".to_string(),
                summary: "normalize".to_string(),
                metadata: json!({}),
            },
        );
        envelope.id = "msg-corr-whitespace".to_string();
        envelope.correlation_id = Some(" corr-a ".to_string());
        bus.publish(envelope).expect("publish should succeed");

        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-a")
                .expect("pending by normalized correlation should succeed"),
            1
        );
        let page = bus
            .peek_for_agent_correlation_with_offset("worker", "corr-a", 0, 10)
            .expect("peek by normalized correlation should succeed");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].envelope.id, "msg-corr-whitespace");
    }

    #[test]
    fn registered_agents_and_context_snapshot_are_available() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker-b").expect("register worker-b");
        bus.register_agent("worker-a").expect("register worker-a");

        let patch = CoordinationEnvelope::new_broadcast(
            "worker-a",
            "conv-snapshot",
            "context",
            CoordinationPayload::ContextPatch {
                key: "shared/key".to_string(),
                expected_version: 0,
                value: json!({"ok": true}),
            },
        );
        bus.publish(patch).expect("publish patch");

        let agents = bus.registered_agents();
        assert_eq!(agents, vec!["worker-a".to_string(), "worker-b".to_string()]);

        let snapshot = bus.context_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            snapshot
                .get("shared/key")
                .expect("shared key should exist")
                .value,
            json!({"ok": true})
        );
    }

    #[test]
    fn inbox_limit_drops_oldest_and_records_dead_letter() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 2,
            max_dead_letters: 8,
            max_context_entries: 16,
            max_seen_message_ids: 32,
        });
        bus.register_agent("worker").expect("register worker");

        for index in 0..3 {
            let mut envelope = CoordinationEnvelope::new_direct(
                "lead",
                "worker",
                "conv-limit",
                "coordination",
                CoordinationPayload::DelegateTask {
                    task_id: format!("task-{index}"),
                    summary: format!("work-{index}"),
                    metadata: json!({}),
                },
            );
            envelope.id = format!("msg-limit-{index}");
            envelope.correlation_id = Some("corr-limit".to_string());
            bus.publish(envelope).expect("publish should succeed");
        }

        let pending = bus
            .pending_for_agent("worker")
            .expect("pending should work");
        assert_eq!(pending, 2);
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-limit")
                .expect("pending by correlation should work"),
            2
        );

        let drained = bus.drain_for_agent("worker", 0).expect("drain should work");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].envelope.id, "msg-limit-1");
        assert_eq!(drained[1].envelope.id, "msg-limit-2");
        assert_eq!(
            bus.pending_for_agent_correlation("worker", "corr-limit")
                .expect("pending by correlation after drain should work"),
            0
        );

        let dead_letters = bus.dead_letters();
        assert_eq!(dead_letters.len(), 1);
        assert_eq!(dead_letters[0].envelope.id, "msg-limit-0");
        assert!(dead_letters[0].reason.contains("inbox overflow"));

        let stats = bus.stats();
        assert_eq!(stats.publish_attempts_total, 3);
        assert_eq!(stats.deliveries_total, 3);
        assert_eq!(stats.inbox_overflow_evictions_total, 1);
        assert_eq!(stats.dead_letters_total, 1);
        assert_eq!(stats.dead_letter_evictions_total, 0);
        assert_eq!(stats.context_evictions_total, 0);
        assert_eq!(stats.seen_message_id_evictions_total, 0);
    }

    #[test]
    fn dead_letter_limit_is_capped() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 16,
            max_dead_letters: 2,
            max_context_entries: 16,
            max_seen_message_ids: 32,
        });
        bus.register_agent("worker").expect("register worker");

        for index in 0..4 {
            let mut invalid = CoordinationEnvelope::new_direct(
                "worker",
                "worker",
                "conv-dead-letter-limit",
                "coordination",
                CoordinationPayload::TaskResult {
                    task_id: format!("task-{index}"),
                    success: false,
                    output: "failed".to_string(),
                },
            );
            invalid.id = format!("msg-dead-{index}");
            // Missing correlation id -> dead-letter.
            let _ = bus.publish(invalid);
        }

        let dead_letters = bus.dead_letters();
        assert_eq!(dead_letters.len(), 2);
        assert_eq!(dead_letters[0].envelope.id, "msg-dead-2");
        assert_eq!(dead_letters[1].envelope.id, "msg-dead-3");
        assert_eq!(bus.dead_letter_count(), 2);

        let stats = bus.stats();
        assert_eq!(stats.publish_attempts_total, 0);
        assert_eq!(stats.deliveries_total, 0);
        assert_eq!(stats.inbox_overflow_evictions_total, 0);
        assert_eq!(stats.dead_letters_total, 4);
        assert_eq!(stats.dead_letter_evictions_total, 2);
        assert_eq!(stats.context_evictions_total, 0);
        assert_eq!(stats.seen_message_id_evictions_total, 0);
    }

    #[test]
    fn context_limit_evicts_oldest_entries_and_tracks_stats() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 16,
            max_dead_letters: 16,
            max_context_entries: 2,
            max_seen_message_ids: 32,
        });

        for index in 0..3 {
            let mut patch = CoordinationEnvelope::new_broadcast(
                "lead",
                "conv-context-limit",
                "delegate.state",
                CoordinationPayload::ContextPatch {
                    key: format!("delegate/corr-{index}/state"),
                    expected_version: 0,
                    value: json!({"phase":"queued","index":index}),
                },
            );
            patch.id = format!("context-msg-{index}");
            patch.correlation_id = Some(format!("corr-{index}"));
            bus.publish(patch).expect("context patch should publish");
        }

        let snapshot = bus.context_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(!snapshot.contains_key("delegate/corr-0/state"));
        assert!(snapshot.contains_key("delegate/corr-1/state"));
        assert!(snapshot.contains_key("delegate/corr-2/state"));

        let stats = bus.stats();
        assert_eq!(stats.publish_attempts_total, 3);
        assert_eq!(stats.deliveries_total, 0);
        assert_eq!(stats.dead_letters_total, 0);
        assert_eq!(stats.context_evictions_total, 1);
        assert_eq!(stats.seen_message_id_evictions_total, 0);
    }

    #[test]
    fn context_limit_uses_write_recency_and_preserves_hot_keys() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 16,
            max_dead_letters: 16,
            max_context_entries: 2,
            max_seen_message_ids: 32,
        });

        let mut patch_a = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-context-lru",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_a.id = "ctx-lru-a0".to_string();
        patch_a.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a).expect("first patch should publish");

        let mut patch_b = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-context-lru",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-b/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_b.id = "ctx-lru-b0".to_string();
        patch_b.correlation_id = Some("corr-b".to_string());
        bus.publish(patch_b).expect("second patch should publish");

        // Update key A to make it the most recently written key.
        let mut patch_a_update = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-context-lru",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 1,
                value: json!({"phase":"running"}),
            },
        );
        patch_a_update.id = "ctx-lru-a1".to_string();
        patch_a_update.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_update)
            .expect("recency update patch should publish");

        let mut patch_c = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-context-lru",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-c/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_c.id = "ctx-lru-c0".to_string();
        patch_c.correlation_id = Some("corr-c".to_string());
        bus.publish(patch_c)
            .expect("new key should trigger eviction under limit");

        let snapshot = bus.context_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.contains_key("delegate/corr-a/state"));
        assert!(snapshot.contains_key("delegate/corr-c/state"));
        assert!(!snapshot.contains_key("delegate/corr-b/state"));
        assert_eq!(
            snapshot
                .get("delegate/corr-a/state")
                .expect("A key should remain")
                .version,
            2
        );

        let stats = bus.stats();
        assert_eq!(stats.context_evictions_total, 1);
        assert_eq!(stats.seen_message_id_evictions_total, 0);
    }

    #[test]
    fn context_entries_recent_with_offset_returns_newest_first_pages() {
        let bus = InMemoryMessageBus::new();
        for key in [
            "delegate/corr-a/state",
            "delegate/corr-b/state",
            "delegate/corr-c/state",
        ] {
            let mut patch = CoordinationEnvelope::new_broadcast(
                "lead",
                "conv-context-page",
                "delegate.state",
                CoordinationPayload::ContextPatch {
                    key: key.to_string(),
                    expected_version: 0,
                    value: json!({"phase":"queued"}),
                },
            );
            patch.id = format!("ctx-page-{key}");
            patch.correlation_id =
                parse_delegate_context_correlation_from_key(key).map(str::to_string);
            bus.publish(patch).expect("context patch should publish");
        }

        let page = bus.context_entries_recent_with_offset(1, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].0, "delegate/corr-b/state");
        assert_eq!(page[1].0, "delegate/corr-a/state");
    }

    #[test]
    fn dead_letters_recent_returns_newest_first_pages() {
        let bus = InMemoryMessageBus::new();
        bus.register_agent("worker").expect("register worker");

        for index in 0..4 {
            let mut invalid = CoordinationEnvelope::new_direct(
                "lead",
                "worker",
                "conv-dead-letter-page",
                "delegate.result",
                CoordinationPayload::TaskResult {
                    task_id: format!("task-{index}"),
                    success: false,
                    output: "failure".to_string(),
                },
            );
            invalid.id = format!("dead-page-{index}");
            // Missing correlation id causes dead-letter.
            let _ = bus.publish(invalid);
        }

        let page = bus.dead_letters_recent(1, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].envelope.id, "dead-page-2");
        assert_eq!(page[1].envelope.id, "dead-page-1");
    }

    #[test]
    fn context_entries_recent_for_correlation_support_paging_and_count() {
        let bus = InMemoryMessageBus::new();

        let mut patch_a_state = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-correlation-context",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_a_state.id = "ctx-corr-a-state-0".to_string();
        patch_a_state.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_state)
            .expect("corr-a state patch should publish");

        let mut patch_b_state = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-correlation-context",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-b/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        patch_b_state.id = "ctx-corr-b-state-0".to_string();
        patch_b_state.correlation_id = Some("corr-b".to_string());
        bus.publish(patch_b_state)
            .expect("corr-b state patch should publish");

        let mut patch_a_state_update = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-correlation-context",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 1,
                value: json!({"phase":"running"}),
            },
        );
        patch_a_state_update.id = "ctx-corr-a-state-1".to_string();
        patch_a_state_update.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_state_update)
            .expect("corr-a state update should publish");

        let mut patch_a_output = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-correlation-context",
            "delegate.output",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/output".to_string(),
                expected_version: 0,
                value: json!({"summary":"done"}),
            },
        );
        patch_a_output.id = "ctx-corr-a-output-0".to_string();
        patch_a_output.correlation_id = Some("corr-a".to_string());
        bus.publish(patch_a_output)
            .expect("corr-a output patch should publish");

        assert_eq!(bus.context_count_for_correlation("corr-a"), 2);
        assert_eq!(bus.context_count_for_correlation("corr-b"), 1);
        assert_eq!(bus.context_count_for_correlation("corr-missing"), 0);

        let page = bus.context_entries_recent_for_correlation_with_offset("corr-a", 0, 2);
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].0, "delegate/corr-a/output");
        assert_eq!(page[1].0, "delegate/corr-a/state");

        let second_page = bus.context_entries_recent_for_correlation_with_offset("corr-a", 1, 1);
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].0, "delegate/corr-a/state");
    }

    #[test]
    fn delegate_context_indexes_exclude_non_delegate_keys_and_support_paging() {
        let bus = InMemoryMessageBus::new();

        let mut delegate_a_state = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context",
            "delegate.state",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/state".to_string(),
                expected_version: 0,
                value: json!({"phase":"queued"}),
            },
        );
        delegate_a_state.id = "delegate-a-state-0".to_string();
        delegate_a_state.correlation_id = Some("corr-a".to_string());
        bus.publish(delegate_a_state)
            .expect("delegate a state patch should publish");

        let mut non_delegate = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context",
            "context",
            CoordinationPayload::ContextPatch {
                key: "shared/other".to_string(),
                expected_version: 0,
                value: json!({"k":"v"}),
            },
        );
        non_delegate.id = "shared-other-0".to_string();
        non_delegate.correlation_id = Some("corr-a".to_string());
        bus.publish(non_delegate)
            .expect("non-delegate patch should publish");

        let mut delegate_a_output = CoordinationEnvelope::new_broadcast(
            "lead",
            "conv-delegate-context",
            "delegate.output",
            CoordinationPayload::ContextPatch {
                key: "delegate/corr-a/output".to_string(),
                expected_version: 0,
                value: json!({"summary":"done"}),
            },
        );
        delegate_a_output.id = "delegate-a-output-0".to_string();
        delegate_a_output.correlation_id = Some("corr-a".to_string());
        bus.publish(delegate_a_output)
            .expect("delegate a output patch should publish");

        assert_eq!(bus.context_count(), 3);
        assert_eq!(bus.delegate_context_count(), 2);
        assert_eq!(bus.delegate_context_count_for_correlation("corr-a"), 2);
        assert_eq!(
            bus.delegate_context_count_for_correlation("corr-missing"),
            0
        );

        let all_delegate = bus.delegate_context_entries_recent_with_offset(0, 0);
        assert_eq!(all_delegate.len(), 2);
        assert_eq!(all_delegate[0].0, "delegate/corr-a/output");
        assert_eq!(all_delegate[1].0, "delegate/corr-a/state");

        let delegate_page =
            bus.delegate_context_entries_recent_for_correlation_with_offset("corr-a", 1, 1);
        assert_eq!(delegate_page.len(), 1);
        assert_eq!(delegate_page[0].0, "delegate/corr-a/state");
    }

    #[test]
    fn dead_letter_correlation_index_tracks_evictions_and_paging() {
        let bus = InMemoryMessageBus::with_limits(InMemoryMessageBusLimits {
            max_inbox_messages_per_agent: 16,
            max_dead_letters: 2,
            max_context_entries: 16,
            max_seen_message_ids: 32,
        });
        bus.register_agent("worker").expect("register worker");

        let publish_invalid_with_correlation = |message_id: &str, correlation_id: &str| {
            let mut envelope = CoordinationEnvelope::new_direct(
                "lead",
                "missing-worker",
                "conv-correlation-dead-letters",
                "delegate.request",
                CoordinationPayload::DelegateTask {
                    task_id: message_id.to_string(),
                    summary: "should dead-letter".to_string(),
                    metadata: json!({}),
                },
            );
            envelope.id = message_id.to_string();
            envelope.correlation_id = Some(correlation_id.to_string());
            let _ = bus.publish(envelope);
        };

        publish_invalid_with_correlation("dead-corr-a-0", "corr-a");
        publish_invalid_with_correlation("dead-corr-b-0", "corr-b");
        publish_invalid_with_correlation("dead-corr-a-1", "corr-a");

        assert_eq!(bus.dead_letter_count(), 2);
        assert_eq!(bus.dead_letter_count_for_correlation("corr-a"), 1);
        assert_eq!(bus.dead_letter_count_for_correlation("corr-b"), 1);
        assert_eq!(bus.dead_letter_count_for_correlation("corr-missing"), 0);

        let corr_a_page = bus.dead_letters_recent_for_correlation("corr-a", 0, 2);
        assert_eq!(corr_a_page.len(), 1);
        assert_eq!(corr_a_page[0].envelope.id, "dead-corr-a-1");

        let corr_a_offset_page = bus.dead_letters_recent_for_correlation("corr-a", 1, 2);
        assert!(corr_a_offset_page.is_empty());

        let corr_b_page = bus.dead_letters_recent_for_correlation("corr-b", 0, 2);
        assert_eq!(corr_b_page.len(), 1);
        assert_eq!(corr_b_page[0].envelope.id, "dead-corr-b-0");
    }
}
