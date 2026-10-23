//! Pure history projections. These values are disposable and never execute effects.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnStatus {
    Queued,
    Running,
    Committed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct TurnView {
    pub accepted_event_id: EventId,
    /// Derived ordering coordinate; EventId and map order are not chronological.
    pub accepted_sequence: u64,
    pub last_event_id: EventId,
    pub status: TurnStatus,
}

#[derive(Clone, Debug)]
pub struct InvocationView {
    pub started_event_id: EventId,
    pub last_event_id: EventId,
    pub branch_id: Option<BranchId>,
    pub turn_id: Option<TurnId>,
    pub status: InvocationStatus,
    /// Derived from the start event; never a persisted scheduling authority.
    pub background: bool,
}

#[derive(Clone, Debug)]
pub struct ContentView {
    pub event_id: EventId,
    pub content_ref: StoredPayloadRef,
    pub assistant: bool,
}

/// Reconstruct from a contiguous prefix. Unknown events remain visible as
/// holes in semantic coverage; callers must not treat this as complete context.
#[derive(Clone, Debug)]
pub struct HistoryProjection {
    pub source_cursor: EventCursor,
    pub turns: BTreeMap<TurnId, TurnView>,
    pub invocations: BTreeMap<InvocationId, InvocationView>,
    pub content: Vec<ContentView>,
    pub unknown_event_ids: Vec<EventId>,
}

impl HistoryProjection {
    pub fn empty(stream_id: SessionStreamId) -> Self {
        Self {
            source_cursor: EventCursor::beginning(stream_id),
            turns: BTreeMap::new(),
            invocations: BTreeMap::new(),
            content: Vec::new(),
            unknown_event_ids: Vec::new(),
        }
    }

