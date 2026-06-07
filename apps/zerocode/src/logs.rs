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

/// Preview row stored in `LogsPane.events`. Carries only the fields
/// rendered in the left-side list. The right-side detail pane fetches
/// the full event payload via `logs/get` when opened and drops it on
/// close — keeping the per-row footprint to a few short strings even
/// across thousands of buffered events.
struct LogEntry {
    /// Stable event id from the persistent log store. Used to lazy-fetch
    /// the full payload via `logs/get { id }` when the detail pane opens.
    id: String,
    timestamp: String,
    severity_number: u8,
    category: String,
    action: String,
    message: String,
}

/// Full event payload — populated by `logs/get` when the detail pane
/// opens, dropped back to `None` when the pane closes. Holds the raw
/// `Value` (with trace ids, attribution map, attributes JSON, …) so
/// the renderer can read every field on demand without the list ever
/// storing them.
pub(crate) struct LogDetail {
    raw: Value,
}

/// Three-state lifecycle for the detail pane body. `logs/get` can
/// legitimately fail — events that arrive via the `logs/event` push
/// before the daemon's writer has flushed them carry a fallback id
/// (the timestamp) that the persistent store cannot resolve. Without
/// a distinct failed state the renderer cannot tell an in-flight
/// fetch from a resolved-but-empty one, and the pane sticks on
/// "Loading…" forever. `Ready` carries either the full payload or a
/// preview-only fallback built from the list row.
pub(crate) enum DetailState {
    /// `logs/get` is in flight (or the pane just opened).
    Loading,
    /// The fetch resolved — full payload or preview-only fallback.
    Ready(LogDetail),
}

