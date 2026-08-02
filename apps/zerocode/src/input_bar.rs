//! Reusable input bar widget with text editing, file attachments,
//! file explorer, and clipboard paste support.

use std::path::PathBuf;
use std::time::Instant;

use directories::UserDirs;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::attachment::PendingAttachment;
use crate::clipboard;
use crate::file_explorer::{ExplorerAction, FileExplorerState};
use crate::mouse;
use crate::theme;
use crate::turn_status::TurnStatus;

// ── Constants ────────────────────────────────────────────────────

/// Maximum number of visible content rows before the input bar scrolls.
const MAX_INPUT_ROWS: u16 = 5;

/// Maximum number of attachment rows visible in the manager before it scrolls.
const MAX_ATTACHMENT_MANAGER_ROWS: usize = 8;

const ATTACHMENT_REMOVE_LABEL: &str = "[×]";

/// Slash commands available for auto-complete.
const SLASH_COMMANDS: &[&str] = &[
    "/attach",
    "/attachments",
    "/browse",
    "/clear-queue",
    "/detach",
    "/help",
    "/model",
    "/model-provider",
    "/new",
    "/new-session",
    "/restart-session",
    "/toggle-thinking",
];

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
    /// User requested immediate injection (Ctrl+Enter) — skip the queue.
    Inject {
        text: Option<String>,
        attachments: Vec<PendingAttachment>,
    },
    /// Empty Enter (no text, no attachments). Carries no payload but is still
    /// a deliberate keystroke — the parent uses it to resume a paused queue so
    /// a silent pause can never trap the user.
    ResumeQueue,
    /// User typed `/restart-session`, `/new-session`, or `/new` — parent should close
    /// the current session and open a fresh one for the same agent/workspace.
    RestartSession,
    /// User typed `/clear-queue [N]`. The input bar doesn't own the queue, so
    /// it hands removal up to the parent. None = clear all; Some(N) = the
    /// 1-based queue position (Some(0) is an invalid-index sentinel).
    ClearQueue(Option<usize>),
    /// Status message to show in conversation (e.g. "Attached: photo.png").
    StatusMessage(String),
    /// User typed `/toggle-thinking` — parent should toggle thought visibility.
    ToggleThinking,
    /// User typed `/browse` — parent should enter transcript browse mode.
    EnterBrowseMode,
    /// User typed `/help` — parent should open the app-level Help overlay.
    OpenHelp,
    /// User chose a model directly (`/model <name>`) — parent applies it via
    /// `session/configure`.
    SetModel(String),
    /// User chose a model_provider directly (`/model-provider <name>`) — parent
    /// applies it via `session/configure`.
    SetModelProvider(String),
    /// User typed `/model` with no argument — parent opens the model picker
    /// modal over the cached model catalog.
    OpenModelPicker,
    /// User typed `/model-provider` with no argument — parent opens the
    /// two-stage model_provider picker modal.
    OpenModelProviderPicker,
    /// Key was not handled by the input bar — parent should handle it.
    NotHandled,
}

// ── Slash commands ───────────────────────────────────────────────

enum SlashCommand<'a> {
    Attach(&'a str),
    Detach(Option<usize>),
    ListAttachments,
    /// `/clear-queue` (None = clear all) or `/clear-queue N` (Some(N), 1-based).
    /// A malformed index parses to `Some(0)` so the handler can reject it
    /// rather than silently clearing the whole queue.
    ClearQueue(Option<usize>),
    ToggleThinking,
    /// `/model <name>` — switch model directly.
    Model(&'a str),
    /// `/model` (no arg) — open the model picker modal.
    ModelPicker,
    /// `/model-provider <name>` — switch model_provider directly.
    ModelProvider(&'a str),
    /// `/model-provider` (no arg) — open the two-stage model_provider picker.
    ModelProviderPicker,
    RestartSession,
    EnterBrowseMode,
    OpenHelp,
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
    } else if let Some(arg) = trimmed.strip_prefix("/clear-queue ") {
        // Malformed index -> Some(0): an invalid index, never a clear-all, so a
        // typo cannot wipe the whole queue. Only the bare form clears all.
        SlashCommand::ClearQueue(Some(arg.trim().parse().unwrap_or(0)))
    } else if trimmed == "/clear-queue" {
        SlashCommand::ClearQueue(None)
    } else if trimmed == "/attachments" {
        SlashCommand::ListAttachments
    } else if trimmed == "/restart-session" || trimmed == "/new-session" || trimmed == "/new" {
        SlashCommand::RestartSession
    } else if trimmed == "/toggle-thinking" {
        SlashCommand::ToggleThinking
    } else if trimmed == "/browse" {
        SlashCommand::EnterBrowseMode
    } else if trimmed == "/help" {
        SlashCommand::OpenHelp
    } else if let Some(name) = trimmed.strip_prefix("/model-provider ") {
        let name = name.trim();
        if name.is_empty() {
            SlashCommand::ModelProviderPicker
        } else {
            SlashCommand::ModelProvider(name)
        }
    } else if trimmed == "/model-provider" {
        SlashCommand::ModelProviderPicker
    } else if let Some(name) = trimmed.strip_prefix("/model ") {
        let name = name.trim();
        if name.is_empty() {
            SlashCommand::ModelPicker
        } else {
            SlashCommand::Model(name)
        }
    } else if trimmed == "/model" {
        SlashCommand::ModelPicker
    } else {
        SlashCommand::NotACommand
    }
}

// ── Wrap geometry helpers ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisualLine {
    start: usize,
    end: usize,
    width: u16,
}

fn str_cell_width(text: &str) -> u16 {
    crate::display_width::display_width(text)
        .try_into()
        .unwrap_or(u16::MAX)
}

fn grapheme_is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().next().is_some_and(char::is_whitespace)
}

fn push_hard_wrapped(
    lines: &mut Vec<VisualLine>,
    text: &str,
    start: usize,
    end: usize,
    width: u16,
) {
    let mut line_start = start;
    let mut line_width = 0;

    // Advance by grapheme so presentation sequences (e.g. ⚠️) stay one unit.
    for (offset, grapheme, g_width_usize) in
        crate::display_width::grapheme_widths(&text[start..end])
    {
        let byte_idx = start + offset;
        let g_width = u16::try_from(g_width_usize).unwrap_or(u16::MAX);
        if g_width > width {
            continue;
        }
        if line_width > 0 && line_width + g_width > width {
            lines.push(VisualLine {
                start: line_start,
                end: byte_idx,
                width: line_width,
            });
            line_start = byte_idx;
            line_width = 0;
        }
        // Silence unused-binding lint if grapheme is only needed for width.
        let _ = grapheme;
        line_width = line_width.saturating_add(g_width);
    }

    if line_start < end || line_width > 0 {
        lines.push(VisualLine {
            start: line_start,
            end,
            width: line_width,
        });
    }
}

fn push_wrapped_physical_line(
    lines: &mut Vec<VisualLine>,
    text: &str,
    start: usize,
    end: usize,
    width: u16,
) {
    if start == end {
        lines.push(VisualLine {
            start,
            end,
            width: 0,
        });
        return;
    }

    let mut line_start = start;
    let mut line_end = start;
    let mut line_width = 0u16;
    let mut pending_ws_start: Option<usize> = None;
    let mut pending_ws_end = start;
    let mut pending_ws_width = 0u16;
    let mut idx = start;

    while idx < end {
        let Some((_, first_grapheme, _)) =
            crate::display_width::grapheme_widths(&text[idx..end]).next()
        else {
            break;
        };

        if grapheme_is_whitespace(first_grapheme) {
            let ws_start = idx;
            let mut ws_end = idx;
            let mut ws_width = 0u16;
            for (off, g, w) in crate::display_width::grapheme_widths(&text[idx..end]) {
                if !grapheme_is_whitespace(g) {
                    break;
                }
                ws_width = ws_width.saturating_add(u16::try_from(w).unwrap_or(u16::MAX));
                ws_end = idx + off + g.len();
            }
            pending_ws_start = Some(ws_start);
            pending_ws_end = ws_end;
            pending_ws_width = ws_width;
            idx = ws_end;
            continue;
        }

        let word_start = idx;
        let mut word_end = idx;
        let mut word_width = 0u16;
        for (off, g, w) in crate::display_width::grapheme_widths(&text[idx..end]) {
            if grapheme_is_whitespace(g) {
                break;
            }
            word_width = word_width.saturating_add(u16::try_from(w).unwrap_or(u16::MAX));
            word_end = idx + off + g.len();
        }

        if word_width > width {
            if line_end > line_start {
                lines.push(VisualLine {
                    start: line_start,
                    end: line_end,
                    width: line_width,
                });
            }
            push_hard_wrapped(lines, text, word_start, word_end, width);
            line_start = word_end;
            line_end = word_end;
            line_width = 0;
            pending_ws_start = None;
            pending_ws_width = 0;
            idx = word_end;
            continue;
        }

        if line_end == line_start {
            if let Some(ws_start) = pending_ws_start {
                let combined_width = pending_ws_width + word_width;
                if combined_width <= width {
                    line_start = ws_start;
                    line_end = word_end;
                    line_width = combined_width;
                } else {
                    line_start = word_start;
                    line_end = word_end;
                    line_width = word_width;
                }
            } else {
                line_start = word_start;
                line_end = word_end;
                line_width = word_width;
            }
        } else if line_width + pending_ws_width + word_width <= width {
            line_end = word_end;
            line_width = line_width
                .saturating_add(pending_ws_width)
                .saturating_add(word_width);
        } else {
            lines.push(VisualLine {
                start: line_start,
                end: line_end,
                width: line_width,
            });
            line_start = word_start;
            line_end = word_end;
            line_width = word_width;
        }

        pending_ws_start = None;
        pending_ws_width = 0;
        idx = word_end;
    }

    if let Some(ws_start) = pending_ws_start {
        if line_end == line_start {
            push_hard_wrapped(lines, text, ws_start, pending_ws_end, width);
            return;
        }
        if line_width + pending_ws_width <= width {
            line_end = pending_ws_end;
            line_width += pending_ws_width;
        }
    }

    if line_end > line_start {
        lines.push(VisualLine {
            start: line_start,
            end: line_end,
            width: line_width,
        });
    }
}

