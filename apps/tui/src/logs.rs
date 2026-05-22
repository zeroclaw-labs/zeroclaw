use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::client::{LogsQueryParams, RpcClient, RpcNotification};
use crate::theme;

const MAX_EVENTS: usize = 2000;
const LOGS_EVENT_METHOD: &str = "logs/event";
const INITIAL_LOAD: usize = 200;
const PAGE_SIZE: usize = 100;
const SCROLL_LINES: usize = 3;

// ── OTel severity buckets ────────────────────────────────────────

const SEV_TRACE: u8 = 1;
const SEV_DEBUG: u8 = 5;
const SEV_INFO: u8 = 9;
const SEV_WARN: u8 = 13;
const SEV_ERROR: u8 = 17;

const SEV_LEVELS: [u8; 5] = [SEV_TRACE, SEV_DEBUG, SEV_INFO, SEV_WARN, SEV_ERROR];

fn severity_style(num: u8) -> Style {
    match num {
        SEV_TRACE..SEV_DEBUG => Style::default().fg(Color::DarkGray),
        SEV_DEBUG..SEV_INFO => Style::default().fg(Color::Rgb(100, 200, 255)),
        SEV_INFO..SEV_WARN => Style::default().fg(Color::Rgb(220, 240, 255)),
        SEV_WARN..SEV_ERROR => Style::default().fg(Color::Rgb(255, 220, 80)),
        _ => Style::default().fg(Color::Rgb(255, 100, 80)),
    }
}

fn severity_label(num: u8) -> &'static str {
    match num {
        SEV_TRACE..SEV_DEBUG => "TRC",
        SEV_DEBUG..SEV_INFO => "DBG",
        SEV_INFO..SEV_WARN => "INF",
        SEV_WARN..SEV_ERROR => "WRN",
        _ => "ERR",
    }
}

// ── Log entry ────────────────────────────────────────────────────

struct LogEntry {
    timestamp: String,
    severity_number: u8,
    category: String,
    action: String,
    outcome: String,
    message: String,
    trace_id: Option<String>,
    span_id: Option<String>,
    zeroclaw: BTreeMap<String, String>,
    duration_ms: Option<u64>,
    attributes: Value,
}

impl LogEntry {
    fn from_value(v: &Value) -> Option<Self> {
        let timestamp = v.get("@timestamp")?.as_str()?.to_string();
        let severity_number = v.get("severity_number")?.as_u64()? as u8;
        let event = v.get("event")?;
        let category = event
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let action = event
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let outcome = event
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let message = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let trace_id = v.get("trace_id").and_then(Value::as_str).map(String::from);
        let span_id = v.get("span_id").and_then(Value::as_str).map(String::from);

        let zc = v.get("zeroclaw").cloned().unwrap_or(Value::Null);
        let duration_ms = zc.get("duration_ms").and_then(Value::as_u64);
        let mut zeroclaw = BTreeMap::new();
        if let Value::Object(map) = &zc {
            for (k, val) in map {
                if k == "duration_ms" {
                    continue;
                }
                if let Some(s) = val.as_str() {
                    zeroclaw.insert(k.clone(), s.to_string());
                }
            }
        }

        let attributes = v.get("attributes").cloned().unwrap_or(Value::Null);

        Some(Self {
            timestamp,
            severity_number,
            category,
            action,
            outcome,
            message,
            trace_id,
            span_id,
            zeroclaw,
            duration_ms,
            attributes,
        })
    }

    fn short_time(&self) -> &str {
        if let Some(t_pos) = self.timestamp.find('T') {
            let after_t = &self.timestamp[t_pos + 1..];
            let end = after_t
                .find('Z')
                .or_else(|| after_t.find('+'))
                .unwrap_or(after_t.len());
            &after_t[..end.min(12)]
        } else {
            &self.timestamp
        }
    }

    /// Case-insensitive substring match against searchable fields.
    fn matches_query(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.message.to_lowercase().contains(&q)
            || self.category.to_lowercase().contains(&q)
            || self.action.to_lowercase().contains(&q)
            || self
                .zeroclaw
                .values()
                .any(|v| v.to_lowercase().contains(&q))
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
        let label_style = theme::dim_style();
        let val_style = theme::body_style();
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled("Timestamp  ", label_style),
            Span::styled(self.timestamp.clone(), val_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Severity   ", label_style),
            Span::styled(
                format!(
                    "{} ({})",
                    severity_label(self.severity_number),
                    self.severity_number
                ),
                severity_style(self.severity_number).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Category   ", label_style),
            Span::styled(self.category.clone(), val_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Action     ", label_style),
            Span::styled(self.action.clone(), val_style),
        ]));
        if !self.outcome.is_empty() && self.outcome != "unknown" {
            lines.push(Line::from(vec![
                Span::styled("Outcome    ", label_style),
                Span::styled(self.outcome.clone(), val_style),
            ]));
        }
        if let Some(ms) = self.duration_ms {
            lines.push(Line::from(vec![
                Span::styled("Duration   ", label_style),
                Span::styled(format!("{ms}ms"), val_style),
            ]));
        }

        if !self.message.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Message", theme::heading_style())));
            for msg_line in self.message.lines() {
                lines.push(Line::from(Span::styled(msg_line.to_string(), val_style)));
            }
        }

