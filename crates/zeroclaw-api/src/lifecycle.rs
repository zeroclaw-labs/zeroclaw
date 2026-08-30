//! Content-free lifecycle semantics shared by interactive and runtime surfaces.
//!
//! This module owns the small state vocabulary used by terminal status and by
//! richer lifecycle projections. It deliberately carries no prompt, response,
//! tool input/output, credential, memory, transport, or vendor-specific data.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Wire version of [`LifecycleEventV1`].
pub const LIFECYCLE_SCHEMA_VERSION: u16 = 1;

/// Maximum number of Unicode scalar values carried in an optional tool name.
pub const MAX_LIFECYCLE_TOOL_NAME_CHARS: usize = 128;

/// Canonical externally observable state of an agent session or turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    /// No turn is currently executing.
    Idle,
    /// A turn or tool is making progress without requiring operator input.
    Working,
    /// Progress is paused at an operator-input boundary.
    Blocked,
    /// The session has ended and relinquished lifecycle ownership.
    Done,
}

impl LifecycleState {
    /// Rank states by how urgently an external surface should request attention.
    ///
    /// `Blocked` outranks `Working`; quiescent `Idle` and terminal `Done` do not
    /// request attention.
    pub const fn attention_rank(self) -> u8 {
        match self {
            Self::Idle | Self::Done => 0,
            Self::Working => 1,
            Self::Blocked => 2,
        }
    }
}

/// Content-free activity class mapped into [`LifecycleState`].
///
/// Producers retain their local detail (for example thinking versus response
/// streaming) and classify it here only when projecting external lifecycle
/// state. This keeps the state transition rule in one API-owned location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleActivity {
    /// Waiting for a new turn.
    Idle,
    /// A model turn is in flight.
    Turn,
    /// A tool call is in flight.
    Tool,
    /// The turn is waiting for approval or structured operator input.
    Approval,
    /// The session has finished and released ownership.
    Finished,
}

/// Content-free lifecycle event identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleEventKind {
    /// A session became externally observable.
    SessionStarted,
    /// A turn began inside the session.
    TurnStarted,
    /// A tool began executing.
    ToolStarted,
    /// A tool finished executing.
    ToolCompleted,
    /// The turn stopped for an operator decision.
    ApprovalRequested,
    /// The operator decision was received.
    ApprovalResponded,
    /// The turn reached its terminal result.
    TurnCompleted,
    /// The session relinquished lifecycle ownership.
    SessionEnded,
}

impl LifecycleEventKind {
    /// All supported event kinds in stable wire order.
    pub const ALL: [Self; 8] = [
        Self::SessionStarted,
        Self::TurnStarted,
        Self::ToolStarted,
        Self::ToolCompleted,
        Self::ApprovalRequested,
        Self::ApprovalResponded,
        Self::TurnCompleted,
        Self::SessionEnded,
    ];

    /// Stable snake-case wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::TurnStarted => "turn_started",
            Self::ToolStarted => "tool_started",
            Self::ToolCompleted => "tool_completed",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalResponded => "approval_responded",
            Self::TurnCompleted => "turn_completed",
            Self::SessionEnded => "session_ended",
        }
    }

    /// Parse a stable snake-case wire name.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }
}

/// Outcome of an operator decision without its prompt or replacement content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOutcome {
    /// The original action may proceed.
    Granted,
    /// The original action was rejected.
    Denied,
    /// The request ended without an allow/deny decision.
    Cancelled,
}

/// Stable identity carried by every projected lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LifecycleCorrelation {
    /// Configured agent alias.
    pub agent_alias: String,
    /// Stable session identity for the producing surface.
    pub session_id: String,
    /// Stable turn identity for turn-scoped events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