impl LogEntry {
    fn from_value(v: &Value) -> Option<Self> {
        // Prefer the persistent id from the log store. Fall back to
        // `(timestamp, span_id)` for events arriving via the
        // `logs/event` push notification before a persistent id is
        // assigned — those rows lazy-fetch full detail via
        // `logs/get { id }` once the daemon's writer has flushed them.
        let timestamp = v.get("@timestamp")?.as_str()?.to_string();
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| timestamp.clone());
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
        let message = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Some(Self {
            id,
            timestamp,
            severity_number,
            category,
            action,
            message,
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

    /// Case-insensitive substring match against preview fields only.
    /// Full-text search across attributes / attribution map is handled
    /// server-side via `LogsQueryParams.q` so the TUI never has to
    /// load full payloads into memory just to filter them.
    fn matches_query(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.message.to_lowercase().contains(&q)
            || self.category.to_lowercase().contains(&q)
            || self.action.to_lowercase().contains(&q)
    }
}

impl LogDetail {
    pub(crate) fn new(raw: Value) -> Self {
        Self { raw }
    }

    /// Build a detail body from the preview row alone, for events whose
    /// full payload could not be fetched (e.g. push-delivered rows not
    /// yet flushed to the persistent store). Carries only the fields the
    /// list already holds; the renderer marks it as preview-only.
    fn from_preview(entry: &LogEntry) -> Self {
        let raw = serde_json::json!({
            "@timestamp": entry.timestamp,
            "severity_number": entry.severity_number,
            "event": {
                "category": entry.category,
                "action": entry.action,
            },
            "message": entry.message,
            "_preview_only": true,
        });
        Self { raw }
    }

    fn is_preview_only(&self) -> bool {
        self.raw
            .get("_preview_only")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn timestamp(&self) -> &str {
        self.raw
            .get("@timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn severity_number(&self) -> u8 {
        self.raw
            .get("severity_number")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u8
    }

    fn event_field(&self, key: &str) -> &str {
        self.raw
            .get("event")
            .and_then(|e| e.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn message(&self) -> &str {
        self.raw
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
    }

    fn trace_id(&self) -> Option<&str> {
        self.raw.get("trace_id").and_then(Value::as_str)
    }

    fn span_id(&self) -> Option<&str> {
        self.raw.get("span_id").and_then(Value::as_str)
    }

    fn duration_ms(&self) -> Option<u64> {
        self.raw.get("zeroclaw")?.get("duration_ms")?.as_u64()
    }

    fn zeroclaw(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if let Some(Value::Object(map)) = self.raw.get("zeroclaw") {
            for (k, val) in map {
                if k == "duration_ms" {
                    continue;
                }
                if let Some(s) = val.as_str() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
        out
    }

    fn attributes(&self) -> &Value {
        static NULL: Value = Value::Null;
        self.raw.get("attributes").unwrap_or(&NULL)
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
        let label_style = theme::dim_style();
        let val_style = theme::body_style();
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-logs-label-timestamp")),
                label_style,
            ),
            Span::styled(self.timestamp().to_string(), val_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-logs-label-severity")),
                label_style,
            ),
            Span::styled(
                format!(
                    "{} ({})",
                    severity_label(self.severity_number()),
                    self.severity_number()
                ),
                severity_style(self.severity_number()).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-logs-label-category")),
                label_style,
            ),
            Span::styled(self.event_field("category").to_string(), val_style),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-logs-label-action")),
                label_style,
            ),
            Span::styled(self.event_field("action").to_string(), val_style),
        ]));
        let outcome = self.event_field("outcome");
        if !outcome.is_empty() && outcome != "unknown" {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-logs-label-outcome")),
                    label_style,
                ),
                Span::styled(outcome.to_string(), val_style),
            ]));
        }
        if let Some(ms) = self.duration_ms() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-logs-label-duration")),
                    label_style,
                ),
                Span::styled(format!("{ms}ms"), val_style),
            ]));
        }

        let msg = self.message();
        if !msg.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-logs-section-message"),
                theme::heading_style(),
            )));
            for msg_line in msg.lines() {
                lines.push(Line::from(Span::styled(msg_line.to_string(), val_style)));
            }
        }

        if self.trace_id().is_some() || self.span_id().is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-logs-section-trace"),
                theme::heading_style(),
            )));
            if let Some(tid) = self.trace_id() {
                lines.push(Line::from(vec![
                    Span::styled("trace_id   ", label_style),
                    Span::styled(tid.to_string(), val_style),
                ]));
            }
            if let Some(sid) = self.span_id() {
                lines.push(Line::from(vec![
                    Span::styled("span_id    ", label_style),
                    Span::styled(sid.to_string(), val_style),
                ]));
            }
        }

        let zc = self.zeroclaw();
        if !zc.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-logs-section-attribution"),
                theme::heading_style(),
            )));
            for (k, v) in &zc {
                let pad = 12usize.saturating_sub(k.len());
                lines.push(Line::from(vec![
                    Span::styled(format!("{k}{}", " ".repeat(pad)), label_style),
                    Span::styled(v.clone(), val_style),
                ]));
            }
        }

        let attrs = self.attributes();
        if !attrs.is_null() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-logs-section-attributes"),
                theme::heading_style(),
            )));
            if let Ok(pretty) = serde_json::to_string_pretty(attrs) {
                for json_line in pretty.lines() {
                    lines.push(Line::from(Span::styled(json_line.to_string(), val_style)));
                }
            }
        }

        if self.is_preview_only() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-logs-preview-only"),
                theme::dim_style(),
            )));
        }

        lines
    }

    /// Plain-text rendering of the detail fields for clipboard.
    fn clipboard_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{:<11}{}\n",
            crate::i18n::t("zc-logs-label-timestamp"),
            self.timestamp()
        ));
        out.push_str(&format!(
            "{:<11}{} ({})\n",
            crate::i18n::t("zc-logs-label-severity"),
            severity_label(self.severity_number()),
            self.severity_number()
        ));
        out.push_str(&format!(
            "{:<11}{}\n",
            crate::i18n::t("zc-logs-label-category"),
            self.event_field("category")
        ));
        out.push_str(&format!(
            "{:<11}{}\n",
            crate::i18n::t("zc-logs-label-action"),
            self.event_field("action")
        ));
        let outcome = self.event_field("outcome");
        if !outcome.is_empty() && outcome != "unknown" {
            out.push_str(&format!(
                "{:<11}{}\n",
                crate::i18n::t("zc-logs-label-outcome"),
                outcome
            ));
        }
        if let Some(ms) = self.duration_ms() {
            out.push_str(&format!(
                "{:<11}{ms}ms\n",
                crate::i18n::t("zc-logs-label-duration")
            ));
        }
        let msg = self.message();
        if !msg.is_empty() {
            out.push_str(&format!(
                "\n{}\n{}\n",
                crate::i18n::t("zc-logs-section-message"),
                msg
            ));
        }
        if self.trace_id().is_some() || self.span_id().is_some() {
            out.push('\n');
            if let Some(tid) = self.trace_id() {
                out.push_str(&format!("trace_id   {tid}\n"));
            }
            if let Some(sid) = self.span_id() {
                out.push_str(&format!("span_id    {sid}\n"));
            }
        }
        let zc = self.zeroclaw();
        if !zc.is_empty() {
            out.push_str("\nAttribution\n");
            for (k, v) in &zc {
                let pad = 12usize.saturating_sub(k.len());
                out.push_str(&format!("{k}{}{v}\n", " ".repeat(pad)));
            }
        }
        let attrs = self.attributes();
        if !attrs.is_null() {
            out.push_str("\nAttributes\n");
            if let Ok(pretty) = serde_json::to_string_pretty(attrs) {
                out.push_str(&pretty);
                out.push('\n');
            }
        }
        out
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
    /// Lazy-loaded full event payload, tracked as a three-state
    /// machine so the renderer can tell a fetch still in flight
    /// apart from one that resolved with no payload. Closing the
    /// pane resets this to `Loading` so long sessions never
    /// accumulate detail bodies for events scrolled past.
    detail: DetailState,
    /// Id of the event whose detail is currently being fetched
    /// or shown. Used to ignore stale `logs/get` responses when
    /// the user moves the selection before the daemon answers.
    detail_request_id: Option<String>,
    detail_scroll: u16,
    detail_pct: u16,
    // Search
    search_active: bool,
    search_buf: String,
    search_query: String, // committed query (applied on Enter)
    // Pagination
    next_cursor: Option<(String, String)>,
    at_end: bool,
    loading: bool,
    // Viewport
    list_height: u16,
    last_list_area: Rect,
    last_detail_area: Option<Rect>,
    double_click: crate::mouse::DoubleClickTracker,
}