fn wrap_visual_lines(text: &str, width: u16) -> Vec<VisualLine> {
    if width == 0 {
        return vec![VisualLine {
            start: 0,
            end: 0,
            width: 0,
        }];
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for segment in text.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let content_end = start + segment.len() - usize::from(has_newline);
        push_wrapped_physical_line(&mut lines, text, start, content_end, width);
        start += segment.len();
    }

    if text.is_empty() || text.ends_with('\n') {
        lines.push(VisualLine {
            start: text.len(),
            end: text.len(),
            width: 0,
        });
    }

    lines
}

/// Count the number of visual rows `text` occupies when soft-wrapped at `width` columns.
/// Each `\n` starts a new visual line. Empty input returns 1 (cursor needs a row).
fn wrapped_line_count(text: &str, width: u16) -> u16 {
    wrap_visual_lines(text, width)
        .len()
        .try_into()
        .unwrap_or(u16::MAX)
}

fn is_word_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn previous_word_boundary(text: &str, cursor: usize) -> Option<usize> {
    let mut chars = text[..cursor].char_indices().rev();

    let mut target_is_word = None;
    for (_, character) in chars.by_ref() {
        if character.is_whitespace() {
            continue;
        }
        target_is_word = Some(is_word_character(character));
        break;
    }

    let Some(target_is_word) = target_is_word else {
        return (cursor > 0).then_some(0);
    };

    for (index, character) in chars {
        if character.is_whitespace() || is_word_character(character) != target_is_word {
            return Some(index + character.len_utf8());
        }
    }

    Some(0)
}

/// Decide which overflow arrows to show for `(up, down)` given the total
/// content rows, the visible window, and the current scroll offset. Arrows
/// only appear when content exceeds the window.
fn overflow_arrows(content_rows: u16, visible_rows: u16, scroll_offset: u16) -> (bool, bool) {
    if content_rows <= visible_rows {
        return (false, false);
    }
    let max_scroll = content_rows.saturating_sub(visible_rows);
    (scroll_offset > 0, scroll_offset < max_scroll)
}

/// Map a byte offset within `text` to `(row, col)` in wrapped coordinates.
/// `width` is the inner area width (excluding borders).
fn cursor_to_visual(text: &str, cursor: usize, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let lines = wrap_visual_lines(text, width);
    for (row, line) in lines.iter().enumerate() {
        if cursor >= line.start && cursor <= line.end {
            if cursor == line.end && lines.get(row + 1).is_some_and(|next| next.start == cursor) {
                return ((row + 1).try_into().unwrap_or(u16::MAX), 0);
            }
            let col = if cursor == line.end {
                line.width
            } else {
                str_cell_width(&text[line.start..cursor])
            };
            return (row.try_into().unwrap_or(u16::MAX), col.min(width));
        }
        if cursor < line.start {
            return (row.try_into().unwrap_or(u16::MAX), 0);
        }
    }
    let row = lines.len().saturating_sub(1).try_into().unwrap_or(u16::MAX);
    let col = lines.last().map_or(0, |line| line.width);
    (row, col.min(width))
}

/// Map a visual `(row, col)` position back to a byte offset in `text`.
/// Clamps to valid positions. Returns `text.len()` if past end.
fn visual_to_cursor(text: &str, target_row: u16, target_col: u16, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    let lines = wrap_visual_lines(text, width);
    let Some(line) = lines.get(target_row as usize) else {
        return text.len();
    };

    let mut col = 0u16;
    for (offset, _grapheme, g_width_usize) in
        crate::display_width::grapheme_widths(&text[line.start..line.end])
    {
        if col >= target_col {
            return line.start + offset;
        }
        let g_width = u16::try_from(g_width_usize).unwrap_or(u16::MAX);
        col = col.saturating_add(g_width);
        if col > target_col {
            return line.start + offset;
        }
    }
    if target_col >= line.width {
        line.end
    } else {
        line.start
    }
}

fn attachment_row_at(
    area: Option<Rect>,
    first_index: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    let area = area?;
    if !mouse::in_rect(column, row, area) {
        return None;
    }
    Some(first_index + usize::from(row - area.y))
}

fn attachment_remove_at(
    area: Option<Rect>,
    first_index: usize,
    attachments: &[PendingAttachment],
    column: u16,
    row: u16,
) -> Option<usize> {
    let area = area?;
    let index = attachment_row_at(Some(area), first_index, column, row)?;
    let attachment = attachments.get(index)?;
    let (_, remove_col) = attachment_line(index, &attachment.label(), area.width);
    let remove_col = remove_col?;
    let remove_width = crate::display_width::display_width(ATTACHMENT_REMOVE_LABEL) as u16;
    let relative_col = column - area.x;
    if relative_col < remove_col || relative_col >= remove_col + remove_width {
        return None;
    }
    Some(index)
}

fn attachment_line(index: usize, label: &str, width: u16) -> (String, Option<u16>) {
    let remove_width = crate::display_width::display_width(ATTACHMENT_REMOVE_LABEL) as u16;
    if width < remove_width {
        return (truncate_to_cells(label, width as usize), None);
    }

    let main_width = width - remove_width;
    if main_width == 0 {
        return (String::new(), Some(0));
    }

    let raw = format!(" [{index}] {label}");
    let mut main = truncate_to_cells(&raw, main_width.saturating_sub(1) as usize);
    main.push(' ');
    let remove_col = crate::display_width::display_width(&main) as u16;
    (main, Some(remove_col))
}

fn truncate_to_cells(text: &str, max_width: usize) -> String {
    if crate::display_width::display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut out = String::new();
    let budget = max_width - 1;
    let mut used = 0;
    for (_, grapheme, width) in crate::display_width::grapheme_widths(text) {
        if used + width > budget {
            break;
        }
        out.push_str(grapheme);
        used += width;
    }
    out.push('…');
    out
}

fn attachment_manager_key_labels() -> (Vec<String>, Vec<String>, Vec<String>) {
    use crate::keymap::{InputBarAction as Ib, ModalAction as M, action_key_labels};

    let mut navigate = action_key_labels(M::Up);
    navigate.extend(action_key_labels(M::Down));
    let mut remove = action_key_labels(Ib::Backspace);
    remove.push("Del".to_string());
    let close = action_key_labels(M::Cancel);
    (navigate, remove, close)
}

// ── State ────────────────────────────────────────────────────────

/// Input bar state. Each pane (Chat, ACP) owns its own instance.
#[derive(Debug)]
pub(crate) struct InputBarState {
    input: String,
    /// Byte offset of the editing cursor within `input`. Always on a char boundary.
    cursor: usize,
    pending_attachments: Vec<PendingAttachment>,
    attachment_manager: Option<AttachmentManagerState>,
    /// Latest composer and modal list geometry for mouse hit-testing.
    last_attachment_area: Option<Rect>,
    last_attachment_manager_area: Option<Rect>,
    file_explorer: Option<FileExplorerState>,
    clipboard_temps: Vec<PathBuf>,

    // Phase 1: Soft-wrap / dynamic height
    /// Vertical scroll offset within the input bar (0-based row index of first visible line).
    scroll_offset: u16,
    /// Cached Rect of the last rendered input area (for mouse hit-testing).
    last_input_area: Rect,
    /// Cached inner width from the most recent render.
    last_inner_width: u16,

    // Phase 4: Text selection
    /// Text selection range as byte offsets (start, end) where start <= end.
    selection: Option<(usize, usize)>,
    /// Anchor point of the selection (byte offset where drag started).
    selection_anchor: Option<usize>,

    // Phase 6: Auto-complete
    /// Filtered list of matching slash commands.
    /// Candidate completions for the popup. Command names (`/model`) and
    /// argument values (model / model_provider names) both land here as owned
    /// strings.
    autocomplete_matches: Vec<String>,
    /// What the current popup is completing — drives whether Tab replaces the
    /// whole input or only the trailing argument token.
    autocomplete_target: AutocompleteTarget,
    /// Index of the currently highlighted match in the popup.
    autocomplete_index: Option<usize>,
    /// Whether the autocomplete popup is visible.
    autocomplete_active: bool,

    /// Cached model catalog for the active model_provider, pushed in by the app
    /// layer (the input bar is synchronous and cannot fetch). Filtered for
    /// `/model <partial>` argument autocomplete.
    model_catalog: Vec<String>,
    /// Which model_provider `model_catalog` was fetched for; lets the app layer
    /// decide whether a refetch is needed on provider change.
    model_catalog_provider: Option<String>,
    /// Cached model_provider names for `/model-provider <partial>` autocomplete.
    provider_catalog: Vec<String>,
}

/// What the autocomplete popup is currently offering, so Tab-completion knows
/// how much of the input to rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutocompleteTarget {
    /// Completing a slash-command name (replaces the whole input).
    Command,
    /// Completing the `/model <arg>` value (replaces only the argument).
    ModelArg,
    /// Completing the `/model-provider <arg>` value (replaces only the argument).
    ModelProviderArg,
}