impl LifecycleCorrelation {
    /// Construct validated lifecycle correlation.
    ///
    /// # Errors
    ///
    /// Returns an error when the agent or session identity is blank.
    pub fn new(
        agent_alias: impl Into<String>,
        session_id: impl Into<String>,
        turn_id: Option<String>,
    ) -> Result<Self, LifecycleMapError> {
        let agent_alias = agent_alias.into();
        let session_id = session_id.into();
        if agent_alias.trim().is_empty() {
            return Err(LifecycleMapError::MissingAgentAlias);
        }
        if session_id.trim().is_empty() {
            return Err(LifecycleMapError::MissingSessionId);
        }
        Ok(Self {
            agent_alias,
            session_id,
            turn_id: turn_id.filter(|id| !id.trim().is_empty()),
        })
    }
}

/// Input accepted by [`LifecycleMapper`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleSignal {
    /// Begin or re-open a session.
    SessionStarted,
    /// Begin a turn.
    TurnStarted,
    /// A tool started; its bounded name may be exposed.
    ToolStarted { tool_name: Option<String> },
    /// A tool finished; only its success bit may be exposed.
    ToolCompleted {
        tool_name: Option<String>,
        success: bool,
    },
    /// An approval or structured-input request blocked the turn.
    ApprovalRequested { tool_name: Option<String> },
    /// An operator response resumed the turn.
    ApprovalResponded {
        tool_name: Option<String>,
        outcome: ApprovalOutcome,
    },
    /// Complete the active turn.
    TurnCompleted,
    /// End and release the session.
    SessionEnded,
}

/// Versioned, content-free lifecycle payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleEventV1 {
    /// Payload schema version; currently [`LIFECYCLE_SCHEMA_VERSION`].
    pub schema_version: u16,
    /// Process-local monotonic ordering key.
    pub sequence: u64,
    /// Event time as Unix epoch milliseconds.
    pub timestamp_unix_ms: u64,
    /// Semantic event identity.
    pub event: LifecycleEventKind,
    /// State after applying the event.
    pub state: LifecycleState,
    /// Stable agent/session/turn correlation.
    pub correlation: LifecycleCorrelation,
    /// Optional bounded tool name; arguments and results are never carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Optional tool success bit without result content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_success: Option<bool>,
    /// Optional content-free operator outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_outcome: Option<ApprovalOutcome>,
}

/// Invalid lifecycle correlation or transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleMapError {
    /// Agent alias was blank.
    #[error("lifecycle agent_alias must not be blank")]
    MissingAgentAlias,
    /// Session identity was blank.
    #[error("lifecycle session_id must not be blank")]
    MissingSessionId,
    /// A turn-scoped event had no turn identity.
    #[error("lifecycle event {event:?} requires turn_id")]
    MissingTurnId { event: LifecycleEventKind },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventFingerprint {
    event: LifecycleEventKind,
    state: LifecycleState,
    turn_id: Option<String>,
    tool_name: Option<String>,
    tool_success: Option<bool>,
    approval_outcome: Option<ApprovalOutcome>,
}

#[derive(Debug, Default)]
struct SessionProjection {
    agent_alias: String,
    turn_states: HashMap<String, LifecycleState>,
    last: Option<EventFingerprint>,
}

/// Stateful canonical mapper for lifecycle state, correlation and deduplication.
#[derive(Debug, Default)]
pub struct LifecycleMapper {
    next_sequence: u64,
    sessions: HashMap<String, SessionProjection>,
}

