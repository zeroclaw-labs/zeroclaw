//! Shell-level agent sidebar: every session the chat-like panes track, with
//! live status dots, a `+` picker to add an agent, and the Quickstart
//! launcher at the bottom.
//!
//! The sidebar owns only widget state (visibility, width, scroll, hit rects,
//! picker). Session rows are derived per frame from the panes'
//! `session_summaries()` — the panes stay the single source of truth.

use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use tokio::sync::mpsc;

use crate::chat::{PaneKind, SidebarSessionSummary, SidebarStatus};
use crate::client::RpcClient;
use crate::i18n::{t, t_args};
use crate::keymap::ModalAction;
use crate::{config, mouse, theme, widgets};

pub(crate) const SIDEBAR_COLS_MIN: u16 = 18;
pub(crate) const SIDEBAR_COLS_MAX: u16 = 40;
/// Minimum columns the main content keeps; below this the sidebar auto-skips
/// for the frame instead of squeezing the pane.
pub(crate) const CONTENT_MIN_COLS: u16 = 40;
/// Row width at which the right-aligned pane tag is shown.
const PANE_TAG_MIN_COLS: u16 = 24;

/// Per-frame shell context the sidebar renders against.
pub(crate) struct SidebarCtx {
    /// The chat-like pane the current mode maps to, if any.
    pub active_pane: Option<PaneKind>,
    pub quickstart_active: bool,
    pub connected: bool,
}

/// A user action the shell must route (mode switch + pane call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarEvent {
    FocusSession {
        pane: PaneKind,
        session_id: String,
    },
    CloseSession {
        pane: PaneKind,
        session_id: String,
    },
    OpenPicker,
    PickAgent {
        pane: PaneKind,
        alias: String,
        /// The alias's session already open in the target pane, if any:
        /// focus it instead of starting a duplicate.
        existing_session: Option<String>,
    },
    OpenQuickstart,
}

/// The `+` agent picker modal.
struct SidebarPicker {
    /// Pane a pick adds to, captured when the picker opened.
    target: PaneKind,
    /// Display labels (alias + open-marker suffix), parallel to `aliases`.
    state: widgets::PickerState,
    aliases: Vec<String>,
    /// alias -> session already open in `target`, from summaries at open time.
    open_by_alias: HashMap<String, String>,
    loading: bool,
    error: Option<String>,
    rx: mpsc::UnboundedReceiver<Result<Vec<String>, String>>,
    double_click: mouse::DoubleClickTracker,
    /// Recorded at draw for click routing.
    modal_rect: Rect,
}

impl SidebarPicker {
    fn selectable(&self) -> bool {
        !self.loading && self.error.is_none() && !self.aliases.is_empty()
    }

    /// The rows the modal shows: real aliases, or a single status row.
    fn display_items(&self) -> Vec<String> {
        if self.loading {
            vec![t("zc-sidebar-picker-loading")]
        } else if let Some(ref e) = self.error {
            vec![t_args("zc-sidebar-picker-error", &[("error", e)])]
        } else if self.aliases.is_empty() {
            vec![t("zc-sidebar-picker-empty")]
        } else {
            self.state.items.clone()
        }
    }
}

pub(crate) struct AgentSidebar {
    visible: bool,
    /// Configured target width (clamped per frame in `carve`).
    width: u16,
    /// Scroll offset into the session rows.
    scroll: u16,
    // Geometry recorded by draw, read by the mouse handler (repo convention:
    // draw records, mouse reads). All `Rect::default()` while hidden.
    area: Rect,
    plus_rect: Rect,
    quickstart_rect: Rect,
    row_rects: Vec<(PaneKind, String, Rect)>,
    /// The `✕` cell on the focused row of the active pane, if drawn.
    close_rect: Option<(PaneKind, String, Rect)>,
    picker: Option<SidebarPicker>,
}

