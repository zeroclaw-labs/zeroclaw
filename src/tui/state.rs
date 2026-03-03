//! TUI view-model state.
//!
//! This module intentionally stays UI-focused and does not import agent internals.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Editing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiStatus {
    Idle,
    Thinking,
    ToolRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiRole {
    User,
    Assistant,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiChatMessage {
    pub role: TuiRole,
    pub content: String,
}

impl TuiChatMessage {
    pub fn new(role: TuiRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Debug)]
pub struct TuiState {
    pub messages: Vec<TuiChatMessage>,
    pub input_buffer: String,
    pub input_history: Vec<String>,
    pub scroll_offset: usize,
    pub should_quit: bool,
    pub mode: InputMode,
    pub progress_block: Option<String>,
    pub progress_line: Option<String>,
    pub status: TuiStatus,
    pub provider_id: String,
    pub model_id: String,
    pub awaiting_response: bool,
    pub streaming_assistant_idx: Option<usize>,
    history_cursor: Option<usize>,
}

impl TuiState {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            messages: Vec::new(),
            input_buffer: String::new(),
            input_history: Vec::new(),
            scroll_offset: 0,
            should_quit: false,
            mode: InputMode::Editing,
            progress_block: None,
            progress_line: None,
            status: TuiStatus::Idle,
            provider_id: provider_id.into(),
            model_id: model_id.into(),
            awaiting_response: false,
            streaming_assistant_idx: None,
            history_cursor: None,
        }
    }

    pub fn push_chat_message(&mut self, role: TuiRole, content: impl Into<String>) {
        self.messages.push(TuiChatMessage::new(role, content));
        // New messages should keep the viewport pinned to bottom by default.
        self.scroll_offset = 0;
    }

    pub fn start_streaming_assistant(&mut self) {
        if let Some(idx) = self.streaming_assistant_idx {
            if let Some(msg) = self.messages.get_mut(idx) {
                msg.content.clear();
                return;
            }
        }
        self.push_chat_message(TuiRole::Assistant, String::new());
        self.streaming_assistant_idx = Some(self.messages.len().saturating_sub(1));
    }

    pub fn append_stream_delta(&mut self, delta: &str) {
        if self.streaming_assistant_idx.is_none() {
            self.start_streaming_assistant();
        }
        if let Some(idx) = self.streaming_assistant_idx {
            if let Some(msg) = self.messages.get_mut(idx) {
                msg.content.push_str(delta);
            }
        }
    }

    pub fn finish_streaming_assistant(&mut self) {
        self.streaming_assistant_idx = None;
    }

    pub fn note_submitted_input(&mut self, content: &str) {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            self.history_cursor = None;
            return;
        }
        let should_push = self
            .input_history
            .last()
            .is_none_or(|last| last.as_str() != trimmed);
        if should_push {
            self.input_history.push(trimmed.to_string());
        }
        self.history_cursor = None;
    }

    pub fn history_prev(&mut self) -> Option<String> {
        if self.input_history.is_empty() {
            return None;
        }
        let next_idx = match self.history_cursor {
            None => self.input_history.len().saturating_sub(1),
            Some(idx) => idx.saturating_sub(1),
        };
        self.history_cursor = Some(next_idx);
        self.input_history.get(next_idx).cloned()
    }

    pub fn history_next(&mut self) -> Option<String> {
        let current = self.history_cursor?;
        if current + 1 >= self.input_history.len() {
            self.history_cursor = None;
            return Some(String::new());
        }
        let next_idx = current + 1;
        self.history_cursor = Some(next_idx);
        self.input_history.get(next_idx).cloned()
    }

    pub fn scroll_page_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines.max(1));
    }

    pub fn scroll_page_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines.max(1));
    }

    pub fn set_thinking(&mut self, line: Option<String>) {
        self.status = TuiStatus::Thinking;
        self.progress_line = line;
    }

    pub fn set_tool_running(&mut self, block: String) {
        self.status = TuiStatus::ToolRunning;
        self.progress_block = Some(block);
    }

    pub fn clear_progress(&mut self) {
        self.progress_line = None;
        self.progress_block = None;
    }

    pub fn set_idle(&mut self) {
        self.status = TuiStatus::Idle;
    }

    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            InputMode::Normal => InputMode::Editing,
            InputMode::Editing => InputMode::Normal,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{InputMode, TuiRole, TuiState};

    #[test]
    fn push_message_appends_and_keeps_bottom_scroll() {
        let mut state = TuiState::new("provider", "model");
        state.scroll_offset = 8;
        state.push_chat_message(TuiRole::User, "hello");
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "hello");
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn toggle_mode_switches_between_normal_and_editing() {
        let mut state = TuiState::new("provider", "model");
        assert_eq!(state.mode, InputMode::Editing);
        state.toggle_mode();
        assert_eq!(state.mode, InputMode::Normal);
        state.toggle_mode();
        assert_eq!(state.mode, InputMode::Editing);
    }

    #[test]
    fn scroll_offset_respects_saturating_bounds() {
        let mut state = TuiState::new("provider", "model");
        state.scroll_page_up(20);
        assert_eq!(state.scroll_offset, 20);
        state.scroll_page_down(7);
        assert_eq!(state.scroll_offset, 13);
        state.scroll_page_down(50);
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn history_navigation_walks_back_and_forward() {
        let mut state = TuiState::new("provider", "model");
        state.note_submitted_input("first");
        state.note_submitted_input("second");
        state.note_submitted_input("third");

        assert_eq!(state.history_prev().as_deref(), Some("third"));
        assert_eq!(state.history_prev().as_deref(), Some("second"));
        assert_eq!(state.history_prev().as_deref(), Some("first"));
        assert_eq!(state.history_next().as_deref(), Some("second"));
        assert_eq!(state.history_next().as_deref(), Some("third"));
        assert_eq!(state.history_next().as_deref(), Some(""));
    }
}