impl LifecycleMapper {
    /// Apply one content-free signal.
    ///
    /// Returns `Ok(None)` for a duplicate event or a turn-scoped event that
    /// belongs to a different active turn. Session end removes mapper state so
    /// a later session start with the same identity opens a fresh generation.
    ///
    /// # Errors
    ///
    /// Returns an error when required correlation is missing.
    pub fn project(
        &mut self,
        signal: LifecycleSignal,
        correlation: LifecycleCorrelation,
        timestamp_unix_ms: u64,
    ) -> Result<Option<LifecycleEventV1>, LifecycleMapError> {
        let (event, activity, tool_name, tool_success, approval_outcome, turn_scoped) =
            signal_parts(signal);
        let turn_id = correlation.turn_id.clone();
        if turn_scoped && turn_id.is_none() {
            return Err(LifecycleMapError::MissingTurnId { event });
        }

        if event == LifecycleEventKind::SessionStarted {
            if self.sessions.contains_key(&correlation.session_id) {
                return Ok(None);
            }
            self.sessions.insert(
                correlation.session_id.clone(),
                SessionProjection {
                    agent_alias: correlation.agent_alias.clone(),
                    ..SessionProjection::default()
                },
            );
        } else if event == LifecycleEventKind::SessionEnded
            && !self.sessions.contains_key(&correlation.session_id)
        {
            return Ok(None);
        }

        let Some(session) = self.sessions.get_mut(&correlation.session_id) else {
            return Ok(None);
        };
        if session.agent_alias != correlation.agent_alias {
            return Ok(None);
        }
        match event {
            LifecycleEventKind::SessionStarted => {}
            LifecycleEventKind::TurnStarted => {
                let Some(turn_id) = turn_id.as_ref() else {
                    return Err(LifecycleMapError::MissingTurnId { event });
                };
                if session
                    .turn_states
                    .insert(turn_id.clone(), LifecycleState::Working)
                    .is_some()
                {
                    return Ok(None);
                }
            }
            LifecycleEventKind::ToolStarted
            | LifecycleEventKind::ToolCompleted
            | LifecycleEventKind::ApprovalRequested
            | LifecycleEventKind::ApprovalResponded => {
                let Some(turn_id) = turn_id.as_ref() else {
                    return Err(LifecycleMapError::MissingTurnId { event });
                };
                let Some(turn_state) = session.turn_states.get_mut(turn_id) else {
                    return Ok(None);
                };
                *turn_state = activity.state();
            }
            LifecycleEventKind::TurnCompleted => {
                let Some(turn_id) = turn_id.as_ref() else {
                    return Err(LifecycleMapError::MissingTurnId { event });
                };
                if session.turn_states.remove(turn_id).is_none() {
                    return Ok(None);
                }
            }
            LifecycleEventKind::SessionEnded => {
                if !session.turn_states.is_empty() {
                    return Ok(None);
                }
            }
        }

        let state = match event {
            LifecycleEventKind::SessionStarted => LifecycleState::Idle,
            LifecycleEventKind::SessionEnded => LifecycleState::Done,
            _ if session
                .turn_states
                .values()
                .any(|state| *state == LifecycleState::Blocked) =>
            {
                LifecycleState::Blocked
            }
            _ if session.turn_states.is_empty() => LifecycleState::Idle,
            _ => LifecycleState::Working,
        };
        let fingerprint = EventFingerprint {
            event,
            state,
            turn_id: turn_id.clone(),
            tool_name: tool_name.clone(),
            tool_success,
            approval_outcome,
        };
        if session.last.as_ref() == Some(&fingerprint) {
            return Ok(None);
        }
        session.last = Some(fingerprint);

        self.next_sequence = self.next_sequence.wrapping_add(1).max(1);
        let projected = LifecycleEventV1 {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            sequence: self.next_sequence,
            timestamp_unix_ms,
            event,
            state,
            correlation,
            tool_name,
            tool_success,
            approval_outcome,
        };
        if event == LifecycleEventKind::SessionEnded {
            self.sessions.remove(&projected.correlation.session_id);
        }
        Ok(Some(projected))
    }
}

