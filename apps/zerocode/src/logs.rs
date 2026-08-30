use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::client::{LogsQueryParams, RpcClient, RpcNotification};
use crate::text_selection::{TextRowBreak, TextSelection, TextSnapshot, row_breaks_for_lines};
use crate::theme;

const MAX_EVENTS: usize = 2000;
const LOGS_EVENT_METHOD: &str = "logs/event";
const INITIAL_LOAD: usize = 200;
const PAGE_SIZE: usize = 100;
const SCROLL_LINES: usize = 3;
const COPY_FEEDBACK_TTL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogsTextSurface {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogsTextSelection {
    surface: LogsTextSurface,
    selection: TextSelection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogsCopyTarget {
    text: String,
    anchor: Rect,
    surface: LogsTextSurface,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LogsCopyMenu {
    rect: Rect,
    target: LogsCopyTarget,
    surface: LogsTextSurface,
}

impl LogsCopyMenu {
    fn action_contains(&self, column: u16, row: u16) -> bool {
        self.rect.width > 2
            && self.rect.height > 2
            && crate::mouse::in_rect(
                column,
                row,
                Rect::new(self.rect.x + 1, self.rect.y + 1, self.rect.width - 2, 1),
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogsCopyFeedback {
    rect: Rect,
    surface: LogsTextSurface,
    shown_at: Instant,
}

fn copy_menu_rect(column: u16, row: u16, bounds: Rect) -> Option<Rect> {
    use unicode_width::UnicodeWidthStr;

    if bounds.width < 3 || bounds.height < 3 {
        return None;
    }
    let width = (UnicodeWidthStr::width(crate::i18n::t("zc-logs-copy").as_str()) as u16 + 4)
        .min(bounds.width)
        .max(3);
    let height = 3;
    let max_x = bounds.x.saturating_add(bounds.width.saturating_sub(width));
    let max_y = bounds
        .y
        .saturating_add(bounds.height.saturating_sub(height));
    Some(Rect::new(
        column.clamp(bounds.x, max_x),
        row.clamp(bounds.y, max_y),
        width,
        height,
    ))
}

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
    /// Stable event id from the persistent log store. Used to lazy-fetch
    /// the full payload via `logs/get { id }` when the detail pane opens.
    id: String,
    timestamp: String,
    severity_number: u8,
    category: String,
    action: String,
    message: String,
    live_detail_fallback: Option<Value>,
}

pub(crate) struct LogDetail {
    raw: Value,
}

pub(crate) enum DetailState {
    /// `logs/get` is in flight (or the pane just opened).
    Loading,
    /// The fetch resolved — full payload or preview-only fallback.
    Ready(LogDetail),
}

impl LogEntry {
    fn from_value(v: &Value) -> Option<Self> {
        Self::from_value_with_fallback(v, None)
    }

    fn from_live_value(v: Value) -> Option<Self> {
        let mut entry = Self::from_value_with_fallback(&v, None)?;
        entry.live_detail_fallback = Some(v);
        Some(entry)
    }

    fn from_value_with_fallback(v: &Value, live_detail_fallback: Option<Value>) -> Option<Self> {
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
            live_detail_fallback,
        })
    }

    fn fallback_detail(&self) -> LogDetail {
        self.live_detail_fallback
            .clone()
            .map(LogDetail::new)
            .unwrap_or_else(|| LogDetail::from_preview(self))
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

pub(crate) struct Logs {
    rpc: Arc<RpcClient>,
    notif_rx: broadcast::Receiver<RpcNotification>,
    events: Vec<LogEntry>,
    list_state: ListState,
    follow: bool,
    min_severity: u8,
    subscribed: bool,
    detail_open: bool,
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
    next_cursor_offset: Option<u64>,
    next_cursor_legacy: Option<(String, String)>,
    at_end: bool,
    loading: bool,
    // Viewport
    list_height: u16,
    last_list_area: Rect,
    last_detail_area: Option<Rect>,
    double_click: crate::mouse::DoubleClickTracker,
    list_snapshot: Option<TextSnapshot>,
    list_snapshot_event_ids: Vec<String>,
    detail_snapshot: Option<TextSnapshot>,
    detail_snapshot_event_id: Option<String>,
    text_selection: Option<LogsTextSelection>,
    copy_menu: Option<LogsCopyMenu>,
    copy_feedback: Option<LogsCopyFeedback>,
}

impl Logs {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        let notif_rx = rpc.subscribe_notifications();
        Self {
            rpc,
            notif_rx,
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
            next_cursor_offset: None,
            next_cursor_legacy: None,
            at_end: false,
            loading: false,
            list_height: 0,
            last_list_area: Rect::default(),
            last_detail_area: None,
            double_click: crate::mouse::DoubleClickTracker::new(),
            list_snapshot: None,
            list_snapshot_event_ids: Vec::new(),
            detail_snapshot: None,
            detail_snapshot_event_id: None,
            text_selection: None,
            copy_menu: None,
            copy_feedback: None,
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.rpc.logs_subscribe().await?;
        self.subscribed = true;
        // Load initial history
        self.load_page(None, None).await;
        Ok(())
    }

    /// Fetch a page of older events. If `cursor` is None, fetches the newest.
    async fn load_page(
        &mut self,
        cursor_offset: Option<u64>,
        cursor_legacy: Option<(String, String)>,
    ) {
        self.loading = true;
        let params = LogsQueryParams {
            until_ts: cursor_legacy.as_ref().map(|(ts, _)| ts.clone()),
            until_id: cursor_legacy.as_ref().map(|(_, id)| id.clone()),
            until_line_offset: cursor_offset,
            severity_min: Some(self.min_severity),
            q: if self.search_query.is_empty() {
                None
            } else {
                Some(self.search_query.clone())
            },
            hide_internal: true,
            limit: Some(if cursor_offset.is_none() && cursor_legacy.is_none() {
                INITIAL_LOAD
            } else {
                PAGE_SIZE
            }),
            ..Default::default()
        };
        let has_cursor = cursor_offset.is_some() || cursor_legacy.is_some();
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
                if has_cursor && prepended > 0 {
                    // Prepend older events before the existing buffer
                    let mut combined = new_entries;
                    combined.append(&mut self.events);
                    self.events = combined;
                    // Shift selection to keep the same item visible
                    if let Some(sel) = self.list_state.selected() {
                        self.list_state.select(Some(sel + prepended));
                    }
                } else if !has_cursor {
                    self.events = new_entries;
                }
                // Prefer the byte-offset cursor (independent of id ordering);
                // fall back to the legacy `[timestamp, id]` pair when the
                // daemon has not been upgraded to expose it.
                self.next_cursor_offset = result.next_cursor_line_offset;
                self.next_cursor_legacy = result.next_cursor;
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
        self.next_cursor_offset = None;
        self.next_cursor_legacy = None;
        self.at_end = false;

        let filtered = self.filtered_indices();
        if filtered.is_empty() {
            self.follow = false;
            self.list_state.select(None);
            self.detail = DetailState::Loading;
            self.detail_request_id = None;
            self.detail_scroll = 0;
            self.clear_surface_transients(LogsTextSurface::Detail);
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
                    if let Some(entry) = LogEntry::from_live_value(notif.params) {
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

    fn message_at_filtered_position(&self, position: usize) -> Option<String> {
        let event_idx = self.filtered_indices().get(position).copied()?;
        self.events
            .get(event_idx)
            .map(|entry| entry.message.clone())
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
        self.expire_copy_feedback();

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
            self.clear_snapshot(LogsTextSurface::Detail);
            self.draw_list(frame, content_chunk, &filtered);
        }

        self.render_copy_feedback(frame);
        self.render_copy_menu(frame);

        // Footer: ?=help hint at bottom-left.
        frame.render_widget(
            Paragraph::new(Span::styled(crate::mouse::HELP_HINT, theme::dim_style())),
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

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        let list = List::new(items)
            .block(block)
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.list_state);
        let row_breaks = vec![TextRowBreak::Hard; usize::from(inner.height)];
        let visible_event_ids = filtered
            .iter()
            .skip(self.list_state.offset())
            .take(usize::from(inner.height))
            .map(|&idx| self.events[idx].id.clone())
            .collect();
        self.set_list_snapshot(
            TextSnapshot::capture(frame, inner, row_breaks),
            visible_event_ids,
        );
        self.render_text_selection(frame, LogsTextSurface::List);
    }

    fn draw_detail(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if self.selected_event_idx().is_none() {
            vec![Line::from(Span::styled(
                crate::i18n::t("zc-logs-no-event-selected"),
                theme::dim_style(),
            ))]
        } else if let Some(detail) = self.current_resolved_detail() {
            detail.detail_lines()
        } else {
            vec![Line::from(Span::styled(
                crate::i18n::t("zc-logs-loading"),
                theme::dim_style(),
            ))]
        };
        let row_breaks = row_breaks_for_lines(&lines, inner.width)
            .into_iter()
            .skip(usize::from(self.detail_scroll))
            .take(usize::from(inner.height))
            .chain(std::iter::repeat(TextRowBreak::Hard))
            .take(usize::from(inner.height))
            .collect();
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
        let selected_event_id = self
            .selected_event_idx()
            .map(|idx| self.events[idx].id.clone());
        self.set_detail_snapshot(
            TextSnapshot::capture(frame, inner, row_breaks),
            selected_event_id,
        );
        self.render_text_selection(frame, LogsTextSurface::Detail);
    }

    fn snapshot(&self, surface: LogsTextSurface) -> Option<&TextSnapshot> {
        match surface {
            LogsTextSurface::List => self.list_snapshot.as_ref(),
            LogsTextSurface::Detail => self.detail_snapshot.as_ref(),
        }
    }

    fn set_list_snapshot(&mut self, snapshot: TextSnapshot, event_ids: Vec<String>) {
        let changed = self.list_snapshot.as_ref().is_some_and(|current| {
            current != &snapshot || self.list_snapshot_event_ids != event_ids
        });
        if changed {
            self.clear_surface_transients(LogsTextSurface::List);
        }
        self.list_snapshot = Some(snapshot);
        self.list_snapshot_event_ids = event_ids;
    }

    fn set_detail_snapshot(&mut self, snapshot: TextSnapshot, event_id: Option<String>) {
        let changed = self.detail_snapshot.as_ref().is_some_and(|current| {
            current != &snapshot || self.detail_snapshot_event_id != event_id
        });
        if changed {
            self.clear_surface_transients(LogsTextSurface::Detail);
        }
        self.detail_snapshot = Some(snapshot);
        self.detail_snapshot_event_id = event_id;
    }

    fn clear_snapshot(&mut self, surface: LogsTextSurface) {
        match surface {
            LogsTextSurface::List => {
                self.list_snapshot = None;
                self.list_snapshot_event_ids.clear();
            }
            LogsTextSurface::Detail => {
                self.detail_snapshot = None;
                self.detail_snapshot_event_id = None;
            }
        }
        self.clear_surface_transients(surface);
    }

    fn clear_surface_transients(&mut self, surface: LogsTextSurface) {
        if self
            .text_selection
            .is_some_and(|selection| selection.surface == surface)
        {
            self.text_selection = None;
        }
        if self
            .copy_menu
            .as_ref()
            .is_some_and(|menu| menu.surface == surface || menu.target.surface == surface)
        {
            self.copy_menu = None;
        }
        if self
            .copy_feedback
            .is_some_and(|feedback| feedback.surface == surface)
        {
            self.copy_feedback = None;
        }
    }

    fn surface_at(&self, column: u16, row: u16) -> Option<LogsTextSurface> {
        [LogsTextSurface::List, LogsTextSurface::Detail]
            .into_iter()
            .find(|&surface| {
                self.snapshot(surface)
                    .and_then(|snapshot| snapshot.point_at(column, row))
                    .is_some()
            })
    }

    fn begin_text_drag(&mut self, column: u16, row: u16) -> bool {
        let Some(surface) = self.surface_at(column, row) else {
            self.clear_copy_interaction();
            return false;
        };
        let Some(snapshot) = self.snapshot(surface) else {
            return false;
        };
        let Some(point) = snapshot.point_at(column, row) else {
            return false;
        };
        if snapshot.row_text_bounds(point.row).is_none() {
            self.clear_copy_interaction();
            return false;
        }

        self.follow = false;
        self.copy_menu = None;
        self.copy_feedback = None;
        self.text_selection = Some(LogsTextSelection {
            surface,
            selection: TextSelection {
                anchor: point,
                head: point,
                dragged: false,
            },
        });
        true
    }

    fn update_text_drag(&mut self, column: u16, row: u16) -> bool {
        let Some(current) = self.text_selection else {
            return false;
        };
        let Some(head) = self
            .snapshot(current.surface)
            .and_then(|snapshot| snapshot.point_at(column, row))
        else {
            return false;
        };
        self.text_selection = Some(LogsTextSelection {
            surface: current.surface,
            selection: TextSelection {
                anchor: current.selection.anchor,
                head,
                dragged: head != current.selection.anchor,
            },
        });
        true
    }

    fn finish_text_drag(&mut self) {
        if self
            .text_selection
            .is_some_and(|selection| !selection.selection.dragged)
        {
            self.text_selection = None;
        }
    }

    fn selected_text_target(&self) -> Option<LogsCopyTarget> {
        let current = self.text_selection?;
        if current.surface == LogsTextSurface::Detail {
            self.current_resolved_detail()?;
        }
        let snapshot = self.snapshot(current.surface)?;
        Some(LogsCopyTarget {
            text: snapshot.selected_text(current.selection)?,
            anchor: snapshot.selection_anchor_rect(current.selection)?,
            surface: current.surface,
        })
    }

    fn selected_list_row_target(&self) -> Option<LogsCopyTarget> {
        let snapshot = self.list_snapshot.as_ref()?;
        let selected = self.list_state.selected()?;
        let visible_row = selected.checked_sub(self.list_state.offset())?;
        let row = u16::try_from(visible_row).ok()?;
        if row >= snapshot.area.height {
            return None;
        }
        Some(LogsCopyTarget {
            text: self.message_at_filtered_position(selected)?,
            anchor: Rect::new(
                snapshot.area.x,
                snapshot.area.y + row,
                snapshot.area.width,
                1,
            ),
            surface: LogsTextSurface::List,
        })
    }

    fn clear_copy_interaction(&mut self) {
        self.text_selection = None;
        self.copy_menu = None;
        self.copy_feedback = None;
    }

    fn copy_target(&mut self, target: LogsCopyTarget) -> bool {
        if target.text.is_empty() {
            return false;
        }
        crate::mouse::copy_osc52(&target.text);
        self.text_selection = None;
        self.copy_menu = None;
        self.copy_feedback = self
            .feedback_rect(target.anchor, target.surface)
            .map(|rect| LogsCopyFeedback {
                rect,
                surface: target.surface,
                shown_at: Instant::now(),
            });
        true
    }

    fn copy_current_selection_or_row(&mut self) -> bool {
        let target = if self.text_selection.is_some() {
            self.selected_text_target()
        } else {
            self.selected_list_row_target()
        };
        let Some(target) = target else {
            return false;
        };
        self.copy_target(target)
    }

    fn copy_detail(&mut self) -> bool {
        let Some(detail) = self.current_resolved_detail() else {
            return false;
        };
        let text = detail.clipboard_text();
        let anchor = self
            .detail_snapshot
            .as_ref()
            .map(|snapshot| snapshot.area)
            .or(self.last_detail_area)
            .unwrap_or_default();
        self.copy_target(LogsCopyTarget {
            text,
            anchor,
            surface: LogsTextSurface::Detail,
        })
    }

    fn open_copy_menu(&mut self, column: u16, row: u16) -> bool {
        let Some(clicked_surface) = self.surface_at(column, row) else {
            self.copy_menu = None;
            return false;
        };
        let Some(clicked_snapshot) = self.snapshot(clicked_surface) else {
            return false;
        };
        let menu_bounds = match clicked_surface {
            LogsTextSurface::List => self.last_list_area,
            LogsTextSurface::Detail => self.last_detail_area.unwrap_or(clicked_snapshot.area),
        };
        let target = if self.text_selection.is_some() {
            let Some(selected) = self.selected_text_target() else {
                return false;
            };
            selected
        } else {
            let Some(point) = clicked_snapshot.point_at(column, row) else {
                return false;
            };
            match clicked_surface {
                LogsTextSurface::List => {
                    let filtered_position = self.list_state.offset() + usize::from(point.row);
                    let Some(text) = self.message_at_filtered_position(filtered_position) else {
                        return false;
                    };
                    LogsCopyTarget {
                        text,
                        anchor: Rect::new(
                            clicked_snapshot.area.x,
                            clicked_snapshot.area.y + point.row,
                            clicked_snapshot.area.width,
                            1,
                        ),
                        surface: clicked_surface,
                    }
                }
                LogsTextSurface::Detail => {
                    let Some(detail) = self.current_resolved_detail() else {
                        return false;
                    };
                    LogsCopyTarget {
                        text: detail.clipboard_text(),
                        anchor: clicked_snapshot.area,
                        surface: clicked_surface,
                    }
                }
            }
        };
        let Some(rect) = copy_menu_rect(column, row, menu_bounds) else {
            return false;
        };
        self.copy_feedback = None;
        self.copy_menu = Some(LogsCopyMenu {
            rect,
            target,
            surface: clicked_surface,
        });
        true
    }

    fn current_resolved_detail(&self) -> Option<&LogDetail> {
        let selected_idx = self.selected_event_idx()?;
        let selected_id = self.events.get(selected_idx)?.id.as_str();
        if self.detail_request_id.as_deref() != Some(selected_id) {
            return None;
        }
        match &self.detail {
            DetailState::Ready(detail) => Some(detail),
            DetailState::Loading => None,
        }
    }

    fn activate_copy_menu(&mut self) -> bool {
        let Some(menu) = self.copy_menu.take() else {
            return false;
        };
        self.copy_target(menu.target)
    }

    fn confirm_copy_menu_key(&mut self) -> bool {
        self.activate_copy_menu();
        false
    }

    fn dismiss_copy_menu_key(&mut self) -> bool {
        self.copy_menu = None;
        false
    }

    fn feedback_rect(&self, anchor: Rect, surface: LogsTextSurface) -> Option<Rect> {
        use unicode_width::UnicodeWidthStr;

        let bounds = self.snapshot(surface)?.area;
        let label = crate::i18n::t("zc-logs-copied");
        let width = (UnicodeWidthStr::width(label.as_str()) as u16).min(bounds.width);
        if width == 0 || bounds.height == 0 {
            return None;
        }
        let max_x = bounds.x.saturating_add(bounds.width.saturating_sub(width));
        let center = anchor.x.saturating_add(anchor.width / 2);
        let x = center.saturating_sub(width / 2).clamp(bounds.x, max_x);
        let y = anchor.y.clamp(bounds.y, bounds.y + bounds.height - 1);
        Some(Rect::new(x, y, width, 1))
    }

    fn expire_copy_feedback(&mut self) {
        if self
            .copy_feedback
            .is_some_and(|feedback| feedback.shown_at.elapsed() >= COPY_FEEDBACK_TTL)
        {
            self.copy_feedback = None;
        }
    }

    fn render_text_selection(&self, frame: &mut ratatui::Frame, surface: LogsTextSurface) {
        let Some(current) = self
            .text_selection
            .filter(|selection| selection.surface == surface)
        else {
            return;
        };
        if let Some(snapshot) = self.snapshot(surface) {
            snapshot.render_selection(frame, current.selection, theme::selected_bg_style());
        }
    }

    fn render_copy_feedback(&self, frame: &mut ratatui::Frame) {
        let Some(feedback) = self.copy_feedback else {
            return;
        };
        frame.render_widget(Clear, feedback.rect);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                crate::i18n::t("zc-logs-copied"),
                theme::success_style().add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Center),
            feedback.rect,
        );
    }

    fn render_copy_menu(&self, frame: &mut ratatui::Frame) {
        let Some(menu) = &self.copy_menu else {
            return;
        };
        frame.render_widget(Clear, menu.rect);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                crate::i18n::t("zc-logs-copy"),
                theme::accent_style().add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::accent_style()),
            ),
            menu.rect,
        );
    }

    // ── Key handling ─────────────────────────────────────────────

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::LogsTabAction;

        if self.search_active {
            self.copy_menu = None;
            self.text_selection = None;
            self.copy_feedback = None;
            return self.handle_search_key(key).await;
        }
        if self.copy_menu.is_some() {
            match key.code {
                KeyCode::Enter => return self.confirm_copy_menu_key(), // keyguard: transient copy-menu confirmation
                KeyCode::Esc => return self.dismiss_copy_menu_key(), // keyguard: transient copy-menu dismissal
                _ => self.copy_menu = None,
            }
        }
        if matches!(
            LogsTabAction::from_chord(&key),
            Some(LogsTabAction::CopySelection)
        ) {
            self.copy_current_selection_or_row();
            return false;
        }
        self.text_selection = None;
        self.copy_feedback = None;
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
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
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
                self.copy_detail();
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
            && (self.next_cursor_offset.is_some() || self.next_cursor_legacy.is_some())
        {
            self.load_page(self.next_cursor_offset, self.next_cursor_legacy.clone())
                .await;
        }
    }

    // ── Mouse handling ───────────────────────────────────────────

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, _content_area: Rect) {
        use crate::mouse;

        let col = mouse.column;
        let row = mouse.row;
        let filtered_len = self.filtered_indices().len();

        let in_list = mouse::in_rect(col, row, self.last_list_area);
        let in_detail = self
            .last_detail_area
            .is_some_and(|r| mouse::in_rect(col, row, r));

        if self.search_active {
            self.copy_menu = None;
        }
        if let Some(menu) = &self.copy_menu
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            let activate = menu.action_contains(col, row);
            if activate {
                self.activate_copy_menu();
            } else {
                self.copy_menu = None;
            }
            return;
        }

        let opens_context_menu = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
            || (cfg!(target_os = "macos")
                && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
                && mouse.modifiers.contains(KeyModifiers::CONTROL));
        if opens_context_menu {
            if !self.search_active {
                self.open_copy_menu(col, row);
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if in_list
                    && let Some(idx) = mouse::list_click_index(
                        row,
                        self.last_list_area,
                        self.list_state.offset(),
                        filtered_len,
                    )
                {
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
                if in_list || in_detail {
                    self.begin_text_drag(col, row);
                } else {
                    self.clear_copy_interaction();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.update_text_drag(col, row);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.finish_text_drag();
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.clear_copy_interaction();
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
        // timestamp). Prefer the pushed full event as a bounded
        // in-memory fallback; only drop to preview fields when the row
        // truly has no full payload.
        let resolved = match self.rpc.logs_get(&id).await {
            Ok(r) => LogDetail::new(r.event),
            Err(_) => self.events[idx].fallback_detail(),
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

impl crate::widgets::HelpContext for Logs {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::help::entries_for;
        use crate::keymap::LogsTabAction as L;
        use crate::widgets::{HelpEntry as E, HelpNode};
        if self.search_active {
            HelpNode::entries(entries_for([
                crate::keymap::SearchBoxAction::Accept,
                crate::keymap::SearchBoxAction::Cancel,
            ]))
        } else if self.detail_open {
            HelpNode::entries(entries_for([
                L::CloseDetail,
                L::Up,
                L::Down,
                L::DetailScrollUp,
                L::DetailScrollDown,
                L::DetailWidenLeft,
                L::DetailWidenRight,
                L::ToggleFollow,
                L::BeginSearch,
                L::IncreaseLevel,
                L::DecreaseLevel,
                L::ClearSearch,
                L::CopyDetail,
                L::CopySelection,
            ]))
        } else {
            let mut entries = entries_for([
                L::Up,
                L::Down,
                L::JumpEnd,
                L::JumpStart,
                L::PageDown,
                L::OpenDetail,
                L::ToggleFollow,
                L::BeginSearch,
                L::IncreaseLevel,
                L::DecreaseLevel,
                L::ClearSearch,
                L::CopySelection,
            ]);
            entries.push(E::spacer());
            entries.push(E::desc(format!(
                "{}: {}",
                crate::i18n::t("zc-logs-help-mouse-label"),
                crate::i18n::t("zc-logs-help-mouse-desc"),
            )));
            HelpNode::entries(entries)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::RpcOutbound;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};
    use tokio::sync::mpsc;

    fn sample_entry() -> LogEntry {
        LogEntry {
            id: "2026-05-29T11:31:43.543Z".into(),
            timestamp: "2026-05-29T11:31:43.543Z".into(),
            severity_number: SEV_INFO,
            category: "internal".into(),
            action: "note".into(),
            message: "TUI disconnected; session ended".into(),
            live_detail_fallback: None,
        }
    }

    fn test_logs() -> Logs {
        let (tx, _rx) = mpsc::channel::<String>(16);
        Logs::new(Arc::new(RpcClient::with_rpc(Arc::new(RpcOutbound::new(
            tx,
        )))))
    }

    fn draw(logs: &mut Logs, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| logs.draw(frame, frame.area()))
            .expect("draw logs");
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[tokio::test]
    async fn search_input_owns_copy_chords() {
        let mut logs = test_logs();
        logs.events.push(sample_entry());
        logs.list_state.select(Some(0));
        draw(&mut logs, 100, 24);
        logs.search_active = true;
        logs.search_buf = "needle".into();
        let list_area = logs.list_snapshot.as_ref().expect("list snapshot").area;

        logs.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Right),
                list_area.x,
                list_area.y,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 100, 24),
        );
        assert!(logs.copy_menu.is_none());

        for key in [
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER),
            KeyEvent::new(
                KeyCode::Char('C'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        ] {
            logs.handle_key(key).await;
            assert_eq!(logs.search_buf, "needle");
            assert!(logs.search_active);
            assert!(
                logs.copy_feedback.is_none(),
                "copy chord must not target the selected row while search owns input"
            );
        }

        logs.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .await;
        assert_eq!(logs.search_buf, "needlex");

        logs.search_active = false;
        logs.search_buf = logs.search_query.clone();
        assert!(logs.open_copy_menu(list_area.x, list_area.y));
        logs.search_active = true;
        logs.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;
        assert!(!logs.search_active);
        assert!(logs.copy_menu.is_none());
        assert!(logs.copy_feedback.is_none());

        assert!(logs.open_copy_menu(list_area.x, list_area.y));
        logs.search_active = true;
        logs.search_buf = "cancelled".into();
        logs.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;
        assert!(!logs.search_active);
        assert!(logs.copy_menu.is_none());
        assert!(logs.copy_feedback.is_none());
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

    #[test]
    fn live_fallback_preserves_full_event_attributes() {
        let raw = serde_json::json!({
            "@timestamp": "2026-07-04T06:32:41.044Z",
            "severity_number": SEV_INFO,
            "event": { "category": "provider", "action": "send" },
            "message": "llm_request",
            "attributes": {
                "model": "switched-model",
                "messages_count": 2
            }
        });
        let entry = LogEntry::from_live_value(raw).expect("live event row");
        let detail = entry.fallback_detail();
        assert!(!detail.is_preview_only());
        assert_eq!(detail.attributes()["model"], "switched-model");

        let text: String = detail
            .detail_lines()
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("switched-model"));
        assert!(!text.contains(&crate::i18n::t("zc-logs-preview-only")));
    }

    #[tokio::test]
    async fn list_drag_can_start_in_side_whitespace() {
        let mut logs = test_logs();
        logs.events.push(sample_entry());
        draw(&mut logs, 90, 12);

        let snapshot = logs.list_snapshot.as_ref().expect("list snapshot");
        let row = snapshot.area.y;
        let from_side = snapshot.area.x + snapshot.area.width - 1;
        let into_text = snapshot.area.x + 5;
        logs.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                from_side,
                row,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 90, 12),
        );
        logs.handle_mouse(
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                into_text,
                row,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 90, 12),
        );
        logs.handle_mouse(
            mouse(
                MouseEventKind::Up(MouseButton::Left),
                into_text,
                row,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 90, 12),
        );

        let target = logs.selected_text_target().expect("drag selection");
        assert_eq!(target.surface, LogsTextSurface::List);
        assert!(target.text.contains("TUI disconnected; session ended"));
        assert!(!logs.follow, "starting a selection must pause follow mode");
    }

    #[tokio::test]
    async fn active_detail_selection_wins_over_right_clicked_list_row() {
        let mut logs = test_logs();
        let entry = sample_entry();
        logs.detail_request_id = Some(entry.id.clone());
        logs.detail = DetailState::Ready(LogDetail::from_preview(&entry));
        logs.events.push(entry);
        logs.detail_open = true;
        draw(&mut logs, 100, 18);

        let detail = logs.detail_snapshot.as_ref().expect("detail snapshot");
        let row = detail.area.y;
        let start = detail.area.x;
        let end = (start + 8).min(detail.area.x + detail.area.width - 1);
        assert!(logs.begin_text_drag(start, row));
        assert!(logs.update_text_drag(end, row));
        logs.finish_text_drag();
        let selected = logs.selected_text_target().expect("detail selection").text;

        let list = logs.list_snapshot.as_ref().expect("list snapshot");
        assert!(logs.open_copy_menu(list.area.x, list.area.y));
        let menu = logs.copy_menu.as_ref().expect("copy menu");
        assert_eq!(menu.target.text, selected);
        assert_eq!(menu.target.surface, LogsTextSurface::Detail);
        assert!(logs.copy_feedback.is_none(), "opening must not copy");

        assert!(logs.activate_copy_menu());
        assert!(logs.text_selection.is_none());
        assert!(logs.copy_menu.is_none());
        assert!(logs.copy_feedback.is_some());
    }

    #[tokio::test]
    async fn copy_shortcut_prefers_selection_and_whole_detail_y_still_works() {
        let mut logs = test_logs();
        let entry = sample_entry();
        logs.detail_request_id = Some(entry.id.clone());
        logs.detail = DetailState::Ready(LogDetail::from_preview(&entry));
        logs.events.push(entry);
        logs.detail_open = true;
        draw(&mut logs, 100, 18);

        let detail_area = logs.detail_snapshot.as_ref().expect("detail snapshot").area;
        assert!(logs.begin_text_drag(detail_area.x, detail_area.y));
        assert!(logs.update_text_drag(detail_area.x + 5, detail_area.y));
        logs.finish_text_drag();
        logs.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER))
            .await;
        assert!(logs.text_selection.is_none());
        assert_eq!(
            logs.copy_feedback.expect("selection feedback").surface,
            LogsTextSurface::Detail,
        );

        logs.copy_feedback = None;
        logs.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
            .await;
        assert!(logs.copy_feedback.is_some(), "whole-detail y still copies");
    }

    #[tokio::test]
    async fn copy_shortcut_without_selection_copies_highlighted_list_row() {
        let mut logs = test_logs();
        let complete_message =
            "second message continues far beyond the narrow terminal viewport without truncation";
        for message in ["first", complete_message, "third"] {
            let mut entry = sample_entry();
            entry.message = message.to_string();
            logs.events.push(entry);
        }
        logs.follow = false;
        logs.list_state.select(Some(1));
        draw(&mut logs, 80, 10);

        let target = logs
            .selected_list_row_target()
            .expect("highlighted row target");
        assert_eq!(target.text, complete_message);

        logs.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER))
            .await;
        assert_eq!(
            logs.copy_feedback.expect("row feedback").surface,
            LogsTextSurface::List,
        );
    }

    #[tokio::test]
    async fn right_click_targets_complete_filtered_scrolled_row_and_dismisses() {
        let mut logs = test_logs();
        let messages = [
            "first message continues far beyond the narrow terminal viewport without truncation",
            "second message continues far beyond the narrow terminal viewport without truncation",
            "third message continues far beyond the narrow terminal viewport without truncation",
            "fourth message continues far beyond the narrow terminal viewport without truncation",
            "fifth message continues far beyond the narrow terminal viewport without truncation",
            "sixth message continues far beyond the narrow terminal viewport without truncation",
        ];
        logs.min_severity = SEV_INFO;
        for (index, message) in messages.iter().enumerate() {
            let mut excluded = sample_entry();
            excluded.message = format!("excluded debug message {index}");
            excluded.severity_number = SEV_DEBUG;
            logs.events.push(excluded);

            let mut entry = sample_entry();
            entry.message = (*message).to_string();
            logs.events.push(entry);
        }
        logs.follow = false;
        logs.list_state.select(Some(messages.len() - 1));
        draw(&mut logs, 70, 7);

        let snapshot = logs.list_snapshot.as_ref().expect("list snapshot");
        let visible_position = logs.list_state.offset();
        assert!(visible_position > 0, "the fixture must scroll the list");
        let expected = messages[visible_position];
        assert!(
            expected.len() > usize::from(snapshot.area.width),
            "the fixture message must exceed the viewport width",
        );
        let column = snapshot.area.x;
        let row = snapshot.area.y;
        logs.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Right),
                column,
                row,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 70, 7),
        );

        let menu = logs.copy_menu.as_ref().expect("right-click menu");
        assert_eq!(menu.target.text, expected);
        assert!(logs.copy_feedback.is_none(), "opening must not copy");

        logs.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                0,
                0,
                KeyModifiers::NONE,
            ),
            Rect::new(0, 0, 70, 7),
        );
        assert!(logs.copy_menu.is_none());
        assert!(logs.copy_feedback.is_none());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn control_click_opens_the_copy_menu() {
        let mut logs = test_logs();
        logs.events.push(sample_entry());
        draw(&mut logs, 70, 10);
        let area = logs.list_snapshot.as_ref().expect("list snapshot").area;

        logs.handle_mouse(
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
                KeyModifiers::CONTROL,
            ),
            Rect::new(0, 0, 70, 10),
        );

        assert!(logs.copy_menu.is_some());
        assert!(logs.copy_feedback.is_none());
    }

    #[tokio::test]
    async fn detail_scroll_snapshot_change_dismisses_selection_and_menu() {
        let mut logs = test_logs();
        let entry = sample_entry();
        logs.detail_request_id = Some(entry.id.clone());
        logs.detail = DetailState::Ready(LogDetail::from_preview(&entry));
        logs.events.push(entry);
        logs.detail_open = true;
        draw(&mut logs, 100, 12);

        let area = logs.detail_snapshot.as_ref().expect("detail snapshot").area;
        assert!(logs.begin_text_drag(area.x, area.y));
        assert!(logs.update_text_drag(area.x + 5, area.y));
        logs.finish_text_drag();
        assert!(logs.open_copy_menu(area.x, area.y));

        logs.detail_scroll = 1;
        draw(&mut logs, 100, 12);
        assert!(logs.text_selection.is_none());
        assert!(logs.copy_menu.is_none());
    }

    #[tokio::test]
    async fn filtering_to_no_rows_clears_stale_detail_copy_state() {
        let mut logs = test_logs();
        let entry = sample_entry();
        logs.detail_request_id = Some(entry.id.clone());
        logs.detail = DetailState::Ready(LogDetail::from_preview(&entry));
        logs.events.push(entry);
        logs.detail_open = true;
        draw(&mut logs, 100, 12);

        let detail_area = logs.detail_snapshot.as_ref().expect("detail snapshot").area;
        assert!(logs.open_copy_menu(detail_area.x, detail_area.y));

        let anchor = logs.cursor_anchor();
        logs.search_query = "does-not-match".into();
        logs.refilter(anchor);

        assert!(logs.list_state.selected().is_none());
        assert!(matches!(logs.detail, DetailState::Loading));
        assert!(logs.detail_request_id.is_none());
        assert!(logs.copy_menu.is_none());
        assert!(!logs.copy_detail());

        draw(&mut logs, 100, 12);
        let detail_area = logs.detail_snapshot.as_ref().expect("detail snapshot").area;
        assert!(!logs.open_copy_menu(detail_area.x, detail_area.y));
    }

    #[tokio::test]
    async fn identical_list_cells_with_new_event_id_dismiss_copy_menu() {
        let mut logs = test_logs();
        logs.events.push(sample_entry());
        draw(&mut logs, 100, 12);

        let list_area = logs.list_snapshot.as_ref().expect("list snapshot").area;
        assert!(logs.open_copy_menu(list_area.x, list_area.y));

        logs.events[0].id = "replacement-event-id".into();
        draw(&mut logs, 100, 12);

        assert!(logs.copy_menu.is_none());
        assert_eq!(logs.list_snapshot_event_ids, ["replacement-event-id"]);
    }

    #[tokio::test]
    async fn changed_detail_event_id_rejects_stale_resolved_payload() {
        let mut logs = test_logs();
        let entry = sample_entry();
        logs.detail_request_id = Some(entry.id.clone());
        logs.detail = DetailState::Ready(LogDetail::from_preview(&entry));
        logs.events.push(entry);
        logs.detail_open = true;
        draw(&mut logs, 100, 12);

        let detail_area = logs.detail_snapshot.as_ref().expect("detail snapshot").area;
        assert!(logs.open_copy_menu(detail_area.x, detail_area.y));

        logs.events[0].id = "replacement-event-id".into();
        draw(&mut logs, 100, 12);

        assert!(logs.copy_menu.is_none());
        assert_eq!(
            logs.detail_snapshot_event_id.as_deref(),
            Some("replacement-event-id")
        );
        assert!(!logs.copy_detail());

        assert!(logs.begin_text_drag(detail_area.x, detail_area.y));
        assert!(logs.update_text_drag(detail_area.x + 5, detail_area.y));
        logs.finish_text_drag();
        assert!(logs.selected_text_target().is_none());
        assert!(!logs.copy_current_selection_or_row());

        let list_area = logs.list_snapshot.as_ref().expect("list snapshot").area;
        assert!(!logs.open_copy_menu(list_area.x, list_area.y));
        assert!(!logs.open_copy_menu(detail_area.x, detail_area.y));
    }
}
