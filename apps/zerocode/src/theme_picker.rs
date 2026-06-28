use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::config;
use crate::keymap::{SearchBoxAction, action_key_labels};
use crate::theme::{self, Theme};
use crate::widgets::PickerState;

const CATEGORY_ORDER: &[&str] = &[
    "Essentials",
    "Editor",
    "Retro",
    "Holiday",
    "Flags",
    "Pride",
    "Brand",
    "Sci-Fi",
    "Other",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThemePickerCommit {
    pub name: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThemePickerOutcome {
    Continue,
    Confirmed(ThemePickerCommit),
    Cancelled,
}

#[derive(Debug, Clone)]
pub(crate) struct ThemePicker {
    config_dir: PathBuf,
    original: Theme,
    search: String,
    picker: PickerState,
    status: Option<String>,
}

impl ThemePicker {
    pub(crate) fn new(config_dir: &Path) -> Self {
        let original = theme::active_raw();
        let current = theme_name_for(original).unwrap_or(theme::DEFAULT_THEME_NAME);
        let (items, selectable) = grouped_theme_rows("");
        Self {
            config_dir: config_dir.to_path_buf(),
            original,
            search: String::new(),
            picker: PickerState::new_grouped(items, selectable, Some(current)),
            status: None,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ThemePickerOutcome {
        match SearchBoxAction::from_chord(&key) {
            Some(SearchBoxAction::Accept) => {
                return self
                    .confirm()
                    .map(ThemePickerOutcome::Confirmed)
                    .unwrap_or(ThemePickerOutcome::Continue);
            }
            Some(SearchBoxAction::Cancel) => {
                self.cancel();
                return ThemePickerOutcome::Cancelled;
            }
            Some(SearchBoxAction::Backspace) => {
                if self.search.pop().is_some() {
                    self.rebuild_preserving_selection();
                    self.preview_selected();
                }
                return ThemePickerOutcome::Continue;
            }
            Some(SearchBoxAction::Up) => {
                self.move_up();
                return ThemePickerOutcome::Continue;
            }
            Some(SearchBoxAction::Down) => {
                self.move_down();
                return ThemePickerOutcome::Continue;
            }
            None => {}
        }

        if let KeyCode::Char(c) = key.code
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.search.push(c);
            self.rebuild_preserving_selection();
            self.preview_selected();
        }

        ThemePickerOutcome::Continue
    }

    pub(crate) fn move_up(&mut self) {
        self.picker.move_up();
        self.preview_selected();
    }

    pub(crate) fn move_down(&mut self) {
        self.picker.move_down();
        self.preview_selected();
    }

    pub(crate) fn select_row(&mut self, idx: usize) -> bool {
        let before = self.picker.cursor;
        self.picker.select(idx);
        let selected =
            self.picker.cursor != before || self.picker.selected().is_some_and(|_| idx == before);
        if selected {
            self.preview_selected();
        }
        selected
    }

    pub(crate) fn cancel(&self) {
        theme::set_active(self.original);
    }

    pub(crate) fn confirm(&mut self) -> Option<ThemePickerCommit> {
        let name = self.selected_name()?.to_string();
        let t = theme::theme_by_name(&name)?;
        theme::set_active(t);
        let error = config::persist_theme(&self.config_dir, &name)
            .err()
            .map(|e| e.to_string());
        self.original = t;
        self.status = Some(match &error {
            Some(e) => crate::i18n::t_args("zc-theme-picker-save-failed", &[("error", e)]),
            None => crate::i18n::t_args("zc-theme-picker-saved", &[("theme", &name)]),
        });
        Some(ThemePickerCommit { name, error })
    }

    pub(crate) fn item_count(&self) -> usize {
        self.picker.items.len()
    }

    pub(crate) fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        let title = self.title();
        if self.picker.items.is_empty() {
            let rows = vec![crate::i18n::t("zc-theme-picker-empty")];
            crate::widgets::PickerModal::new(&title, &rows, 0).render(frame, area);
            return;
        }
        crate::widgets::PickerModal::grouped(
            &title,
            &self.picker.items,
            &self.picker.selectable,
            self.picker.cursor,
        )
        .render(frame, area);
    }

    pub(crate) fn overlay_area(&self, area: Rect) -> Option<Rect> {
        let title = self.title();
        if self.picker.items.is_empty() {
            let rows = vec![crate::i18n::t("zc-theme-picker-empty")];
            return crate::widgets::PickerModal::area_for(&title, &rows, area);
        }
        crate::widgets::PickerModal::area_for(&title, &self.picker.items, area)
    }

    pub(crate) fn draw_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", crate::i18n::t("zc-pane-theme")),
                theme::title_style(),
            ))
            .borders(Borders::ALL)
            .border_style(theme::dim_style())
            .style(theme::fill_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(Span::styled(self.filter_label(), theme::input_style()))
                .style(theme::fill_style()),
            rows[0],
        );

        if self.picker.items.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-theme-picker-empty"),
                    theme::dim_style(),
                ))
                .style(theme::fill_style()),
                rows[1],
            );
        } else {
            let items: Vec<ListItem> = self
                .picker
                .items
                .iter()
                .enumerate()
                .map(|(i, label)| {
                    let style = if !self.picker.selectable.get(i).copied().unwrap_or(false) {
                        theme::heading_style()
                    } else {
                        theme::body_style()
                    };
                    ListItem::new(Line::from(Span::styled(label.clone(), style)))
                })
                .collect();
            let mut state = ListState::default();
            state.select(Some(self.picker.cursor));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(theme::selected_style())
                    .highlight_symbol("\u{203a} "),
                rows[1],
                &mut state,
            );
        }

        frame.render_widget(
            Paragraph::new(Span::styled(
                crate::i18n::t_args(
                    "zc-theme-picker-help",
                    &[
                        (
                            "confirm",
                            &action_key_labels(SearchBoxAction::Accept).join("/"),
                        ),
                        (
                            "cancel",
                            &action_key_labels(SearchBoxAction::Cancel).join("/"),
                        ),
                    ],
                ),
                theme::dim_style(),
            ))
            .style(theme::fill_style()),
            rows[2],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                self.status.as_deref().unwrap_or(""),
                theme::dim_style(),
            ))
            .style(theme::fill_style()),
            rows[3],
        );
    }

    fn rebuild_preserving_selection(&mut self) {
        let current = self.selected_name().map(str::to_string);
        let (items, selectable) = grouped_theme_rows(&self.search);
        self.picker = PickerState::new_grouped(items, selectable, current.as_deref());
    }

    fn preview_selected(&self) {
        if let Some(name) = self.selected_name()
            && let Some(t) = theme::theme_by_name(name)
        {
            theme::set_active(t);
        }
    }

    fn selected_name(&self) -> Option<&str> {
        self.picker.selected()
    }

    fn title(&self) -> String {
        crate::i18n::t_args("zc-theme-picker-title", &[("filter", &self.filter_value())])
    }

    fn filter_label(&self) -> String {
        crate::i18n::t_args(
            "zc-theme-picker-filter",
            &[("filter", &self.filter_value())],
        )
    }

    fn filter_value(&self) -> String {
        if self.search.is_empty() {
            crate::i18n::t("zc-theme-picker-filter-all")
        } else {
            self.search.clone()
        }
    }

    #[cfg(test)]
    fn names_for_test(&self) -> Vec<&str> {
        self.picker
            .items
            .iter()
            .zip(self.picker.selectable.iter())
            .filter_map(|(item, selectable)| selectable.then_some(item.as_str()))
            .collect()
    }

    #[cfg(test)]
    fn set_filter_for_test(&mut self, value: &str) {
        self.search = value.to_string();
        self.rebuild_preserving_selection();
        self.preview_selected();
    }
}