#[allow(clippy::type_complexity)]
fn signal_parts(
    signal: LifecycleSignal,
) -> (
    LifecycleEventKind,
    LifecycleActivity,
    Option<String>,
    Option<bool>,
    Option<ApprovalOutcome>,
    bool,
) {
    match signal {
        LifecycleSignal::SessionStarted => (
            LifecycleEventKind::SessionStarted,
            LifecycleActivity::Idle,
            None,
            None,
            None,
            false,
        ),
        LifecycleSignal::TurnStarted => (
            LifecycleEventKind::TurnStarted,
            LifecycleActivity::Turn,
            None,
            None,
            None,
            true,
        ),
        LifecycleSignal::ToolStarted { tool_name } => (
            LifecycleEventKind::ToolStarted,
            LifecycleActivity::Tool,
            bounded_tool_name(tool_name),
            None,
            None,
            true,
        ),
        LifecycleSignal::ToolCompleted { tool_name, success } => (
            LifecycleEventKind::ToolCompleted,
            LifecycleActivity::Tool,
            bounded_tool_name(tool_name),
            Some(success),
            None,
            true,
        ),
        LifecycleSignal::ApprovalRequested { tool_name } => (
            LifecycleEventKind::ApprovalRequested,
            LifecycleActivity::Approval,
            bounded_tool_name(tool_name),
            None,
            None,
            true,
        ),
        LifecycleSignal::ApprovalResponded { tool_name, outcome } => (
            LifecycleEventKind::ApprovalResponded,
            LifecycleActivity::Turn,
            bounded_tool_name(tool_name),
            None,
            Some(outcome),
            true,
        ),
        LifecycleSignal::TurnCompleted => (
            LifecycleEventKind::TurnCompleted,
            LifecycleActivity::Idle,
            None,
            None,
            None,
            true,
        ),
        LifecycleSignal::SessionEnded => (
            LifecycleEventKind::SessionEnded,
            LifecycleActivity::Finished,
            None,
            None,
            None,
            false,
        ),
    }
}

fn bounded_tool_name(name: Option<String>) -> Option<String> {
    let bounded: String = name?
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_LIFECYCLE_TOOL_NAME_CHARS)
        .collect();
    (!bounded.trim().is_empty()).then_some(bounded)
}

impl LifecycleActivity {
    /// Map this activity to the canonical lifecycle state.
    pub const fn state(self) -> LifecycleState {
        match self {
            Self::Idle => LifecycleState::Idle,
            Self::Turn | Self::Tool => LifecycleState::Working,
            Self::Approval => LifecycleState::Blocked,
            Self::Finished => LifecycleState::Done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_activity_maps_to_the_canonical_state() {
        assert_eq!(LifecycleActivity::Idle.state(), LifecycleState::Idle);
        assert_eq!(LifecycleActivity::Turn.state(), LifecycleState::Working);
        assert_eq!(LifecycleActivity::Tool.state(), LifecycleState::Working);
        assert_eq!(LifecycleActivity::Approval.state(), LifecycleState::Blocked);
        assert_eq!(LifecycleActivity::Finished.state(), LifecycleState::Done);
    }

    #[test]
    fn attention_rank_is_blocked_then_working_then_quiescent() {
        assert_eq!(LifecycleState::Blocked.attention_rank(), 2);
        assert_eq!(LifecycleState::Working.attention_rank(), 1);
        assert_eq!(LifecycleState::Idle.attention_rank(), 0);
        assert_eq!(LifecycleState::Done.attention_rank(), 0);
    }

    #[test]
    fn wire_names_are_stable_snake_case() {
        assert_eq!(
            serde_json::to_value(LifecycleState::Blocked).unwrap(),
            "blocked"
        );
        assert_eq!(
            serde_json::to_value(LifecycleActivity::Finished).unwrap(),
            "finished"
        );
        for event in LifecycleEventKind::ALL {
            assert_eq!(LifecycleEventKind::parse(event.as_str()), Some(event));
            assert_eq!(serde_json::to_value(event).unwrap(), event.as_str());
        }
    }

    fn correlation(turn_id: Option<&str>) -> LifecycleCorrelation {
        correlation_for("agent", turn_id)
    }

    fn correlation_for(agent_alias: &str, turn_id: Option<&str>) -> LifecycleCorrelation {
        LifecycleCorrelation::new(
            agent_alias,
            "session",
            turn_id.map(std::string::ToString::to_string),
        )
        .unwrap()
    }

    #[test]
    fn mapper_tracks_state_sequence_and_correlation() {
        let mut mapper = LifecycleMapper::default();
        let session = mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap()
            .unwrap();
        let turn = mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("turn")), 2)
            .unwrap()
            .unwrap();
        let blocked = mapper
            .project(
                LifecycleSignal::ApprovalRequested {
                    tool_name: Some("shell".into()),
                },
                correlation(Some("turn")),
                3,
            )
            .unwrap()
            .unwrap();
        let resumed = mapper
            .project(
                LifecycleSignal::ApprovalResponded {
                    tool_name: Some("shell".into()),
                    outcome: ApprovalOutcome::Denied,
                },
                correlation(Some("turn")),
                4,
            )
            .unwrap()
            .unwrap();
        let complete = mapper
            .project(LifecycleSignal::TurnCompleted, correlation(Some("turn")), 5)
            .unwrap()
            .unwrap();
        let ended = mapper
            .project(LifecycleSignal::SessionEnded, correlation(None), 6)
            .unwrap()
            .unwrap();

        assert_eq!(session.state, LifecycleState::Idle);
        assert_eq!(turn.state, LifecycleState::Working);
        assert_eq!(blocked.state, LifecycleState::Blocked);
        assert_eq!(resumed.state, LifecycleState::Working);
        assert_eq!(complete.state, LifecycleState::Idle);
        assert_eq!(ended.state, LifecycleState::Done);
        assert_eq!(ended.sequence, 6);
        assert_eq!(blocked.correlation.turn_id.as_deref(), Some("turn"));
    }

