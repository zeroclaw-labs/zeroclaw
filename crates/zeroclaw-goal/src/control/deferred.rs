//! Shared control classification and opaque ordinary-turn ownership.

use std::fmt;

use zeroclaw_commands::goal::GoalCommand;

/// Timing class shared by all Goal surfaces, independent of presentation text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalControlClass {
    Normal(GoalQueuedControl),
    Immediate,
    ReadOnly,
}

/// Native acknowledgement tag for a control waiting at a turn boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalQueuedControl {
    Start,
    Resume,
    Pause,
    Budget,
}

/// Classify every command without interpreting its objective or resume answer.
pub const fn classify_goal_control(command: &GoalCommand) -> GoalControlClass {
    match command {
        GoalCommand::Start { .. } => GoalControlClass::Normal(GoalQueuedControl::Start),
        GoalCommand::Resume { .. } => GoalControlClass::Normal(GoalQueuedControl::Resume),
        GoalCommand::Pause { immediate: false } => {
            GoalControlClass::Normal(GoalQueuedControl::Pause)
        }
        GoalCommand::SetBudget(_) => GoalControlClass::Normal(GoalQueuedControl::Budget),
        GoalCommand::Cancel | GoalCommand::Pause { immediate: true } => GoalControlClass::Immediate,
        GoalCommand::Help | GoalCommand::Status | GoalCommand::Budget => GoalControlClass::ReadOnly,
    }
}

/// Owned ordinary-turn permit. Dropping it releases only the existing boundary;
/// it creates neither a command reservation nor a durable lifecycle owner.
pub struct GoalTurnBoundary {
    guard: Box<dyn Send + Sync>,
}

impl GoalTurnBoundary {
    /// Retain the surface's ordinary foreground permit through a control commit.
    pub fn new<G: Send + Sync + 'static>(guard: G) -> Self {
        Self {
            guard: Box::new(guard),
        }
    }
}

impl fmt::Debug for GoalTurnBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GoalTurnBoundary")
            .field("guard_size", &std::mem::size_of_val(self.guard.as_ref()))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_answer_does_not_change_the_timing_class() {
        for response in [None, Some("Choice A\nKeep the limits".into())] {
            assert_eq!(
                classify_goal_control(&GoalCommand::Resume { response }),
                GoalControlClass::Normal(GoalQueuedControl::Resume)
            );
        }
    }

    #[test]
    fn queued_tags_roundtrip_without_rendering_conventions() {
        for (kind, tag) in [
            (GoalQueuedControl::Start, "start"),
            (GoalQueuedControl::Resume, "resume"),
            (GoalQueuedControl::Pause, "pause"),
            (GoalQueuedControl::Budget, "budget"),
        ] {
            let value = serde_json::to_value(kind).unwrap();
            assert_eq!(value, tag);
            assert_eq!(
                serde_json::from_value::<GoalQueuedControl>(value).unwrap(),
                kind
            );
        }
    }
}
