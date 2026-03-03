//! TUI event translation and typed event definitions.
//!
//! `translate_delta` maps agent draft-stream payloads into explicit UI events.

use crate::agent::loop_::{
    DRAFT_CLEAR_SENTINEL, DRAFT_PROGRESS_BLOCK_SENTINEL, DRAFT_PROGRESS_SENTINEL,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiEvent {
    // Agent delta-stream events
    Delta { text: String },
    Clear,
    ProgressLine { text: String },
    ProgressBlock { content: String },

    // User intent events
    UserMessage { content: String },
    Cancel,
    Quit,

    // Terminal events
    Key(ratatui::crossterm::event::KeyEvent),
    Resize(u16, u16),
}

pub fn translate_delta(delta: String) -> TuiEvent {
    if delta == DRAFT_CLEAR_SENTINEL {
        TuiEvent::Clear
    } else if let Some(content) = delta.strip_prefix(DRAFT_PROGRESS_BLOCK_SENTINEL) {
        TuiEvent::ProgressBlock {
            content: content.to_string(),
        }
    } else if let Some(text) = delta.strip_prefix(DRAFT_PROGRESS_SENTINEL) {
        TuiEvent::ProgressLine {
            text: text.to_string(),
        }
    } else {
        TuiEvent::Delta { text: delta }
    }
}

#[cfg(test)]
mod tests {
    use super::{translate_delta, TuiEvent};
    use crate::agent::loop_::{
        DRAFT_CLEAR_SENTINEL, DRAFT_PROGRESS_BLOCK_SENTINEL, DRAFT_PROGRESS_SENTINEL,
    };

    #[test]
    fn translate_clear_sentinel() {
        let event = translate_delta(DRAFT_CLEAR_SENTINEL.to_string());
        assert_eq!(event, TuiEvent::Clear);
    }

    #[test]
    fn translate_progress_line_with_payload() {
        let event = translate_delta(format!("{DRAFT_PROGRESS_SENTINEL}phase\n"));
        assert_eq!(
            event,
            TuiEvent::ProgressLine {
                text: "phase\n".to_string()
            }
        );
    }

    #[test]
    fn translate_progress_block_with_payload() {
        let event = translate_delta(format!("{DRAFT_PROGRESS_BLOCK_SENTINEL}⏳ shell: ls\n"));
        assert_eq!(
            event,
            TuiEvent::ProgressBlock {
                content: "⏳ shell: ls\n".to_string()
            }
        );
    }

    #[test]
    fn translate_plain_delta() {
        let raw = "plain response".to_string();
        let event = translate_delta(raw.clone());
        assert_eq!(event, TuiEvent::Delta { text: raw });
    }

    #[test]
    fn translate_empty_progress_payload_as_empty_line() {
        let event = translate_delta(DRAFT_PROGRESS_SENTINEL.to_string());
        assert_eq!(
            event,
            TuiEvent::ProgressLine {
                text: String::new()
            }
        );
    }

    #[test]
    fn null_byte_injection_falls_back_to_plain_delta() {
        let payload = "\x00FAKE\x00alert".to_string();
        let event = translate_delta(payload.clone());
        assert_eq!(event, TuiEvent::Delta { text: payload });
    }
}