impl AgentSidebar {
    pub(crate) fn from_config_dir(config_dir: &std::path::Path) -> Self {
        let section = config::ensure_and_load(config_dir)
            .map(|c| c.sidebar)
            .unwrap_or_default();
        Self {
            visible: section.visible,
            width: section.width,
            scroll: 0,
            area: Rect::default(),
            plus_rect: Rect::default(),
            quickstart_rect: Rect::default(),
            row_rects: Vec::new(),
            close_rect: None,
            picker: None,
        }
    }

    /// Flip visibility and persist the toggle. A persist failure only loses
    /// the preference across restarts, so it is not surfaced.
    pub(crate) fn toggle(&mut self, config_dir: &std::path::Path) {
        self.visible = !self.visible;
        let _ = config::persist_sidebar_visible(config_dir, self.visible);
    }

    pub(crate) fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub(crate) fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Whether `(col, row)` falls inside the sidebar panel (not the picker).
    pub(crate) fn contains(&self, col: u16, row: u16) -> bool {
        self.area.width > 0 && mouse::in_rect(col, row, self.area)
    }

    /// Split the content area into (sidebar, body). Returns `(None, content)`
    /// when hidden or when the terminal is too narrow to keep the main pane
    /// at [`CONTENT_MIN_COLS`].
    pub(crate) fn carve(&mut self, content: Rect) -> (Option<Rect>, Rect) {
        self.area = Rect::default();
        self.plus_rect = Rect::default();
        self.quickstart_rect = Rect::default();
        self.row_rects.clear();
        self.close_rect = None;
        if !self.visible {
            return (None, content);
        }
        let width = self.width.clamp(SIDEBAR_COLS_MIN, SIDEBAR_COLS_MAX);
        if content.width < width + CONTENT_MIN_COLS {
            return (None, content);
        }
        let sidebar = Rect { width, ..content };
        let body = Rect {
            x: content.x + width,
            width: content.width - width,
            ..content
        };
        (Some(sidebar), body)
    }

