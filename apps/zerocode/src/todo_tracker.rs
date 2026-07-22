//! Read-only TodoWrite tracker widget for the Code pane.

use crate::wire::{ConfigFieldEntry, PlanEntry, PlanStatus};

/// Where the tracker renders relative to the Code pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TodoLocation {
    Bottom,
    Left,
    Right,
}

impl TodoLocation {
    /// Parse the `[todotracker] location` config value. Unknown values
    /// fall back to `Right` (the schema default).
    fn from_config_str(s: &str) -> Self {
        match s {
            "bottom" => Self::Bottom,
            "left" => Self::Left,
            _ => Self::Right,
        }
    }
}

/// Parsed `[todotracker]` config, sourced from the daemon over
/// `config/list` at pane init. Defaults mirror the schema defaults so a
/// fetch failure or absent section still yields correct behavior.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct TodoTrackerSettings {
    pub enabled: bool,
    pub enabled_at_start: bool,
    pub location: TodoLocation,
    pub width: u16,
    pub max_height: u16,
}

impl Default for TodoTrackerSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            enabled_at_start: false,
            location: TodoLocation::Right,
            width: 32,
            max_height: 5,
        }
    }
}

impl TodoTrackerSettings {
    #[allow(dead_code)]
    pub(crate) fn from_config_fields(fields: &[ConfigFieldEntry]) -> Self {
        let mut s = Self::default();
        for f in fields {
            let key = f.path.rsplit('.').next().unwrap_or(f.path.as_str());
            let Some(value) = f.value.as_ref() else {
                continue;
            };
            match key {
                "enabled" => {
                    if let Some(b) = value.as_bool() {
                        s.enabled = b;
                    }
                }
                "enabled_at_start" => {
                    if let Some(b) = value.as_bool() {
                        s.enabled_at_start = b;
                    }
                }
                "location" => {
                    if let Some(loc) = value.as_str() {
                        s.location = TodoLocation::from_config_str(loc);
                    }
                }
                "width" => {
                    if let Some(n) = value.as_u64() {
                        s.width = n.clamp(1, u16::MAX as u64) as u16;
                    }
                }
                "max_height" => {
                    if let Some(n) = value.as_u64() {
                        s.max_height = n.clamp(1, u16::MAX as u64) as u16;
                    }
                }
                _ => {}
            }
        }
        s
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct TodoTracker {
    entries: Vec<PlanEntry>,
    visible: bool,
    has_ever_popped: bool,
    location: TodoLocation,
    enabled: bool,
    /// Side-panel target column width (left/right); runtime-clamped.
    width: u16,
    /// Bottom-strip max height in rows (grows up to this).
    max_height: u16,
}

#[allow(dead_code)]
impl TodoTracker {
    /// Construct from parsed `[todotracker]` settings.
    pub(crate) fn from_settings(settings: TodoTrackerSettings) -> Self {
        Self {
            entries: Vec::new(),
            visible: settings.enabled && settings.enabled_at_start,
            has_ever_popped: false,
            location: settings.location,
            enabled: settings.enabled,
            width: settings.width,
            max_height: settings.max_height,
        }
    }

    #[cfg(test)]
    pub(crate) fn new(location: TodoLocation, enabled: bool, enabled_at_start: bool) -> Self {
        Self::from_settings(TodoTrackerSettings {
            enabled,
            enabled_at_start,
            location,
            ..TodoTrackerSettings::default()
        })
    }

    pub(crate) fn location(&self) -> TodoLocation {
        self.location
    }

    /// Side-panel target column width from config (left/right).
    pub(crate) fn width(&self) -> u16 {
        self.width
    }

    /// Bottom-strip max height from config.
    pub(crate) fn max_height(&self) -> u16 {
        self.max_height
    }

    /// Replace the plan wholesale. On the first non-empty plan of the
    /// session, auto-pop into view exactly once (unless master-disabled).
    pub(crate) fn set_plan(&mut self, entries: Vec<PlanEntry>) {
        self.entries = entries;
        if self.enabled && !self.has_ever_popped && !self.entries.is_empty() {
            self.visible = true;
            self.has_ever_popped = true;
        }
    }

    /// User show/hide. Inert while master-disabled.
    pub(crate) fn toggle(&mut self) {
        if self.enabled {
            self.visible = !self.visible;
        }
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.enabled && self.visible
    }

    pub(crate) fn entries(&self) -> &[PlanEntry] {
        &self.entries
    }

    pub(crate) fn total(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn done(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.status == PlanStatus::Completed)
            .count()
    }

    /// Whether the tracker should be allocated layout space right now.
    /// Side panels always claim space when visible (placeholder when
    /// empty); the bottom strip claims space only when it has entries
    /// (terminal row height is precious).
    pub(crate) fn wants_space(&self) -> bool {
        if !self.is_visible() {
            return false;
        }
        match self.location {
            TodoLocation::Left | TodoLocation::Right => true,
            TodoLocation::Bottom => !self.entries.is_empty(),
        }
    }

    pub(crate) fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::Paragraph;

        use crate::theme;

        let title = format!(
            " Plan ({}) — {}/{} done ",
            self.total(),
            self.done(),
            self.total()
        );
        // Themed pane chrome: dim border + bold themed title, matching
        // every other split-pane in the Code/Chat view. `fill_style`
        // paints the panel interior with the theme background so the
        // tracker never shows the terminal default through.
        let block = theme::panel_block(&title).style(theme::fill_style());

        if self.entries.is_empty() {
            let placeholder = Paragraph::new(Span::styled("No active plan", theme::dim_style()))
                .style(theme::fill_style())
                .block(block);
            frame.render_widget(placeholder, area);
            return;
        }

        let mut lines: Vec<Line> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            // Map each plan status onto a theme role so the tracker
            // tracks the active palette (and per-agent overrides) live:
            // completed → dim, in-progress → bold accent, pending → body.
            let (glyph, style, label): (&str, Style, &str) = match e.status {
                PlanStatus::Completed => (
                    "✔",
                    theme::dim_style().add_modifier(Modifier::DIM),
                    e.content.as_str(),
                ),
                PlanStatus::InProgress => (
                    "▶",
                    theme::accent_style(),
                    e.active_form.as_deref().unwrap_or(&e.content),
                ),
                PlanStatus::Pending => ("○", theme::body_style(), e.content.as_str()),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), style),
                Span::styled(label.to_string(), style),
            ]));
        }

        let para = Paragraph::new(lines)
            .style(theme::fill_style())
            .block(block);
        frame.render_widget(para, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{PlanEntry, PlanPriority, PlanStatus};

    fn entry(content: &str, status: PlanStatus) -> PlanEntry {
        PlanEntry {
            content: content.to_string(),
            status,
            priority: PlanPriority::Medium,
            active_form: None,
        }
    }

    fn field(path: &str, value: serde_json::Value) -> crate::wire::ConfigFieldEntry {
        crate::wire::ConfigFieldEntry {
            path: path.to_string(),
            category: "todotracker".to_string(),
            kind: crate::wire::PropKind::String,
            type_hint: String::new(),
            value: Some(value),
            populated: true,
            is_secret: false,
            is_env_overridden: false,
            enum_variants: Vec::new(),
            description: String::new(),
            section: None,
            tab: crate::wire::ConfigTab::None,
            alias_source: None,
        }
    }

    #[test]
    fn settings_default_matches_schema_defaults() {
        let s = TodoTrackerSettings::default();
        assert!(s.enabled);
        assert!(!s.enabled_at_start);
        assert_eq!(s.location, TodoLocation::Right);
        assert_eq!(s.width, 32);
        assert_eq!(s.max_height, 5);
    }

    #[test]
    fn settings_parse_all_fields_from_config() {
        let fields = vec![
            field("todotracker.enabled", serde_json::json!(true)),
            field("todotracker.enabled_at_start", serde_json::json!(true)),
            field("todotracker.location", serde_json::json!("bottom")),
            field("todotracker.width", serde_json::json!(40)),
            field("todotracker.max_height", serde_json::json!(8)),
        ];
        let s = TodoTrackerSettings::from_config_fields(&fields);
        assert!(s.enabled);
        assert!(s.enabled_at_start);
        assert_eq!(s.location, TodoLocation::Bottom);
        assert_eq!(s.width, 40);
        assert_eq!(s.max_height, 8);
    }

    #[test]
    fn settings_keep_defaults_for_absent_or_bad_fields() {
        // Unknown location string falls back to Right; missing fields keep defaults.
        let fields = vec![field("todotracker.location", serde_json::json!("diagonal"))];
        let s = TodoTrackerSettings::from_config_fields(&fields);
        assert_eq!(s.location, TodoLocation::Right);
        assert_eq!(s.width, 32);
        assert!(s.enabled);
    }

    #[test]
    fn config_enabled_false_disables_tracker() {
        // The reviewer's core case: [todotracker] enabled = false must
        // actually disable the running tracker.
        let fields = vec![field("todotracker.enabled", serde_json::json!(false))];
        let s = TodoTrackerSettings::from_config_fields(&fields);
        let mut t = TodoTracker::from_settings(s);
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        assert!(
            !t.is_visible(),
            "enabled=false must keep the tracker hidden"
        );
        assert!(!t.wants_space());
    }

    #[test]
    fn config_enabled_at_start_shows_tracker_at_launch() {
        let fields = vec![
            field("todotracker.enabled", serde_json::json!(true)),
            field("todotracker.enabled_at_start", serde_json::json!(true)),
        ];
        let t = TodoTracker::from_settings(TodoTrackerSettings::from_config_fields(&fields));
        assert!(
            t.is_visible(),
            "enabled_at_start=true must be visible at launch"
        );
    }

    #[test]
    fn config_width_and_max_height_flow_to_tracker() {
        let fields = vec![
            field("todotracker.width", serde_json::json!(50)),
            field("todotracker.max_height", serde_json::json!(9)),
        ];
        let t = TodoTracker::from_settings(TodoTrackerSettings::from_config_fields(&fields));
        assert_eq!(t.width(), 50);
        assert_eq!(t.max_height(), 9);
    }

    #[test]
    fn disabled_never_visible_even_after_plan() {
        let mut t = TodoTracker::new(TodoLocation::Right, false, true);
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        assert!(!t.is_visible());
        t.toggle();
        assert!(!t.is_visible(), "toggle is inert while master-disabled");
    }

    #[test]
    fn hidden_at_start_autopops_on_first_plan() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, false);
        assert!(!t.is_visible());
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        assert!(t.is_visible(), "first plan auto-pops");
    }

    #[test]
    fn autopop_is_one_time_toggle_authoritative_after() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, false);
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        t.toggle();
        assert!(!t.is_visible());
        t.set_plan(vec![entry("B", PlanStatus::InProgress)]);
        assert!(!t.is_visible(), "toggle authoritative after first pop");
    }

    #[test]
    fn visible_at_start_when_enabled_at_start_true() {
        let t = TodoTracker::new(TodoLocation::Right, true, true);
        assert!(t.is_visible());
    }

    #[test]
    fn set_plan_replaces_wholesale() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![
            entry("A", PlanStatus::Pending),
            entry("B", PlanStatus::Pending),
        ]);
        t.set_plan(vec![entry("C", PlanStatus::Completed)]);
        assert_eq!(t.entries().len(), 1);
        assert_eq!(t.entries()[0].content, "C");
    }

    #[test]
    fn empty_plan_clears_entries_but_keeps_visibility() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        t.set_plan(vec![]);
        assert!(t.entries().is_empty());
        assert!(t.is_visible(), "clearing does not hide the panel");
    }

    #[test]
    fn done_count_and_total() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![
            entry("A", PlanStatus::Completed),
            entry("B", PlanStatus::InProgress),
            entry("C", PlanStatus::Pending),
        ]);
        assert_eq!(t.total(), 3);
        assert_eq!(t.done(), 1);
    }

    // ── rendering tests ────────────────────────────────────────────────────

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    fn render_to_string(t: &TodoTracker, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| t.render(f, Rect::new(0, 0, w, h))).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    /// Render into a `TestBackend` and return the whole buffer so tests
    /// can inspect per-cell styling (foreground colours), not just text.
    fn render_to_buffer(t: &TodoTracker, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| t.render(f, Rect::new(0, 0, w, h))).unwrap();
        term.backend().buffer().clone()
    }

    /// The foreground colour of the first cell whose symbol equals
    /// `needle` (a single grapheme — `TestBackend` stores one grapheme per
    /// cell). Used to prove entry spans carry themed colours.
    fn fg_of_symbol(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<ratatui::style::Color> {
        buf.content()
            .iter()
            .find(|c| c.symbol() == needle)
            .map(|c| c.fg)
    }

    #[test]
    fn renders_entries_with_status_glyphs() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![
            entry("Alpha", PlanStatus::Completed),
            entry("Beta", PlanStatus::InProgress),
            entry("Gamma", PlanStatus::Pending),
        ]);
        let out = render_to_string(&t, 30, 8);
        assert!(out.contains("Alpha"));
        assert!(out.contains("Beta"));
        assert!(out.contains("Gamma"));
    }

    #[test]
    fn render_obeys_active_theme() {
        // Regression guard: the tracker panel must paint from the active
        // ZeroCode theme, not ratatui defaults. Pin a known palette and
        // assert entry spans carry that theme's colours (routed through the
        // same colour-depth downgrade the renderer uses, so the assertion
        // is independent of the test terminal's detected depth).
        use ratatui::style::Color;

        let theme = crate::theme::theme_by_name("icy_blue").expect("icy_blue registered");
        let _guard = crate::theme::set_active_for_test(theme);

        let expect = |c: Color| crate::color_depth::downgrade(c);

        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![
            entry("Alpha", PlanStatus::Completed),
            entry("Beta", PlanStatus::InProgress),
            entry("Gamma", PlanStatus::Pending),
        ]);
        let buf = render_to_buffer(&t, 30, 8);

        // Pending entry uses the theme body colour ("Gamma" → 'G').
        assert_eq!(
            fg_of_symbol(&buf, "G"),
            Some(expect(theme.body)),
            "pending entry must use theme body colour"
        );
        // In-progress uses the accent colour ("Beta" → 'B').
        assert_eq!(
            fg_of_symbol(&buf, "B"),
            Some(expect(theme.accent)),
            "in-progress entry must use theme accent colour"
        );
        // Completed uses the dim colour (unique '✔' glyph).
        assert_eq!(
            fg_of_symbol(&buf, "✔"),
            Some(expect(theme.dim)),
            "completed entry must use theme dim colour"
        );
        // No rendered cell should fall back to the terminal default fg:
        // every painted cell carries a themed colour.
        assert!(
            buf.content().iter().all(|c| c.fg != Color::Reset),
            "no cell should use the terminal default foreground"
        );
    }

    #[test]
    fn placeholder_obeys_active_theme() {
        // The empty-state placeholder must also honour the theme (dim
        // foreground), not the ratatui default.
        use ratatui::style::Color;

        let theme = crate::theme::theme_by_name("icy_blue").expect("icy_blue registered");
        let _guard = crate::theme::set_active_for_test(theme);

        let t = TodoTracker::new(TodoLocation::Right, true, true);
        let buf = render_to_buffer(&t, 24, 5);
        // "No active plan" → unique 'N' cell carries the placeholder style.
        assert_eq!(
            fg_of_symbol(&buf, "N"),
            Some(crate::color_depth::downgrade(theme.dim)),
            "empty placeholder must use theme dim colour"
        );
        assert!(
            buf.content().iter().all(|c| c.fg != Color::Reset),
            "no cell should use the terminal default foreground"
        );
    }

    #[test]
    fn terminal_theme_distinguishes_completed_from_pending() {
        use ratatui::style::{Color, Modifier};

        let theme = crate::theme::theme_by_name("terminal").expect("terminal registered");
        let _guard = crate::theme::set_active_for_test(theme);

        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![
            entry("Completed", PlanStatus::Completed),
            entry("Pending", PlanStatus::Pending),
        ]);
        let buf = render_to_buffer(&t, 30, 7);
        let completed = buf
            .content()
            .iter()
            .find(|cell| cell.symbol() == "✔")
            .expect("completed row rendered");
        let pending = buf
            .content()
            .iter()
            .find(|cell| cell.symbol() == "○")
            .expect("pending row rendered");

        assert_eq!(completed.fg, Color::Reset);
        assert_eq!(pending.fg, Color::Reset);
        assert!(completed.modifier.contains(Modifier::DIM));
        assert!(!pending.modifier.contains(Modifier::DIM));
        assert_ne!(completed.modifier, pending.modifier);
    }

    #[test]
    fn in_progress_uses_active_form_when_present() {
        let mut t = TodoTracker::new(TodoLocation::Right, true, true);
        t.set_plan(vec![PlanEntry {
            content: "Wire ACP".to_string(),
            status: PlanStatus::InProgress,
            priority: PlanPriority::Medium,
            active_form: Some("Wiring ACP".to_string()),
        }]);
        let out = render_to_string(&t, 30, 6);
        assert!(
            out.contains("Wiring ACP"),
            "active_form shown for in_progress"
        );
    }

    #[test]
    fn side_panel_shows_placeholder_when_empty() {
        let t = TodoTracker::new(TodoLocation::Right, true, true);
        assert!(t.wants_space());
        let out = render_to_string(&t, 24, 5);
        assert!(out.contains("No active plan"));
    }

    #[test]
    fn bottom_strip_wants_no_space_when_empty() {
        let t = TodoTracker::new(TodoLocation::Bottom, true, true);
        assert!(!t.wants_space(), "empty bottom strip claims zero rows");
    }

    #[test]
    fn bottom_strip_wants_space_with_entries() {
        let mut t = TodoTracker::new(TodoLocation::Bottom, true, true);
        t.set_plan(vec![entry("A", PlanStatus::Pending)]);
        assert!(t.wants_space());
    }
}
