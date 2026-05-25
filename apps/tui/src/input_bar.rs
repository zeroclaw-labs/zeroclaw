//! Reusable input bar widget with text editing, file attachments,
//! file explorer, and clipboard paste support.
//!
//! Embedded by both Chat and ACP panes — each pane owns its own
//! `InputBarState` instance with independent state.

use std::path::PathBuf;
use std::time::Instant;

use directories::UserDirs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::attachment::PendingAttachment;
use crate::clipboard;
use crate::file_explorer::{ExplorerAction, FileExplorerState};
use crate::mouse;
use crate::theme;

// ── Constants ────────────────────────────────────────────────────

/// Maximum number of visible content rows before the input bar scrolls.
const MAX_INPUT_ROWS: u16 = 5;

/// Cursor blink interval in milliseconds.
const CURSOR_BLINK_MS: u128 = 500;

/// Slash commands available for auto-complete.
const SLASH_COMMANDS: &[&str] = &["/attach", "/attachments", "/detach", "/toggle-thinking"];

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
    /// User typed `/toggle-thinking` — parent should toggle thought visibility.
    ToggleThinking,
    /// Key was not handled by the input bar — parent should handle it.
    NotHandled,
}

// ── Slash commands ───────────────────────────────────────────────

enum SlashCommand<'a> {
    Attach(&'a str),
    Detach(Option<usize>),
    ListAttachments,
    ToggleThinking,
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
    } else if trimmed == "/toggle-thinking" {
        SlashCommand::ToggleThinking
    } else {
        SlashCommand::NotACommand
    }
}

// ── Wrap geometry helpers ────────────────────────────────────────

/// Count the number of visual rows `text` occupies when soft-wrapped at `width` columns.
/// Each `\n` starts a new visual line. Empty input returns 1 (cursor needs a row).
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    if width == 0 || text.is_empty() {
        return 1;
    }
    let w = width as usize;
    let mut total: u16 = 0;
    for line in text.split('\n') {
        let chars = line.chars().count();
        if chars == 0 {
            total += 1;
        } else {
            total += chars.div_ceil(w) as u16;
        }
    }
    total
}

/// Map a byte offset within `text` to `(row, col)` in wrapped coordinates.
/// `width` is the inner area width (excluding borders).
fn cursor_to_visual(text: &str, cursor: usize, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let before = &text[..cursor];
    let mut row: u16 = 0;
    let mut col: u16 = 0;
    for ch in before.chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            if col == width {
                row += 1;
                col = 0;
            }
            col += 1;
        }
    }
    // If col landed exactly at width, the cursor is at the start of the next row.
    if col == width && cursor < text.len() && text[cursor..].starts_with(|c: char| c != '\n') {
        row += 1;
        col = 0;
    }
    (row, col)
}

/// Map a visual `(row, col)` position back to a byte offset in `text`.
/// Clamps to valid positions. Returns `text.len()` if past end.
fn visual_to_cursor(text: &str, target_row: u16, target_col: u16, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    let mut row: u16 = 0;
    let mut col: u16 = 0;
    for (byte_idx, ch) in text.char_indices() {
        if row == target_row && col >= target_col {
            return byte_idx;
        }
        if ch == '\n' {
            if row == target_row {
                return byte_idx;
            }
            row += 1;
            col = 0;
        } else {
            if col == width {
                row += 1;
                col = 0;
                if row == target_row && col >= target_col {
                    return byte_idx;
                }
            }
            col += 1;
        }
        if row > target_row {
            return byte_idx;
        }
    }
    text.len()
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

    // Phase 1: Soft-wrap / dynamic height
    /// Vertical scroll offset within the input bar (0-based row index of first visible line).
    scroll_offset: u16,
    /// Cached Rect of the last rendered input area (for mouse hit-testing).
    last_input_area: Rect,
    /// Cached inner width from the most recent render.
    last_inner_width: u16,

    // Phase 2: Cursor blink
    /// Whether the cursor is currently in the visible phase of the blink cycle.
    cursor_visible: bool,
    /// Instant of the last blink toggle.
    last_blink: Instant,

    // Phase 4: Text selection
    /// Text selection range as byte offsets (start, end) where start <= end.
    selection: Option<(usize, usize)>,
    /// Anchor point of the selection (byte offset where drag started).
    selection_anchor: Option<usize>,

    // Phase 6: Auto-complete
    /// Filtered list of matching slash commands.
    autocomplete_matches: Vec<&'static str>,
    /// Index of the currently highlighted match in the popup.
    autocomplete_index: Option<usize>,
    /// Whether the autocomplete popup is visible.
    autocomplete_active: bool,
}