impl<'a> Logs<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self {
            rpc,
            notif_rx: rpc.subscribe_notifications(),
            events: Vec::new(),
            list_state: ListState::default(),
            follow: true,
            min_severity: SEV_DEBUG,
            subscribed: false,
            detail_open: false,
            detail: DetailState::Loading,
            detail_request_id: None,
            detail_scroll: 0,
            detail_pct: 50,
            search_active: false,
            search_buf: String::new(),
            search_query: String::new(),
            next_cursor: None,
            at_end: false,
            loading: false,
            list_height: 0,
            last_list_area: Rect::default(),
            last_detail_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
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
                    .filter_map(LogEntry::from_value)
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

    /// Snapshot the raw event index and follow state the cursor
    /// currently points at. Must be called *before* mutating filters.
    fn cursor_anchor(&self) -> (Option<usize>, bool) {
        (self.selected_event_idx(), self.follow)
    }

    /// Reset view state after a filter change. Keeps the in-memory
    /// event buffer intact — `filtered_indices` handles the filtering.
    /// Moves the cursor to the nearest match relative to `anchor`
    /// (captured via `cursor_anchor()` before the filter was changed).
    fn refilter(&mut self, anchor: (Option<usize>, bool)) {
        let (prev_raw_idx, was_following) = anchor;

        // Reset pagination so subsequent scroll-to-top loads can
        // fetch history matching the new filter set.
        self.next_cursor = None;
        self.at_end = false;

        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            self.follow = false;
            self.list_state.select(None);
            return;
        }

        if was_following {
            // Stay pinned to the newest matching event.
            self.follow = true;
            self.list_state.select(Some(filtered.len() - 1));
        } else {
            self.follow = false;
            // Find the filtered position whose raw index is closest to
            // where the cursor was.
            let target = prev_raw_idx.unwrap_or(0);
            let best_pos = filtered
                .iter()
                .enumerate()
                .min_by_key(|(_, raw)| (**raw as isize - target as isize).unsigned_abs())
                .map(|(pos, _)| pos)
                .unwrap_or(0);
            self.list_state.select(Some(best_pos));
            // Center the viewport on the selected item.
            let half = (self.list_height as usize) / 2;
            *self.list_state.offset_mut() = best_pos.saturating_sub(half);
        }
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

    /// Per-tick work: drain events, update follow selection, lazy-fetch
    /// detail body. Async for the detail RPC.
    pub(crate) async fn tick(&mut self) {
        self.drain_notifications();
        let filtered = self.filtered_indices();
        if self.follow && !filtered.is_empty() {
            self.list_state.select(Some(filtered.len() - 1));
        }
        if self.detail_open {
            self.sync_detail_to_selection().await;
        }
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        // Drain + follow re-anchor again so events arriving between tick
        // and draw render this frame. Detail body is fetched only in tick.
        self.drain_notifications();

        let filtered = self.filtered_indices();

        if self.follow && !filtered.is_empty() {
            self.list_state.select(Some(filtered.len() - 1));
        }

        // Layout: status bar (1) + filter bar (1) + content + footer (1)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        // Status bar
        let help: String = if self.search_active {
            format!(
                "Enter:{apply}  Esc:{cancel}",
                apply = crate::i18n::t("zc-logs-search-action-apply"),
                cancel = crate::i18n::t("zc-logs-search-action-cancel"),
            )
        } else {
            String::new()
        };

        let status = Line::from(vec![
            Span::styled(" Logs ", theme::title_style()),
            Span::styled(format!("({}) ", filtered.len()), theme::dim_style()),
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

        // Filter bar (always visible)
        let filter_line = if self.search_active {
            Line::from(vec![
                Span::styled(" sev\u{2265}", theme::dim_style()),
                Span::styled(
                    format!("{} ", severity_label(self.min_severity)),
                    severity_style(self.min_severity).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" /", theme::accent_style()),
                Span::styled(&self.search_buf, theme::input_style()),
                Span::styled("\u{2588}", theme::accent_style()),
            ])
        } else {
            let mut spans = vec![
                Span::styled(" sev\u{2265}", theme::dim_style()),
                Span::styled(
                    format!("{} ", severity_label(self.min_severity)),
                    severity_style(self.min_severity).add_modifier(Modifier::BOLD),
                ),
                if self.follow {
                    Span::styled("[follow] ", theme::accent_style())
                } else {
                    Span::styled("[paused] ", theme::warn_style())
                },
            ];
            if !self.search_query.is_empty() {
                spans.push(Span::styled(" search: ", theme::dim_style()));
                spans.push(Span::styled(&self.search_query, theme::accent_style()));
                spans.push(Span::styled("  (c:clear)", theme::dim_style()));
            }
            Line::from(spans)
        };
        frame.render_widget(Paragraph::new(filter_line), chunks[1]);

        let content_chunk = chunks[2];

        // Main content
        if self.detail_open {
            let list_pct = 100u16.saturating_sub(self.detail_pct);
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(self.detail_pct),
                ])
                .split(content_chunk);
            self.last_detail_area = Some(hsplit[1]);
            self.draw_list(frame, hsplit[0], &filtered);
            self.draw_detail(frame, hsplit[1]);
        } else {
            self.last_detail_area = None;
            self.draw_list(frame, content_chunk, &filtered);
        }

        // Footer: ?=help hint at bottom-left.
        frame.render_widget(
            Paragraph::new(Span::styled(" ?=help", theme::dim_style())),
            chunks[3],
        );
    }

    fn draw_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        self.last_list_area = area;
        // Track inner height (minus borders) for scroll centering.
        self.list_height = area.height.saturating_sub(2);

        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&idx| {
                let e = &self.events[idx];
                let line = Line::from(vec![
                    Span::styled(format!("{} ", e.short_time()), theme::dim_style()),
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

        let Some(_idx) = self.selected_event_idx() else {
            let hint = Paragraph::new(Span::styled(
                crate::i18n::t("zc-logs-no-event-selected"),
                theme::dim_style(),
            ));
            frame.render_widget(hint, inner);
            return;
        };

        // Detail body is lazy-loaded via `logs/get` when the pane
        // opens (see `sync_detail_to_selection`). While the daemon is
        // still answering, show a placeholder; once the fetch resolves
        // — with the full payload or a preview-only fallback — render
        // the fields so the pane never sticks on "Loading…".
        let lines = match &self.detail {
            DetailState::Ready(d) => d.detail_lines(),
            DetailState::Loading => {
                vec![Line::from(Span::styled(
                    crate::i18n::t("zc-logs-loading"),
                    theme::dim_style(),
                ))]
            }
        };
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
            return self.handle_detail_key(key).await;
        }
        self.handle_normal_key(key).await
    }

    async fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::SearchBoxAction;
        match SearchBoxAction::from_chord(&key) {
            Some(SearchBoxAction::Accept) => {
                let anchor = self.cursor_anchor();
                self.search_query = self.search_buf.clone();
                self.search_active = false;
                self.refilter(anchor);
            }
            Some(SearchBoxAction::Cancel) => {
                self.search_active = false;
                self.search_buf = self.search_query.clone();
            }
            Some(SearchBoxAction::Backspace) => {
                self.search_buf.pop();
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.search_buf.push(c);
                }
            }
        }
        false
    }

    async fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::LogsTabAction;
        match LogsTabAction::from_chord(&key) {
            Some(LogsTabAction::CloseDetail) | Some(LogsTabAction::OpenDetail) => {
                self.detail_open = false;
                self.detail_scroll = 0;
                self.detail = DetailState::Loading;
                self.detail_request_id = None;
            }
            Some(LogsTabAction::ClearSearch) if !self.search_query.is_empty() => {
                let anchor = self.cursor_anchor();
                self.search_query.clear();
                self.search_buf.clear();
                self.refilter(anchor);
            }
            Some(LogsTabAction::CopyDetail) => {
                if let DetailState::Ready(d) = &self.detail {
                    crate::mouse::copy_osc52(&d.clipboard_text());
                }
            }
            Some(LogsTabAction::BeginSearch) => {
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            Some(LogsTabAction::DetailScrollDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            Some(LogsTabAction::DetailScrollUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            Some(LogsTabAction::DetailWidenDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            Some(LogsTabAction::DetailWidenUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            Some(LogsTabAction::DetailWidenLeft) => {
                self.detail_pct = (self.detail_pct + 5).min(80);
            }
            Some(LogsTabAction::DetailWidenRight) => {
                self.detail_pct = self.detail_pct.saturating_sub(5).max(20);
            }
            Some(LogsTabAction::IncreaseLevel) => {
                let anchor = self.cursor_anchor();
                self.cycle_severity_up();
                self.refilter(anchor);
            }
            Some(LogsTabAction::DecreaseLevel) => {
                let anchor = self.cursor_anchor();
                self.cycle_severity_down();
                self.refilter(anchor);
            }
            Some(LogsTabAction::Down) => {
                self.move_selection_down();
                self.detail_scroll = 0;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::Up) => {
                self.move_selection_up();
                self.detail_scroll = 0;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::ToggleFollow) => {
                self.follow = !self.follow;
            }
            _ => {}
        }
        false
    }

    async fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::LogsTabAction;
        let filtered_len = self.filtered_indices().len();
        match LogsTabAction::from_chord(&key) {
            Some(LogsTabAction::ClearSearch) if !self.search_query.is_empty() => {
                let anchor = self.cursor_anchor();
                self.search_query.clear();
                self.search_buf.clear();
                self.refilter(anchor);
            }
            Some(LogsTabAction::BeginSearch) => {
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            Some(LogsTabAction::OpenDetail) if self.selected_event_idx().is_some() => {
                self.detail_open = true;
                self.detail_scroll = 0;
                self.detail_pct = 50;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::Down) => {
                self.move_selection_down();
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::Up) => {
                self.move_selection_up();
                self.maybe_load_older().await;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::JumpEnd) => {
                if filtered_len > 0 {
                    self.list_state.select(Some(filtered_len - 1));
                }
                self.follow = true;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::JumpStart) => {
                self.follow = false;
                self.list_state.select(Some(0));
                self.maybe_load_older().await;
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::ToggleFollow) => {
                self.follow = !self.follow;
            }
            Some(LogsTabAction::IncreaseLevel) => {
                let anchor = self.cursor_anchor();
                self.cycle_severity_up();
                self.refilter(anchor);
            }
            Some(LogsTabAction::DecreaseLevel) => {
                let anchor = self.cursor_anchor();
                self.cycle_severity_down();
                self.refilter(anchor);
            }
            Some(LogsTabAction::PageDown) => {
                self.follow = false;
                let i = self.list_state.selected().unwrap_or(0);
                self.list_state
                    .select(Some((i + 20).min(filtered_len.saturating_sub(1))));
                self.sync_detail_to_selection().await;
            }
            Some(LogsTabAction::PageUp) => {
                self.follow = false;
                let i = self.list_state.selected().unwrap_or(0);
                self.list_state.select(Some(i.saturating_sub(20)));
                self.maybe_load_older().await;
                self.sync_detail_to_selection().await;
            }
            _ => {}
        }
        false
    }

    /// Load older events if the selection is near the top and more are available.
    async fn maybe_load_older(&mut self) {
        let sel = self.list_state.selected().unwrap_or(0);
        if sel == 0
            && !self.at_end
            && !self.loading
            && let Some(cursor) = self.next_cursor.clone()
        {
            self.load_page(Some(cursor)).await;
        }
    }

    // ── Mouse handling ───────────────────────────────────────────

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, _content_area: Rect) {
        use crate::mouse;
        use crossterm::event::MouseButton;

        let col = mouse.column;
        let row = mouse.row;
        let filtered_len = self.filtered_indices().len();

        let in_list = mouse::in_rect(col, row, self.last_list_area);
        let in_detail = self
            .last_detail_area
            .is_some_and(|r| mouse::in_rect(col, row, r));

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                if let Some(idx) = mouse::list_click_index(
                    row,
                    self.last_list_area,
                    self.list_state.offset(),
                    filtered_len,
                ) {
                    self.follow = false;
                    self.list_state.select(Some(idx));
                    if self.detail_open {
                        self.detail_scroll = 0;
                    }
                    if self.double_click.click(col, row) {
                        self.detail_open = true;
                        self.detail_scroll = 0;
                        self.detail_pct = 50;
                    }
                }
                // Clicks in detail area are ignored (no selection there).
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                if in_detail {
                    if up {
                        self.detail_scroll = self.detail_scroll.saturating_sub(SCROLL_LINES as u16);
                    } else {
                        self.detail_scroll = self.detail_scroll.saturating_add(SCROLL_LINES as u16);
                    }
                } else if in_list && filtered_len > 0 {
                    self.follow = false;
                    let i = self.list_state.selected().unwrap_or(0);
                    let new_i = mouse::list_scroll(i, filtered_len, up, SCROLL_LINES);
                    self.list_state.select(Some(new_i));
                    if self.detail_open {
                        self.detail_scroll = 0;
                    }
                }
            }
            _ => {}
        }
    }

    // ── Navigation helpers ───────────────────────────────────────

    async fn sync_detail_to_selection(&mut self) {
        if !self.detail_open {
            return;
        }
        let Some(idx) = self.selected_event_idx() else {
            self.detail = DetailState::Loading;
            self.detail_request_id = None;
            return;
        };
        let id = self.events[idx].id.clone();
        // Already resolved for this id — don't re-fire. This guard is
        // what stops a failed `logs/get` from looping forever: the
        // fetch below always resolves to `Ready` (full payload or
        // preview fallback), so once it lands this short-circuits.
        if self.detail_request_id.as_deref() == Some(id.as_str())
            && matches!(self.detail, DetailState::Ready(_))
        {
            return;
        }
        self.detail = DetailState::Loading;
        self.detail_request_id = Some(id.clone());
        // `logs/get` can fail for push-delivered rows the persistent
        // store hasn't flushed yet (their id falls back to the
        // timestamp). Fall back to the preview row rather than leaving
        // the pane stuck on "Loading…".
        let resolved = match self.rpc.logs_get(&id).await {
            Ok(r) => LogDetail::new(r.event),
            Err(_) => LogDetail::from_preview(&self.events[idx]),
        };
        if self.detail_request_id.as_deref() == Some(id.as_str()) {
            self.detail = DetailState::Ready(resolved);
        }
    }

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
        if let Some(pos) = SEV_LEVELS.iter().position(|&l| l == self.min_severity)
            && pos + 1 < SEV_LEVELS.len()
        {
            self.min_severity = SEV_LEVELS[pos + 1];
        }
    }

    fn cycle_severity_down(&mut self) {
        if let Some(pos) = SEV_LEVELS.iter().position(|&l| l == self.min_severity)
            && pos > 0
        {
            self.min_severity = SEV_LEVELS[pos - 1];
        }
    }

    /// Whether the pane is in a text-input mode (search bar active).
    pub(crate) fn wants_text_input(&self) -> bool {
        self.search_active
    }

    /// Route a bracketed-paste payload into the search buffer when the
    /// search bar is open. Mirrors the char-insertion path in
    /// `handle_search_key`; ignored when search isn't active so a stray
    /// paste can't silently mutate hidden state.
    pub(crate) fn handle_paste(&mut self, text: &str) {
        if self.search_active {
            self.search_buf.push_str(text);
        }
    }
}

