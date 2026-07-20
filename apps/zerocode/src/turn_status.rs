//! Status of the current agent turn, surfaced in the input-bar title.

use std::time::Instant;

/// Public so tests and the input bar can pattern-match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TurnStatus {
    #[default]
    Idle,
    /// Request sent; waiting on the first chunk from the model.
    Working,
    /// `AgentThoughtChunk` is currently streaming.
    Thinking,
    /// `AgentMessageChunk` is currently streaming.
    Responding,
    /// A tool call is in flight; carries the tool name for display.
    CallingTool(String),
    /// Approval request is blocking the turn.
    WaitingForApproval,
    /// Cancel request fired; awaiting `TurnComplete` so input stays gated
    /// until the daemon actually winds the turn down.
    Cancelling,
}

impl TurnStatus {
    /// Verb (no parens, no dots) — `None` for states that render without dots.
    fn verb(&self) -> Option<String> {
        match self {
            TurnStatus::Idle => None,
            TurnStatus::Working => Some("working".into()),
            TurnStatus::Thinking => Some("thinking".into()),
            TurnStatus::Responding => Some("responding".into()),
            TurnStatus::CallingTool(name) => Some(format!("calling tool {name}")),
            TurnStatus::WaitingForApproval => None,
            TurnStatus::Cancelling => Some("cancelling".into()),
        }
    }

    pub fn label(&self, animation_origin: Instant) -> String {
        match self {
            TurnStatus::Idle => " > ".to_string(),
            TurnStatus::WaitingForApproval => " (awaiting approval) ".to_string(),
            _ => {
                let verb = self.verb().unwrap_or_default();
                let dots = dots_for(animation_origin);
                format!(" ({verb}{dots}) ")
            }
        }
    }
}

fn dots_for(origin: Instant) -> &'static str {
    let phase = (origin.elapsed().as_millis() / 400) % 4;
    match phase {
        0 => "",
        1 => ".",
        2 => "..",
        _ => "...",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn idle_label_is_unchanged() {
        let now = Instant::now();
        assert_eq!(TurnStatus::Idle.label(now), " > ");
    }

    #[test]
    fn approval_label_has_no_dots() {
        // No dots even as time passes — it's a static "blocked" state.
        let past = Instant::now() - Duration::from_secs(5);
        assert_eq!(
            TurnStatus::WaitingForApproval.label(past),
            " (awaiting approval) "
        );
    }

    #[test]
    fn working_label_has_dots_animation() {
        // origin = now → 0 ms elapsed → phase 0 → no dots.
        assert_eq!(TurnStatus::Working.label(Instant::now()), " (working) ");
    }

    #[test]
    fn calling_tool_includes_name() {
        let s = TurnStatus::CallingTool("git_diff".into()).label(Instant::now());
        assert!(s.starts_with(" (calling tool git_diff"), "got: {s}");
    }

    #[test]
    fn dots_cycle_through_four_phases() {
        // Build origins that are N ms in the past.
        let mk = |ms: u64| Instant::now() - Duration::from_millis(ms);
        assert_eq!(dots_for(mk(0)), "");
        assert_eq!(dots_for(mk(400)), ".");
        assert_eq!(dots_for(mk(800)), "..");
        assert_eq!(dots_for(mk(1200)), "...");
        assert_eq!(dots_for(mk(1600)), ""); // wraps
    }
}
