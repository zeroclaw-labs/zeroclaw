//! Pure, replayable historical admission rules. No live permissions or I/O.
use super::index::{OverlayMap, OverlaySet};
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct TurnState {
    stream: SessionStreamId,
    branch: BranchId,
    started: bool,
    terminal: bool,
    // Disposable index of the canonical, not-yet-consumed recovery marker.
    pending_requeue: Option<EventId>,
}
type InvocationScope = (SessionStreamId, Option<BranchId>, Option<TurnId>, bool);

/// Historical eligibility only. Restart still requires current authorization
/// and proof from the live execution owner that every previous writer stopped.
pub fn can_requeue_turn<'a>(
    history: impl IntoIterator<Item = &'a SessionEvent>,
    turn: &TurnId,
) -> bool {
    let mut accepted = false;
    for event in history {
        if event.turn_id.as_ref() != Some(turn) {
            continue;
        }
        match &event.body.data {
            EventData::TurnAccepted { .. } => accepted = true,
            EventData::UserContentRecorded { .. }
            | EventData::FileRegistered { .. }
            | EventData::FileAttached { .. }
            | EventData::TurnStarted { .. }
            | EventData::AuthorityRevisionObserved { .. }
            | EventData::ProviderRequestPrepared { .. }
            | EventData::RecoveryObserved { .. } => {}
            EventData::FileUsed { operation, .. }
                if operation == "ingress_representation_captured" => {}
            _ => return false,
        }
    }
    accepted
}