        if self.trace_id.is_some() || self.span_id.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Trace", theme::heading_style())));
            if let Some(tid) = &self.trace_id {
                lines.push(Line::from(vec![
                    Span::styled("trace_id   ", label_style),
                    Span::styled(tid.clone(), val_style),
                ]));
            }
            if let Some(sid) = &self.span_id {
                lines.push(Line::from(vec![
                    Span::styled("span_id    ", label_style),
                    Span::styled(sid.clone(), val_style),
                ]));
            }
        }

        if !self.zeroclaw.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Attribution",
                theme::heading_style(),
            )));
            for (k, v) in &self.zeroclaw {
                let pad = 12usize.saturating_sub(k.len());
                lines.push(Line::from(vec![
                    Span::styled(format!("{k}{}", " ".repeat(pad)), label_style),
                    Span::styled(v.clone(), val_style),
                ]));
            }
        }

        if !self.attributes.is_null() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Attributes",
                theme::heading_style(),
            )));
            if let Ok(pretty) = serde_json::to_string_pretty(&self.attributes) {
                for json_line in pretty.lines() {
                    lines.push(Line::from(Span::styled(json_line.to_string(), val_style)));
                }
            }
        }

        lines
    }
}

// ── Logs pane ────────────────────────────────────────────────────

pub(crate) struct Logs<'a> {
    rpc: &'a RpcClient,
    notif_rx: broadcast::Receiver<RpcNotification>,
    events: Vec<LogEntry>,
    list_state: ListState,
    follow: bool,
    min_severity: u8,
    subscribed: bool,
    detail_open: bool,
    detail_scroll: u16,
    // Search
    search_active: bool,
    search_buf: String,
    search_query: String, // committed query (applied on Enter)
    // Pagination
    next_cursor: Option<(String, String)>,
    at_end: bool,
    loading: bool,
}