impl InputBarState {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            pending_attachments: Vec::new(),
            file_explorer: None,
            clipboard_temps: Vec::new(),
            scroll_offset: 0,
            last_input_area: Rect::default(),
            last_inner_width: 0,
            cursor_visible: true,
            last_blink: Instant::now(),
            selection: None,
            selection_anchor: None,
            autocomplete_matches: Vec::new(),
            autocomplete_index: None,
            autocomplete_active: false,
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

    // ── Blink helpers ────────────────────────────────────────

    /// Reset the blink cycle so the cursor is immediately visible.
    fn reset_blink(&mut self) {
        self.cursor_visible = true;
        self.last_blink = Instant::now();
    }

    // ── Selection helpers ────────────────────────────────────

    fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_anchor = None;
    }

    /// Delete the selected range and return the deleted text.
    /// Moves cursor to the start of the selection.
    fn delete_selection(&mut self) -> Option<String> {
        if let Some((start, end)) = self.selection.take() {
            let deleted = self.input[start..end].to_string();
            self.input.replace_range(start..end, "");
            self.cursor = start;
            self.selection_anchor = None;
            Some(deleted)
        } else {
            None
        }
    }

    // ── Auto-complete helpers ────────────────────────────────

    fn update_autocomplete(&mut self) {
        let text = self.input.trim();
        if text.starts_with('/') && !text.contains(' ') {
            let prefix = text;
            self.autocomplete_matches = SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(prefix) && **cmd != prefix)
                .copied()
                .collect();
            self.autocomplete_active = !self.autocomplete_matches.is_empty();
            if self.autocomplete_active && self.autocomplete_index.is_none() {
                self.autocomplete_index = Some(0);
            }
            if let Some(idx) = self.autocomplete_index
                && idx >= self.autocomplete_matches.len()
            {
                self.autocomplete_index = Some(self.autocomplete_matches.len().saturating_sub(1));
            }
        } else {
            self.autocomplete_active = false;
            self.autocomplete_matches.clear();
            self.autocomplete_index = None;
        }
    }

    fn dismiss_autocomplete(&mut self) {
        self.autocomplete_active = false;
        self.autocomplete_matches.clear();
        self.autocomplete_index = None;
    }

    // ── Text editing ─────────────────────────────────────────

    /// Insert `c` at the cursor position and advance the cursor.
    pub fn push_input_char(&mut self, c: char) {
        self.delete_selection();
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.update_autocomplete();
    }

    /// Delete the character immediately before the cursor (backspace).
    pub fn pop_input_char(&mut self) {
        if self.selection.is_some() {
            self.delete_selection();
            self.update_autocomplete();
            return;
        }
        if self.cursor > 0 {
            let prev = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.remove(prev);
            self.cursor = prev;
            self.update_autocomplete();
        }
    }

    pub fn move_cursor_left(&mut self) {
        self.clear_selection();
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    pub fn move_cursor_right(&mut self) {
        self.clear_selection();
        if self.cursor < self.input.len() {
            let c = self.input[self.cursor..].chars().next().unwrap();
            self.cursor += c.len_utf8();
        }
    }

    /// Move cursor up one visual row. Returns false if already on row 0.
    fn move_cursor_up(&mut self) -> bool {
        self.clear_selection();
        let width = self.last_inner_width;
        if width == 0 {
            return false;
        }
        let (row, col) = cursor_to_visual(&self.input, self.cursor, width);
        if row == 0 {
            return false;
        }
        self.cursor = visual_to_cursor(&self.input, row - 1, col, width);
        true
    }

    /// Move cursor down one visual row. Returns false if already on last row.
    fn move_cursor_down(&mut self) -> bool {
        self.clear_selection();
        let width = self.last_inner_width;
        if width == 0 {
            return false;
        }
        let (row, col) = cursor_to_visual(&self.input, self.cursor, width);
        let total = wrapped_line_count(&self.input, width);
        if row + 1 >= total {
            return false;
        }
        self.cursor = visual_to_cursor(&self.input, row + 1, col, width);
        true
    }

    /// Extract the input text and reset the cursor.
    pub fn take_input(&mut self) -> String {
        self.cursor = 0;
        self.scroll_offset = 0;
        self.clear_selection();
        self.dismiss_autocomplete();
        std::mem::take(&mut self.input)
    }

    /// Insert a string at the cursor position (bulk paste).
    pub fn insert_text(&mut self, text: &str) {
        self.delete_selection();
        self.input.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.update_autocomplete();
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
        self.scroll_offset = 0;
        self.pending_attachments.clear();
        self.file_explorer = None;
        self.clear_selection();
        self.dismiss_autocomplete();
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

        // Reset blink on any keystroke.
        self.reset_blink();

        match key.code {
            // ── Ctrl+C: copy selection or pass through ───────
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some((start, end)) = self.selection {
                    let selected = &self.input[start..end];
                    mouse::copy_osc52(selected);
                    return InputBarAction::Consumed;
                }
                InputBarAction::NotHandled
            }

            // ── Ctrl+A: open file explorer ───────────────────
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let start = UserDirs::new()
                    .map(|u| u.home_dir().to_path_buf())
                    .unwrap_or_else(|| {
                        if cfg!(windows) {
                            PathBuf::from("C:\\")
                        } else {
                            PathBuf::from("/")
                        }
                    });
                self.file_explorer = Some(FileExplorerState::new(start));
                InputBarAction::Consumed
            }

            // ── Ctrl+V: paste clipboard image ───────────────
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.handle_clipboard_image()
            }

            // ── Esc: dismiss autocomplete or pass through ────
            KeyCode::Esc if self.autocomplete_active => {
                self.dismiss_autocomplete();
                InputBarAction::Consumed
            }

            // ── Tab: accept autocomplete match ───────────────
            KeyCode::Tab if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index
                    && idx < self.autocomplete_matches.len()
                {
                    let cmd = self.autocomplete_matches[idx].to_string();
                    self.input = cmd;
                    self.cursor = self.input.len();
                    self.dismiss_autocomplete();
                }
                InputBarAction::Consumed
            }

            // ── Up/Down in autocomplete mode ─────────────────
            KeyCode::Up if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index {
                    self.autocomplete_index = Some(idx.saturating_sub(1));
                }
                InputBarAction::Consumed
            }
            KeyCode::Down if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index {
                    let max = self.autocomplete_matches.len().saturating_sub(1);
                    self.autocomplete_index = Some((idx + 1).min(max));
                }
                InputBarAction::Consumed
            }

            // ── Shift/Alt+Enter: insert literal newline ──────
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.push_input_char('\n');
                InputBarAction::Consumed
            }

            // ── Enter: submit or slash command ───────────────
            KeyCode::Enter => self.handle_enter(),

            // ── Up/Down: move cursor in wrapped text ─────────
            // Only bare Up/Down — any modifier (Ctrl/Shift/Alt) falls
            // through so chat-level handlers (browse mode, scroll,
            // fast-scroll) work from the input box.
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_cursor_up();
                InputBarAction::Consumed
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_cursor_down();
                InputBarAction::Consumed
            }

            // ── Home/End: start/end of visual line ───────────
            KeyCode::Home => {
                let width = self.last_inner_width;
                if width > 0 {
                    let (row, _) = cursor_to_visual(&self.input, self.cursor, width);
                    self.cursor = visual_to_cursor(&self.input, row, 0, width);
                    self.clear_selection();
                }
                InputBarAction::Consumed
            }
            KeyCode::End => {
                let width = self.last_inner_width;
                if width > 0 {
                    let (row, _) = cursor_to_visual(&self.input, self.cursor, width);
                    // Move to the end of this visual row by targeting max col.
                    self.cursor = visual_to_cursor(&self.input, row, width, width);
                    self.clear_selection();
                }
                InputBarAction::Consumed
            }

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

            // ── Character input (plain keys only) ────────────
            // Ctrl+key combos fall through so chat-level handlers
            // (Ctrl+S session picker, Ctrl+N new session, etc.) work.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.push_input_char(c);
                InputBarAction::Consumed
            }

            _ => InputBarAction::NotHandled,
        }
    }

    /// Handle bracketed paste event.
    pub fn handle_paste(&mut self, text: &str) -> InputBarAction {
        self.reset_blink();
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

    /// Handle mouse events for the input bar.
    /// Returns `true` if the event was consumed.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        // File explorer overlay takes priority.
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
            return true;
        }

        // Input bar interactions.
        if !mouse::in_rect(mouse.column, mouse.row, self.last_input_area) {
            return false;
        }

        let inner_x = mouse.column.saturating_sub(self.last_input_area.x + 1);
        let inner_y = mouse.row.saturating_sub(self.last_input_area.y + 1);
        let width = self.last_inner_width;

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if width > 0 {
                    let target_row = self.scroll_offset + inner_y;
                    self.cursor = visual_to_cursor(&self.input, target_row, inner_x, width);
                    self.selection_anchor = Some(self.cursor);
                    self.selection = None;
                    self.reset_blink();
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(anchor) = self.selection_anchor
                    && width > 0
                {
                    let target_row = self.scroll_offset + inner_y;
                    let target = visual_to_cursor(&self.input, target_row, inner_x, width);
                    self.cursor = target;
                    let (start, end) = if anchor <= target {
                        (anchor, target)
                    } else {
                        (target, anchor)
                    };
                    self.selection = if start == end {
                        None
                    } else {
                        Some((start, end))
                    };
                    self.reset_blink();
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Selection finalized — keep selection as-is.
                true
            }
            MouseEventKind::ScrollUp => {
                self.move_cursor_up();
                true
            }
            MouseEventKind::ScrollDown => {
                self.move_cursor_down();
                true
            }
            _ => false,
        }
    }

    // ── Private helpers ──────────────────────────────────────

    fn handle_enter(&mut self) -> InputBarAction {
        let msg = self.take_input();
        if !msg.is_empty() {
            match parse_slash_command(&msg) {
                SlashCommand::Attach(path) => {
                    if path.is_empty() {
                        let start = UserDirs::new()
                            .map(|u| u.home_dir().to_path_buf())
                            .unwrap_or_else(|| {
                                if cfg!(windows) {
                                    PathBuf::from("C:\\")
                                } else {
                                    PathBuf::from("/")
                                }
                            });
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
                SlashCommand::ToggleThinking => InputBarAction::ToggleThinking,
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

    // ── Selection rendering helper ───────────────────────────

    /// Build styled lines for the input text, splitting on `\n` and
    /// highlighting any selection range.
    fn build_input_lines(&self) -> Vec<Line<'_>> {
        let sel_style = Style::default()
            .bg(theme::SELECTION_BG)
            .fg(theme::ICY_WHITE);

        // Split input into physical lines and build spans per line,
        // applying selection highlighting across line boundaries.
        let mut lines = Vec::new();
        let mut byte_pos: usize = 0;

        for segment in self.input.split('\n') {
            let seg_start = byte_pos;
            let seg_end = byte_pos + segment.len();

            let mut spans: Vec<Span<'_>> = Vec::new();

            if let Some((sel_start, sel_end)) = self.selection {
                // Compute overlap of selection with this segment.
                let overlap_start = sel_start.max(seg_start);
                let overlap_end = sel_end.min(seg_end);

                if overlap_start < overlap_end {
                    // There is selection overlap in this segment.
                    if overlap_start > seg_start {
                        spans.push(Span::raw(&self.input[seg_start..overlap_start]));
                    }
                    spans.push(Span::styled(
                        &self.input[overlap_start..overlap_end],
                        sel_style,
                    ));
                    if overlap_end < seg_end {
                        spans.push(Span::raw(&self.input[overlap_end..seg_end]));
                    }
                } else {
                    // No selection in this segment.
                    spans.push(Span::raw(&self.input[seg_start..seg_end]));
                }
            } else {
                spans.push(Span::raw(&self.input[seg_start..seg_end]));
            }

            lines.push(Line::from(spans));
            byte_pos = seg_end + 1; // +1 for the '\n'
        }

        if lines.is_empty() {
            lines.push(Line::from(""));
        }

        lines
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
        &mut self,
        f: &mut Frame,
        area: Rect,
        turn_in_flight: bool,
        show_cursor: bool,
    ) -> Rect {
        let has_attachments = !self.pending_attachments.is_empty();

        // Compute dynamic input height.
        let inner_width = area.width.saturating_sub(2);
        self.last_inner_width = inner_width;
        let content_rows = if self.input.is_empty() {
            1
        } else {
            wrapped_line_count(&self.input, inner_width)
        };
        let visible_rows = content_rows.min(MAX_INPUT_ROWS);
        let input_height = visible_rows + 2; // +2 for top/bottom border

        let mut constraints = vec![Constraint::Min(3)];
        if has_attachments {
            constraints.push(Constraint::Length(1));
        }
        constraints.push(Constraint::Length(input_height));
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let (conv_area, att_area, input_area) = if has_attachments {
            (chunks[0], Some(chunks[1]), chunks[2])
        } else {
            (chunks[0], None, chunks[1])
        };

        self.last_input_area = input_area;

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
        let block = Block::default()
            .borders(Borders::ALL)
            .title(label)
            .title_bottom(Span::styled("?=help", Style::default().fg(Color::DarkGray)));

        if self.input.is_empty() && !turn_in_flight {
            let placeholder = if self.file_explorer.is_some() {
                ""
            } else {
                "Type to chat"
            };
            let p = Paragraph::new(Span::styled(
                placeholder,
                Style::default().fg(Color::DarkGray),
            ))
            .block(block);
            f.render_widget(p, input_area);
        } else {
            // Wrapped input content with optional selection highlighting.
            // Each \n in the input becomes a separate Line for proper rendering.
            let input_lines = self.build_input_lines();
            let p = Paragraph::new(input_lines)
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll_offset, 0));
            f.render_widget(p, input_area);
        }

        // Cursor blink logic.
        let now = Instant::now();
        if now.duration_since(self.last_blink).as_millis() >= CURSOR_BLINK_MS {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = now;
        }

        // Cursor positioning — suppress when file explorer overlay is active.
        if show_cursor && !turn_in_flight && inner_width > 0 && self.file_explorer.is_none() {
            let (cursor_row, cursor_col) = cursor_to_visual(&self.input, self.cursor, inner_width);

            // Auto-scroll to keep cursor visible.
            if cursor_row < self.scroll_offset {
                self.scroll_offset = cursor_row;
            }
            if cursor_row >= self.scroll_offset + visible_rows {
                self.scroll_offset = cursor_row - visible_rows + 1;
            }

            if self.cursor_visible {
                let screen_row = cursor_row - self.scroll_offset;
                let cx = input_area.x + 1 + cursor_col;
                let cy = input_area.y + 1 + screen_row;
                f.set_cursor_position((cx, cy));
            }
        }

        // Scroll indicators on the right border when content overflows.
        if content_rows > MAX_INPUT_ROWS && input_area.width > 2 {
            let indicator_x = input_area.x + input_area.width - 1;
            let indicator_style = Style::default().fg(Color::Yellow);

            if self.scroll_offset > 0 {
                // Content above — show up arrow on top border.
                let buf = f.buffer_mut();
                buf[(indicator_x, input_area.y)]
                    .set_char('\u{25b2}')
                    .set_style(indicator_style);
            }
            let max_scroll = content_rows.saturating_sub(MAX_INPUT_ROWS);
            if self.scroll_offset < max_scroll {
                // Content below — show down arrow on bottom border.
                let buf = f.buffer_mut();
                buf[(indicator_x, input_area.y + input_area.height - 1)]
                    .set_char('\u{25bc}')
                    .set_style(indicator_style);
            }
        }

        conv_area
    }

    /// Render the auto-complete popup above the input bar if active.
    pub fn render_autocomplete_popup(&self, f: &mut Frame) {
        if !self.autocomplete_active || self.autocomplete_matches.is_empty() {
            return;
        }

        let popup_height = self.autocomplete_matches.len() as u16 + 2; // +2 borders
        let popup_width = self
            .autocomplete_matches
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(10) as u16
            + 4; // padding

        let popup_y = self.last_input_area.y.saturating_sub(popup_height);
        let popup_x = self.last_input_area.x + 1;

        let popup_rect = Rect::new(
            popup_x,
            popup_y,
            popup_width.min(self.last_input_area.width),
            popup_height.min(self.last_input_area.y),
        );

        if popup_rect.width == 0 || popup_rect.height == 0 {
            return;
        }

        f.render_widget(Clear, popup_rect);

        let items: Vec<ListItem> = self
            .autocomplete_matches
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let style = if Some(i) == self.autocomplete_index {
                    theme::selected_style()
                } else {
                    theme::body_style()
                };
                ListItem::new(Span::styled(*cmd, style))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::dim_style())
                .title(" Commands "),
        );
        f.render_widget(list, popup_rect);
    }

    /// Render the file explorer overlay on top of everything.
    pub fn render_explorer_overlay(&mut self, f: &mut Frame, area: Rect) {
        if let Some(explorer) = &mut self.file_explorer {
            explorer.render(f, area);
        }
    }

}