fn grouped_theme_rows(filter: &str) -> (Vec<String>, Vec<bool>) {
    let needle = filter.trim().to_ascii_lowercase();
    let catalog: Vec<(&'static str, &'static str)> = theme::theme_catalog().collect();
    let mut categories: Vec<&'static str> = CATEGORY_ORDER.to_vec();
    let mut extra: Vec<&'static str> = catalog
        .iter()
        .map(|(_, category)| *category)
        .filter(|category| !categories.contains(category))
        .collect();
    extra.sort_unstable();
    extra.dedup();
    categories.extend(extra);

    let mut items = Vec::new();
    let mut selectable = Vec::new();
    for category in categories {
        let mut names: Vec<&str> = catalog
            .iter()
            .filter_map(|(name, theme_category)| {
                (*theme_category == category && matches_filter(&needle, name, theme_category))
                    .then_some(*name)
            })
            .collect();
        if names.is_empty() {
            continue;
        }
        names.sort_unstable();
        items.push(category.to_string());
        selectable.push(false);
        for name in names {
            items.push(name.to_string());
            selectable.push(true);
        }
    }
    (items, selectable)
}

fn matches_filter(needle: &str, name: &str, category: &str) -> bool {
    needle.is_empty()
        || name.to_ascii_lowercase().contains(needle)
        || category.to_ascii_lowercase().contains(needle)
}

fn theme_name_for(t: Theme) -> Option<&'static str> {
    theme::theme_names().find(|name| theme::theme_by_name(name) == Some(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_lists_all_theme_names_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let picker = ThemePicker::new(dir.path());
        let mut got = picker.names_for_test();
        got.sort_unstable();
        let mut expected: Vec<&str> = theme::theme_names().collect();
        expected.sort_unstable();
        assert_eq!(got, expected);
    }

    #[test]
    fn cancel_restores_original_theme_after_preview() {
        let original = theme::theme_by_name("dracula").expect("dracula theme");
        let _guard = theme::set_active_for_test(original);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut picker = ThemePicker::new(dir.path());

        picker.set_filter_for_test("nord");
        assert_ne!(theme::active_raw(), original);
        picker.cancel();
        assert_eq!(theme::active_raw(), original);
    }
}