    /// Render the sidebar into `area` (as returned by [`Self::carve`]),
    /// recording hit rects for the mouse handler.
    pub(crate) fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        rows: &[SidebarSessionSummary],
        ctx: &SidebarCtx,
    ) {
        self.area = area;
        frame.render_widget(Clear, area);
        let block = theme::panel_block(&t("zc-sidebar-title")).style(theme::fill_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // The `+` affordance lives on the top border, right-aligned. It dims
        // while disconnected (the shell gates the actions anyway).
        if area.width >= 6 {
            let plus = Rect {
                x: area.x + area.width - 4,
                y: area.y,
                width: 3,
                height: 1,
            };
            let plus_style = if ctx.connected {
                theme::accent_style()
            } else {
                theme::dim_style()
            };
            frame.render_widget(Paragraph::new(Span::styled("[+]", plus_style)), plus);
            self.plus_rect = plus;
        }

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        // Bottom of the panel: separator + Quickstart launcher row.
        let quickstart_rows = u16::from(inner.height >= 1) + u16::from(inner.height >= 2);
        let rows_area = Rect {
            height: inner.height - quickstart_rows,
            ..inner
        };

        self.draw_session_rows(frame, rows_area, rows, ctx);

        if quickstart_rows == 2 {
            let sep = Rect {
                y: inner.y + inner.height - 2,
                height: 1,
                ..inner
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "\u{2500}".repeat(inner.width as usize),
                    theme::dim_style(),
                )),
                sep,
            );
        }
        if quickstart_rows >= 1 {
            let qs = Rect {
                y: inner.y + inner.height - 1,
                height: 1,
                ..inner
            };
            let style = if ctx.quickstart_active {
                theme::selected_style()
            } else {
                theme::accent_style()
            };
            let label = widgets::truncate_to_width(
                &format!("\u{bb} {}", t("zc-pane-quickstart")),
                qs.width as usize,
            );
            frame.render_widget(Paragraph::new(Span::styled(label, style)), qs);
            self.quickstart_rect = qs;
        }
    }

    fn draw_session_rows(
        &mut self,
        frame: &mut Frame,
        rows_area: Rect,
        rows: &[SidebarSessionSummary],
        ctx: &SidebarCtx,
    ) {
        if rows_area.height == 0 {
            self.scroll = 0;
            return;
        }
        if rows.is_empty() {
            self.scroll = 0;
            let hint = Rect {
                y: rows_area.y + rows_area.height / 2,
                height: 1,
                ..rows_area
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    widgets::truncate_to_width(&t("zc-sidebar-empty"), hint.width as usize),
                    theme::dim_style(),
                ))
                .centered(),
                hint,
            );
            return;
        }

        let visible = rows_area.height as usize;
        let max_scroll = rows.len().saturating_sub(visible) as u16;
        self.scroll = self.scroll.min(max_scroll);

        for (i, summary) in rows
            .iter()
            .skip(self.scroll as usize)
            .take(visible)
            .enumerate()
        {
            let row_rect = Rect {
                y: rows_area.y + i as u16,
                height: 1,
                ..rows_area
            };
            let focused_here = summary.focused && ctx.active_pane == Some(summary.pane_kind);
            let row_style = if focused_here {
                theme::selection_highlight(true, true)
            } else if summary.focused {
                theme::selection_highlight(false, true)
            } else {
                Style::default()
            };

            // Focused row of the active pane swaps its pane tag for a close
            // affordance; other rows show the dim pane tag when wide enough.
            let tag = if focused_here {
                Some(("\u{2715}".to_string(), true))
            } else if row_rect.width >= PANE_TAG_MIN_COLS {
                let label = match summary.pane_kind {
                    PaneKind::Chat => t("zc-pane-chat"),
                    PaneKind::Acp => t("zc-pane-code"),
                };
                Some((label, false))
            } else {
                None
            };
            let tag_width = tag
                .as_ref()
                .map(|(s, _)| crate::display_width::display_width(s) + 1)
                .unwrap_or(0);

            let name_width = (row_rect.width as usize).saturating_sub(2 + tag_width);
            let name = widgets::truncate_to_width(&summary.agent_alias, name_width);
            let pad = name_width.saturating_sub(crate::display_width::display_width(&name));

            let mut spans = vec![
                Span::styled("\u{25cf} ", status_style(summary.status)),
                Span::styled(name, theme::body_style()),
            ];
            if let Some((label, is_close)) = tag {
                spans.push(Span::raw(" ".repeat(pad + 1)));
                spans.push(Span::styled(label, theme::dim_style()));
                if is_close {
                    // The clickable ✕ zone: last two cells of the row.
                    self.close_rect = Some((
                        summary.pane_kind,
                        summary.session_id.clone(),
                        Rect {
                            x: row_rect.x + row_rect.width.saturating_sub(2),
                            width: 2,
                            ..row_rect
                        },
                    ));
                }
            }
            frame.render_widget(Paragraph::new(Line::from(spans)).style(row_style), row_rect);
            self.row_rects
                .push((summary.pane_kind, summary.session_id.clone(), row_rect));
        }
    }

    /// Render the `+` picker modal (drawn above the panes, below the help
    /// overlay). Records the modal rect for click routing.
    pub(crate) fn draw_picker(&mut self, frame: &mut Frame, screen: Rect) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let title = t("zc-sidebar-picker-title");
        let items = picker.display_items();
        let cursor = if picker.selectable() {
            picker.state.cursor
        } else {
            usize::MAX // no highlighted row for status-only content
        };
        picker.modal_rect =
            widgets::PickerModal::area_for(&title, &items, screen).unwrap_or_default();
        widgets::PickerModal::new(&title, &items, cursor).render(frame, screen);
    }

    /// Open the picker targeting `pane`, spawning a background agent fetch.
    /// `open_by_alias` maps aliases already open in that pane to their
    /// session ids (from the pane's summaries).
    pub(crate) fn open_picker(
        &mut self,
        target: PaneKind,
        open_by_alias: HashMap<String, String>,
        rpc: &Arc<RpcClient>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        let rpc = Arc::clone(rpc);
        tokio::spawn(async move {
            let result = match rpc.agents_status().await {
                Ok(result) => Ok(result
                    .agents
                    .into_iter()
                    .filter(|a| a.enabled)
                    .map(|a| a.alias)
                    .collect::<Vec<_>>()),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(result);
        });
        self.picker = Some(SidebarPicker {
            target,
            state: widgets::PickerState::default(),
            aliases: Vec::new(),
            open_by_alias,
            loading: true,
            error: None,
            rx,
            double_click: mouse::DoubleClickTracker::new(),
            modal_rect: Rect::default(),
        });
    }

    /// Drain the background agent fetch, if one is pending. Called once per
    /// tick from the app loop.
    pub(crate) fn drain_picker_fetch(&mut self) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        if !picker.loading {
            return;
        }
        match picker.rx.try_recv() {
            Ok(Ok(aliases)) => {
                picker.loading = false;
                let open_suffix = t("zc-sidebar-picker-open-suffix");
                let labels = aliases
                    .iter()
                    .map(|alias| {
                        if picker.open_by_alias.contains_key(alias) {
                            format!("{alias} {open_suffix}")
                        } else {
                            alias.clone()
                        }
                    })
                    .collect();
                picker.aliases = aliases;
                picker.state = widgets::PickerState::new(labels, None);
            }
            Ok(Err(e)) => {
                picker.loading = false;
                picker.error = Some(e);
            }
            Err(_) => {}
        }
    }

    /// Keys while the picker is open. Returns an event on confirm; `Cancel`
    /// and confirms on status-only content close the picker.
    pub(crate) fn handle_picker_key(&mut self, key: &KeyEvent) -> Option<SidebarEvent> {
        let picker = self.picker.as_mut()?;
        match ModalAction::from_chord(key) {
            Some(ModalAction::Up) => {
                picker.state.move_up();
                None
            }
            Some(ModalAction::Down) => {
                picker.state.move_down();
                None
            }
            Some(ModalAction::Confirm) => {
                let event = Self::picker_confirm(picker);
                self.picker = None;
                event
            }
            Some(ModalAction::Cancel) => {
                self.picker = None;
                None
            }
            _ => None,
        }
    }

    fn picker_confirm(picker: &SidebarPicker) -> Option<SidebarEvent> {
        if !picker.selectable() {
            return None;
        }
        let alias = picker.aliases.get(picker.state.cursor)?.clone();
        Some(SidebarEvent::PickAgent {
            pane: picker.target,
            existing_session: picker.open_by_alias.get(&alias).cloned(),
            alias,
        })
    }

    /// Mouse routing. The app forwards events here when the picker is open
    /// or the click falls inside the sidebar area.
    pub(crate) fn handle_mouse(&mut self, mouse: &MouseEvent) -> Option<SidebarEvent> {
        if self.picker.is_some() {
            return self.handle_picker_mouse(mouse);
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (col, row) = (mouse.column, mouse.row);
                if mouse::in_rect(col, row, self.plus_rect) {
                    return Some(SidebarEvent::OpenPicker);
                }
                if mouse::in_rect(col, row, self.quickstart_rect) {
                    return Some(SidebarEvent::OpenQuickstart);
                }
                if let Some((pane, sid, rect)) = &self.close_rect
                    && mouse::in_rect(col, row, *rect)
                {
                    return Some(SidebarEvent::CloseSession {
                        pane: *pane,
                        session_id: sid.clone(),
                    });
                }
                for (pane, sid, rect) in &self.row_rects {
                    if mouse::in_rect(col, row, *rect) {
                        return Some(SidebarEvent::FocusSession {
                            pane: *pane,
                            session_id: sid.clone(),
                        });
                    }
                }
                None
            }
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            MouseEventKind::ScrollDown => {
                // Clamped against the row count on the next draw.
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            _ => None,
        }
    }

    fn handle_picker_mouse(&mut self, mouse: &MouseEvent) -> Option<SidebarEvent> {
        let picker = self.picker.as_mut()?;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (col, row) = (mouse.column, mouse.row);
                if !mouse::in_rect(col, row, picker.modal_rect) {
                    self.picker = None;
                    return None;
                }
                if !picker.selectable() {
                    return None;
                }
                if let Some(idx) =
                    mouse::list_click_index(row, picker.modal_rect, 0, picker.aliases.len())
                {
                    picker.state.cursor = idx;
                    if picker.double_click.click(col, row) {
                        let event = Self::picker_confirm(picker);
                        self.picker = None;
                        return event;
                    }
                }
                None
            }
            MouseEventKind::ScrollUp => {
                picker.state.move_up();
                None
            }
            MouseEventKind::ScrollDown => {
                picker.state.move_down();
                None
            }
            _ => None,
        }
    }
}

