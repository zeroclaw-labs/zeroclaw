//! TUI widget composition and layout.

pub mod chat;
pub mod input;
pub mod sanitize;
pub mod status;
pub mod tools;

use crate::tui::state::TuiState;
use ratatui::prelude::*;

pub fn render(frame: &mut Frame<'_>, state: &TuiState) -> Option<Position> {
    let show_tools = state
        .progress_block
        .as_ref()
        .is_some_and(|content| !content.trim().is_empty());
    let input_height = input::preferred_height(&state.input_buffer);

    let mut constraints = vec![Constraint::Min(6)];
    if show_tools {
        constraints.push(Constraint::Length(7));
    }
    constraints.push(Constraint::Length(input_height));
    constraints.push(Constraint::Length(1));

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let mut idx = 0usize;
    chat::render(frame, areas[idx], state);
    idx += 1;
    if show_tools {
        tools::render(frame, areas[idx], state);
        idx += 1;
    }
    let cursor = input::render(frame, areas[idx], state);
    idx += 1;
    status::render(frame, areas[idx], state);
    cursor
}