/// Disposable admission index. Its only inputs are canonical committed events;
/// it owns no permissions, scheduling decisions or writable session state.
#[derive(Default)]
pub struct HistoryIndex {
    pub(super) prior: HashMap<EventId, SessionEvent>,
    branches: HashMap<BranchId, SessionStreamId>,
    turns: HashMap<TurnId, TurnState>,
    invocations: HashMap<InvocationId, (EventKind, bool)>,
    invocation_scopes: HashMap<InvocationId, InvocationScope>,
    turn_invocations: HashMap<TurnId, Vec<InvocationId>>,
    turn_unknowns: HashMap<TurnId, Vec<EventId>>,
    by_cursor: HashMap<(SessionStreamId, u64), EventId>,
    child_derivations: HashMap<EventId, EventId>,
    start_events: HashMap<InvocationId, EventId>,
    files: HashSet<(SessionStreamId, String)>,
    transforms: HashMap<EventId, bool>,
    elicitations: HashMap<(SessionStreamId, String), (Option<BranchId>, bool)>,
    consumed: HashSet<String>,
    reconciled: HashSet<EventId>,
    streams: HashSet<SessionStreamId>,
    closed: HashSet<SessionStreamId>,
}
pub struct HistoryDelta {
    prior: HashMap<EventId, SessionEvent>,
    branches: HashMap<BranchId, SessionStreamId>,
    turns: HashMap<TurnId, TurnState>,
    invocations: HashMap<InvocationId, (EventKind, bool)>,
    invocation_scopes: HashMap<InvocationId, InvocationScope>,
    turn_invocations: HashMap<TurnId, Vec<InvocationId>>,
    turn_unknowns: HashMap<TurnId, Vec<EventId>>,
    by_cursor: HashMap<(SessionStreamId, u64), EventId>,
    child_derivations: HashMap<EventId, EventId>,
    start_events: HashMap<InvocationId, EventId>,
    files: HashSet<(SessionStreamId, String)>,
    transforms: HashMap<EventId, bool>,
    elicitations: HashMap<(SessionStreamId, String), (Option<BranchId>, bool)>,
    consumed: HashSet<String>,
    reconciled: HashSet<EventId>,
    streams: HashSet<SessionStreamId>,
    closed: HashSet<SessionStreamId>,
}
impl HistoryIndex {
    pub fn from_history(history: &[SessionEvent]) -> Result<Self, AppendError> {
        let mut index = Self::default();
        // Cross-stream histories can be supplied in stream order. All earlier
        // committed reference coordinates remain available during reconstruction.
        index.prior.extend(
            history
                .iter()
                .map(|event| (event.event_id.clone(), event.clone())),
        );
        let delta = index.stage(history.iter(), &[])?;
        // Prior already contains these committed bytes. Keep only lifecycle
        // deltas instead of cloning the complete history again on reopen.
        index.commit(delta, &[]);
        Ok(index)
    }
    pub fn plan(&self, request: &AppendRequest) -> Result<HistoryDelta, AppendError> {
        self.stage(request.events.iter(), &request.events)
    }
    /// Publish only after the storage transaction/manifest commits. Override
    /// proposed coordinates with the backend-assigned canonical coordinates.
    pub fn commit(&mut self, delta: HistoryDelta, committed: &[SessionEvent]) {
        self.prior.extend(delta.prior);
        self.prior.extend(
            committed
                .iter()
                .map(|event| (event.event_id.clone(), event.clone())),
        );
        self.branches.extend(delta.branches);
        self.turns.extend(delta.turns);
        self.invocations.extend(delta.invocations);
        self.invocation_scopes.extend(delta.invocation_scopes);
        self.turn_invocations.extend(delta.turn_invocations);
        self.turn_unknowns.extend(delta.turn_unknowns);
        self.files.extend(delta.files);
        self.by_cursor.extend(delta.by_cursor);
        self.child_derivations.extend(delta.child_derivations);
        self.start_events.extend(delta.start_events);
        self.by_cursor.extend(
            committed
                .iter()
                .filter(|event| event.sequence > 0)
                .map(|event| {
                    (
                        (event.stream_id.clone(), event.sequence),
                        event.event_id.clone(),
                    )
                }),
        );
        self.transforms.extend(delta.transforms);
        self.elicitations.extend(delta.elicitations);
        self.consumed.extend(delta.consumed);
        self.reconciled.extend(delta.reconciled);
        self.streams.extend(delta.streams);
        self.closed.extend(delta.closed);
    }
    pub fn get_event(&self, id: &EventId) -> Option<&SessionEvent> {
        self.prior.get(id)
    }
    pub(super) fn invocation_origin(&self, id: &InvocationId) -> Option<&SessionEvent> {
        self.start_events.get(id).and_then(|id| self.prior.get(id))
    }
    pub(super) fn child_derivation(&self, id: &EventId) -> Option<&SessionEvent> {
        self.child_derivations
            .get(id)
            .and_then(|id| self.prior.get(id))
    }
    pub fn extend_committed(&mut self, events: &[SessionEvent]) -> Result<(), AppendError> {
        let delta = self.stage(events.iter(), &[])?;
        self.commit(delta, events);
        Ok(())
    }
    pub fn events(&self) -> impl Iterator<Item = &SessionEvent> {
        self.prior.values()
    }
    fn stage<'a>(
        &self,
        events: impl Iterator<Item = &'a SessionEvent>,
        batch_events: &[SessionEvent],
    ) -> Result<HistoryDelta, AppendError> {
        let mut prior = OverlayMap::new(&self.prior);
        let mut branches = OverlayMap::new(&self.branches);
        let mut turns = OverlayMap::new(&self.turns);
        let mut invocations = OverlayMap::new(&self.invocations);
        let mut invocation_scopes = OverlayMap::new(&self.invocation_scopes);
        let mut transforms = OverlayMap::new(&self.transforms);
        let mut elicitations = OverlayMap::new(&self.elicitations);
        let mut turn_invocations = OverlayMap::new(&self.turn_invocations);
        let mut turn_unknowns = OverlayMap::new(&self.turn_unknowns);
        let mut by_cursor = OverlayMap::new(&self.by_cursor);
        let mut child_derivations = OverlayMap::new(&self.child_derivations);
        let mut start_events = OverlayMap::new(&self.start_events);
        let mut files = OverlaySet::new(&self.files);
        let mut consumed = OverlaySet::new(&self.consumed);
        let mut reconciled = OverlaySet::new(&self.reconciled);
        let mut streams = OverlaySet::new(&self.streams);
        let mut closed = OverlaySet::new(&self.closed);
        for event in events {
            let incoming = event.sequence == 0;
            if incoming {
                event.validate_for_append()?;
                if prior.contains_key(&event.event_id) {
                    return Err(invalid("event ID already exists"));
                }
                if let Some(cause) = &event.causation_id
                    && !prior.contains_key(cause)
                {
                    return Err(reference(
                        "causation must reference prior committed or batch event",
                    ));
                }
                if closed.contains(&event.stream_id)
                    && (matches!(
                        event.body.kind(),
                        EventKind::TurnAccepted
                            | EventKind::TurnStarted
                            | EventKind::ProviderAttemptStarted
                            | EventKind::ToolEffectStarted
                            | EventKind::ContextTransformStarted
                            | EventKind::ElicitationRequested
                    ) || matches!(
                        event.body.data,
                        EventData::RecoveryObserved {
                            disposition: RecoveryDisposition::Requeued
                                | RecoveryDisposition::Rescheduled { .. },
                            ..
                        }
                    ))
                {
                    return Err(invalid("closed stream rejects new execution"));
                }
                if !matches!(
                    event.body.kind(),
                    EventKind::SessionStarted | EventKind::DerivedSessionStarted
                ) && !streams.contains(&event.stream_id)
                {
                    return Err(invalid("stream has not been created"));
                }
                if requires_branch(&event.body.kind()) && event.branch_id.is_none() {
                    return Err(invalid("event requires branch binding"));
                }
                if requires_turn(&event.body.kind()) && event.turn_id.is_none() {
                    return Err(invalid("event requires turn binding"));
                }
                if event.body.kind() == EventKind::AssistantContentRecorded
                    && event.turn_id.is_none()
                    && event
                        .causation_id
                        .as_ref()
                        .and_then(|id| prior.get(id))
                        .is_none_or(|cause| {
                            cause.body.kind() != EventKind::ControlInputRecorded
                                || cause.stream_id != event.stream_id
                                || cause.branch_id != event.branch_id
                        })
                {
                    return Err(reference(
                        "idle content requires admitted control causation",
                    ));
                }
                if !matches!(
                    event.body.kind(),
                    EventKind::SessionStarted
                        | EventKind::DerivedSessionStarted
                        | EventKind::BranchCreated
                ) && let Some(branch) = &event.branch_id
                    && branches.get(branch) != Some(&event.stream_id)
                {
                    return Err(reference("branch does not belong to stream"));
                }
                if let Some(turn) = &event.turn_id {
                    if event.body.kind() != EventKind::TurnAccepted && !turns.contains_key(turn) {
                        return Err(reference("turn has not been accepted"));
                    }
                    if let Some(state) = turns.get(turn)
                        && (state.stream != event.stream_id
                            || event.branch_id.as_ref() != Some(&state.branch))
                    {
                        return Err(reference("turn is bound to another stream or branch"));
                    }
                    if turns
                        .get(turn)
                        .is_some_and(|state| state.pending_requeue.is_some())
                        && matches!(
                            event.body.kind(),
                            EventKind::ProviderAttemptStarted
                                | EventKind::ToolEffectStarted
                                | EventKind::ChildInvocationRequested
                                | EventKind::ContextTransformStarted
                        )
                    {
                        return Err(invalid(
                            "requeued turn must restart before effect admission",
                        ));
                    }
                }
                for (id, kinds, same_invocation) in event_refs(&event.body.data) {
                    let target = prior
                        .get(id)
                        .ok_or_else(|| reference("event role target is absent"))?;
                    if !kinds.is_empty() && !kinds.contains(&target.body.kind()) {
                        return Err(reference("event role target has incorrect kind"));
                    }
                    if target.stream_id != event.stream_id
                        && !matches!(
                            event.body.kind(),
                            EventKind::DerivedSessionStarted
                                | EventKind::ChildInvocationCompleted
                                | EventKind::ChildResultConsumed
                        )
                    {
                        return Err(reference("event role reference belongs to another stream"));
                    }
                    if same_invocation && target.invocation_id != event.invocation_id {
                        return Err(reference(
                            "event role reference belongs to another invocation",
                        ));
                    }
                    if target.turn_id.is_some()
                        && event.turn_id.is_some()
                        && target.turn_id != event.turn_id
                        && !matches!(
                            event.body.kind(),
                            EventKind::ContextTransformStarted
                                | EventKind::ContextProjectionCreated
                                | EventKind::ProviderRequestPrepared
                                | EventKind::ContentSuperseded
                                | EventKind::ChildResultConsumed
                                | EventKind::RecoveryObserved
                                | EventKind::SessionModelOverrideChanged
                        )
                    {
                        return Err(reference("event role reference belongs to another turn"));
                    }
                }
                validate_coordinates(
                    event,
                    &prior,
                    &branches,
                    &by_cursor,
                    &child_derivations,
                    &start_events,
                )?;
                if let Some(id) = &event.invocation_id
                    && let Some((stream, branch, turn, background)) = invocation_scopes.get(id)
                {
                    if stream != &event.stream_id
                        || branch != &event.branch_id
                        || (turn != &event.turn_id
                            && event.body.kind() != EventKind::ChildResultConsumed)
                    {
                        return Err(reference("invocation execution scope changed"));
                    }
                    if !background
                        && event
                            .turn_id
                            .as_ref()
                            .and_then(|turn| turns.get(turn))
                            .is_some_and(|state| state.terminal)
                        && matches!(
                            event.body.kind(),
                            EventKind::ProviderResponseProgressObserved
                                | EventKind::ProviderAttemptCompleted
                                | EventKind::ProviderAttemptFailed
                                | EventKind::ToolEffectProgressObserved
                                | EventKind::ToolEffectCompleted
                                | EventKind::ToolEffectFailed
                        )
                    {
                        return Err(invalid("late foreground effect requires reconciliation"));
                    }
                }
                match &event.body.data {
                    EventData::ControlInputRecorded { control_ref, .. }
                        if control_ref.codec == "session.approval-reply/v1"
                            && event
                                .turn_id
                                .as_ref()
                                .and_then(|turn| turns.get(turn))
                                .is_none_or(|state| {
                                    !state.started
                                        || state.terminal
                                        || state.pending_requeue.is_some()
                                }) =>
                    {
                        return Err(invalid("approval reply requires a running turn"));
                    }
                    EventData::ProviderRequestPrepared {
                        purpose: ProviderPurpose::Main,
                        ..
                    } if event
                        .turn_id
                        .as_ref()
                        .and_then(|turn| turns.get(turn))
                        .is_none_or(|state| {
                            !state.started || state.terminal || state.pending_requeue.is_some()
                        }) =>
                    {
                        return Err(invalid("main provider request requires a running turn"));
                    }
                    EventData::ProviderRequestPrepared {
                        purpose: ProviderPurpose::Summary | ProviderPurpose::ContextRepair,
                        ..
                    } if event
                        .causation_id
                        .as_ref()
                        .and_then(|id| prior.get(id))
                        .is_none_or(|cause| {
                            cause.body.kind() != EventKind::ContextTransformStarted
                        }) =>
                    {
                        return Err(reference(
                            "maintenance request requires transform causation",
                        ));
                    }
                    EventData::ToolEffectStarted { .. }
                        if event
                            .turn_id
                            .as_ref()
                            .and_then(|turn| turns.get(turn))
                            .is_none_or(|state| {
                                !state.started || state.terminal || state.pending_requeue.is_some()
                            }) =>
                    {
                        return Err(invalid("tool start requires a running turn"));
                    }
                    EventData::ElicitationRequested { .. }
                        if event.turn_id.is_some()
                            && event
                                .turn_id
                                .as_ref()
                                .and_then(|turn| turns.get(turn))
                                .is_none_or(|state| {
                                    !state.started
                                        || state.terminal
                                        || state.pending_requeue.is_some()
                                }) =>
                    {
                        return Err(invalid("turn elicitation requires a running turn"));
                    }
                    EventData::SessionModelOverrideChanged {
                        resolution:
                            resolution @ (ModelResolution::Applied { .. }
                            | ModelResolution::AppliedForTurn),
                    } if (event.turn_id.is_some()
                        || matches!(resolution, ModelResolution::AppliedForTurn))
                        && event
                            .turn_id
                            .as_ref()
                            .and_then(|turn| turns.get(turn))
                            .is_none_or(|state| {
                                !state.started || state.terminal || state.pending_requeue.is_some()
                            }) =>
                    {
                        return Err(invalid("turn model selection requires a running turn"));
                    }
                    EventData::SessionModelOverrideChanged { .. }
                        if event
                            .causation_id
                            .as_ref()
                            .and_then(|id| prior.get(id))
                            .is_none_or(|cause| {
                                cause.body.kind() != EventKind::ControlInputRecorded
                                    || cause.stream_id != event.stream_id
                                    || cause.branch_id != event.branch_id
                                    || cause.turn_id != event.turn_id
                            }) =>
                    {
                        return Err(reference(
                            "model selection requires admitted control causation",
                        ));
                    }
                    EventData::FileAttached { file_id, .. }
                    | EventData::FileUsed { file_id, .. }
                        if !files.contains(&(event.stream_id.clone(), file_id.clone())) =>
                    {
                        return Err(reference(
                            "file use requires registered FileId in session scope",
                        ));
                    }
                    _ => {}
                }
            }
            match &event.body.data {
                EventData::ElicitationRequested {
                    request_id,
                    expires_at,
                    session_incarnation,
                    ..
                } => {
                    if request_id.is_empty()
                        || session_incarnation.is_empty()
                        || *expires_at <= event.occurred_at
                    {
                        return Err(invalid("invalid elicitation request"));
                    }
                    let key = (event.stream_id.clone(), request_id.clone());
                    if elicitations.contains_key(&key) {
                        return Err(invalid("elicitation request identity already exists"));
                    }
                    elicitations.insert(key, (event.branch_id.clone(), false));
                }
                EventData::ElicitationResolved { request_id, .. } => {
                    let state = elicitations
                        .get_mut(&(event.stream_id.clone(), request_id.clone()))
                        .ok_or_else(|| invalid("elicitation resolution has no request"))?;
                    if state.1 || state.0 != event.branch_id {
                        return Err(invalid(
                            "elicitation resolution is duplicate or belongs to another branch",
                        ));
                    }
                    state.1 = true;
                }
                _ => {}
            }
            if let EventData::FileRegistered { file_id, .. } = &event.body.data {
                if incoming && files.contains(&(event.stream_id.clone(), file_id.clone())) {
                    return Err(invalid("file identity already registered"));
                }
                files.insert((event.stream_id.clone(), file_id.clone()));
            }
            match &event.body.kind() {
                EventKind::SessionStarted | EventKind::DerivedSessionStarted => {
                    if incoming && streams.contains(&event.stream_id) {
                        return Err(invalid("stream already created"));
                    }
                    if incoming && event.body.kind() == EventKind::DerivedSessionStarted {
                        let EventData::DerivedSessionStarted {
                            execution_turn_id: Some(turn),
                        } = &event.body.data
                        else {
                            return Err(reference("child derivation requires execution binding"));
                        };
                        TurnId::parse(turn.as_str())?;
                        if turns.contains_key(turn) || child_derivations.values().filter_map(|id| prior.get(id)).any(|item| matches!(&item.body.data, EventData::DerivedSessionStarted { execution_turn_id: Some(bound) } if bound == turn)) {
                            return Err(invalid("child execution identity already bound"));
                        }
                        let parent = event
                            .causation_id
                            .as_ref()
                            .and_then(|id| prior.get(id))
                            .ok_or_else(|| reference("derived stream requires parent request"))?;
                        if parent.body.kind() != EventKind::ChildInvocationRequested {
                            return Err(reference("invalid child origin"));
                        }
                        if child_derivations.contains_key(&parent.event_id) {
                            return Err(invalid("parent request already created a child"));
                        }
                    }
                    streams.insert(event.stream_id.clone());
                    if let Some(branch) = &event.branch_id {
                        if incoming && branches.contains_key(branch) {
                            return Err(invalid("branch identity already exists"));
                        }
                        branches.insert(branch.clone(), event.stream_id.clone());
                    }
                }
                EventKind::BranchCreated => {
                    if let Some(branch) = &event.branch_id {
                        if incoming && branches.contains_key(branch) {
                            return Err(invalid("branch already exists"));
                        }
                        branches.insert(branch.clone(), event.stream_id.clone());
                    }
                }
                EventKind::TurnAccepted => {
                    if let Some(turn) = &event.turn_id {
                        if incoming && turns.contains_key(turn) {
                            return Err(invalid("turn already accepted"));
                        }
                        let branch = event
                            .branch_id
                            .clone()
                            .ok_or_else(|| reference("accepted turn missing branch"))?;
                        if incoming && child_derivations.values().filter_map(|id| prior.get(id)).any(|item| matches!(&item.body.data, EventData::DerivedSessionStarted { execution_turn_id: Some(bound) } if bound == turn)
                            && (item.stream_id != event.stream_id || item.branch_id.as_ref() != Some(&branch))) {
                            return Err(reference("child execution is bound to another stream or branch"));
                        }
                        turns.insert(
                            turn.clone(),
                            TurnState {
                                stream: event.stream_id.clone(),
                                branch,
                                started: false,
                                terminal: false,
                                pending_requeue: None,
                            },
                        );
                    }
                }
                EventKind::TurnStarted => {
                    if let Some(turn) = &event.turn_id {
                        let state = turns
                            .get_mut(turn)
                            .ok_or_else(|| reference("turn has not been accepted"))?;
                        if incoming && (state.started || state.terminal) {
                            return Err(invalid("turn cannot start twice or after terminal"));
                        }
                        state.started = true;
                        state.pending_requeue = None;
                    }
                }
                EventKind::RecoveryObserved => {
                    if let Some(turn) = &event.turn_id {
                        let state = turns
                            .get_mut(turn)
                            .ok_or_else(|| reference("turn has not been accepted"))?;
                        match &event.body.data {
                            EventData::RecoveryObserved {
                                disposition: RecoveryDisposition::Requeued,
                                ..
                            } => {
                                if incoming && state.terminal {
                                    return Err(invalid("terminal turn cannot requeue"));
                                }
                                state.pending_requeue = Some(event.event_id.clone());
                            }
                            EventData::RecoveryObserved {
                                disposition: RecoveryDisposition::Rescheduled { .. },
                                ..
                            } => {
                                if incoming
                                    && (!state.started
                                        || state.terminal
                                        || state.pending_requeue.as_ref()
                                            != event.causation_id.as_ref()
                                        || state.pending_requeue.is_none())
                                {
                                    return Err(invalid(
                                        "reschedule requires a started turn and latest requeue marker",
                                    ));
                                }
                                state.pending_requeue = None;
                            }
                            _ => {}
                        }
                    }
                }
                EventKind::TurnCommitted
                | EventKind::TurnCancelled
                | EventKind::TurnOutcomeUnknown => {
                    if let Some(turn) = &event.turn_id {
                        let state = turns
                            .get_mut(turn)
                            .ok_or_else(|| reference("turn has not been accepted"))?;
                        if incoming && state.terminal {
                            return Err(invalid("turn has a terminal outcome"));
                        }
                        if incoming
                            && event.body.kind() == EventKind::TurnOutcomeUnknown
                            && !state.started
                        {
                            return Err(invalid("unknown turn must have started"));
                        }
                        if incoming
                            && turn_invocations.get(turn).into_iter().flatten().any(|id| {
                                invocations.get(id).is_some_and(|(_, terminal)| !terminal)
                                    && invocation_scopes.get(id).is_some_and(|scope| !scope.3)
                            })
                        {
                            return Err(invalid(
                                "turn cannot settle with running foreground effects",
                            ));
                        }
                        if incoming {
                            let declared = match &event.body.data {
                                EventData::TurnOutcomeUnknown { invocation_ids, .. } => {
                                    Some(invocation_ids)
                                }
                                EventData::TurnCancelled {
                                    unresolved_invocation_ids,
                                    ..
                                } => Some(unresolved_invocation_ids),
                                _ => None,
                            };
                            if let Some(declared) = declared {
                                let actual: HashSet<_> = turn_unknowns
                                    .get(turn)
                                    .into_iter()
                                    .flatten()
                                    .filter(|id| !reconciled.contains(id))
                                    .filter_map(|id| prior.get(id))
                                    .filter_map(|item| item.invocation_id.as_ref())
                                    .filter(|id| {
                                        invocation_scopes.get(id).is_some_and(|scope| !scope.3)
                                    })
                                    .collect();
                                let supplied: HashSet<_> = declared.iter().collect();
                                if supplied.len() != declared.len() || actual != supplied {
                                    return Err(reference(
                                        "terminal turn unresolved invocation set is incomplete or foreign",
                                    ));
                                }
                            }
                        }
                        state.terminal = true;
                        state.pending_requeue = None;
                    }
                }
                EventKind::ProviderAttemptStarted
                | EventKind::ToolEffectStarted
                | EventKind::DeliveryStarted
                | EventKind::ChildInvocationRequested => {
                    if let Some(id) = &event.invocation_id {
                        if incoming && invocations.contains_key(id) {
                            return Err(invalid("invocation already started"));
                        }
                        if incoming
                            && event.body.kind() != EventKind::DeliveryStarted
                            && event
                                .turn_id
                                .as_ref()
                                .and_then(|turn| turns.get(turn))
                                .is_some_and(|state| state.terminal)
                        {
                            return Err(invalid("terminal turn cannot start another effect"));
                        }
                        invocations.insert(id.clone(), (event.body.kind().clone(), false));
                        let background = matches!(
                            &event.body.data,
                            EventData::ChildInvocationRequested {
                                execution_mode: ChildExecutionMode::Background,
                                ..
                            }
                        ) || event.body.kind() == EventKind::DeliveryStarted;
                        invocation_scopes.insert(
                            id.clone(),
                            (
                                event.stream_id.clone(),
                                event.branch_id.clone(),
                                event.turn_id.clone(),
                                background,
                            ),
                        );
                    }
                }
                kind if observation(kind) => {
                    if let Some(id) = &event.invocation_id {
                        let state = invocations.get_mut(id);
                        if incoming {
                            let Some(state) = state else {
                                return Err(invalid(
                                    "observation requires durable started invocation",
                                ));
                            };
                            if state.1 {
                                return Err(invalid("invocation is already terminal"));
                            }
                            if !compatible(&state.0, kind) {
                                return Err(invalid("observation kind does not match invocation"));
                            }
                            if terminal(kind) {
                                state.1 = true;
                            }
                        } else if let Some(state) = state
                            && terminal(kind)
                        {
                            state.1 = true;
                        }
                    }
                }
                EventKind::ApprovalConsumed => {
                    if let EventData::ApprovalConsumed { confirmation_id } = &event.body.data {
                        if incoming && !consumed.insert(confirmation_id.clone()) {
                            return Err(invalid("confirmation already consumed"));
                        }
                        consumed.insert(confirmation_id.clone());
                        if incoming
                            && !batch_events.iter().any(|item| {
                                item.body.kind() == EventKind::ToolEffectStarted
                                    && item.invocation_id == event.invocation_id
                            })
                        {
                            return Err(invalid(
                                "approval consumption and effect start must be atomic",
                            ));
                        }
                    }
                }
                EventKind::OutcomeReconciled => {
                    if let EventData::OutcomeReconciled {
                        unknown_event_id, ..
                    } = &event.body.data
                    {
                        if incoming && !reconciled.insert(unknown_event_id.clone()) {
                            return Err(invalid("unknown outcome can only be reconciled once"));
                        }
                        reconciled.insert(unknown_event_id.clone());
                    }
                }
                EventKind::ContextTransformStarted => {
                    transforms.insert(event.event_id.clone(), false);
                }
                EventKind::ContextTransformFailed
                | EventKind::ContextTransformCancelled
                | EventKind::ContextProjectionCreated => {
                    let id = match &event.body.data {
                        EventData::ContextTransformFailed {
                            started_event_id, ..
                        }
                        | EventData::ContextTransformCancelled {
                            started_event_id, ..
                        } => Some(started_event_id),
                        EventData::ContextProjectionCreated {
                            transform_started_event_id,
                            ..
                        } => transform_started_event_id.as_ref(),
                        _ => None,
                    };
                    if let Some(id) = id {
                        if incoming && transforms.get(id).is_none_or(|terminal| *terminal) {
                            return Err(invalid("context transform absent or already terminal"));
                        }
                        transforms.insert(id.clone(), true);
                    }
                }
                EventKind::SessionClosed if !matches!(&event.body.data, EventData::SessionClosed { reason_key } if reason_key == "session_ttl_expired") =>
                {
                    closed.insert(event.stream_id.clone());
                }
                _ => {}
            }
            if let Some(turn) = &event.turn_id {
                if matches!(
                    event.body.kind(),
                    EventKind::ProviderAttemptStarted
                        | EventKind::ToolEffectStarted
                        | EventKind::DeliveryStarted
                        | EventKind::ChildInvocationRequested
                ) {
                    if turn_invocations.get(turn).is_none() {
                        turn_invocations.insert(turn.clone(), Vec::new());
                    }
                    if let Some(ids) = turn_invocations.get_mut(turn)
                        && let Some(id) = &event.invocation_id
                    {
                        ids.push(id.clone());
                    }
                }
                if event.body.kind() == EventKind::InvocationOutcomeUnknown {
                    if turn_unknowns.get(turn).is_none() {
                        turn_unknowns.insert(turn.clone(), Vec::new());
                    }
                    if let Some(ids) = turn_unknowns.get_mut(turn) {
                        ids.push(event.event_id.clone());
                    }
                }
            }
            if matches!(
                event.body.kind(),
                EventKind::ProviderAttemptStarted
                    | EventKind::ToolEffectStarted
                    | EventKind::DeliveryStarted
                    | EventKind::ChildInvocationRequested
            ) && let Some(id) = &event.invocation_id
            {
                start_events.insert(id.clone(), event.event_id.clone());
            }
            if event.body.kind() == EventKind::DerivedSessionStarted
                && let Some(id) = &event.causation_id
            {
                child_derivations.insert(id.clone(), event.event_id.clone());
            }
            if event.sequence > 0 {
                by_cursor.insert(
                    (event.stream_id.clone(), event.sequence),
                    event.event_id.clone(),
                );
            }
            if !prior.contains_key(&event.event_id) {
                prior.insert(event.event_id.clone(), event.clone());
            }
        }
        Ok(HistoryDelta {
            prior: prior.into_changes(),
            branches: branches.into_changes(),
            turns: turns.into_changes(),
            invocations: invocations.into_changes(),
            invocation_scopes: invocation_scopes.into_changes(),
            turn_invocations: turn_invocations.into_changes(),
            turn_unknowns: turn_unknowns.into_changes(),
            files: files.into_changes(),
            by_cursor: by_cursor.into_changes(),
            child_derivations: child_derivations.into_changes(),
            start_events: start_events.into_changes(),
            transforms: transforms.into_changes(),
            elicitations: elicitations.into_changes(),
            consumed: consumed.into_changes(),
            reconciled: reconciled.into_changes(),
            streams: streams.into_changes(),
            closed: closed.into_changes(),
        })
    }
}

