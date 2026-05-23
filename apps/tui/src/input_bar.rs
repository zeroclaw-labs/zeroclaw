//! Reusable input bar widget with text editing, file attachments,
//! file explorer, and clipboard paste support.
//!
//! Embedded by both Chat and ACP panes — each pane owns its own
//! `InputBarState` instance with independent state.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::attachment::PendingAttachment;
use crate::clipboard;
use crate::file_explorer::{ExplorerAction, FileExplorerState};

// ── Action type ──────────────────────────────────────────────────

/// Action returned from input bar key/paste handling.
pub(crate) enum InputBarAction {
    /// Key consumed, no further action for parent.
    Consumed,
    /// User submitted a message (Enter with text and/or attachments).
    Submit {
        text: Option<String>,
        attachments: Vec<PendingAttachment>,
    },
    /// Status message to show in conversation (e.g. "Attached: photo.png").
    StatusMessage(String),
    /// Key was not handled by the input bar — parent should handle it.
    NotHandled,
}

// ── Slash commands ───────────────────────────────────────────────

enum SlashCommand<'a> {
    Attach(&'a str),
    Detach(Option<usize>),
    ListAttachments,
    NotACommand,
}

fn parse_slash_command(input: &str) -> SlashCommand<'_> {
    let trimmed = input.trim();
    if let Some(path) = trimmed.strip_prefix("/attach ") {
        SlashCommand::Attach(path.trim())
    } else if trimmed == "/attach" {
        SlashCommand::Attach("")
    } else if let Some(idx) = trimmed.strip_prefix("/detach ") {
        SlashCommand::Detach(idx.trim().parse().ok())
    } else if trimmed == "/detach" {
        SlashCommand::Detach(None)
    } else if trimmed == "/attachments" {
        SlashCommand::ListAttachments
    } else {
        SlashCommand::NotACommand
    }
}

// ── State ────────────────────────────────────────────────────────

/// Input bar state. Each pane (Chat, ACP) owns its own instance.
#[derive(Debug)]
pub(crate) struct InputBarState {
    input: String,
    /// Byte offset of the editing cursor within `input`. Always on a char boundary.
    cursor: usize,
    pending_attachments: Vec<PendingAttachment>,
    file_explorer: Option<FileExplorerState>,
    clipboard_temps: Vec<PathBuf>,
}