fn status_style(status: SidebarStatus) -> Style {
    match status {
        SidebarStatus::Ready => theme::status_ready_style(),
        SidebarStatus::Running => theme::status_running_style(),
        SidebarStatus::NeedsHuman => theme::status_attention_style(),
        SidebarStatus::Errored => theme::status_error_style(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEventKind};

    fn sidebar() -> AgentSidebar {
        AgentSidebar {
            visible: true,
            width: 24,
            scroll: 0,
            area: Rect::default(),
            plus_rect: Rect::default(),
            quickstart_rect: Rect::default(),
            row_rects: Vec::new(),
            close_rect: None,
            picker: None,
        }
    }

    fn summary(alias: &str, sid: &str, focused: bool) -> SidebarSessionSummary {
        SidebarSessionSummary {
            session_id: sid.into(),
            agent_alias: alias.into(),
            status: SidebarStatus::Ready,
            pane_kind: PaneKind::Chat,
            focused,
        }
    }

    fn click(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn carve_hidden_passes_content_through() {
        let mut s = sidebar();
        s.visible = false;
        let content = Rect::new(0, 1, 100, 30);
        let (side, body) = s.carve(content);
        assert!(side.is_none());
        assert_eq!(body, content);
        assert!(!s.contains(1, 2), "hidden sidebar owns no cells");
    }

    #[test]
    fn carve_splits_at_configured_width() {
        let mut s = sidebar();
        let content = Rect::new(0, 1, 100, 30);
        let (side, body) = s.carve(content);
        let side = side.expect("visible sidebar carves");
        assert_eq!(side, Rect::new(0, 1, 24, 30));
        assert_eq!(body, Rect::new(24, 1, 76, 30));
    }

    #[test]
    fn carve_clamps_width_to_supported_range() {
        let mut s = sidebar();
        s.width = 200;
        let (side, _) = s.carve(Rect::new(0, 0, 120, 30));
        assert_eq!(side.expect("carves").width, SIDEBAR_COLS_MAX);
        s.width = 1;
        let (side, _) = s.carve(Rect::new(0, 0, 120, 30));
        assert_eq!(side.expect("carves").width, SIDEBAR_COLS_MIN);
    }

    #[test]
    fn carve_auto_skips_on_narrow_terminals() {
        let mut s = sidebar();
        let content = Rect::new(0, 1, 24 + CONTENT_MIN_COLS - 1, 30);
        let (side, body) = s.carve(content);
        assert!(side.is_none(), "content below the floor keeps full width");
        assert_eq!(body, content);
        assert!(s.visible, "auto-skip must not flip the persisted toggle");
    }

    #[test]
    fn draw_records_row_plus_and_quickstart_rects() {
        let mut s = sidebar();
        let area = s.carve(Rect::new(0, 1, 100, 12)).0.unwrap();
        let rows = vec![summary("alpha", "s1", true), summary("beta", "s2", false)];
        let ctx = SidebarCtx {
            active_pane: Some(PaneKind::Chat),
            quickstart_active: false,
            connected: true,
        };
        let backend = ratatui::backend::TestBackend::new(100, 14);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| s.draw(frame, area, &rows, &ctx)).unwrap();

        assert_eq!(s.row_rects.len(), 2);
        assert!(s.plus_rect.width > 0, "plus affordance recorded");
        assert!(s.quickstart_rect.width > 0, "quickstart row recorded");
        let (pane, sid, rect) = s.close_rect.clone().expect("focused row shows close");
        assert_eq!((pane, sid.as_str()), (PaneKind::Chat, "s1"));

        // Click routing through the recorded rects.
        let (_, sid, row2) = s.row_rects[1].clone();
        assert_eq!(
            s.handle_mouse(&click(row2.x + 1, row2.y)),
            Some(SidebarEvent::FocusSession {
                pane: PaneKind::Chat,
                session_id: sid,
            })
        );
        assert_eq!(
            s.handle_mouse(&click(rect.x, rect.y)),
            Some(SidebarEvent::CloseSession {
                pane: PaneKind::Chat,
                session_id: "s1".into(),
            })
        );
        assert_eq!(
            s.handle_mouse(&click(s.plus_rect.x, s.plus_rect.y)),
            Some(SidebarEvent::OpenPicker)
        );
        assert_eq!(
            s.handle_mouse(&click(s.quickstart_rect.x, s.quickstart_rect.y)),
            Some(SidebarEvent::OpenQuickstart)
        );
    }

    #[test]
    fn scroll_clamps_to_row_overflow() {
        let mut s = sidebar();
        let area = s.carve(Rect::new(0, 0, 100, 6)).0.unwrap();
        // inner height 4 => rows area 2 (separator + quickstart take 2).
        let rows: Vec<_> = (0..5)
            .map(|i| summary(&format!("a{i}"), &format!("s{i}"), false))
            .collect();
        let ctx = SidebarCtx {
            active_pane: None,
            quickstart_active: false,
            connected: true,
        };
        s.scroll = 99;
        let backend = ratatui::backend::TestBackend::new(100, 6);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| s.draw(frame, area, &rows, &ctx)).unwrap();
        assert_eq!(s.scroll, 3, "scroll clamps to rows.len() - visible");
        assert_eq!(s.row_rects.len(), 2);
        assert_eq!(s.row_rects[0].1, "s3", "clamped scroll shows the tail");
    }

    #[tokio::test]
    async fn picker_labels_mark_open_aliases_and_confirm_dedupes() {
        let mut s = sidebar();
        let (tx, rx) = mpsc::unbounded_channel();
        s.picker = Some(SidebarPicker {
            target: PaneKind::Chat,
            state: widgets::PickerState::default(),
            aliases: Vec::new(),
            open_by_alias: HashMap::from([("alpha".to_string(), "s1".to_string())]),
            loading: true,
            error: None,
            rx,
            double_click: mouse::DoubleClickTracker::new(),
            modal_rect: Rect::default(),
        });
        tx.send(Ok(vec!["alpha".to_string(), "beta".to_string()]))
            .unwrap();
        s.drain_picker_fetch();

        let picker = s.picker.as_ref().unwrap();
        assert!(picker.state.items[0].ends_with(&t("zc-sidebar-picker-open-suffix")));
        assert_eq!(picker.state.items[1], "beta");

        let confirm = KeyEvent::from(crossterm::event::KeyCode::Enter);
        let event = s.handle_picker_key(&confirm);
        assert_eq!(
            event,
            Some(SidebarEvent::PickAgent {
                pane: PaneKind::Chat,
                alias: "alpha".into(),
                existing_session: Some("s1".into()),
            })
        );
        assert!(!s.picker_open(), "confirm closes the picker");
    }

    #[tokio::test]
    async fn picker_error_row_is_not_selectable() {
        let mut s = sidebar();
        let (tx, rx) = mpsc::unbounded_channel();
        s.picker = Some(SidebarPicker {
            target: PaneKind::Acp,
            state: widgets::PickerState::default(),
            aliases: Vec::new(),
            open_by_alias: HashMap::new(),
            loading: true,
            error: None,
            rx,
            double_click: mouse::DoubleClickTracker::new(),
            modal_rect: Rect::default(),
        });
        tx.send(Err("socket closed".to_string())).unwrap();
        s.drain_picker_fetch();

        let confirm = KeyEvent::from(crossterm::event::KeyCode::Enter);
        assert_eq!(s.handle_picker_key(&confirm), None);
        assert!(!s.picker_open(), "confirm on an error row closes");
    }
}