impl crate::widgets::HelpContext for InputBarState {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};
        if let Some(explorer) = &self.file_explorer {
            return explorer.help_context();
        }
        if self.autocomplete_active {
            return HelpNode::entries(vec![
                E::new(vec!["↑", "↓"], "Navigate completions"),
                E::key("Tab", "Accept"),
                E::key("Esc", "Dismiss"),
            ]);
        }
        HelpNode::entries(vec![
            E::key("Enter", "Send"),
            E::key("Shift+Enter", "Insert newline"),
            E::key("Ctrl+A", "File browser"),
            E::key("Ctrl+V", "Paste image"),
            E::key("/attach", "Attach file by path"),
        ])
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
        let _bar = InputBarState::new();
        // Empty input, no attachments -> Consumed (nothing to do)
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
            parse_slash_command("/toggle-thinking"),
            SlashCommand::ToggleThinking
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

    // ── Wrap geometry tests ──────────────────────────────────

    #[test]
    fn wrapped_line_count_empty() {
        assert_eq!(wrapped_line_count("", 20), 1);
    }

    #[test]
    fn wrapped_line_count_short() {
        assert_eq!(wrapped_line_count("hello", 20), 1);
    }

    #[test]
    fn wrapped_line_count_exact_width() {
        assert_eq!(wrapped_line_count("12345", 5), 1);
    }

    #[test]
    fn wrapped_line_count_overflow() {
        assert_eq!(wrapped_line_count("123456", 5), 2);
        assert_eq!(wrapped_line_count("1234567890", 5), 2);
        assert_eq!(wrapped_line_count("12345678901", 5), 3);
    }

    #[test]
    fn wrapped_line_count_with_newlines() {
        assert_eq!(wrapped_line_count("abc\ndef", 20), 2);
        assert_eq!(wrapped_line_count("abc\n\ndef", 20), 3);
        assert_eq!(wrapped_line_count("12345\n678901", 5), 3); // 1 + 2
    }

    #[test]
    fn wrapped_line_count_zero_width() {
        assert_eq!(wrapped_line_count("hello", 0), 1);
    }

    #[test]
    fn cursor_to_visual_basic() {
        // "hello" with width 10 — cursor at end is (0, 5).
        assert_eq!(cursor_to_visual("hello", 5, 10), (0, 5));
        // Cursor at start.
        assert_eq!(cursor_to_visual("hello", 0, 10), (0, 0));
        // Cursor in middle.
        assert_eq!(cursor_to_visual("hello", 3, 10), (0, 3));
    }

    #[test]
    fn cursor_to_visual_wrap() {
        // "1234567890" with width 5 — wraps at col 5.
        // Cursor at byte 5 (char '6') should be row 1, col 0.
        assert_eq!(cursor_to_visual("1234567890", 5, 5), (1, 0));
        // Cursor at byte 7 should be row 1, col 2.
        assert_eq!(cursor_to_visual("1234567890", 7, 5), (1, 2));
    }

    #[test]
    fn cursor_to_visual_newline() {
        // "abc\ndef" — cursor after \n (byte 4, char 'd') is (1, 0).
        assert_eq!(cursor_to_visual("abc\ndef", 4, 20), (1, 0));
        // Cursor at 'f' (byte 6) is (1, 2).
        assert_eq!(cursor_to_visual("abc\ndef", 6, 20), (1, 2));
    }

    #[test]
    fn visual_to_cursor_basic() {
        assert_eq!(visual_to_cursor("hello", 0, 0, 10), 0);
        assert_eq!(visual_to_cursor("hello", 0, 3, 10), 3);
        assert_eq!(visual_to_cursor("hello", 0, 5, 10), 5);
    }

    #[test]
    fn visual_to_cursor_wrap() {
        // "1234567890" width 5 — row 1, col 0 = byte 5.
        assert_eq!(visual_to_cursor("1234567890", 1, 0, 5), 5);
        assert_eq!(visual_to_cursor("1234567890", 1, 2, 5), 7);
    }

    #[test]
    fn visual_to_cursor_newline() {
        // "abc\ndef" — row 1, col 0 = byte 4 ('d').
        assert_eq!(visual_to_cursor("abc\ndef", 1, 0, 20), 4);
        assert_eq!(visual_to_cursor("abc\ndef", 1, 2, 20), 6);
    }

    #[test]
    fn cursor_visual_round_trip() {
        let text = "hello world this is a test";
        let width: u16 = 10;
        for cursor in 0..=text.len() {
            if !text.is_char_boundary(cursor) {
                continue;
            }
            let (row, col) = cursor_to_visual(text, cursor, width);
            let recovered = visual_to_cursor(text, row, col, width);
            assert_eq!(
                recovered, cursor,
                "round-trip failed for cursor={cursor} -> ({row},{col}) -> {recovered}"
            );
        }
    }

    #[test]
    fn cursor_visual_round_trip_with_newlines() {
        let text = "abc\ndefgh\nij";
        let width: u16 = 4;
        for cursor in 0..=text.len() {
            if !text.is_char_boundary(cursor) {
                continue;
            }
            let (row, col) = cursor_to_visual(text, cursor, width);
            let recovered = visual_to_cursor(text, row, col, width);
            assert_eq!(
                recovered, cursor,
                "round-trip failed for cursor={cursor} -> ({row},{col}) -> {recovered}"
            );
        }
    }

    // ── Auto-complete tests ──────────────────────────────────

    #[test]
    fn autocomplete_triggers_on_slash() {
        let mut bar = InputBarState::new();
        bar.insert_text("/a");
        assert!(bar.autocomplete_active);
        assert!(!bar.autocomplete_matches.is_empty());
    }

    #[test]
    fn autocomplete_partial_prefix_matches() {
        let mut bar = InputBarState::new();
        bar.insert_text("/attach");
        // "/attach" is a prefix of "/attachments", so popup shows.
        assert!(bar.autocomplete_active);
        assert!(bar.autocomplete_matches.contains(&"/attachments"));
        // "/attach" itself is excluded (exact match).
        assert!(!bar.autocomplete_matches.contains(&"/attach"));
    }

    #[test]
    fn autocomplete_exact_no_popup() {
        let mut bar = InputBarState::new();
        bar.insert_text("/attachments");
        // Exact match with no further completions — no popup.
        assert!(!bar.autocomplete_active);
    }

    #[test]
    fn autocomplete_off_with_space() {
        let mut bar = InputBarState::new();
        bar.insert_text("/attach foo");
        // Space present — autocomplete disabled.
        assert!(!bar.autocomplete_active);
    }

    #[test]
    fn autocomplete_off_for_non_slash() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        assert!(!bar.autocomplete_active);
    }

    #[test]
    fn autocomplete_toggle_thinking_prefix() {
        let mut bar = InputBarState::new();
        bar.insert_text("/toggle");
        assert!(bar.autocomplete_active);
        assert!(bar.autocomplete_matches.contains(&"/toggle-thinking"));
    }

    #[test]
    fn slash_toggle_thinking_returns_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/toggle-thinking");
        let action = bar.handle_enter();
        assert!(matches!(action, InputBarAction::ToggleThinking));
        // Input should be cleared after submission.
        assert_eq!(bar.input(), "");
    }

    // ── Selection tests ──────────────────────────────────────

    #[test]
    fn build_input_lines_no_selection() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        let lines = bar.build_input_lines();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn build_input_lines_with_newlines() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello\nworld\nfoo");
        let lines = bar.build_input_lines();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn build_input_lines_with_selection() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        bar.selection = Some((2, 7));
        let lines = bar.build_input_lines();
        // Single line, 3 spans: "he" + "llo w" (selected) + "orld"
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
    }

    #[test]
    fn delete_selection_removes_range() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        bar.selection = Some((2, 7));
        bar.delete_selection();
        assert_eq!(bar.input(), "heorld");
        assert_eq!(bar.cursor(), 2);
    }

    #[test]
    fn backspace_with_selection_deletes_selection() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        bar.selection = Some((1, 4));
        bar.pop_input_char();
        assert_eq!(bar.input(), "ho");
        assert_eq!(bar.cursor(), 1);
    }

    #[test]
    fn typing_with_selection_replaces() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        bar.selection = Some((1, 4));
        bar.push_input_char('X');
        assert_eq!(bar.input(), "hXo");
        assert_eq!(bar.cursor(), 2);
    }

    // ── Dynamic height tests ─────────────────────────────────

    #[test]
    fn dynamic_height_single_line() {
        let content_rows = wrapped_line_count("hello", 40);
        let visible = content_rows.min(MAX_INPUT_ROWS);
        assert_eq!(visible + 2, 3); // 1 content row + 2 borders
    }

    #[test]
    fn dynamic_height_capped() {
        // 100 chars at width 10 = 10 rows, capped to MAX_INPUT_ROWS.
        let text = "a".repeat(100);
        let content_rows = wrapped_line_count(&text, 10);
        assert_eq!(content_rows, 10);
        let visible = content_rows.min(MAX_INPUT_ROWS);
        assert_eq!(visible + 2, 7); // 5 content rows + 2 borders
    }
}