    pub fn apply_page(&mut self, page: &EventPage) -> Result<(), ReadError> {
        self.apply_page_scope(page, None)
    }
    /// Settlement may start at this turn's admission while another turn's
    /// terminal rows are interleaved. Validate all coordinates, but reduce
    /// lifecycle facts only for the specified turn using the same reducer.
    pub fn apply_page_for_turn(
        &mut self,
        page: &EventPage,
        turn: &TurnId,
    ) -> Result<(), ReadError> {
        self.apply_page_scope(page, Some(turn))
    }
    fn apply_page_scope(
        &mut self,
        page: &EventPage,
        turn: Option<&TurnId>,
    ) -> Result<(), ReadError> {
        if self.source_cursor != page.after || page.stream_id != self.source_cursor.stream_id {
            return Err(ReadError::Gap(
                "projection prefix does not match page".into(),
            ));
        }
        // Stage only keys touched by this page. Copying the entire accumulated
        // projection on every page made bounded replay quadratic in history size.
        let mut next = Self::empty(self.source_cursor.stream_id.clone());
        next.source_cursor = self.source_cursor.clone();
        for event in page
            .events
            .iter()
            .filter(|event| turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn)))
        {
            if let Some(id) = &event.turn_id
                && let Some(view) = self.turns.get(id)
            {
                next.turns.insert(id.clone(), view.clone());
            }
            if let Some(id) = &event.invocation_id
                && let Some(view) = self.invocations.get(id)
            {
                next.invocations.insert(id.clone(), view.clone());
            }
        }
        for event in &page.events {
            if event.stream_id != next.source_cursor.stream_id
                || Some(event.sequence) != next.source_cursor.sequence.checked_add(1)
            {
                return Err(ReadError::Gap(
                    "projection received noncontiguous events".into(),
                ));
            }
            if turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn)) {
                next.apply(event)?;
            }
            next.source_cursor.sequence = event.sequence;
        }
        self.turns.extend(next.turns);
        self.invocations.extend(next.invocations);
        // Supersession affects pre-page content too; process these cheap,
        // infallible content operations only after the whole page validates.
        for event in page
            .events
            .iter()
            .filter(|event| turn.is_none_or(|turn| event.turn_id.as_ref() == Some(turn)))
        {
            self.apply_content(event);
        }
        self.source_cursor = next.source_cursor;
        Ok(())
    }

    /// Reduce one validated event for consumers that resolve structural
    /// payloads between events. The caller owns prefix validation.
    pub(crate) fn apply_event(&mut self, event: &SessionEvent) -> Result<(), ReadError> {
        self.apply(event)?;
        self.apply_content(event);
        self.source_cursor.sequence = event.sequence;
        Ok(())
    }

    fn apply_content(&mut self, event: &SessionEvent) {
        match &event.body.data {
            EventData::UserContentRecorded { content_ref, .. }
            | EventData::AssistantContentRecorded { content_ref, .. } => {
                self.content.push(ContentView {
                    event_id: event.event_id.clone(),
                    content_ref: content_ref.clone(),
                    assistant: event.body.kind() == EventKind::AssistantContentRecorded,
                })
            }
            EventData::ContentSuperseded {
                target_event_ids, ..
            } => self
                .content
                .retain(|item| !target_event_ids.contains(&item.event_id)),
            EventData::Unknown => self.unknown_event_ids.push(event.event_id.clone()),
            _ => {}
        }
    }

    fn apply(&mut self, event: &SessionEvent) -> Result<(), ReadError> {
        let kind = event.body.kind();
        if kind == EventKind::TurnAccepted {
            let id = event
                .turn_id
                .clone()
                .ok_or_else(|| ReadError::Corrupt("accepted turn missing identity".into()))?;
            self.turns.insert(
                id,
                TurnView {
                    accepted_event_id: event.event_id.clone(),
                    accepted_sequence: event.sequence,
                    last_event_id: event.event_id.clone(),
                    status: TurnStatus::Queued,
                },
            );
        } else if let Some(id) = &event.turn_id {
            let status = match kind {
                EventKind::TurnStarted => Some(TurnStatus::Running),
                EventKind::TurnCommitted => Some(TurnStatus::Committed),
                EventKind::TurnCancelled => Some(TurnStatus::Cancelled),
                EventKind::TurnOutcomeUnknown => Some(TurnStatus::Unknown),
                EventKind::RecoveryObserved => match &event.body.data {
                    EventData::RecoveryObserved {
                        disposition: RecoveryDisposition::Requeued,
                        ..
                    } => Some(TurnStatus::Queued),
                    EventData::RecoveryObserved {
                        disposition: RecoveryDisposition::Rescheduled { .. },
                        ..
                    } => Some(TurnStatus::Running),
                    _ => None,
                },
                _ => None,
            };
            if let Some(status) = status {
                let turn = self
                    .turns
                    .get_mut(id)
                    .ok_or_else(|| ReadError::Corrupt("turn lifecycle lacks admission".into()))?;
                turn.status = status;
                turn.last_event_id = event.event_id.clone();
            }
        }
        if let Some(id) = &event.invocation_id {
            if matches!(
                kind,
                EventKind::ProviderAttemptStarted
                    | EventKind::ToolEffectStarted
                    | EventKind::DeliveryStarted
                    | EventKind::ChildInvocationRequested
            ) {
                self.invocations.insert(
                    id.clone(),
                    InvocationView {
                        started_event_id: event.event_id.clone(),
                        last_event_id: event.event_id.clone(),
                        branch_id: event.branch_id.clone(),
                        turn_id: event.turn_id.clone(),
                        status: InvocationStatus::Running,
                        background: kind == EventKind::DeliveryStarted
                            || matches!(
                                &event.body.data,
                                EventData::ChildInvocationRequested {
                                    execution_mode: ChildExecutionMode::Background,
                                    ..
                                }
                            ),
                    },
                );
            } else if let Some(invocation) = self.invocations.get_mut(id) {
                let status = match kind {
                    EventKind::ProviderAttemptCompleted
                    | EventKind::ToolEffectCompleted
                    | EventKind::DeliveryCompleted
                    | EventKind::ChildInvocationCompleted => Some(InvocationStatus::Completed),
                    EventKind::ProviderAttemptFailed
                    | EventKind::ToolEffectFailed
                    | EventKind::DeliveryFailed
                    | EventKind::ChildInvocationRejected => Some(InvocationStatus::Failed),
                    EventKind::InvocationCancelled => Some(InvocationStatus::Cancelled),
                    EventKind::InvocationOutcomeUnknown => Some(InvocationStatus::Unknown),
                    EventKind::OutcomeReconciled => match &event.body.data {
                        EventData::OutcomeReconciled { resolution, .. } => Some(match resolution {
                            Reconciliation::Completed { .. } => InvocationStatus::Completed,
                            Reconciliation::Failed { .. } => InvocationStatus::Failed,
                            Reconciliation::Cancelled => InvocationStatus::Cancelled,
                        }),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(status) = status {
                    invocation.status = status;
                }
                invocation.last_event_id = event.event_id.clone();
            }
        }
        Ok(())
    }
}