    #[test]
    fn mapper_deduplicates_and_ignores_foreign_turns() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("parent")), 2)
            .unwrap();
        assert!(
            mapper
                .project(LifecycleSignal::TurnStarted, correlation(Some("parent")), 3)
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(
                    LifecycleSignal::TurnCompleted,
                    correlation(Some("child")),
                    4,
                )
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(
                    LifecycleSignal::ApprovalRequested { tool_name: None },
                    correlation(Some("parent")),
                    5,
                )
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn mapper_keeps_foreign_child_lifecycle_from_displacing_parent() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("parent")), 2)
            .unwrap();
        mapper
            .project(
                LifecycleSignal::ToolStarted {
                    tool_name: Some("shell".into()),
                },
                correlation(Some("parent")),
                3,
            )
            .unwrap();

        assert!(
            mapper
                .project(
                    LifecycleSignal::SessionStarted,
                    correlation_for("child", None),
                    4,
                )
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(
                    LifecycleSignal::TurnStarted,
                    correlation_for("child", Some("child")),
                    5,
                )
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(
                    LifecycleSignal::TurnCompleted,
                    correlation_for("child", Some("child")),
                    6,
                )
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(
                    LifecycleSignal::SessionEnded,
                    correlation_for("child", None),
                    7,
                )
                .unwrap()
                .is_none()
        );

        let blocked = mapper
            .project(
                LifecycleSignal::ApprovalRequested {
                    tool_name: Some("shell".into()),
                },
                correlation(Some("parent")),
                8,
            )
            .unwrap()
            .unwrap();
        assert_eq!(blocked.state, LifecycleState::Blocked);
    }

    #[test]
    fn mapper_keeps_session_active_until_all_same_agent_turns_complete() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("first")), 2)
            .unwrap();
        let second = mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("second")), 3)
            .unwrap()
            .unwrap();
        assert_eq!(second.state, LifecycleState::Working);

        let blocked = mapper
            .project(
                LifecycleSignal::ApprovalRequested {
                    tool_name: Some("shell".into()),
                },
                correlation(Some("second")),
                4,
            )
            .unwrap()
            .unwrap();
        assert_eq!(blocked.state, LifecycleState::Blocked);

        let first_complete = mapper
            .project(
                LifecycleSignal::TurnCompleted,
                correlation(Some("first")),
                5,
            )
            .unwrap()
            .unwrap();
        assert_eq!(first_complete.state, LifecycleState::Blocked);
        assert!(
            mapper
                .project(LifecycleSignal::SessionEnded, correlation(None), 6)
                .unwrap()
                .is_none()
        );

        let resumed = mapper
            .project(
                LifecycleSignal::ApprovalResponded {
                    tool_name: Some("shell".into()),
                    outcome: ApprovalOutcome::Granted,
                },
                correlation(Some("second")),
                7,
            )
            .unwrap()
            .unwrap();
        assert_eq!(resumed.state, LifecycleState::Working);
        let second_complete = mapper
            .project(
                LifecycleSignal::TurnCompleted,
                correlation(Some("second")),
                8,
            )
            .unwrap()
            .unwrap();
        assert_eq!(second_complete.state, LifecycleState::Idle);
        assert!(
            mapper
                .project(LifecycleSignal::SessionEnded, correlation(None), 9)
                .unwrap()
                .is_some_and(|event| event.state == LifecycleState::Done)
        );
    }

    #[test]
    fn mapper_coalesces_repeated_working_edges_until_the_state_advances() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("turn")), 2)
            .unwrap();
        let signal = LifecycleSignal::ToolStarted {
            tool_name: Some("shell".into()),
        };
        assert!(
            mapper
                .project(signal.clone(), correlation(Some("turn")), 3)
                .unwrap()
                .is_some()
        );
        assert!(
            mapper
                .project(signal, correlation(Some("turn")), 4)
                .unwrap()
                .is_none()
        );
        assert!(
            mapper
                .project(LifecycleSignal::TurnStarted, correlation(Some("turn")), 5)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn payload_is_closed_and_content_free() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        mapper
            .project(LifecycleSignal::TurnStarted, correlation(Some("turn")), 2)
            .unwrap();
        let event = mapper
            .project(
                LifecycleSignal::ToolStarted {
                    tool_name: Some("bad\u{7}tool".repeat(32)),
                },
                correlation(Some("turn")),
                3,
            )
            .unwrap()
            .unwrap();
        let value = serde_json::to_value(event).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            [
                "correlation",
                "event",
                "schema_version",
                "sequence",
                "state",
                "timestamp_unix_ms",
                "tool_name",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        );
        let serialized = serde_json::to_string(&value).unwrap();
        for forbidden in [
            "prompt",
            "response",
            "arguments",
            "result",
            "credential",
            "memory",
            "metadata",
        ] {
            assert!(!serialized.contains(forbidden));
        }
        assert!(event_tool_name(&value).chars().count() <= MAX_LIFECYCLE_TOOL_NAME_CHARS);
    }

    fn event_tool_name(value: &serde_json::Value) -> &str {
        value["tool_name"].as_str().unwrap()
    }

    #[test]
    fn mapper_requires_turn_identity_for_turn_events() {
        let mut mapper = LifecycleMapper::default();
        mapper
            .project(LifecycleSignal::SessionStarted, correlation(None), 1)
            .unwrap();
        assert_eq!(
            mapper
                .project(LifecycleSignal::TurnStarted, correlation(None), 2)
                .unwrap_err(),
            LifecycleMapError::MissingTurnId {
                event: LifecycleEventKind::TurnStarted
            }
        );
    }

    #[test]
    fn mapper_does_not_retain_events_for_unknown_sessions() {
        let mut mapper = LifecycleMapper::default();
        assert!(
            mapper
                .project(
                    LifecycleSignal::ToolStarted {
                        tool_name: Some("shell".into()),
                    },
                    correlation(Some("orphan-turn")),
                    1,
                )
                .unwrap()
                .is_none()
        );
        assert!(mapper.sessions.is_empty());
    }
}