impl<'a> Logs<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self {
            rpc,
            notif_rx: rpc.subscribe_notifications(),
            events: Vec::new(),
            list_state: ListState::default(),
            follow: true,
            min_severity: SEV_INFO,
            subscribed: false,
            detail_open: false,
            detail_scroll: 0,
            search_active: false,
            search_buf: String::new(),
            search_query: String::new(),
            next_cursor: None,
            at_end: false,
            loading: false,
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.rpc.logs_subscribe().await?;
        self.subscribed = true;
        // Load initial history
        self.load_page(None).await;
        Ok(())
    }

    /// Fetch a page of older events. If `cursor` is None, fetches the newest.
    async fn load_page(&mut self, cursor: Option<(String, String)>) {
        self.loading = true;
        let params = LogsQueryParams {
            until_ts: cursor.as_ref().map(|(ts, _)| ts.clone()),
            until_id: cursor.as_ref().map(|(_, id)| id.clone()),
            severity_min: Some(self.min_severity),
            q: if self.search_query.is_empty() {
                None
            } else {
                Some(self.search_query.clone())
            },
            hide_internal: true,
            limit: Some(if cursor.is_none() {
                INITIAL_LOAD
            } else {
                PAGE_SIZE
            }),
            ..Default::default()
        };
        match self.rpc.logs_query(params).await {
            Ok(result) => {
                // Events come newest-first from the daemon; reverse to chronological
                let new_entries: Vec<LogEntry> = result
                    .events
                    .iter()
                    .rev()
                    .filter_map(|v| LogEntry::from_value(v))
                    .collect();
                let prepended = new_entries.len();
                if cursor.is_some() && prepended > 0 {
                    // Prepend older events before the existing buffer
                    let mut combined = new_entries;
                    combined.append(&mut self.events);
                    self.events = combined;
                    // Shift selection to keep the same item visible
                    if let Some(sel) = self.list_state.selected() {
                        self.list_state.select(Some(sel + prepended));
                    }
                } else if cursor.is_none() {
                    self.events = new_entries;
                }
                self.next_cursor = result.next_cursor;
                self.at_end = result.at_end;
            }
            Err(_) => {
                // Query unavailable (old daemon without logs/query, or no log file).
                // Mark at_end so we don't keep retrying.
                self.at_end = true;
            }
        }
        self.loading = false;
    }

    fn drain_notifications(&mut self) {
        loop {
            match self.notif_rx.try_recv() {
                Ok(notif) if notif.method == LOGS_EVENT_METHOD => {
                    if let Some(entry) = LogEntry::from_value(&notif.params) {
                        self.events.push(entry);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if self.events.len() > MAX_EVENTS {
            let excess = self.events.len() - MAX_EVENTS;
            self.events.drain(..excess);
        }
    }

    fn filtered_indices(&self) -> Vec<usize> {
        self.events
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e.severity_number >= self.min_severity
                    && (self.search_query.is_empty() || e.matches_query(&self.search_query))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_event_idx(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        let sel = self.list_state.selected()?;
        filtered.get(sel).copied()
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.drain_notifications();

        let filtered = self.filtered_indices();

        if self.follow && !filtered.is_empty() {
            self.list_state.select(Some(filtered.len() - 1));
        }

        // Layout: status bar (1) + optional search bar (1) + content
        let has_search_bar = self.search_active || !self.search_query.is_empty();
        let constraints = if has_search_bar {
            vec![
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ]
        } else {
            vec![Constraint::Length(1), Constraint::Min(0)]
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Status bar
        let help = if self.search_active {
            "Enter:search  Esc:cancel"
        } else if self.detail_open {
            "Esc:close  J/K:scroll detail  q:quit"
        } else {
            "j/k:scroll  /:search  Enter:detail  +/-:sev  f:follow  q:quit"
        };

        let status = Line::from(vec![
            Span::styled(" Logs ", theme::title_style()),
            Span::styled(format!("({}) ", filtered.len()), theme::dim_style()),
            Span::styled("Sev\u{2265}", theme::dim_style()),
            Span::styled(
                format!("{} ", severity_label(self.min_severity)),
                severity_style(self.min_severity).add_modifier(Modifier::BOLD),
            ),
            if self.follow {
                Span::styled("[follow] ", theme::accent_style())
            } else {
                Span::styled("[paused] ", theme::warn_style())
            },
            if self.loading {
                Span::styled("[loading] ", theme::warn_style())
            } else if !self.at_end {
                Span::styled("[more\u{2191}] ", theme::dim_style())
            } else {
                Span::raw("")
            },
            if !self.subscribed {
                Span::styled("[no sub] ", theme::warn_style())
            } else {
                Span::raw("")
            },
            Span::styled(help, theme::dim_style()),
        ]);
        frame.render_widget(Paragraph::new(status), chunks[0]);

        // Search bar
        let content_chunk = if has_search_bar {
            let search_line = if self.search_active {
                Line::from(vec![
                    Span::styled(" /", theme::accent_style()),
                    Span::styled(&self.search_buf, theme::input_style()),
                    Span::styled("\u{2588}", theme::accent_style()), // cursor block
                ])
            } else {
                Line::from(vec![
                    Span::styled(" filter: ", theme::dim_style()),
                    Span::styled(&self.search_query, theme::body_style()),
                    Span::styled("  (/ to edit, Esc to clear)", theme::dim_style()),
                ])
            };
            frame.render_widget(Paragraph::new(search_line), chunks[1]);
            chunks[2]
        } else {
            chunks[1]
        };

        // Main content
        if self.detail_open {
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(content_chunk);
            self.draw_list(frame, hsplit[0], &filtered);
            self.draw_detail(frame, hsplit[1]);
        } else {
            self.draw_list(frame, content_chunk, &filtered);
        }
    }

    fn draw_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&idx| {
                let e = &self.events[idx];
                let line = Line::from(vec![
                    Span::styled(
                        format!("{} ", e.short_time()),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{} ", severity_label(e.severity_number)),
                        severity_style(e.severity_number).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{}/{} ", e.category, e.action), theme::dim_style()),
                    Span::styled(e.message.clone(), severity_style(e.severity_number)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(idx) = self.selected_event_idx() else {
            let hint = Paragraph::new(Span::styled("No event selected", theme::dim_style()));
            frame.render_widget(hint, inner);
            return;
        };

        let lines = self.events[idx].detail_lines();
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    // ── Key handling ─────────────────────────────────────────────

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.search_active {
            return self.handle_search_key(key).await;
        }
        if self.detail_open {
            return self.handle_detail_key(key);
        }
        self.handle_normal_key(key).await
    }

    async fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => {
                self.search_query = self.search_buf.clone();
                self.search_active = false;
                self.list_state.select(None);
                self.follow = true;
                // Re-query daemon with the search term
                self.events.clear();
                self.load_page(None).await;
            }
            KeyCode::Esc => {
                self.search_active = false;
                self.search_buf = self.search_query.clone();
            }
            KeyCode::Backspace => {
                self.search_buf.pop();
            }
            KeyCode::Char(c) => {
                self.search_buf.push(c);
            }
            _ => {}
        }
        false
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc | KeyCode::Enter => {
                self.detail_open = false;
                self.detail_scroll = 0;
            }
            // Shift+J / Shift+K / Shift+Arrow scroll the detail pane
            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Down if shift => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            KeyCode::Up if shift => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            // j/k / plain arrows move the list cursor
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection_down();
                self.detail_scroll = 0;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection_up();
                self.detail_scroll = 0;
            }
            _ => {}
        }
        false
    }

    async fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        let filtered_len = self.filtered_indices().len();

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                // If search is active, Esc clears it
                if !self.search_query.is_empty() {
                    self.search_query.clear();
                    self.search_buf.clear();
                    self.events.clear();
                    self.follow = true;
                    self.load_page(None).await;
                    return false;
                }
                return true;
            }
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            KeyCode::Enter => {
                if self.selected_event_idx().is_some() {
                    self.detail_open = true;
                    self.detail_scroll = 0;
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection_down(),
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection_up();
                self.maybe_load_older().await;
            }
            KeyCode::Char('G') | KeyCode::End => {
                if filtered_len > 0 {
                    self.list_state.select(Some(filtered_len - 1));
                }
                self.follow = true;
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.follow = false;
                self.list_state.select(Some(0));
                self.maybe_load_older().await;
            }
            KeyCode::Char('f') => self.follow = !self.follow,
            KeyCode::Char('+') | KeyCode::Char('=') => self.cycle_severity_up(),
            KeyCode::Char('-') => self.cycle_severity_down(),
            KeyCode::PageDown => {
                self.follow = false;
                let i = self.list_state.selected().unwrap_or(0);
                self.list_state
                    .select(Some((i + 20).min(filtered_len.saturating_sub(1))));
            }
            KeyCode::PageUp => {
                self.follow = false;
                let i = self.list_state.selected().unwrap_or(0);
                self.list_state.select(Some(i.saturating_sub(20)));
                self.maybe_load_older().await;
            }
            _ => {}
        }
        false
    }

    /// Load older events if the selection is near the top and more are available.
    async fn maybe_load_older(&mut self) {
        let sel = self.list_state.selected().unwrap_or(0);
        if sel == 0 && !self.at_end && !self.loading {
            if let Some(cursor) = self.next_cursor.clone() {
                self.load_page(Some(cursor)).await;
            }
        }
    }

    // ── Mouse handling ───────────────────────────────────────────

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, content_area: Rect) {
        let is_scroll_up = matches!(mouse.kind, MouseEventKind::ScrollUp);
        let is_scroll_down = matches!(mouse.kind, MouseEventKind::ScrollDown);
        if !is_scroll_up && !is_scroll_down {
            return;
        }

        let col = mouse.column;
        let row = mouse.row;
        if !content_area.contains((col, row).into()) {
            return;
        }

        let in_detail = self.detail_open && col >= content_area.x + content_area.width / 2;

        if in_detail {
            if is_scroll_down {
                self.detail_scroll = self.detail_scroll.saturating_add(SCROLL_LINES as u16);
            } else {
                self.detail_scroll = self.detail_scroll.saturating_sub(SCROLL_LINES as u16);
            }
        } else {
            let filtered_len = self.filtered_indices().len();
            if filtered_len == 0 {
                return;
            }
            self.follow = false;
            let i = self.list_state.selected().unwrap_or(0);
            let new_i = if is_scroll_down {
                (i + SCROLL_LINES).min(filtered_len - 1)
            } else {
                i.saturating_sub(SCROLL_LINES)
            };
            self.list_state.select(Some(new_i));
            if self.detail_open {
                self.detail_scroll = 0;
            }
        }
    }

    // ── Navigation helpers ───────────────────────────────────────

    fn move_selection_down(&mut self) {
        self.follow = false;
        let filtered_len = self.filtered_indices().len();
        let i = self.list_state.selected().unwrap_or(0);
        if i + 1 < filtered_len {
            self.list_state.select(Some(i + 1));
        }
    }

    fn move_selection_up(&mut self) {
        self.follow = false;
        let i = self.list_state.selected().unwrap_or(0);
        if i > 0 {
            self.list_state.select(Some(i - 1));
        }
    }

    fn cycle_severity_up(&mut self) {
        if let Some(pos) = SEV_LEVELS.iter().position(|&l| l == self.min_severity) {
            if pos + 1 < SEV_LEVELS.len() {
                self.min_severity = SEV_LEVELS[pos + 1];
            }
        }
    }

    fn cycle_severity_down(&mut self) {
        if let Some(pos) = SEV_LEVELS.iter().position(|&l| l == self.min_severity) {
            if pos > 0 {
                self.min_severity = SEV_LEVELS[pos - 1];
            }
        }
    }
}
