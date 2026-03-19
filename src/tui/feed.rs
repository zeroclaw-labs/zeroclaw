use std::collections::VecDeque;
use std::time::Instant;

/// Event type symbols for the feed display.
pub mod symbols {
    pub const CREATED: &str = "+";
    pub const IN_PROGRESS: &str = "\u{2192}"; // →
    pub const COMPLETED: &str = "\u{2713}"; // ✓
    pub const FAILED: &str = "\u{2717}"; // ✗
    pub const DELETED: &str = "\u{2298}"; // ⊘
    pub const PATROL: &str = "\u{1F989}"; // 🦉
    pub const NUDGE: &str = "\u{26A1}"; // ⚡
    pub const TARGET: &str = "\u{1F3AF}"; // 🎯
    pub const HANDOFF: &str = "\u{1F91D}"; // 🤝
}

/// Which panel is focused in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedPanel {
    Agents,
    Convoys,
    Events,
}

/// A single feed event.
#[derive(Debug, Clone)]
pub struct FeedEvent {
    pub timestamp: Instant,
    pub agent: String,
    pub role: String,
    pub event_type: FeedEventType,
    pub message: String,
}

/// Types of events that appear in the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedEventType {
    AgentStarted,
    AgentStopped,
    AgentStuck,
    AgentKilled,
    TaskAssigned,
    TaskCompleted,
    TaskFailed,
    PingSuccess,
    PingFailed,
    Nudge,
    Handoff,
    Custom(String),
}

impl FeedEventType {
    pub fn symbol(&self) -> &str {
        match self {
            Self::AgentStarted => symbols::CREATED,
            Self::AgentStopped | Self::TaskCompleted | Self::PingSuccess => symbols::COMPLETED,
            Self::AgentStuck => symbols::PATROL,
            Self::AgentKilled | Self::TaskFailed | Self::PingFailed => symbols::FAILED,
            Self::TaskAssigned => symbols::IN_PROGRESS,
            Self::Nudge => symbols::NUDGE,
            Self::Handoff => symbols::HANDOFF,
            Self::Custom(_) => symbols::TARGET,
        }
    }
}

/// Feed application state.
pub struct FeedApp {
    /// Active panel focus.
    pub active_panel: FeedPanel,
    /// Event buffer (most recent first).
    pub events: VecDeque<FeedEvent>,
    /// Maximum events to keep in buffer.
    pub max_events: usize,
    /// Scroll offset for events panel.
    pub events_scroll: usize,
    /// Whether we're in problems-only mode.
    pub problems_mode: bool,
}

impl FeedApp {
    pub fn new(max_events: usize) -> Self {
        Self {
            active_panel: FeedPanel::Events,
            events: VecDeque::with_capacity(max_events),
            max_events,
            events_scroll: 0,
            problems_mode: false,
        }
    }

    /// Push a new event to the feed.
    pub fn push_event(&mut self, event: FeedEvent) {
        self.events.push_front(event);
        if self.events.len() > self.max_events {
            self.events.pop_back();
        }
    }

    /// Cycle to the next panel (Tab key).
    pub fn next_panel(&mut self) {
        self.active_panel = match self.active_panel {
            FeedPanel::Agents => FeedPanel::Convoys,
            FeedPanel::Convoys => FeedPanel::Events,
            FeedPanel::Events => FeedPanel::Agents,
        };
    }

    /// Jump to a specific panel by number (1/2/3).
    pub fn jump_to_panel(&mut self, num: u8) {
        self.active_panel = match num {
            1 => FeedPanel::Agents,
            2 => FeedPanel::Convoys,
            _ => FeedPanel::Events,
        };
    }

    /// Scroll down in the events panel (j key).
    pub fn scroll_down(&mut self) {
        if self.events_scroll < self.events.len().saturating_sub(1) {
            self.events_scroll += 1;
        }
    }

    /// Scroll up in the events panel (k key).
    pub fn scroll_up(&mut self) {
        self.events_scroll = self.events_scroll.saturating_sub(1);
    }

    /// Toggle problems-only mode.
    pub fn toggle_problems(&mut self) {
        self.problems_mode = !self.problems_mode;
    }

    /// Get filtered events (all or problems only).
    pub fn visible_events(&self) -> Vec<&FeedEvent> {
        if self.problems_mode {
            self.events
                .iter()
                .filter(|e| {
                    matches!(
                        e.event_type,
                        FeedEventType::AgentStuck
                            | FeedEventType::AgentKilled
                            | FeedEventType::TaskFailed
                            | FeedEventType::PingFailed
                    )
                })
                .collect()
        } else {
            self.events.iter().collect()
        }
    }
}

impl Default for FeedApp {
    fn default() -> Self {
        Self::new(1000)
    }
}

/// Render the feed to plain text (for non-TUI mode).
pub fn render_plain(events: &[FeedEvent]) -> String {
    let mut lines = Vec::new();
    for event in events {
        lines.push(format!(
            "{} [{}] {}: {}",
            event.event_type.symbol(),
            event.role,
            event.agent,
            event.message
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_app_push_and_scroll() {
        let mut app = FeedApp::new(5);
        for i in 0..7 {
            app.push_event(FeedEvent {
                timestamp: Instant::now(),
                agent: format!("agent-{i}"),
                role: "crew".into(),
                event_type: FeedEventType::AgentStarted,
                message: format!("Started agent {i}"),
            });
        }
        // Buffer should cap at max_events
        assert_eq!(app.events.len(), 5);
        // Most recent should be first
        assert_eq!(app.events[0].agent, "agent-6");
    }

    #[test]
    fn panel_cycling() {
        let mut app = FeedApp::default();
        assert_eq!(app.active_panel, FeedPanel::Events);
        app.next_panel();
        assert_eq!(app.active_panel, FeedPanel::Agents);
        app.next_panel();
        assert_eq!(app.active_panel, FeedPanel::Convoys);
        app.next_panel();
        assert_eq!(app.active_panel, FeedPanel::Events);
    }

    #[test]
    fn problems_filter() {
        let mut app = FeedApp::new(100);
        app.push_event(FeedEvent {
            timestamp: Instant::now(),
            agent: "a1".into(),
            role: "crew".into(),
            event_type: FeedEventType::AgentStarted,
            message: "started".into(),
        });
        app.push_event(FeedEvent {
            timestamp: Instant::now(),
            agent: "a2".into(),
            role: "crew".into(),
            event_type: FeedEventType::AgentStuck,
            message: "stuck".into(),
        });
        assert_eq!(app.visible_events().len(), 2);
        app.toggle_problems();
        assert_eq!(app.visible_events().len(), 1);
        assert_eq!(app.visible_events()[0].agent, "a2");
    }
}
