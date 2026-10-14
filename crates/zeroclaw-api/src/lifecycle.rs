//! Content-free lifecycle semantics shared by interactive and runtime surfaces.
//!
//! This module owns the small state vocabulary used by terminal status and by
//! richer lifecycle projections. It deliberately carries no prompt, response,
//! tool input/output, credential, memory, transport, or vendor-specific data.

use serde::{Deserialize, Serialize};

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
    }
}