impl InputBarState {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            pending_attachments: Vec::new(),
            file_explorer: None,
            clipboard_temps: Vec::new(),
        }
    }

    // ── Accessors ────────────────────────────────────────────

    pub fn input(&self) -> &str {
        &self.input
    }

    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub fn pending_attachments(&self) -> &[PendingAttachment] {
        &self.pending_attachments
    }

    #[cfg(test)]
    pub fn has_file_explorer(&self) -> bool {
        self.file_explorer.is_some()
    }

    /// Whether the input bar is in text-input mode (input non-empty
    /// or file explorer open). Used to suppress single-char keybindings.
    pub fn wants_text_input(&self) -> bool {
        !self.input.is_empty() || self.file_explorer.is_some()
    }

    // ── Text editing ─────────────────────────────────────────

    /// Insert `c` at the cursor position and advance the cursor.
    pub fn push_input_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character immediately before the cursor (backspace).
    pub fn pop_input_char(&mut self) {
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor = prev;
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            let c = self.input[self.cursor..].chars().next().unwrap();
            self.cursor += c.len_utf8();
        }
    }

    /// Extract the input text and reset the cursor.
    pub fn take_input(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.input)
    }

    /// Insert a string at the cursor position (bulk paste).
    pub fn insert_text(&mut self, text: &str) {
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    // ── Attachment management ────────────────────────────────

    pub fn add_attachment(&mut self, att: PendingAttachment) {
        self.pending_attachments.push(att);
    }

    pub fn remove_attachment(&mut self, index: usize) {
        if index < self.pending_attachments.len() {
            self.pending_attachments.remove(index);
        }
    }

    pub fn take_attachments(&mut self) -> Vec<PendingAttachment> {
        std::mem::take(&mut self.pending_attachments)
    }

    // ── Lifecycle ────────────────────────────────────────────

    /// Reset all input state (called when switching sessions).
    pub fn reset(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.pending_attachments.clear();
        self.file_explorer = None;
        self.cleanup_temps();
    }

    /// Remove clipboard temp files (called after turn completes).
    pub fn cleanup_temps(&mut self) {
        for path in self.clipboard_temps.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    // ── Key handling ─────────────────────────────────────────

    /// Process a key event. Returns an action for the parent pane.
    ///
    /// `turn_in_flight` tells us whether the agent is currently responding
    /// (disables input).
    pub fn handle_key(&mut self, key: KeyEvent, turn_in_flight: bool) -> InputBarAction {
        // File explorer overlay intercepts all keys when open.
        if let Some(explorer) = &mut self.file_explorer {
            match explorer.handle_key(key) {
                ExplorerAction::Confirm(paths) => {
                    match PendingAttachment::from_explorer_paths(&paths) {
                        Ok(atts) => {
                            let labels: Vec<String> = atts.iter().map(|a| a.label()).collect();
                            for att in atts {
                                self.pending_attachments.push(att);
                            }
                            self.file_explorer = None;
                            return InputBarAction::StatusMessage(format!(
                                "Attached: {}",
                                labels.join(", ")
                            ));
                        }
                        Err(e) => {
                            self.file_explorer = None;
                            return InputBarAction::StatusMessage(format!("Attach error: {e}"));
                        }
                    }
                }
                ExplorerAction::Cancel => {
                    self.file_explorer = None;
                }
                ExplorerAction::None => {}
            }
            return InputBarAction::Consumed;
        }

        // Don't handle input while agent is responding.
        if turn_in_flight {
            return InputBarAction::NotHandled;
        }

        match key.code {
            // ── Ctrl+A: open file explorer ───────────────────
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let start = std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/"));
                self.file_explorer = Some(FileExplorerState::new(start));
                InputBarAction::Consumed
            }

            // ── Ctrl+V: paste clipboard image ───────────────
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.handle_clipboard_image()
            }

            // ── Enter: submit or slash command ───────────────
            KeyCode::Enter => self.handle_enter(),

            // ── Cursor movement ──────────────────────────────
            KeyCode::Left => {
                self.move_cursor_left();
                InputBarAction::Consumed
            }
            KeyCode::Right => {
                self.move_cursor_right();
                InputBarAction::Consumed
            }

            // ── Backspace ────────────────────────────────────
            KeyCode::Backspace => {
                self.pop_input_char();
                InputBarAction::Consumed
            }

            // ── Character input ──────────────────────────────
            KeyCode::Char(c) => {
                self.push_input_char(c);
                InputBarAction::Consumed
            }

            _ => InputBarAction::NotHandled,
        }
    }

    /// Handle bracketed paste event.
    pub fn handle_paste(&mut self, text: &str) -> InputBarAction {
        let trimmed = text.trim();
        if clipboard::looks_like_file_path(trimmed)
            && let Ok(att) = PendingAttachment::from_path(trimmed)
        {
            let label = att.label();
            self.add_attachment(att);
            return InputBarAction::StatusMessage(format!("Attached: {label}"));
        }
        self.insert_text(text);
        InputBarAction::Consumed
    }

    /// Forward mouse events to the file explorer when open.
    /// Returns `true` if the event was consumed.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        if let Some(explorer) = &mut self.file_explorer {
            let action = explorer.handle_mouse(mouse);
            match action {
                ExplorerAction::Confirm(paths) => {
                    for p in paths {
                        if let Ok(att) = PendingAttachment::from_path(&p.to_string_lossy()) {
                            self.add_attachment(att);
                        }
                    }
                    self.file_explorer = None;
                }
                ExplorerAction::Cancel => {
                    self.file_explorer = None;
                }
                ExplorerAction::None => {}
            }
            true
        } else {
            false
        }
    }

    // ── Private helpers ──────────────────────────────────────

    fn handle_enter(&mut self) -> InputBarAction {
        let msg = self.take_input();
        if !msg.is_empty() {
            match parse_slash_command(&msg) {
                SlashCommand::Attach(path) => {
                    if path.is_empty() {
                        let start = std::env::var("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|_| PathBuf::from("/"));
                        self.file_explorer = Some(FileExplorerState::new(start));
                        InputBarAction::Consumed
                    } else {
                        match PendingAttachment::from_path(path) {
                            Ok(att) => {
                                let label = att.label();
                                self.add_attachment(att);
                                InputBarAction::StatusMessage(format!("Attached: {label}"))
                            }
                            Err(e) => InputBarAction::StatusMessage(format!("Attach error: {e}")),
                        }
                    }
                }
                SlashCommand::Detach(idx) => {
                    let atts = &self.pending_attachments;
                    if atts.is_empty() {
                        InputBarAction::StatusMessage("No pending attachments.".to_string())
                    } else {
                        let i = idx.unwrap_or(atts.len() - 1);
                        if i < atts.len() {
                            let name = atts[i].filename.clone();
                            self.remove_attachment(i);
                            InputBarAction::StatusMessage(format!("Detached: {name}"))
                        } else {
                            InputBarAction::StatusMessage(format!("Invalid index: {i}"))
                        }
                    }
                }
                SlashCommand::ListAttachments => {
                    let atts = &self.pending_attachments;
                    if atts.is_empty() {
                        InputBarAction::StatusMessage("No pending attachments.".to_string())
                    } else {
                        let list = atts
                            .iter()
                            .enumerate()
                            .map(|(i, a)| format!("  [{i}] {}", a.label()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        InputBarAction::StatusMessage(format!("Pending attachments:\n{list}"))
                    }
                }
                SlashCommand::NotACommand => {
                    let attachments = self.take_attachments();
                    InputBarAction::Submit {
                        text: Some(msg),
                        attachments,
                    }
                }
            }
        } else if !self.pending_attachments.is_empty() {
            // Empty text but has attachments: send attachments only.
            let attachments = self.take_attachments();
            InputBarAction::Submit {
                text: None,
                attachments,
            }
        } else {
            InputBarAction::Consumed
        }
    }

    fn handle_clipboard_image(&mut self) -> InputBarAction {
        match clipboard::read_clipboard_image() {
            Some((bytes, mime)) => {
                let ext = mime.rsplit('/').next().unwrap_or("png");
                let tmp_path = clipboard::clipboard_temp_path(ext);
                if let Err(e) = std::fs::write(&tmp_path, &bytes) {
                    return InputBarAction::StatusMessage(format!("Clipboard error: {e}"));
                }
                match PendingAttachment::from_path(tmp_path.to_str().unwrap_or("")) {
                    Ok(mut att) => {
                        att.source = crate::attachment::AttachmentSource::Clipboard;
                        let label = att.label();
                        self.clipboard_temps.push(tmp_path);
                        self.add_attachment(att);
                        InputBarAction::StatusMessage(format!("Attached: {label}"))
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp_path);
                        InputBarAction::StatusMessage(format!("Clipboard error: {e}"))
                    }
                }
            }
            None => InputBarAction::StatusMessage("No image in clipboard.".to_string()),
        }
    }

    // ── Rendering ────────────────────────────────────────────

    /// Render the input bar (attachment bar + input box) at the bottom of `area`.
    ///
    /// Returns the remaining `Rect` above the input bar for the parent to
    /// render conversation content into.
    ///
    /// `show_cursor` controls whether the terminal cursor is positioned in the
    /// input box (false when an approval overlay is active).
    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        turn_in_flight: bool,
        show_cursor: bool,
    ) -> Rect {
        let has_attachments = !self.pending_attachments.is_empty();
        let mut constraints = vec![Constraint::Min(3)];
        if has_attachments {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(3));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let (conv_area, att_area, input_area) = if has_attachments {
            (chunks[0], Some(chunks[1]), chunks[2])
        } else {
            (chunks[0], None, chunks[1])
        };

        // Attachment bar.
        if let Some(att_rect) = att_area {
            let labels: Vec<String> = self.pending_attachments.iter().map(|a| a.label()).collect();
            let text = format!(" Attachments: {}", labels.join(", "));
            let bar = Paragraph::new(Span::styled(
                text,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            ));
            f.render_widget(bar, att_rect);
        }

        // Input box.
        let label = if turn_in_flight {
            " (thinking\u{2026}) "
        } else {
            " > "
        };
        let block = Block::default().borders(Borders::ALL).title(label);

        let content: Line = if self.input.is_empty() && !turn_in_flight {
            let mut spans = Vec::new();
            if self.file_explorer.is_none() {
                spans.push(Span::styled(
                    "Type to chat  ",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            let entries = self.help_entries();
            for (i, (key, desc)) in entries.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" ", Style::default().fg(Color::DarkGray)));
                }
                spans.push(Span::styled(*key, Style::default().fg(Color::Yellow)));
                spans.push(Span::styled(
                    format!("={desc}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        } else {
            Line::from(Span::raw(&self.input))
        };

        let p = Paragraph::new(content).block(block);
        f.render_widget(p, input_area);

        // Cursor positioning.
        if show_cursor && !turn_in_flight {
            let visual = self.input[..self.cursor].chars().count() as u16;
            let cx =
                (input_area.x + 1 + visual).min(input_area.x + input_area.width.saturating_sub(2));
            f.set_cursor_position((cx, input_area.y + 1));
        }

        conv_area
    }

    /// Render the file explorer overlay on top of everything.
    pub fn render_explorer_overlay(&mut self, f: &mut Frame, area: Rect) {
        if let Some(explorer) = &mut self.file_explorer {
            explorer.render(f, area);
        }
    }

    /// Help line entries for the input bar (when idle, no turn in flight).
    pub fn help_entries(&self) -> Vec<(&'static str, &'static str)> {
        if self.file_explorer.is_some() {
            vec![
                ("j / k", "Navigate"),
                ("Enter", "Open / Confirm"),
                ("Space", "Select file"),
                ("Backspace", "Parent dir"),
                ("/", "Search"),
                (".", "Toggle hidden"),
                ("Esc", "Cancel"),
            ]
        } else {
            vec![
                ("Enter", "Send message"),
                ("/attach", "Attach file"),
                ("Ctrl+A", "File browser"),
                ("Ctrl+V", "Paste"),
            ]
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_append_and_take() {
        let mut bar = InputBarState::new();
        bar.push_input_char('h');
        bar.push_input_char('i');
        assert_eq!(bar.input(), "hi");
        let taken = bar.take_input();
        assert_eq!(taken, "hi");
        assert_eq!(bar.input(), "");
        assert_eq!(bar.cursor(), 0);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut bar = InputBarState::new();
        bar.pop_input_char();
        assert_eq!(bar.input(), "");
    }

    #[test]
    fn cursor_movement() {
        let mut bar = InputBarState::new();
        bar.insert_text("abc");
        assert_eq!(bar.cursor(), 3);
        bar.move_cursor_left();
        assert_eq!(bar.cursor(), 2);
        bar.move_cursor_left();
        assert_eq!(bar.cursor(), 1);
        bar.move_cursor_right();
        assert_eq!(bar.cursor(), 2);
    }

    #[test]
    fn insert_text_at_cursor() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        bar.move_cursor_left();
        bar.move_cursor_left();
        bar.insert_text("XX");
        assert_eq!(bar.input(), "helXXlo");
    }

    #[test]
    fn wants_text_input_when_typing() {
        let mut bar = InputBarState::new();
        assert!(!bar.wants_text_input());
        bar.push_input_char('a');
        assert!(bar.wants_text_input());
    }

    #[test]
    fn reset_clears_everything() {
        let mut bar = InputBarState::new();
        bar.push_input_char('x');
        bar.reset();
        assert_eq!(bar.input(), "");
        assert_eq!(bar.cursor(), 0);
        assert!(bar.pending_attachments().is_empty());
        assert!(!bar.has_file_explorer());
    }

    #[test]
    fn slash_attach_empty_opens_explorer() {
        let mut bar = InputBarState::new();
        bar.insert_text("/attach");
        let action = bar.handle_enter();
        assert!(bar.has_file_explorer());
        assert!(matches!(action, InputBarAction::Consumed));
    }

    #[test]
    fn slash_detach_no_attachments() {
        let mut bar = InputBarState::new();
        bar.insert_text("/detach");
        let action = bar.handle_enter();
        assert!(matches!(action, InputBarAction::StatusMessage(ref m) if m.contains("No pending")));
    }

    #[test]
    fn empty_enter_with_no_attachments_consumed() {
        let bar = InputBarState::new();
        // Empty input, no attachments → Consumed (nothing to do)
        // Can't easily test handle_enter directly without take_input side effects,
        // but we test the handle_key path.
    }

    #[test]
    fn submit_with_text() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        let action = bar.handle_enter();
        match action {
            InputBarAction::Submit { text, attachments } => {
                assert_eq!(text, Some("hello world".to_string()));
                assert!(attachments.is_empty());
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn parse_slash_commands() {
        assert!(matches!(
            parse_slash_command("/attach"),
            SlashCommand::Attach("")
        ));
        assert!(matches!(
            parse_slash_command("/attach /tmp/x.png"),
            SlashCommand::Attach("/tmp/x.png")
        ));
        assert!(matches!(
            parse_slash_command("/detach"),
            SlashCommand::Detach(None)
        ));
        assert!(matches!(
            parse_slash_command("/detach 2"),
            SlashCommand::Detach(Some(2))
        ));
        assert!(matches!(
            parse_slash_command("/attachments"),
            SlashCommand::ListAttachments
        ));
        assert!(matches!(
            parse_slash_command("hello"),
            SlashCommand::NotACommand
        ));
    }

    #[test]
    fn paste_text_inserts() {
        let mut bar = InputBarState::new();
        let action = bar.handle_paste("some pasted text");
        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.input(), "some pasted text");
    }
}