impl crate::widgets::HelpContext for Logs<'_> {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};
        if self.search_active {
            HelpNode::entries(vec![
                E::key("Enter", crate::i18n::t("zc-logs-help-apply-search")),
                E::key("Esc", crate::i18n::t("zc-logs-help-cancel-search")),
            ])
        } else if self.detail_open {
            HelpNode::entries(vec![
                E::new(
                    vec!["Esc", "Enter"],
                    crate::i18n::t("zc-logs-help-close-detail"),
                ),
                E::new(
                    vec!["j", "k", "↑↓"],
                    crate::i18n::t("zc-logs-help-move-cursor"),
                ),
                E::new(
                    vec!["J", "K", "Shift+↑↓"],
                    crate::i18n::t("zc-logs-help-scroll-detail"),
                ),
                E::key("Shift+←→", crate::i18n::t("zc-logs-help-resize-detail")),
                E::key("f", crate::i18n::t("zc-logs-help-toggle-follow")),
                E::key("/", crate::i18n::t("zc-logs-help-search")),
                E::key("+ / -", crate::i18n::t("zc-logs-help-severity-filter")),
                E::key("c", crate::i18n::t("zc-logs-help-clear-search")),
                E::key("y", crate::i18n::t("zc-logs-help-yank-detail")),
                E::key("?", crate::i18n::t("zc-logs-help-this-help")),
            ])
        } else {
            HelpNode::entries(vec![
                E::new(
                    vec!["j", "k", "↑↓"],
                    crate::i18n::t("zc-logs-help-move-cursor-list"),
                ),
                E::new(vec!["G", "End"], crate::i18n::t("zc-logs-help-jump-bottom")),
                E::new(vec!["g", "Home"], crate::i18n::t("zc-logs-help-jump-top")),
                E::key("PgDn / PgUp", crate::i18n::t("zc-logs-help-page")),
                E::key("Enter", crate::i18n::t("zc-logs-help-open-detail")),
                E::key("f", crate::i18n::t("zc-logs-help-toggle-follow")),
                E::key("/", crate::i18n::t("zc-logs-help-search")),
                E::key("+ / -", crate::i18n::t("zc-logs-help-severity-filter")),
                E::key("c", crate::i18n::t("zc-logs-help-clear-search")),
                E::key("?", crate::i18n::t("zc-logs-help-this-help")),
                E::spacer(),
                E::desc(format!(
                    "{}: {}",
                    crate::i18n::t("zc-logs-help-mouse-label"),
                    crate::i18n::t("zc-logs-help-mouse-desc"),
                )),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> LogEntry {
        LogEntry {
            id: "2026-05-29T11:31:43.543Z".into(),
            timestamp: "2026-05-29T11:31:43.543Z".into(),
            severity_number: SEV_INFO,
            category: "internal".into(),
            action: "note".into(),
            message: "TUI disconnected; session ended".into(),
        }
    }

    #[test]
    fn preview_fallback_renders_row_fields() {
        let detail = LogDetail::from_preview(&sample_entry());
        assert!(detail.is_preview_only());
        assert_eq!(detail.timestamp(), "2026-05-29T11:31:43.543Z");
        assert_eq!(detail.severity_number(), SEV_INFO);
        assert_eq!(detail.event_field("category"), "internal");
        assert_eq!(detail.event_field("action"), "note");
        assert_eq!(detail.message(), "TUI disconnected; session ended");
    }

    #[test]
    fn preview_fallback_is_not_empty_and_notes_partial_payload() {
        let detail = LogDetail::from_preview(&sample_entry());
        let lines = detail.detail_lines();
        assert!(!lines.is_empty());
        // The fallback must visibly signal the payload is partial so
        // the pane never silently masquerades as a full detail view.
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains(&crate::i18n::t("zc-logs-preview-only")));
        // And it must not sit on the "Loading…" placeholder.
        assert!(!text.contains(&crate::i18n::t("zc-logs-loading")));
    }

    #[test]
    fn full_payload_is_not_marked_preview_only() {
        let raw = serde_json::json!({
            "@timestamp": "2026-05-29T11:31:43.543Z",
            "severity_number": SEV_INFO,
            "event": { "category": "internal", "action": "note" },
            "message": "hello",
        });
        let detail = LogDetail::new(raw);
        assert!(!detail.is_preview_only());
        let text: String = detail
            .detail_lines()
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!text.contains(&crate::i18n::t("zc-logs-preview-only")));
    }
}
