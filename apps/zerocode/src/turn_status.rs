//! Status of the current agent turn, surfaced in the input-bar title.

use std::time::Instant;
use zeroclaw_api::lifecycle::{LifecycleActivity, LifecycleState};

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
    /// A structured user question is blocking the turn.
    WaitingForInput,
    /// Cancel request fired; awaiting `TurnComplete` so input stays gated
    /// until the daemon actually winds the turn down.
    Cancelling,
}

impl TurnStatus {
    /// Classify detailed TUI state through the shared content-free contract.
    pub(crate) const fn lifecycle_activity(&self) -> LifecycleActivity {
        match self {
            Self::Idle => LifecycleActivity::Idle,
            Self::CallingTool(_) => LifecycleActivity::Tool,
            Self::WaitingForApproval | Self::WaitingForInput => LifecycleActivity::Approval,
            Self::Working | Self::Thinking | Self::Responding | Self::Cancelling => {
                LifecycleActivity::Turn
            }
        }
    }

    /// Canonical state consumed by terminal and future lifecycle projections.
    pub(crate) const fn lifecycle_state(&self) -> LifecycleState {
        self.lifecycle_activity().state()
    }

    /// Verb (no parens, no dots) — `None` for states that render without dots.
    pub(crate) fn verb(&self) -> Option<String> {
        match self {
            TurnStatus::Idle => None,
            TurnStatus::Working => Some(crate::i18n::t("zc-chat-status-working")),
            TurnStatus::Thinking => Some(crate::i18n::t("zc-chat-status-thinking")),
            TurnStatus::Responding => Some(crate::i18n::t("zc-chat-status-responding")),
            TurnStatus::CallingTool(name) => Some(crate::i18n::t_args(
                "zc-chat-status-calling-tool",
                &[("tool", name)],
            )),
            TurnStatus::WaitingForApproval | TurnStatus::WaitingForInput => None,
            TurnStatus::Cancelling => Some(crate::i18n::t("zc-chat-status-cancelling")),
        }
    }

    pub fn label(&self, animation_origin: Instant) -> String {
        match self {
            TurnStatus::Idle => " > ".to_string(),
            TurnStatus::WaitingForApproval => {
                format!(" ({}) ", crate::i18n::t("zc-chat-status-awaiting-approval"))
            }
            TurnStatus::WaitingForInput => {
                format!(" ({}) ", crate::i18n::t("zc-chat-status-awaiting-input"))
            }
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
            format!(" ({}) ", crate::i18n::t("zc-chat-status-awaiting-approval"))
        );
    }

    #[test]
    fn input_label_has_no_dots() {
        let past = Instant::now() - Duration::from_secs(5);
        assert_eq!(
            TurnStatus::WaitingForInput.label(past),
            format!(" ({}) ", crate::i18n::t("zc-chat-status-awaiting-input"))
        );
        assert_eq!(
            TurnStatus::WaitingForInput.lifecycle_state(),
            LifecycleState::Blocked
        );
    }

    #[test]
    fn working_label_has_dots_animation() {
        // origin = now → 0 ms elapsed → phase 0 → no dots.
        assert_eq!(
            TurnStatus::Working.label(Instant::now()),
            format!(" ({}) ", crate::i18n::t("zc-chat-status-working"))
        );
    }

    #[test]
    fn calling_tool_includes_name() {
        let s = TurnStatus::CallingTool("git_diff".into()).label(Instant::now());
        assert!(s.contains("git_diff"), "got: {s}");
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

    #[test]
    fn every_detailed_status_uses_the_shared_lifecycle_mapping() {
        for status in [
            TurnStatus::Working,
            TurnStatus::Thinking,
            TurnStatus::Responding,
            TurnStatus::Cancelling,
        ] {
            assert_eq!(status.lifecycle_activity(), LifecycleActivity::Turn);
            assert_eq!(status.lifecycle_state(), LifecycleState::Working);
        }

        assert_eq!(
            TurnStatus::CallingTool("git".into()).lifecycle_activity(),
            LifecycleActivity::Tool
        );
        for status in [TurnStatus::WaitingForApproval, TurnStatus::WaitingForInput] {
            assert_eq!(status.lifecycle_activity(), LifecycleActivity::Approval);
            assert_eq!(status.lifecycle_state(), LifecycleState::Blocked);
        }
        assert_eq!(
            TurnStatus::Idle.lifecycle_activity(),
            LifecycleActivity::Idle
        );
        assert_eq!(TurnStatus::Idle.lifecycle_state(), LifecycleState::Idle);
    }
}