fn invalid(reason: &str) -> AppendError {
    AppendError::InvalidTransition(reason.to_owned())
}
fn reference(reason: &str) -> AppendError {
    AppendError::InvalidReference(reason.to_owned())
}
pub fn terminal(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::ProviderAttemptCompleted
            | EventKind::ProviderAttemptFailed
            | EventKind::ToolEffectCompleted
            | EventKind::ToolEffectFailed
            | EventKind::DeliveryCompleted
            | EventKind::DeliveryFailed
            | EventKind::ChildInvocationCompleted
            | EventKind::ChildInvocationRejected
            | EventKind::InvocationOutcomeUnknown
            | EventKind::InvocationCancelled
    )
}
fn observation(kind: &EventKind) -> bool {
    terminal(kind)
        || matches!(
            kind,
            EventKind::ToolEffectProgressObserved | EventKind::ProviderResponseProgressObserved
        )
}
fn compatible(start: &EventKind, kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::InvocationOutcomeUnknown | EventKind::InvocationCancelled
    ) || matches!(
        (start, kind),
        (
            EventKind::ProviderAttemptStarted,
            EventKind::ProviderResponseProgressObserved
                | EventKind::ProviderAttemptCompleted
                | EventKind::ProviderAttemptFailed
        ) | (
            EventKind::ToolEffectStarted,
            EventKind::ToolEffectProgressObserved
                | EventKind::ToolEffectCompleted
                | EventKind::ToolEffectFailed
        ) | (
            EventKind::DeliveryStarted,
            EventKind::DeliveryCompleted | EventKind::DeliveryFailed
        ) | (
            EventKind::ChildInvocationRequested,
            EventKind::ChildInvocationCompleted | EventKind::ChildInvocationRejected
        )
    )
}
fn requires_branch(kind: &EventKind) -> bool {
    !matches!(
        kind,
        EventKind::SessionMetadataChanged
            | EventKind::ControlInputRecorded
            | EventKind::CancellationRequested
            | EventKind::AuthorityRevisionObserved
            | EventKind::SessionModelOverrideChanged
            | EventKind::PayloadErasureRequested
            | EventKind::PayloadRedacted
            | EventKind::SessionClosed
            | EventKind::LegacyHistoryImported
            | EventKind::Unknown
    )
}
fn requires_turn(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::TurnAccepted
            | EventKind::TurnStarted
            | EventKind::UserContentRecorded
            | EventKind::ToolCallProposed
            | EventKind::ToolEffectStarted
            | EventKind::ToolEffectCompleted
            | EventKind::ToolEffectFailed
            | EventKind::ToolEffectProgressObserved
            | EventKind::TurnCommitted
            | EventKind::TurnCancelled
            | EventKind::TurnOutcomeUnknown
    )
}
fn event_refs(data: &EventData) -> Vec<(&EventId, &'static [EventKind], bool)> {
    use EventData as D;
    use EventKind as K;
    let mut refs = Vec::new();
    match data {
        D::TurnStarted {
            accepted_event_id, ..
        }
        | D::UserContentRecorded {
            accepted_event_id, ..
        } => refs.push((accepted_event_id, &[K::TurnAccepted][..], false)),
        D::ProviderRequestPrepared {
            context_event_id, ..
        } => refs.push((context_event_id, &[K::ContextProjectionCreated][..], false)),
        D::ProviderAttemptStarted {
            prepared_event_id,
            previous_attempt_event_id,
            ..
        } => {
            refs.push((prepared_event_id, &[K::ProviderRequestPrepared][..], true));
            if let Some(id) = previous_attempt_event_id {
                refs.push((
                    id,
                    &[
                        K::ProviderAttemptFailed,
                        K::ProviderAttemptCompleted,
                        K::InvocationOutcomeUnknown,
                    ][..],
                    false,
                ));
            }
        }
        D::ProviderAttemptCompleted {
            last_progress_event_id,
            ..
        }
        | D::ProviderAttemptFailed {
            last_progress_event_id,
            ..
        } => {
            if let Some(id) = last_progress_event_id {
                refs.push((id, &[K::ProviderResponseProgressObserved][..], true));
            }
        }
        D::ToolCallProposed {
            provider_result_event_id,
            ..
        } => refs.push((
            provider_result_event_id,
            &[K::ProviderAttemptCompleted][..],
            false,
        )),
        D::PolicyDecisionRecorded {
            proposed_event_id, ..
        } => refs.push((proposed_event_id, &[K::ToolCallProposed][..], false)),
        D::ApprovalRequested {
            policy_event_id, ..
        } => refs.push((policy_event_id, &[K::PolicyDecisionRecorded][..], false)),
        D::ToolEffectStarted {
            policy_event_id,
            confirmation_event_id,
            ..
        } => {
            refs.push((policy_event_id, &[K::PolicyDecisionRecorded][..], false));
            if let Some(id) = confirmation_event_id {
                refs.push((id, &[K::ApprovalResolved][..], false));
            }
        }
        D::DeliveryCompleted {
            started_event_id, ..
        }
        | D::DeliveryFailed {
            started_event_id, ..
        } => refs.push((started_event_id, &[K::DeliveryStarted][..], true)),
        D::InvocationOutcomeUnknown {
            last_observed_event_id,
            ..
        } => refs.push((last_observed_event_id, &[][..], true)),
        D::InvocationCancelled {
            cancellation_event_id,
            ..
        } => refs.push((
            cancellation_event_id,
            &[K::CancellationRequested, K::ChildCancellationRequested][..],
            false,
        )),
        D::ContextTransformStarted {
            input_projection_event_id,
            ..
        } => refs.push((
            input_projection_event_id,
            &[K::ContextProjectionCreated][..],
            false,
        )),
        D::ContextTransformFailed {
            started_event_id, ..
        }
        | D::ContextTransformCancelled {
            started_event_id, ..
        } => refs.push((started_event_id, &[K::ContextTransformStarted][..], false)),
        D::ContextProjectionCreated {
            transform_started_event_id,
            summary_source_refs,
            ..
        } => {
            if let Some(id) = transform_started_event_id {
                refs.push((id, &[K::ContextTransformStarted][..], false));
            }
            for source in summary_source_refs {
                refs.push((
                    &source.result_event_id,
                    &[K::ProviderAttemptCompleted][..],
                    false,
                ));
            }
        }
        D::OutcomeReconciled {
            unknown_event_id, ..
        } => refs.push((unknown_event_id, &[K::InvocationOutcomeUnknown][..], true)),
        D::PayloadRedacted {
            erasure_requested_event_id,
        } => refs.push((
            erasure_requested_event_id,
            &[K::PayloadErasureRequested][..],
            false,
        )),
        D::FileAttached {
            content_event_id, ..
        } => refs.push((
            content_event_id,
            &[K::UserContentRecorded, K::AssistantContentRecorded][..],
            false,
        )),
        D::IngressDecisionRecorded {
            subject_event_id, ..
        } => refs.push((subject_event_id, &[][..], false)),
        D::ChildInvocationRejected {
            requested_event_id, ..
        }
        | D::ChildCancellationRequested {
            requested_event_id, ..
        } => refs.push((requested_event_id, &[K::ChildInvocationRequested][..], true)),
        D::ChildResultConsumed {
            child_outcome_event_id,
            consumer_event_id,
            ..
        } => {
            refs.push((
                child_outcome_event_id,
                &[K::ChildInvocationCompleted, K::OutcomeReconciled][..],
                true,
            ));
            refs.push((
                consumer_event_id,
                &[K::TurnAccepted, K::ContextProjectionCreated][..],
                false,
            ));
        }
        D::AssistantContentRecorded {
            provenance_refs, ..
        } => {
            for source in provenance_refs {
                refs.push((
                    &source.result_event_id,
                    &[K::ProviderAttemptCompleted][..],
                    false,
                ));
            }
        }
        D::ContentSuperseded {
            target_event_ids,
            disposition,
        } => {
            for id in target_event_ids {
                refs.push((
                    id,
                    &[K::UserContentRecorded, K::AssistantContentRecorded][..],
                    false,
                ));
            }
            if let Supersession::Replaced {
                replacement_event_id,
            } = disposition
            {
                refs.push((
                    replacement_event_id,
                    &[K::UserContentRecorded, K::AssistantContentRecorded][..],
                    false,
                ));
            }
        }
        D::AuthorityRevisionObserved {
            previous_observation_event_id: Some(id),
            ..
        } => refs.push((id, &[K::AuthorityRevisionObserved][..], false)),
        D::SessionModelOverrideChanged {
            resolution:
                ModelResolution::Applied {
                    previous_selection_event_id: Some(id),
                },
        } => refs.push((id, &[K::SessionModelOverrideChanged][..], false)),
        _ => {}
    }
    refs
}

