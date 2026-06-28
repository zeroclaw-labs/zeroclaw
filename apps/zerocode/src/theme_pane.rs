//! Top-level global theme picker.
//!
//! Theme definitions live in `theme::theme_names()` / `theme::theme_by_name()`.
//! This pane keeps only transient UI state and resolves the registry on demand.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};

use crate::{config, mouse, theme};

pub struct ThemePane {
    config_dir: PathBuf,
    theme_cursor: usize,
    status: Option<String>,
    list_area: Rect,
    double_click: mouse::DoubleClickTracker,
}

impl ThemePane {
    pub fn new(config_dir: &Path) -> Self {
        let themes = theme_names();
        Self {
            config_dir: config_dir.to_path_buf(),
            theme_cursor: active_theme_cursor(&themes),
            status: None,
            list_area: Rect::default(),
            double_click: mouse::DoubleClickTracker::new(),
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        false
    }

    pub(crate) fn draw_into(&mut self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);

        let themes = theme_names();
        self.clamp_cursor(themes.len());
        self.list_area = rows[0];

        let selected = self.theme_cursor.min(themes.len().saturating_sub(1));
        let items: Vec<ListItem> = themes
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut spans = if i == selected {
                    theme_swatch_spans(name)
                } else {
                    theme_swatch_blank()
                };
                spans.push(Span::styled(name.clone(), theme::body_style()));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(selected));
        }
        frame.render_stateful_widget(
            List::new(items)
                .block(theme::panel_block(&format!(
                    " {} ",
                    crate::i18n::t("zc-pane-theme")
                )))
                .highlight_style(theme::selection_highlight(true, true))
                .highlight_symbol("\u{203a} "),
            rows[0],
            &mut state,
        );

        let footer = self
            .status
            .as_deref()
            .map_or_else(|| self.bottom_hint(), |status| format!(" {status}"));
        frame.render_widget(
            Paragraph::new(Span::styled(footer, theme::dim_style())),
            rows[1],
        );
    }

    /// Returns `true` when the caller should leave the pane.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.status = None;
        use crate::keymap::ConfigTabAction;
        match ConfigTabAction::from_chord(&key) {
            Some(ConfigTabAction::Up) => self.move_cursor(-1),
            Some(ConfigTabAction::Down) => self.move_cursor(1),
            Some(ConfigTabAction::Enter) => self.apply_theme(),
            Some(ConfigTabAction::Back | ConfigTabAction::TabLeft) => return true,
            _ => {}
        }
        false
    }

    pub(crate) fn handle_mouse(&mut self, event: MouseEvent) {
        let themes = theme_names();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if mouse::in_rect(event.column, event.row, self.list_area) =>
            {
                if let Some(idx) =
                    mouse::list_click_index(event.row, self.list_area, 0, themes.len())
                {
                    self.set_cursor(idx, &themes);
                    if self.double_click.click(event.column, event.row) {
                        self.apply_theme();
                    }
                }
            }
            MouseEventKind::ScrollDown
                if mouse::in_rect(event.column, event.row, self.list_area) =>
            {
                self.move_cursor(1);
            }
            MouseEventKind::ScrollUp if mouse::in_rect(event.column, event.row, self.list_area) => {
                self.move_cursor(-1);
            }
            _ => {}
        }
    }

    fn bottom_hint(&self) -> String {
        use crate::keymap::{ConfigTabAction as A, RebindableActions};
        let first = |action: A| {
            action
                .resolved()
                .first()
                .map(crate::keymap::Chord::display)
                .unwrap_or_default()
        };
        format!(
            " {}={}  {}={}  {}={}  ?={}",
            first(A::Up),
            crate::i18n::t("zc-zerocode-help-navigate-rows"),
            first(A::Enter),
            crate::i18n::t("zc-config-footer-action-save"),
            first(A::Back),
            crate::i18n::t("zc-config-footer-action-cancel"),
            crate::i18n::t("zc-config-footer-action-help"),
        )
    }

    fn move_cursor(&mut self, delta: isize) {
        let themes = theme_names();
        if themes.is_empty() {
            return;
        }
        let next = (self.theme_cursor as isize + delta).clamp(0, themes.len() as isize - 1);
        self.set_cursor(next as usize, &themes);
    }

    fn set_cursor(&mut self, idx: usize, themes: &[String]) {
        if themes.is_empty() {
            self.theme_cursor = 0;
            return;
        }
        let next = idx.min(themes.len() - 1);
        if next == self.theme_cursor {
            return;
        }
        self.theme_cursor = next;
        self.preview_theme(themes);
    }

    fn clamp_cursor(&mut self, len: usize) {
        if len == 0 {
            self.theme_cursor = 0;
        } else if self.theme_cursor >= len {
            self.theme_cursor = len - 1;
        }
    }

    fn preview_theme(&self, themes: &[String]) {
        let Some(name) = themes.get(self.theme_cursor) else {
            return;
        };
        if let Some(t) = theme::theme_by_name(name) {
            theme::set_active(t);
        }
    }

    fn apply_theme(&mut self) {
        let themes = theme_names();
        let Some(name) = themes.get(self.theme_cursor).cloned() else {
            return;
        };
        let Some(t) = theme::theme_by_name(&name) else {
            return;
        };
        theme::set_active(t);
        match config::persist_theme(&self.config_dir, &name) {
            Ok(()) => self.status = Some(crate::i18n::t("zc-zerocode-conn-saved")),
            Err(e) => {
                self.status = Some(crate::i18n::t_args(
                    "zc-config-status-save-failed",
                    &[("err", &e.to_string())],
                ));
            }
        }
    }
}