#[derive(Debug, Clone, Copy)]
struct AttachmentManagerState {
    selected: usize,
    scroll: usize,
}

impl InputBarState {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            pending_attachments: Vec::new(),
            attachment_manager: None,
            last_attachment_area: None,
            last_attachment_manager_area: None,
            file_explorer: None,
            clipboard_temps: Vec::new(),
            scroll_offset: 0,
            last_input_area: Rect::default(),
            last_inner_width: 0,
            selection: None,
            selection_anchor: None,
            autocomplete_matches: Vec::new(),
            autocomplete_target: AutocompleteTarget::Command,
            autocomplete_index: None,
            autocomplete_active: false,
            model_catalog: Vec::new(),
            model_catalog_provider: None,
            provider_catalog: Vec::new(),
        }
    }

    // ── Accessors ────────────────────────────────────────────

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn has_pending_attachments(&self) -> bool {
        !self.pending_attachments.is_empty()
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
    pub fn clipboard_temps(&self) -> &[PathBuf] {
        &self.clipboard_temps
    }

    #[cfg(test)]
    pub fn attachment_area(&self) -> Option<Rect> {
        self.last_attachment_area
    }

    pub fn has_file_explorer(&self) -> bool {
        self.file_explorer.is_some()
    }

    pub fn has_attachment_manager(&self) -> bool {
        self.attachment_manager.is_some()
    }

    /// Whether the input bar is in text-input mode (input non-empty or an
    /// input-owned modal open). Used to suppress single-char keybindings.
    pub fn wants_text_input(&self) -> bool {
        !self.input.is_empty() || self.file_explorer.is_some() || self.attachment_manager.is_some()
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
        // Own the trimmed input so subsequent `&mut self` calls don't conflict
        // with a borrow of `self.input`.
        let text = self.input.trim_start().to_string();

        // Command-name completion: a single `/token` with no space yet.
        if text.starts_with('/') && !text.contains(' ') {
            let prefix = text.as_str();
            self.autocomplete_target = AutocompleteTarget::Command;
            self.autocomplete_matches = SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(prefix) && **cmd != prefix)
                .map(|c| (*c).to_string())
                .collect();
            self.finalize_autocomplete();
            return;
        }

        // Argument completion: `/model <partial>` or `/model-provider <partial>`.
        // model_provider is checked first — its prefix is longer and `/model `
        // would otherwise swallow it.
        if let Some(partial) = text.strip_prefix("/model-provider ") {
            let partial = partial.trim_start().to_string();
            self.set_arg_matches(AutocompleteTarget::ModelProviderArg, &partial);
            return;
        }
        if let Some(partial) = text.strip_prefix("/model ") {
            let partial = partial.trim_start().to_string();
            self.set_arg_matches(AutocompleteTarget::ModelArg, &partial);
            return;
        }

        self.autocomplete_active = false;
        self.autocomplete_matches.clear();
        self.autocomplete_index = None;
    }

    /// Filter the relevant cached catalog by `partial` (case-insensitive
    /// substring) and populate the popup. Empty `partial` lists the whole
    /// catalog so the user sees options immediately after the space.
    fn set_arg_matches(&mut self, target: AutocompleteTarget, partial: &str) {
        let catalog = match target {
            AutocompleteTarget::ModelArg => &self.model_catalog,
            AutocompleteTarget::ModelProviderArg => &self.provider_catalog,
            AutocompleteTarget::Command => return,
        };
        let needle = partial.to_ascii_lowercase();
        self.autocomplete_target = target;
        self.autocomplete_matches = catalog
            .iter()
            .filter(|c| needle.is_empty() || c.to_ascii_lowercase().contains(&needle))
            .filter(|c| c.as_str() != partial)
            .cloned()
            .collect();
        self.finalize_autocomplete();
    }

    /// Shared tail of the autocomplete update: toggle visibility and clamp the
    /// highlighted index.
    fn finalize_autocomplete(&mut self) {
        self.autocomplete_active = !self.autocomplete_matches.is_empty();
        if self.autocomplete_active && self.autocomplete_index.is_none() {
            self.autocomplete_index = Some(0);
        }
        if let Some(idx) = self.autocomplete_index
            && idx >= self.autocomplete_matches.len()
        {
            self.autocomplete_index = Some(self.autocomplete_matches.len().saturating_sub(1));
        }
    }

    /// Replace the input's catalog cache for argument autocomplete. Called by
    /// the app layer after an async `catalog_models` / model_provider fetch.
    pub fn set_model_catalog(&mut self, model_provider: String, models: Vec<String>) {
        self.model_catalog = models;
        self.model_catalog_provider = Some(model_provider);
    }

    /// The model_provider the cached model catalog was fetched for, if any.
    pub fn model_catalog_provider(&self) -> Option<&str> {
        self.model_catalog_provider.as_deref()
    }

    /// The cached model catalog (for the model_provider in
    /// [`Self::model_catalog_provider`]).
    pub fn model_catalog(&self) -> &[String] {
        &self.model_catalog
    }

    /// Replace the cached model_provider list for `/model-provider` autocomplete.
    pub fn set_provider_catalog(&mut self, providers: Vec<String>) {
        self.provider_catalog = providers;
    }

    fn dismiss_autocomplete(&mut self) {
        self.autocomplete_active = false;
        self.autocomplete_matches.clear();
        self.autocomplete_index = None;
    }

    /// Apply a chosen popup entry to the input. A command choice replaces the
    /// whole line (and, for the model commands, appends a space so argument
    /// autocomplete kicks in immediately). An argument choice rewrites only the
    /// value after the command prefix.
    fn apply_autocomplete_choice(&mut self, choice: &str) {
        match self.autocomplete_target {
            AutocompleteTarget::Command => {
                let takes_arg = choice == "/model" || choice == "/model-provider";
                self.input = if takes_arg {
                    format!("{choice} ")
                } else {
                    choice.to_string()
                };
            }
            AutocompleteTarget::ModelArg => {
                self.input = format!("/model {choice}");
            }
            AutocompleteTarget::ModelProviderArg => {
                self.input = format!("/model-provider {choice}");
            }
        }
        self.cursor = self.input.len();
    }

    fn accept_completion_on_submit(&mut self) -> InputBarAction {
        let Some(idx) = self.autocomplete_index else {
            return self.handle_enter();
        };
        let Some(choice) = self.autocomplete_matches.get(idx).cloned() else {
            return self.handle_enter();
        };
        let target = self.autocomplete_target;
        self.apply_autocomplete_choice(&choice);
        self.dismiss_autocomplete();
        let fills_only = target == AutocompleteTarget::Command
            && (choice == "/model" || choice == "/model-provider");
        if fills_only {
            return InputBarAction::Consumed;
        }
        self.handle_enter()
    }

    // ── Text editing ─────────────────────────────────────────

    /// Insert `c` at the cursor position and advance the cursor.
    pub fn push_input_char(&mut self, c: char) {
        self.delete_selection();
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.update_autocomplete();
    }

    /// Delete the grapheme cluster immediately before the cursor (backspace).
    pub fn pop_input_char(&mut self) {
        if self.selection.is_some() {
            self.delete_selection();
            self.update_autocomplete();
            return;
        }
        if self.cursor > 0 {
            let prev_grapheme = self.input[..self.cursor]
                .graphemes(true)
                .next_back()
                .unwrap_or("");
            let prev_start = self.cursor - prev_grapheme.len();
            self.input.replace_range(prev_start..self.cursor, "");
            self.cursor = prev_start;
            self.update_autocomplete();
        }
    }

    pub fn delete_previous_word(&mut self) {
        if self.selection.is_some() {
            self.delete_selection();
            self.update_autocomplete();
            return;
        }
        let Some(delete_from) = previous_word_boundary(&self.input, self.cursor) else {
            return;
        };
        self.input.replace_range(delete_from..self.cursor, "");
        self.cursor = delete_from;
        self.update_autocomplete();
    }

    pub fn move_cursor_left(&mut self) {
        self.clear_selection();
        if self.cursor > 0 {
            let prev_grapheme = self.input[..self.cursor]
                .graphemes(true)
                .next_back()
                .unwrap_or("");
            self.cursor -= prev_grapheme.len();
        }
    }

    pub fn move_cursor_right(&mut self) {
        self.clear_selection();
        if self.cursor < self.input.len() {
            let next_grapheme = self.input[self.cursor..]
                .graphemes(true)
                .next()
                .unwrap_or("");
            self.cursor += next_grapheme.len();
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

    pub fn load_for_edit(&mut self, text: String, attachments: Vec<PendingAttachment>) {
        self.input = text;
        self.cursor = self.input.len();
        self.scroll_offset = 0;
        self.attachment_manager = None;
        self.clear_selection();
        self.dismiss_autocomplete();
        for att in &attachments {
            if att.source == crate::attachment::AttachmentSource::Clipboard
                && !self.clipboard_temps.contains(&att.path)
            {
                self.clipboard_temps.push(att.path.clone());
            }
        }
        self.pending_attachments = attachments;
    }

    pub fn remove_attachment(&mut self, index: usize) {
        if index >= self.pending_attachments.len() {
            return;
        }

        let removed = self.pending_attachments.remove(index);
        if removed.source == crate::attachment::AttachmentSource::Clipboard {
            self.clipboard_temps.retain(|path| path != &removed.path);
            let _ = std::fs::remove_file(removed.path);
        }

        if self.pending_attachments.is_empty() {
            self.attachment_manager = None;
        } else if let Some(manager) = &mut self.attachment_manager {
            manager.selected = manager.selected.min(self.pending_attachments.len() - 1);
            manager.scroll = manager.scroll.min(manager.selected);
        }
    }

    fn open_attachment_manager(&mut self) {
        if self.pending_attachments.is_empty() {
            return;
        }
        self.dismiss_autocomplete();
        self.attachment_manager = Some(AttachmentManagerState {
            selected: 0,
            scroll: 0,
        });
    }

    pub fn take_attachments(&mut self) -> Vec<PendingAttachment> {
        self.attachment_manager = None;
        let taken = std::mem::take(&mut self.pending_attachments);
        for att in &taken {
            if att.source == crate::attachment::AttachmentSource::Clipboard {
                self.clipboard_temps.retain(|p| p != &att.path);
            }
        }
        taken
    }

    // ── Lifecycle ────────────────────────────────────────────

    /// Reset all input state (called when switching sessions).
    pub fn reset(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.scroll_offset = 0;
        self.pending_attachments.clear();
        self.attachment_manager = None;
        self.last_attachment_area = None;
        self.last_attachment_manager_area = None;
        self.file_explorer = None;
        self.clear_selection();
        self.dismiss_autocomplete();
        self.cleanup_temps();
    }

    /// Clear the typed text without disturbing pending attachments, history,
    /// or clipboard temps. Bound to the ClearInput action.
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.scroll_offset = 0;
        self.clear_selection();
        self.dismiss_autocomplete();
    }

    /// Remove clipboard temp files (called after turn completes).
    pub fn cleanup_temps(&mut self) {
        for path in self.clipboard_temps.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    // ── Key handling ─────────────────────────────────────────

    /// Process a key event. Returns an action for the parent pane.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputBarAction {
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
                            return InputBarAction::StatusMessage(crate::i18n::t_args(
                                "zc-input-attached",
                                &[("label", &labels.join(", "))],
                            ));
                        }
                        Err(e) => {
                            self.file_explorer = None;
                            return InputBarAction::StatusMessage(crate::i18n::t_args(
                                "zc-input-attach-error",
                                &[("error", &e.to_string())],
                            ));
                        }
                    }
                }
                ExplorerAction::Cancel => {
                    self.file_explorer = None;
                }
                ExplorerAction::ConfirmDir(_) => {
                    self.file_explorer = None;
                }
                ExplorerAction::None => {}
            }
            return InputBarAction::Consumed;
        }

        if self.attachment_manager.is_some() {
            return self.handle_attachment_manager_key(key);
        }

        use crate::keymap::{GlobalAction, InputBarAction as IbWidgetAction};
        let action = IbWidgetAction::from_chord(&key);

        if GlobalAction::from_chord(&key) == Some(GlobalAction::Quit) {
            if let Some((start, end)) = self.selection {
                let selected = &self.input[start..end];
                mouse::copy_osc52(selected);
                return InputBarAction::Consumed;
            }
            return InputBarAction::NotHandled;
        }

        match action {
            Some(IbWidgetAction::Paste) => {
                return self.handle_clipboard_image();
            }
            Some(IbWidgetAction::AutocompleteCancel) if self.autocomplete_active => {
                self.dismiss_autocomplete();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::AutocompleteAccept) if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index
                    && idx < self.autocomplete_matches.len()
                {
                    let choice = self.autocomplete_matches[idx].clone();
                    self.apply_autocomplete_choice(&choice);
                    self.dismiss_autocomplete();
                }
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::HistoryPrev) if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index {
                    self.autocomplete_index = Some(idx.saturating_sub(1));
                }
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::HistoryNext) if self.autocomplete_active => {
                if let Some(idx) = self.autocomplete_index {
                    let max = self.autocomplete_matches.len().saturating_sub(1);
                    self.autocomplete_index = Some((idx + 1).min(max));
                }
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::Submit) if self.autocomplete_active => {
                return self.accept_completion_on_submit();
            }
            Some(IbWidgetAction::NewLine) => {
                self.push_input_char('\n');
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::Submit) => {
                return self.handle_enter();
            }
            Some(IbWidgetAction::Inject) => {
                return self.handle_inject();
            }
            Some(IbWidgetAction::HistoryPrev) => {
                self.move_cursor_up();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::HistoryNext) => {
                self.move_cursor_down();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::OpenFileBrowser) => {
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
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::CursorStart) => {
                let width = self.last_inner_width;
                if width > 0 {
                    let (row, _) = cursor_to_visual(&self.input, self.cursor, width);
                    self.cursor = visual_to_cursor(&self.input, row, 0, width);
                    self.clear_selection();
                }
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::CursorEnd) => {
                let width = self.last_inner_width;
                if width > 0 {
                    let (row, _) = cursor_to_visual(&self.input, self.cursor, width);
                    // Move to the end of this visual row by targeting max col.
                    self.cursor = visual_to_cursor(&self.input, row, width, width);
                    self.clear_selection();
                }
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::CursorLeft) => {
                self.move_cursor_left();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::CursorRight) => {
                self.move_cursor_right();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::Backspace) => {
                self.pop_input_char();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::DeletePreviousWord) => {
                self.delete_previous_word();
                return InputBarAction::Consumed;
            }
            Some(IbWidgetAction::ClearInput) => {
                self.clear_input();
                return InputBarAction::Consumed;
            }
            _ => {}
        }

        if let KeyCode::Char(c) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.push_input_char(c);
            return InputBarAction::Consumed;
        }

        InputBarAction::NotHandled
    }

    /// Handle bracketed paste event.
    pub fn handle_paste(&mut self, text: &str) -> InputBarAction {
        let trimmed = text.trim();
        if clipboard::looks_like_file_path(trimmed)
            && let Ok(att) = PendingAttachment::from_path(trimmed)
        {
            let label = att.label();
            self.add_attachment(att);
            return InputBarAction::StatusMessage(crate::i18n::t_args(
                "zc-input-attached",
                &[("label", &label)],
            ));
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
                ExplorerAction::ConfirmDir(_) => {
                    self.file_explorer = None;
                }
                ExplorerAction::None => {}
            }
            return true;
        }

        if self.attachment_manager.is_some() {
            let first_index = self
                .attachment_manager
                .map(|manager| manager.scroll)
                .unwrap_or(0);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = attachment_remove_at(
                        self.last_attachment_manager_area,
                        first_index,
                        &self.pending_attachments,
                        mouse.column,
                        mouse.row,
                    ) {
                        self.remove_attachment(index);
                    } else if let Some(index) = attachment_row_at(
                        self.last_attachment_manager_area,
                        first_index,
                        mouse.column,
                        mouse.row,
                    ) && let Some(manager) = &mut self.attachment_manager
                    {
                        manager.selected = index;
                    } else {
                        self.attachment_manager = None;
                    }
                }
                MouseEventKind::ScrollUp => self.move_attachment_selection(-1),
                MouseEventKind::ScrollDown => self.move_attachment_selection(1),
                _ => {}
            }
            return true;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(index) = attachment_remove_at(
                self.last_attachment_area,
                0,
                &self.pending_attachments,
                mouse.column,
                mouse.row,
            )
        {
            self.remove_attachment(index);
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
                                InputBarAction::StatusMessage(crate::i18n::t_args(
                                    "zc-input-attached",
                                    &[("label", &label)],
                                ))
                            }
                            Err(e) => InputBarAction::StatusMessage(crate::i18n::t_args(
                                "zc-input-attach-error",
                                &[("error", &e.to_string())],
                            )),
                        }
                    }
                }
                SlashCommand::Detach(idx) => {
                    let atts = &self.pending_attachments;
                    if atts.is_empty() {
                        InputBarAction::StatusMessage(crate::i18n::t(
                            "zc-input-no-pending-attachments",
                        ))
                    } else {
                        let i = idx.unwrap_or(atts.len() - 1);
                        if i < atts.len() {
                            let name = atts[i].filename.clone();
                            self.remove_attachment(i);
                            InputBarAction::StatusMessage(crate::i18n::t_args(
                                "zc-input-detached",
                                &[("name", &name)],
                            ))
                        } else {
                            InputBarAction::StatusMessage(crate::i18n::t_args(
                                "zc-input-invalid-index",
                                &[("index", &i.to_string())],
                            ))
                        }
                    }
                }
                SlashCommand::ListAttachments => {
                    if self.pending_attachments.is_empty() {
                        InputBarAction::StatusMessage(crate::i18n::t(
                            "zc-input-no-pending-attachments",
                        ))
                    } else {
                        self.open_attachment_manager();
                        InputBarAction::Consumed
                    }
                }
                SlashCommand::ClearQueue(idx) => InputBarAction::ClearQueue(idx),
                SlashCommand::RestartSession => InputBarAction::RestartSession,
                SlashCommand::ToggleThinking => InputBarAction::ToggleThinking,
                SlashCommand::EnterBrowseMode => InputBarAction::EnterBrowseMode,
                SlashCommand::OpenHelp => InputBarAction::OpenHelp,
                SlashCommand::Model(name) => InputBarAction::SetModel(name.to_string()),
                SlashCommand::ModelPicker => InputBarAction::OpenModelPicker,
                SlashCommand::ModelProvider(name) => {
                    InputBarAction::SetModelProvider(name.to_string())
                }
                SlashCommand::ModelProviderPicker => InputBarAction::OpenModelProviderPicker,
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
            InputBarAction::ResumeQueue
        }
    }

    fn handle_inject(&mut self) -> InputBarAction {
        let msg = self.take_input();
        if !msg.is_empty() {
            if matches!(parse_slash_command(&msg), SlashCommand::NotACommand) {
                let attachments = self.take_attachments();
                InputBarAction::Inject {
                    text: Some(msg),
                    attachments,
                }
            } else {
                self.insert_text(&msg);
                self.handle_enter()
            }
        } else if !self.pending_attachments.is_empty() {
            let attachments = self.take_attachments();
            InputBarAction::Inject {
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
                    return InputBarAction::StatusMessage(crate::i18n::t_args(
                        "zc-input-clipboard-error",
                        &[("error", &e.to_string())],
                    ));
                }
                match PendingAttachment::from_path(tmp_path.to_str().unwrap_or("")) {
                    Ok(mut att) => {
                        att.source = crate::attachment::AttachmentSource::Clipboard;
                        let label = att.label();
                        self.clipboard_temps.push(tmp_path);
                        self.add_attachment(att);
                        InputBarAction::StatusMessage(crate::i18n::t_args(
                            "zc-input-attached",
                            &[("label", &label)],
                        ))
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp_path);
                        InputBarAction::StatusMessage(crate::i18n::t_args(
                            "zc-input-clipboard-error",
                            &[("error", &e.to_string())],
                        ))
                    }
                }
            }
            None => self.paste_clipboard_text(),
        }
    }

    fn handle_attachment_manager_key(&mut self, key: KeyEvent) -> InputBarAction {
        use crate::keymap::{InputBarAction as Ib, ModalAction};

        match ModalAction::from_chord(&key) {
            Some(ModalAction::Up) => self.move_attachment_selection(-1),
            Some(ModalAction::Down) => self.move_attachment_selection(1),
            Some(ModalAction::Cancel) => self.attachment_manager = None,
            _ if Ib::from_chord(&key) == Some(Ib::Backspace) || key.code == KeyCode::Delete => {
                if let Some(index) = self.attachment_manager.map(|manager| manager.selected) {
                    self.remove_attachment(index);
                }
            }
            _ => match key.code {
                KeyCode::Home => {
                    if let Some(manager) = &mut self.attachment_manager {
                        manager.selected = 0;
                        manager.scroll = 0;
                    }
                }
                KeyCode::End => {
                    if let Some(manager) = &mut self.attachment_manager {
                        manager.selected = self.pending_attachments.len().saturating_sub(1);
                    }
                }
                _ => {}
            },
        };
        InputBarAction::Consumed
    }

    fn move_attachment_selection(&mut self, delta: isize) {
        let Some(manager) = &mut self.attachment_manager else {
            return;
        };
        let last = self.pending_attachments.len().saturating_sub(1);
        manager.selected = if delta < 0 {
            manager.selected.saturating_sub(delta.unsigned_abs())
        } else {
            manager.selected.saturating_add(delta as usize).min(last)
        };
    }

    fn paste_clipboard_text(&mut self) -> InputBarAction {
        match clipboard::read_clipboard_text() {
            Some(text) => {
                // Get-Clipboard -Raw and some tools append a trailing newline.
                // Strip one trailing CRLF/LF so a one-line paste stays one
                // line; interior newlines (genuine multi-line paste) are kept.
                let text = text.strip_suffix('\n').unwrap_or(&text);
                let text = text.strip_suffix('\r').unwrap_or(text);
                self.handle_paste(text)
            }
            None => InputBarAction::StatusMessage(crate::i18n::t("zc-input-no-clipboard-image")),
        }
    }

    // ── Selection rendering helper ───────────────────────────

    fn build_input_lines(&self, width: u16) -> Vec<Line<'_>> {
        let sel_style = Style::default()
            .bg(theme::selection_bg())
            .fg(theme::fg_primary());
        let input_style = theme::input_style();

        let visual = wrap_visual_lines(&self.input, width);

        let mut lines: Vec<Line<'_>> = Vec::with_capacity(visual.len());

        for vl in &visual {
            let seg_start = vl.start;
            let seg_end = vl.end;

            let mut spans: Vec<Span<'_>> = Vec::new();

            if let Some((sel_start, sel_end)) = self.selection {
                let overlap_start = sel_start.max(seg_start);
                let overlap_end = sel_end.min(seg_end);

                if overlap_start < overlap_end {
                    if overlap_start > seg_start {
                        spans.push(Span::styled(
                            &self.input[seg_start..overlap_start],
                            input_style,
                        ));
                    }
                    spans.push(Span::styled(
                        &self.input[overlap_start..overlap_end],
                        sel_style,
                    ));
                    if overlap_end < seg_end {
                        spans.push(Span::styled(&self.input[overlap_end..seg_end], input_style));
                    }
                } else {
                    spans.push(Span::styled(&self.input[seg_start..seg_end], input_style));
                }
            } else {
                spans.push(Span::styled(&self.input[seg_start..seg_end], input_style));
            }

            lines.push(Line::from(spans));
        }

        if lines.is_empty() {
            lines.push(Line::from(""));
        }

        lines
    }

    // ── Rendering ────────────────────────────────────────────

    pub fn render(
        &mut self,
        f: &mut Frame,
        area: Rect,
        turn_in_flight: bool,
        show_cursor: bool,
        turn_status: &TurnStatus,
        turn_started_at: Instant,
        queue_paused_hint: Option<&str>,
    ) -> Rect {
        let has_attachments = !self.pending_attachments.is_empty();
        self.last_attachment_area = None;
        self.last_attachment_manager_area = None;

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

        // Clamp scroll to the valid range unconditionally so the paragraph
        // offset and the overflow arrows always reflect the same true state,
        // even on frames where the cursor-follow block below does not run
        // (e.g. an approval overlay suppresses the cursor).
        let max_scroll = content_rows.saturating_sub(visible_rows);
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        let mut constraints = vec![Constraint::Min(3)];
        if has_attachments {
            let available = area.height.saturating_sub(input_height + 3).max(1);
            constraints.push(Constraint::Length(
                u16::try_from(self.pending_attachments.len())
                    .unwrap_or(u16::MAX)
                    .min(available),
            ));
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
            self.last_attachment_area = Some(att_rect);
            for (row, (index, attachment)) in self
                .pending_attachments
                .iter()
                .enumerate()
                .take(att_rect.height as usize)
                .enumerate()
            {
                let row_rect = Rect::new(att_rect.x, att_rect.y + row as u16, att_rect.width, 1);
                let (main, remove_col) =
                    attachment_line(index, &attachment.label(), row_rect.width);
                let mut spans = vec![Span::styled(
                    main,
                    theme::accent_style().add_modifier(Modifier::ITALIC),
                )];
                if remove_col.is_some() {
                    spans.push(Span::styled(ATTACHMENT_REMOVE_LABEL, theme::warn_style()));
                }
                let line = Line::from(spans);
                f.render_widget(Paragraph::new(line), row_rect);
            }
        }

        let label_owned = turn_status.label(turn_started_at);
        let label: &str = &label_owned;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::dim_style())
            .title(Span::styled(label, theme::title_style()))
            .title_bottom(Span::styled("?=help", theme::dim_style()));

        if self.input.is_empty() && !turn_in_flight {
            // A paused queue takes the empty input line as ghost text in the
            // action colour, so the paused state and how to clear it are shown
            // exactly where the user would act on it.
            let (text, style) = if let Some(hint) = queue_paused_hint {
                (hint.to_string(), theme::accent_style())
            } else if self.file_explorer.is_some() {
                (String::new(), theme::dim_style())
            } else {
                (
                    crate::i18n::t("zc-input-placeholder-chat"),
                    theme::dim_style(),
                )
            };
            let p = Paragraph::new(Span::styled(text, style)).block(block);
            f.render_widget(p, input_area);
        } else {
            // Wrapped input content with optional selection highlighting.
            // Lines are pre-broken by wrap_visual_lines() — the same logic
            // that cursor_to_visual uses — so no Paragraph::wrap() is needed.
            let input_lines = self.build_input_lines(inner_width);
            let p = Paragraph::new(input_lines)
                .block(block)
                .scroll((self.scroll_offset, 0));
            f.render_widget(p, input_area);
        }

        if show_cursor
            && inner_width > 0
            && self.file_explorer.is_none()
            && self.attachment_manager.is_none()
        {
            let (cursor_row, cursor_col) = cursor_to_visual(&self.input, self.cursor, inner_width);

            if cursor_row < self.scroll_offset {
                self.scroll_offset = cursor_row;
            }
            if cursor_row >= self.scroll_offset + visible_rows {
                self.scroll_offset = cursor_row - visible_rows + 1;
            }

            let screen_row = cursor_row - self.scroll_offset;
            let cx = input_area.x + 1 + cursor_col;
            let cy = input_area.y + 1 + screen_row;
            f.set_cursor_position((cx, cy));
        }

        // Scroll indicators on the right border when content overflows.
        let (show_up, show_down) = overflow_arrows(content_rows, visible_rows, self.scroll_offset);
        if (show_up || show_down) && input_area.width > 2 {
            let indicator_x = input_area.x + input_area.width - 1;
            let indicator_style = theme::accent_style();

            if show_up {
                // Content above — show up arrow on top border.
                let buf = f.buffer_mut();
                buf[(indicator_x, input_area.y)]
                    .set_char('\u{25b2}')
                    .set_style(indicator_style);
            }
            if show_down {
                // Content below — show down arrow on bottom border.
                let buf = f.buffer_mut();
                buf[(indicator_x, input_area.y + input_area.height - 1)]
                    .set_char('\u{25bc}')
                    .set_style(indicator_style);
            }
        }

        conv_area
    }

    pub fn render_attachment_manager(&mut self, f: &mut Frame, area: Rect) {
        self.last_attachment_manager_area = None;
        let Some(manager) = &mut self.attachment_manager else {
            return;
        };
        if self.pending_attachments.is_empty() || area.width < 8 || area.height < 3 {
            return;
        }

        let visible_rows = self
            .pending_attachments
            .len()
            .min(MAX_ATTACHMENT_MANAGER_ROWS)
            .min(area.height.saturating_sub(2) as usize)
            .max(1);
        if manager.selected < manager.scroll {
            manager.scroll = manager.selected;
        } else if manager.selected >= manager.scroll + visible_rows {
            manager.scroll = manager.selected + 1 - visible_rows;
        }

        let title = crate::i18n::t_args(
            "zc-input-attachment-manager-title",
            &[("count", &self.pending_attachments.len().to_string())],
        );
        let (navigate_keys, remove_keys, close_keys) = attachment_manager_key_labels();
        let hint = crate::i18n::t_args(
            "zc-input-attachment-manager-hint",
            &[
                ("navigate", &navigate_keys.join("/")),
                ("remove", &remove_keys.join("/")),
                ("close", &close_keys.join("/")),
            ],
        );
        let labels = self
            .pending_attachments
            .iter()
            .map(PendingAttachment::label)
            .collect::<Vec<_>>();
        let desired_width = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                crate::display_width::display_width(label) + index.to_string().len() + 10
            })
            .chain([
                crate::display_width::display_width(&title) + 4,
                crate::display_width::display_width(&hint) + 4,
            ])
            .max()
            .unwrap_or(24);
        let box_width = u16::try_from(desired_width)
            .unwrap_or(u16::MAX)
            .clamp(24.min(area.width), area.width);
        let box_height = visible_rows as u16 + 2;
        let modal = Rect::new(
            area.x + area.width.saturating_sub(box_width) / 2,
            area.y + area.height.saturating_sub(box_height) / 2,
            box_width,
            box_height,
        );
        let inner_width = modal.width.saturating_sub(2);

        let items = labels
            .iter()
            .enumerate()
            .skip(manager.scroll)
            .take(visible_rows)
            .map(|(index, label)| {
                let (main, remove_col) = attachment_line(index, label, inner_width);
                let style = if index == manager.selected {
                    theme::selected_style()
                } else {
                    theme::body_style()
                };
                let mut spans = vec![Span::styled(main, style)];
                if remove_col.is_some() {
                    spans.push(Span::styled(ATTACHMENT_REMOVE_LABEL, theme::warn_style()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect::<Vec<_>>();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::overlay_border_style())
            .style(theme::fill_style())
            .title(Span::styled(format!(" {title} "), theme::heading_style()))
            .title_bottom(Span::styled(format!(" {hint} "), theme::dim_style()));
        f.render_widget(Clear, modal);
        f.render_widget(List::new(items).block(block), modal);
        self.last_attachment_manager_area = Some(Rect::new(
            modal.x + 1,
            modal.y + 1,
            inner_width,
            visible_rows as u16,
        ));

        let buf = f.buffer_mut();
        if manager.scroll > 0 {
            buf[(modal.x + modal.width - 1, modal.y)]
                .set_char('▲')
                .set_style(theme::accent_style());
        }
        if manager.scroll + visible_rows < self.pending_attachments.len() {
            buf[(modal.x + modal.width - 1, modal.y + modal.height - 1)]
                .set_char('▼')
                .set_style(theme::accent_style());
        }
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
                ListItem::new(Span::styled(cmd.clone(), style))
            })
            .collect();

        let fill = theme::fill_style();
        let list = List::new(items).style(fill).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::dim_style())
                .style(fill)
                .title(Span::styled(" Commands ", theme::heading_style())),
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
        if self.attachment_manager.is_some() {
            let (navigate, remove, close) = attachment_manager_key_labels();
            return HelpNode::entries(vec![
                E::new(navigate, crate::i18n::t("zc-chat-help-navigate")),
                E::new(remove, crate::i18n::t("zc-input-help-attachment-remove")),
                E::new(close, crate::i18n::t("zc-chat-help-close")),
                E::new(
                    vec!["/detach N"],
                    crate::i18n::t("zc-input-help-attachment-detach"),
                ),
            ]);
        }
        if self.autocomplete_active {
            use crate::keymap::{InputBarAction as Ib, action_key_labels};
            // Both Enter (Submit, contextual) and the dedicated accept
            // chord (Tab by default) accept the highlighted completion —
            // advertise whatever the live registry has them bound to.
            let mut accept_keys = action_key_labels(Ib::Submit);
            accept_keys.extend(action_key_labels(Ib::AutocompleteAccept));
            return HelpNode::entries(vec![
                E::new(
                    vec!["↑", "↓"],
                    crate::i18n::t("zc-input-help-completions-navigate"),
                ),
                E::new(
                    accept_keys,
                    crate::i18n::t("zc-input-help-completions-accept"),
                ),
                E::new(
                    action_key_labels(Ib::AutocompleteCancel),
                    crate::i18n::t("zc-input-help-completions-dismiss"),
                ),
            ]);
        }
        HelpNode::entries(crate::help::help_entries::<crate::keymap::InputBarAction>())
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn test_attachment(name: &str) -> PendingAttachment {
        PendingAttachment {
            path: PathBuf::from(name),
            mime_type: "image/png".into(),
            filename: name.into(),
            size_bytes: 1,
            source: crate::attachment::AttachmentSource::File,
        }
    }

    fn render_input_bar(bar: &mut InputBarState, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                bar.render(
                    frame,
                    area,
                    false,
                    false,
                    &TurnStatus::Idle,
                    Instant::now(),
                    None,
                );
                bar.render_attachment_manager(frame, area);
            })
            .expect("draw input bar");
    }

    #[test]
    fn attachment_line_preserves_visible_remove_control_when_truncated() {
        let (main, remove_col) = attachment_line(12, "a-very-long-filename.png", 20);

        assert_eq!(remove_col, Some(17));
        assert_eq!(crate::display_width::display_width(&main), 17);
        assert!(main.trim_end().ends_with('…'));
    }

    #[test]
    fn attachment_line_places_remove_control_next_to_short_label() {
        let (main, remove_col) = attachment_line(0, "one.png", 40);

        assert_eq!(main, " [0] one.png ");
        assert_eq!(remove_col, Some(13));
    }

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
    fn clear_input_empties_text_and_resets_cursor() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        assert_eq!(bar.cursor(), 11);
        bar.clear_input();
        assert_eq!(bar.input(), "");
        assert_eq!(bar.cursor(), 0);
    }

    #[test]
    fn ctrl_u_clears_input() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("scratch this");
        let act = bar.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(matches!(act, InputBarAction::Consumed));
        assert_eq!(bar.input(), "");
    }

    #[test]
    fn ctrl_w_deletes_previous_word() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        let action = bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.input(), "hello ");
        assert_eq!(bar.cursor(), 6);
    }

    #[test]
    fn ctrl_w_deletes_trailing_space_and_word() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello world   ");
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(bar.input(), "hello ");
        assert_eq!(bar.cursor(), 6);
    }

    #[test]
    fn ctrl_w_deletes_word_before_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello brave world");
        for _ in 0..5 {
            bar.move_cursor_left();
        }
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(bar.input(), "hello world");
        assert_eq!(bar.cursor(), 6);
    }

    #[test]
    fn ctrl_w_deletes_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        bar.selection = Some((6, 11));
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(bar.input(), "hello ");
        assert_eq!(bar.cursor(), 6);
    }

    #[test]
    fn ctrl_w_deletes_punctuation_run_like_vim() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello world...");
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(bar.input(), "hello world");
        assert_eq!(bar.cursor(), 11);
    }

    #[test]
    fn ctrl_w_deletes_word_after_punctuation_like_vim() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("hello-world");
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(bar.input(), "hello-");
        assert_eq!(bar.cursor(), 6);
    }

    #[test]
    fn ctrl_w_deletes_only_whitespace_before_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut bar = InputBarState::new();
        bar.insert_text("   ");
        bar.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
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
    fn taking_attachments_releases_clipboard_temp_ownership() {
        let mut bar = InputBarState::new();
        let tmp = std::env::temp_dir().join("zc_test_clip_release.png");
        std::fs::write(&tmp, b"x").unwrap();
        bar.clipboard_temps.push(tmp.clone());
        bar.add_attachment(PendingAttachment {
            path: tmp.clone(),
            mime_type: "image/png".into(),
            filename: "clip.png".into(),
            size_bytes: 1,
            source: crate::attachment::AttachmentSource::Clipboard,
        });

        let taken = bar.take_attachments();
        assert_eq!(taken.len(), 1);
        assert!(bar.clipboard_temps().is_empty());

        bar.cleanup_temps();
        assert!(tmp.exists(), "queued clipboard temp must survive cleanup");
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn removing_clipboard_attachment_deletes_owned_temp() {
        let mut bar = InputBarState::new();
        let tmp = std::env::temp_dir().join("zc_test_clip_remove.png");
        std::fs::write(&tmp, b"x").unwrap();
        bar.clipboard_temps.push(tmp.clone());
        bar.add_attachment(PendingAttachment {
            path: tmp.clone(),
            mime_type: "image/png".into(),
            filename: "clip.png".into(),
            size_bytes: 1,
            source: crate::attachment::AttachmentSource::Clipboard,
        });

        bar.remove_attachment(0);

        assert!(bar.pending_attachments().is_empty());
        assert!(bar.clipboard_temps().is_empty());
        assert!(!tmp.exists());
    }

    #[test]
    fn removing_file_attachment_preserves_user_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("user-file.png");
        std::fs::write(&path, b"x").expect("write user file");
        let mut bar = InputBarState::new();
        bar.add_attachment(PendingAttachment {
            path: path.clone(),
            mime_type: "image/png".into(),
            filename: "user-file.png".into(),
            size_bytes: 1,
            source: crate::attachment::AttachmentSource::File,
        });

        bar.remove_attachment(0);

        assert!(bar.pending_attachments().is_empty());
        assert!(
            path.exists(),
            "removal must not delete a user-selected file"
        );
    }

    #[test]
    fn slash_attachments_opens_indexed_manager() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.add_attachment(test_attachment("two.png"));
        bar.insert_text("/attachments");

        let action = bar.handle_enter();

        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.input(), "");
        assert_eq!(bar.attachment_manager.as_ref().map(|m| m.selected), Some(0));
    }

    #[test]
    fn attachment_manager_delete_removes_selected_item() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.add_attachment(test_attachment("two.png"));
        bar.open_attachment_manager();

        bar.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let action = bar.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.pending_attachments().len(), 1);
        assert_eq!(bar.pending_attachments()[0].filename, "one.png");
        assert_eq!(bar.attachment_manager.as_ref().map(|m| m.selected), Some(0));
    }

    #[test]
    fn attachment_manager_backspace_removes_last_item_and_closes() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.open_attachment_manager();

        let action = bar.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert!(matches!(action, InputBarAction::Consumed));
        assert!(bar.pending_attachments().is_empty());
        assert!(bar.attachment_manager.is_none());
    }

    #[test]
    fn attachment_manager_escape_closes_without_removing() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.open_attachment_manager();

        let action = bar.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.pending_attachments().len(), 1);
        assert!(bar.attachment_manager.is_none());
    }

    #[test]
    fn attachment_manager_claims_text_input() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.open_attachment_manager();

        assert!(bar.wants_text_input());
    }

    #[test]
    fn attachment_remove_control_click_removes_target_item() {
        let mut bar = InputBarState::new();
        bar.add_attachment(test_attachment("one.png"));
        bar.add_attachment(test_attachment("two.png"));
        render_input_bar(&mut bar, 40, 12);
        let area = bar.last_attachment_area.expect("attachment rows rendered");

        let consumed = bar.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width - 2,
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        });

        assert!(!consumed);
        assert_eq!(bar.pending_attachments().len(), 2);

        let (_, remove_col) = attachment_line(1, &bar.pending_attachments()[1].label(), area.width);

        let consumed = bar.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + remove_col.expect("remove control rendered"),
            row: area.y + 1,
            modifiers: KeyModifiers::NONE,
        });

        assert!(consumed);
        assert_eq!(bar.pending_attachments().len(), 1);
        assert_eq!(bar.pending_attachments()[0].filename, "one.png");
    }

    #[test]
    fn attachment_manager_scrolled_remove_control_removes_visible_item() {
        let mut bar = InputBarState::new();
        for index in 0..10 {
            bar.add_attachment(test_attachment(&format!("item-{index}.png")));
        }
        bar.open_attachment_manager();
        for _ in 0..9 {
            bar.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        render_input_bar(&mut bar, 60, 16);

        let manager = bar.attachment_manager.as_ref().expect("manager open");
        assert_eq!(manager.selected, 9);
        assert_eq!(manager.scroll, 2);
        let area = bar
            .last_attachment_manager_area
            .expect("attachment manager rendered");
        let (_, remove_col) = attachment_line(9, &bar.pending_attachments()[9].label(), area.width);

        let consumed = bar.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + remove_col.expect("remove control rendered"),
            row: area.y + 7,
            modifiers: KeyModifiers::NONE,
        });

        assert!(consumed);
        assert_eq!(bar.pending_attachments().len(), 9);
        assert!(
            bar.pending_attachments()
                .iter()
                .all(|attachment| attachment.filename != "item-9.png")
        );
    }

    #[test]
    fn loading_for_edit_retakes_clipboard_temp_ownership() {
        let mut bar = InputBarState::new();
        let tmp = std::env::temp_dir().join("zc_test_clip_retake.png");
        let att = PendingAttachment {
            path: tmp.clone(),
            mime_type: "image/png".into(),
            filename: "clip.png".into(),
            size_bytes: 1,
            source: crate::attachment::AttachmentSource::Clipboard,
        };
        bar.load_for_edit("edit".into(), vec![att]);
        assert!(bar.clipboard_temps().contains(&tmp));
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
        let expected = crate::i18n::t("zc-input-no-pending-attachments");
        assert!(matches!(action, InputBarAction::StatusMessage(ref m) if *m == expected));
    }

    #[test]
    fn empty_enter_resumes_queue() {
        let mut bar = InputBarState::new();
        // Empty input, no attachments -> ResumeQueue: a deliberate Enter must
        // never be silently swallowed; the parent uses it to unpause.
        assert!(matches!(bar.handle_enter(), InputBarAction::ResumeQueue));
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
            parse_slash_command("/clear-queue"),
            SlashCommand::ClearQueue(None)
        ));
        assert!(matches!(
            parse_slash_command("/clear-queue 2"),
            SlashCommand::ClearQueue(Some(2))
        ));
        assert!(matches!(
            parse_slash_command("/clear-queue xyz"),
            SlashCommand::ClearQueue(Some(0))
        ));
        assert!(matches!(
            parse_slash_command("/restart-session"),
            SlashCommand::RestartSession
        ));
        assert!(matches!(
            parse_slash_command("/new-session"),
            SlashCommand::RestartSession
        ));
        assert!(matches!(
            parse_slash_command("/new"),
            SlashCommand::RestartSession
        ));
        assert!(matches!(
            parse_slash_command("/toggle-thinking"),
            SlashCommand::ToggleThinking
        ));
        assert!(matches!(
            parse_slash_command("/browse"),
            SlashCommand::EnterBrowseMode
        ));
        assert!(matches!(
            parse_slash_command("/help"),
            SlashCommand::OpenHelp
        ));
        assert!(matches!(
            parse_slash_command("hello"),
            SlashCommand::NotACommand
        ));
    }

    #[test]
    fn parse_model_commands() {
        assert!(matches!(
            parse_slash_command("/model"),
            SlashCommand::ModelPicker
        ));
        assert!(matches!(
            parse_slash_command("/model "),
            SlashCommand::ModelPicker
        ));
        assert!(matches!(
            parse_slash_command("/model gpt-4o"),
            SlashCommand::Model("gpt-4o")
        ));
        assert!(matches!(
            parse_slash_command("/model-provider"),
            SlashCommand::ModelProviderPicker
        ));
        assert!(matches!(
            parse_slash_command("/model-provider anthropic.default"),
            SlashCommand::ModelProvider("anthropic.default")
        ));
        // `/model-provider` must NOT be parsed as `/model` with arg "-provider".
        assert!(!matches!(
            parse_slash_command("/model-provider"),
            SlashCommand::Model(_)
        ));
    }

    #[test]
    fn model_arg_autocomplete_filters_cached_catalog() {
        let mut bar = InputBarState::new();
        bar.set_model_catalog(
            "anthropic.default".into(),
            vec![
                "claude-sonnet-4-6".into(),
                "claude-opus-4".into(),
                "gpt-4o".into(),
            ],
        );
        bar.insert_text("/model claude");
        assert!(bar.autocomplete_active);
        assert_eq!(bar.autocomplete_target, AutocompleteTarget::ModelArg);
        assert!(
            bar.autocomplete_matches
                .iter()
                .any(|s| s == "claude-opus-4")
        );
        assert!(!bar.autocomplete_matches.iter().any(|s| s == "gpt-4o"));
    }

    #[test]
    fn model_arg_empty_lists_whole_catalog() {
        let mut bar = InputBarState::new();
        bar.set_model_catalog("anthropic.default".into(), vec!["a".into(), "b".into()]);
        bar.insert_text("/model ");
        assert!(bar.autocomplete_active);
        assert_eq!(bar.autocomplete_matches.len(), 2);
    }

    #[test]
    fn provider_arg_autocomplete_filters_cached_catalog() {
        let mut bar = InputBarState::new();
        bar.set_provider_catalog(vec![
            "anthropic".into(),
            "openai".into(),
            "openrouter".into(),
        ]);
        bar.insert_text("/model-provider open");
        assert!(bar.autocomplete_active);
        assert_eq!(
            bar.autocomplete_target,
            AutocompleteTarget::ModelProviderArg
        );
        assert!(bar.autocomplete_matches.iter().any(|s| s == "openai"));
        assert!(bar.autocomplete_matches.iter().any(|s| s == "openrouter"));
        assert!(!bar.autocomplete_matches.iter().any(|s| s == "anthropic"));
    }

    #[test]
    fn model_command_autocomplete_appends_space() {
        let mut bar = InputBarState::new();
        bar.apply_autocomplete_choice("/model");
        assert_eq!(bar.input(), "/model ");
    }

    #[test]
    fn model_arg_autocomplete_rewrites_only_arg() {
        let mut bar = InputBarState::new();
        bar.autocomplete_target = AutocompleteTarget::ModelArg;
        bar.apply_autocomplete_choice("claude-opus-4");
        assert_eq!(bar.input(), "/model claude-opus-4");
    }

    #[test]
    fn model_picker_command_returns_open_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/model");
        assert!(matches!(
            bar.handle_enter(),
            InputBarAction::OpenModelPicker
        ));
    }

    #[test]
    fn enter_accepts_highlighted_model_arg_and_submits() {
        let mut bar = InputBarState::new();
        bar.set_model_catalog(
            "anthropic.default".into(),
            vec!["claude-opus-4-8".into(), "claude-sonnet-4-6".into()],
        );
        bar.insert_text("/model ");
        assert!(bar.autocomplete_active);
        let action = bar.handle_key(KeyEvent::from(KeyCode::Enter));
        // First catalog entry is accepted and the line is submitted in one
        // keystroke — no picker modal.
        assert!(matches!(action, InputBarAction::SetModel(m) if m == "claude-opus-4-8"));
        assert!(!bar.autocomplete_active);
    }

    #[test]
    fn enter_on_model_command_completion_fills_without_submitting() {
        let mut bar = InputBarState::new();
        bar.insert_text("/mod");
        assert!(bar.autocomplete_active);
        let action = bar.handle_key(KeyEvent::from(KeyCode::Enter));
        // `/model` still needs an argument: accept-and-fill, do not open the
        // picker or submit.
        assert!(matches!(action, InputBarAction::Consumed));
        assert_eq!(bar.input(), "/model ");
    }

    #[test]
    fn enter_accepts_highlighted_provider_arg_and_submits() {
        let mut bar = InputBarState::new();
        bar.set_provider_catalog(vec!["openai".into(), "openrouter".into()]);
        bar.insert_text("/model-provider open");
        assert!(bar.autocomplete_active);
        let action = bar.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(action, InputBarAction::SetModelProvider(p) if p == "openai"));
        assert!(!bar.autocomplete_active);
    }

    #[test]
    fn completion_help_keys_come_from_keymap_registry() {
        use crate::keymap::{Chord, InputBarAction as Ib, action_key_labels};
        use crate::widgets::HelpContext;
        let mut bar = InputBarState::new();
        bar.insert_text("/mod");
        assert!(bar.autocomplete_active);
        let node = bar.help_context();
        let accept = node
            .entries
            .iter()
            .find(|e| e.action == crate::i18n::t("zc-input-help-completions-accept"))
            .expect("accept entry present");
        // Labels track the live bindings for Submit + AutocompleteAccept,
        // not hardcoded literals.
        assert!(accept.keys.contains(&Chord::key(KeyCode::Tab).display()));
        let mut expected = action_key_labels(Ib::Submit);
        expected.extend(action_key_labels(Ib::AutocompleteAccept));
        assert_eq!(accept.keys, expected);
    }

    #[test]
    fn help_context_keeps_slash_commands_out_of_general_help() {
        use crate::widgets::HelpContext;
        let bar = InputBarState::new();
        let node = bar.help_context();
        let listed = node
            .entries
            .iter()
            .chain(node.children.iter().flat_map(|child| child.entries.iter()))
            .map(|entry| entry.key_str())
            .collect::<Vec<_>>();
        for command in SLASH_COMMANDS {
            assert!(
                !listed.iter().any(|key| key == command),
                "{command} stays in autocomplete, not the general Help modal"
            );
        }
    }

    #[test]
    fn model_provider_picker_command_returns_open_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/model-provider");
        assert!(matches!(
            bar.handle_enter(),
            InputBarAction::OpenModelProviderPicker
        ));
    }

    #[test]
    fn browse_command_returns_enter_browse_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/browse");
        assert!(matches!(
            bar.handle_enter(),
            InputBarAction::EnterBrowseMode
        ));
    }

    #[test]
    fn help_command_returns_open_help_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/help");
        assert!(matches!(bar.handle_enter(), InputBarAction::OpenHelp));
    }

    #[test]
    fn model_command_with_arg_returns_set_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/model gpt-4o");
        match bar.handle_enter() {
            InputBarAction::SetModel(m) => assert_eq!(m, "gpt-4o"),
            _ => panic!("expected SetModel"),
        }
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
    fn overflow_arrows_none_when_fits() {
        assert_eq!(overflow_arrows(3, 5, 0), (false, false));
        assert_eq!(overflow_arrows(5, 5, 0), (false, false));
    }

    #[test]
    fn overflow_arrows_down_only_at_top() {
        // 10 rows, window 5, scrolled to top: more below, none above.
        assert_eq!(overflow_arrows(10, 5, 0), (false, true));
    }

    #[test]
    fn overflow_arrows_both_in_middle() {
        assert_eq!(overflow_arrows(10, 5, 2), (true, true));
    }

    #[test]
    fn overflow_arrows_up_only_at_bottom() {
        // max_scroll = 10 - 5 = 5; at offset 5 nothing remains below.
        assert_eq!(overflow_arrows(10, 5, 5), (true, false));
    }

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
    fn wrapped_line_count_word_wraps_like_paragraph() {
        assert_eq!(wrapped_line_count("hello world", 10), 2);
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
    fn cursor_to_visual_word_wrap() {
        assert_eq!(
            cursor_to_visual("hello world", "hello world".len(), 10),
            (1, 5)
        );
    }

    #[test]
    fn cursor_to_visual_uses_terminal_cell_width() {
        let text = "abcd界";
        assert_eq!(cursor_to_visual(text, text.len(), 5), (1, 2));
    }

    #[test]
    fn cursor_to_visual_emoji_presentation_is_two_cells() {
        // 🏔️ is U+1F3D4 + U+FE0F. unicode-width string width is 2; char-wise
        // sum of the base alone is 1 and leaves the cursor one cell short.
        let emoji = "\u{1F3D4}\u{FE0F}";
        let text = format!("{emoji}x");
        assert_eq!(cursor_to_visual(&text, text.len(), 10), (0, 3));
        assert_eq!(str_cell_width(emoji), 2);
        assert_eq!(str_cell_width(&format!("{emoji} ")), 3);
    }

    #[test]
    fn cursor_to_visual_text_default_presentation_sequence_is_two_cells() {
        // ⚠️ is text-default ⚠ + VS16. Scalar loops that cannot see FE0F would
        // count base width 1 + selector 0 and leave the cursor short.
        let emoji = "\u{26A0}\u{FE0F}";
        let text = format!("{emoji}x");
        assert_eq!(str_cell_width(emoji), 2);
        assert_eq!(cursor_to_visual(&text, text.len(), 10), (0, 3));
        // Bare text form stays width 1.
        let bare = "\u{26A0}x";
        assert_eq!(str_cell_width("\u{26A0}"), 1);
        assert_eq!(cursor_to_visual(bare, bare.len(), 10), (0, 2));
    }

    #[test]
    fn hard_wrap_keeps_text_default_presentation_sequence_together() {
        // Width 2 must not split ⚠️ across lines; the sequence is one unit.
        let emoji = "\u{26A0}\u{FE0F}";
        let text = format!("ab{emoji}cd");
        let lines = wrap_visual_lines(&text, 2);
        assert_eq!(lines.len(), 3);
        assert_eq!(&text[lines[0].start..lines[0].end], "ab");
        assert_eq!(&text[lines[1].start..lines[1].end], emoji);
        assert_eq!(&text[lines[2].start..lines[2].end], "cd");
        assert_eq!(lines[1].width, 2);
    }

    #[test]
    fn visual_to_cursor_text_default_presentation_sequence() {
        // Reverse mapping must land on grapheme boundaries for ⚠️.
        let emoji = "\u{26A0}\u{FE0F}";
        let text = format!("a{emoji}b");
        // col 0 -> 'a', col 1/2 -> start of ⚠️, col 3 -> 'b'
        assert_eq!(visual_to_cursor(&text, 0, 0, 10), 0);
        assert_eq!(visual_to_cursor(&text, 0, 1, 10), 1);
        assert_eq!(visual_to_cursor(&text, 0, 2, 10), 1);
        assert_eq!(visual_to_cursor(&text, 0, 3, 10), text.len() - 1);
        assert_eq!(cursor_to_visual(&text, text.len(), 10), (0, 4));
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
    fn visual_to_cursor_word_wrap() {
        assert_eq!(
            visual_to_cursor("hello world", 1, 5, 10),
            "hello world".len()
        );
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
        assert!(bar.autocomplete_matches.iter().any(|s| s == "/attachments"));
        // "/attach" itself is excluded (exact match).
        assert!(!bar.autocomplete_matches.iter().any(|s| s == "/attach"));
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
        assert!(
            bar.autocomplete_matches
                .iter()
                .any(|s| s == "/toggle-thinking")
        );
    }

    #[test]
    fn autocomplete_restart_session_prefix() {
        let mut bar = InputBarState::new();
        bar.insert_text("/restart");
        assert!(bar.autocomplete_active);
        assert!(
            bar.autocomplete_matches
                .iter()
                .any(|s| s == "/restart-session")
        );
    }

    #[test]
    fn autocomplete_new_session_alias() {
        let mut bar = InputBarState::new();
        bar.insert_text("/ne");
        assert!(bar.autocomplete_active);
        assert!(bar.autocomplete_matches.iter().any(|s| s == "/new"));
        assert!(bar.autocomplete_matches.iter().any(|s| s == "/new-session"));
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

    #[test]
    fn slash_restart_session_returns_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/restart-session");
        let action = bar.handle_enter();
        assert!(matches!(action, InputBarAction::RestartSession));
        assert_eq!(bar.input(), "");
    }

    #[test]
    fn slash_new_alias_returns_restart_session_action() {
        let mut bar = InputBarState::new();
        bar.insert_text("/new");
        let action = bar.handle_enter();
        assert!(matches!(action, InputBarAction::RestartSession));
        assert_eq!(bar.input(), "");
    }

    // ── Selection tests ──────────────────────────────────────

    #[test]
    fn build_input_lines_no_selection() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello");
        let lines = bar.build_input_lines(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn build_input_lines_with_newlines() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello\nworld\nfoo");
        let lines = bar.build_input_lines(80);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn build_input_lines_with_selection() {
        let mut bar = InputBarState::new();
        bar.insert_text("hello world");
        bar.selection = Some((2, 7));
        let lines = bar.build_input_lines(80);
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