fn validate_coordinates(
    event: &SessionEvent,
    prior: &OverlayMap<'_, EventId, SessionEvent>,
    branches: &OverlayMap<'_, BranchId, SessionStreamId>,
    by_cursor: &OverlayMap<'_, (SessionStreamId, u64), EventId>,
    child_derivations: &OverlayMap<'_, EventId, EventId>,
    start_events: &OverlayMap<'_, InvocationId, EventId>,
) -> Result<(), AppendError> {
    match &event.body.data {
        EventData::SessionModelOverrideChanged {
            resolution:
                ModelResolution::Applied {
                    previous_selection_event_id: Some(id),
                },
        } => {
            let branch = event
                .branch_id
                .as_ref()
                .ok_or_else(|| reference("model selection requires branch identity"))?;
            let target = prior
                .get(id)
                .ok_or_else(|| reference("previous model selection missing"))?;
            if !matches!(
                target.body.data,
                EventData::SessionModelOverrideChanged {
                    resolution: ModelResolution::Applied { .. }
                }
            ) {
                return Err(reference(
                    "previous model selection must be session-persistent",
                ));
            }
            let lineage =
                super::lineage::frozen_lineage(prior.values(), &event.stream_id, branch, u64::MAX)
                    .map_err(|error| reference(&error.to_string()))?;
            if !super::lineage::includes(&lineage, target) {
                return Err(reference(
                    "previous model selection outside frozen branch lineage",
                ));
            }
        }
        EventData::BranchCreated {
            parent_branch_id,
            parent_cursor,
        } => {
            if branches.get(parent_branch_id) != Some(&parent_cursor.stream_id)
                || parent_cursor.stream_id != event.stream_id
            {
                return Err(reference(
                    "branch seed does not identify a committed parent prefix",
                ));
            }
            if parent_cursor.sequence > 0 {
                let item = by_cursor
                    .get(&(parent_cursor.stream_id.clone(), parent_cursor.sequence))
                    .and_then(|id| prior.get(id))
                    .ok_or_else(|| reference("branch cursor missing"))?;
                let lineage = super::lineage::frozen_lineage(
                    prior.values(),
                    &event.stream_id,
                    parent_branch_id,
                    u64::MAX,
                )
                .map_err(|error| reference(&error.to_string()))?;
                if !super::lineage::includes(&lineage, item) {
                    return Err(reference("branch cursor outside frozen parent lineage"));
                }
            }
        }
        EventData::ContextProjectionCreated { source_ranges, .. } => {
            for range in source_ranges {
                if range.from_seq == 0
                    || range.from_seq > range.to_seq
                    || !by_cursor.contains_key(&(range.stream_id.clone(), range.to_seq))
                {
                    return Err(reference("context range is not a frozen committed prefix"));
                }
            }
        }
        EventData::RecoveryObserved {
            through_cursor,
            disposition,
            ..
        } => {
            if through_cursor.stream_id != event.stream_id
                || !by_cursor
                    .contains_key(&(through_cursor.stream_id.clone(), through_cursor.sequence))
            {
                return Err(reference("recovery cursor is not a committed prefix"));
            }
            if matches!(
                disposition,
                RecoveryDisposition::Requeued | RecoveryDisposition::Rescheduled { .. }
            ) {
                let turn = event
                    .turn_id
                    .as_ref()
                    .ok_or_else(|| reference("requeue requires turn identity"))?;
                if !can_requeue_turn(prior.values(), turn) {
                    return Err(invalid("turn with effects or output cannot be requeued"));
                }
                if matches!(disposition, RecoveryDisposition::Rescheduled { .. }) {
                    let source = event
                        .causation_id
                        .as_ref()
                        .and_then(|id| prior.get(id))
                        .filter(|source| {
                            source.turn_id == event.turn_id
                                && source.branch_id == event.branch_id
                                && matches!(
                                    source.body.data,
                                    EventData::RecoveryObserved {
                                        disposition: RecoveryDisposition::Requeued,
                                        ..
                                    }
                                )
                        })
                        .ok_or_else(|| reference("reschedule requires bound requeue evidence"))?;
                    if prior.values().any(|candidate| {
                        candidate.turn_id == event.turn_id
                            && (candidate.sequence == 0 || candidate.sequence > source.sequence)
                            && matches!(
                                candidate.body.data,
                                EventData::TurnStarted { .. }
                                    | EventData::RecoveryObserved {
                                        disposition: RecoveryDisposition::Rescheduled { .. },
                                        ..
                                    }
                            )
                    }) {
                        return Err(invalid("requeued execution has already restarted"));
                    }
                }
            }
        }
        EventData::ChildInvocationCompleted {
            child_terminal_cursor,
            ..
        } => {
            let origin = event
                .invocation_id
                .as_ref()
                .and_then(|id| start_events.get(id))
                .and_then(|id| prior.get(id))
                .filter(|item| item.body.kind() == EventKind::ChildInvocationRequested)
                .ok_or_else(|| reference("child request missing"))?;
            let terminal = by_cursor
                .get(&(
                    child_terminal_cursor.stream_id.clone(),
                    child_terminal_cursor.sequence,
                ))
                .and_then(|id| prior.get(id))
                .ok_or_else(|| reference("child terminal cursor absent"))?;
            let derived = child_derivations
                .get(&origin.event_id)
                .and_then(|id| prior.get(id))
                .ok_or_else(|| reference("child derivation missing"))?;
            validate_child_terminal(origin, derived, terminal)?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate_child_terminal(
    origin: &SessionEvent,
    derived: &SessionEvent,
    terminal: &SessionEvent,
) -> Result<(), AppendError> {
    let EventData::DerivedSessionStarted {
        execution_turn_id: Some(execution_turn_id),
    } = &derived.body.data
    else {
        return Err(reference("invalid child derivation"));
    };
    if derived.causation_id.as_ref() != Some(&origin.event_id)
        || terminal.stream_id != derived.stream_id
        || terminal.branch_id != derived.branch_id
        || terminal.turn_id.as_ref() != Some(execution_turn_id)
        || !matches!(
            terminal.body.kind(),
            EventKind::TurnCommitted | EventKind::TurnCancelled
        )
    {
        return Err(reference("child terminal does not match bound execution"));
    }
    Ok(())
}

/// Compatibility facade over the single incremental validator.
pub fn validate_history_batch(
    history: &[SessionEvent],
    request: &AppendRequest,
) -> Result<(), AppendError> {
    HistoryIndex::from_history(history)?.plan(request)?;
    Ok(())
}