impl crate::widgets::HelpContext for ThemePane {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::ConfigTabAction as A;
        use crate::widgets::{HelpEntry as E, HelpNode};
        let keys = |action: A| crate::keymap::action_key_labels(action);
        HelpNode::entries(vec![
            E::new(
                [keys(A::Up), keys(A::Down)].concat(),
                crate::i18n::t("zc-zerocode-help-navigate-rows"),
            ),
            E::new(
                keys(A::Enter),
                crate::i18n::t("zc-zerocode-help-apply-theme"),
            ),
            E::new(
                [keys(A::Back), keys(A::TabLeft)].concat(),
                crate::i18n::t("zc-zerocode-help-back-to-sections"),
            ),
        ])
    }
}

fn theme_names() -> Vec<String> {
    theme::theme_names().map(str::to_string).collect()
}

fn active_theme_cursor(themes: &[String]) -> usize {
    let active = theme::active_raw();
    themes
        .iter()
        .position(|name| theme::theme_by_name(name).is_some_and(|t| t == active))
        .unwrap_or(0)
}

/// Inline palette swatches for a theme row: one block per representative role,
/// in the theme's own colours, followed by a trailing space before the name.
pub(crate) fn theme_swatch_spans(name: &str) -> Vec<Span<'static>> {
    let Some(roles) = theme_swatch_roles(name) else {
        return vec![Span::raw(" ".repeat(SWATCH_STRIP_WIDTH))];
    };
    let mut spans: Vec<Span<'static>> = roles
        .iter()
        .map(|c| {
            let c = crate::color_depth::downgrade(*c);
            Span::styled("█", ratatui::style::Style::default().fg(c))
        })
        .collect();
    spans.push(Span::raw(" "));
    spans
}

/// A blank placeholder the same width as the swatch strip.
pub(crate) fn theme_swatch_blank() -> Vec<Span<'static>> {
    vec![Span::raw(" ".repeat(SWATCH_STRIP_WIDTH))]
}

const SWATCH_ROLE_COUNT: usize = 6;
const SWATCH_STRIP_WIDTH: usize = SWATCH_ROLE_COUNT + 1;

fn theme_swatch_roles(name: &str) -> Option<[ratatui::style::Color; SWATCH_ROLE_COUNT]> {
    use ratatui::style::Color;
    let t = theme::theme_by_name(name)?;
    let roles = [t.background, t.title, t.heading, t.body, t.warn, t.tool];
    if roles.iter().all(|c| *c == Color::Reset) {
        None
    } else {
        Some(roles)
    }
}
