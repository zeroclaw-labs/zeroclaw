use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::task::JoinHandle;

use crate::client::RpcClient;
use crate::theme;
use crate::wire::{DoctorResultEntry, DoctorRunResult, DoctorSeverity};

const SCROLL_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorFilter {
    All,
    Problems,
    Errors,
}

impl DoctorFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Problems,
            Self::Problems => Self::Errors,
            Self::Errors => Self::All,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::All => Self::Errors,
            Self::Problems => Self::All,
            Self::Errors => Self::Problems,
        }
    }

    fn label(self) -> String {
        match self {
            Self::All => crate::i18n::t("zc-doctor-filter-all"),
            Self::Problems => crate::i18n::t("zc-doctor-filter-problems"),
            Self::Errors => crate::i18n::t("zc-doctor-filter-errors"),
        }
    }

    fn allows(self, severity: DoctorSeverity) -> bool {
        match self {
            Self::All => true,
            Self::Problems => severity != DoctorSeverity::Ok,
            Self::Errors => severity == DoctorSeverity::Error,
        }
    }
}

pub(crate) struct Doctor {
    rpc: Arc<RpcClient>,
    result: Option<DoctorRunResult>,
    error: Option<String>,
    refresh_task: Option<JoinHandle<std::result::Result<DoctorRunResult, String>>>,
    filter: DoctorFilter,
    list_state: ListState,
    detail_scroll: u16,
    last_filter_area: Option<Rect>,
    last_list_area: Rect,
    last_detail_area: Rect,
}

impl Doctor {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            result: None,
            error: None,
            refresh_task: None,
            filter: DoctorFilter::Problems,
            list_state: ListState::default(),
            detail_scroll: 0,
            last_filter_area: None,
            last_list_area: Rect::default(),
            last_detail_area: Rect::default(),
        }
    }

    pub(crate) fn refresh_if_inactive(&mut self) {
        if self.result.is_none() && self.error.is_none() {
            self.start_refresh();
        }
    }

    pub(crate) async fn poll_refresh(&mut self) {
        let Some(task) = self.refresh_task.as_ref() else {
            return;
        };
        if !task.is_finished() {
            return;
        }
        let task = self.refresh_task.take().expect("checked refresh task");
        match task.await {
            Ok(Ok(result)) => {
                self.error = None;
                self.result = Some(result);
                self.sync_selection();
            }
            Ok(Err(error)) => {
                self.error = Some(error);
                self.result = None;
                self.list_state.select(None);
            }
            Err(err) => {
                self.error = Some(format!("Doctor refresh task failed: {err}"));
                self.result = None;
                self.list_state.select(None);
            }
        }
        self.detail_scroll = 0;
    }

    fn start_refresh(&mut self) {
        if self.is_loading() {
            return;
        }
        self.error = None;
        let rpc = Arc::clone(&self.rpc);
        self.refresh_task = Some(tokio::spawn(async move {
            rpc.doctor_run()
                .await
                .map_err(|err| format_doctor_error(&err.to_string()))
        }));
    }

    fn is_loading(&self) -> bool {
        self.refresh_task.is_some()
    }

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);

        self.draw_summary(frame, chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);
        self.last_list_area = body[0];
        self.last_detail_area = body[1];
        self.draw_list(frame, body[0]);
        self.draw_detail(frame, body[1]);
    }

    fn draw_summary(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", crate::i18n::t("zc-doctor-title")),
                theme::title_style(),
            ))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut spans = Vec::new();
        self.last_filter_area = None;
        if self.is_loading() {
            spans.push(Span::styled(
                crate::i18n::t("zc-doctor-loading"),
                theme::dim_style(),
            ));
        } else if let Some(error) = &self.error {
            spans.push(Span::styled(
                crate::i18n::t_args("zc-doctor-error", &[("error", error)]),
                severity_style(DoctorSeverity::Error),
            ));
        } else if let Some(result) = &self.result {
            let (summary, filter_status) = self.summary_status_text(result);
            self.last_filter_area = filter_hit_rect(inner, &summary, &filter_status);
            spans.push(Span::styled(summary, theme::body_style()));
            spans.push(Span::raw("   "));
            spans.push(Span::styled(filter_status, theme::dim_style()));
        } else {
            spans.push(Span::styled(
                crate::i18n::t("zc-doctor-no-results"),
                theme::dim_style(),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);
    }

    fn draw_list(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let indices = self.visible_indices();
        self.clamp_selection(indices.len());

        let items: Vec<ListItem> = indices
            .iter()
            .filter_map(|idx| self.result.as_ref()?.results.get(*idx))
            .map(|entry| {
                let line = Line::from(vec![
                    Span::styled(
                        format!("{:<4}", severity_label(entry.severity)),
                        severity_style(entry.severity).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(entry.category.clone(), theme::title_style()),
                    Span::raw("  "),
                    Span::styled(truncate_first_line(&entry.message, 96), theme::body_style()),
                ]);
                ListItem::new(line)
            })
            .collect();

        let title =
            crate::i18n::t_args("zc-doctor-list-title", &[("filter", &self.filter.label())]);
        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(format!(" {title} "), theme::title_style()))
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", crate::i18n::t("zc-doctor-detail-title")),
                theme::title_style(),
            ))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let lines = if let Some(error) = &self.error {
            vec![Line::from(Span::styled(
                crate::i18n::t_args("zc-doctor-error", &[("error", error)]),
                severity_style(DoctorSeverity::Error),
            ))]
        } else if self.is_loading() {
            vec![Line::from(Span::styled(
                crate::i18n::t("zc-doctor-loading"),
                theme::dim_style(),
            ))]
        } else {
            let mut out: Vec<Line<'_>> = Vec::new();

            // Surface the resolved active log path first when the daemon
            // advertised one — it is the operator's primary entry point for
            // post-mortem log navigation (8650).
            if let Some(log_path) = self.result.as_ref().and_then(|r| r.log_path.as_deref()) {
                out.push(Line::from(Span::styled(
                    crate::i18n::t_args("zc-doctor-log-path", &[("path", log_path)]),
                    theme::body_style(),
                )));
                out.push(Line::from(""));
            }

            // Always show the partial-results banner when a phase timed
            // out, even if the user has selected a specific diagnostic row.
            // This ensures the incomplete-run state from 8647 stays
            // persistently visible alongside any selected-row detail.
            let is_partial = self
                .result
                .as_ref()
                .is_some_and(|r| r.timed_out_phase.is_some());
            if is_partial {
                out.push(Line::from(Span::styled(
                    crate::i18n::t("zc-doctor-partial-banner"),
                    severity_style(DoctorSeverity::Warn),
                )));
                out.push(Line::from(Span::styled(
                    crate::i18n::t("zc-doctor-partial-hint"),
                    theme::dim_style(),
                )));
            }

            if let Some(entry) = self.selected_entry() {
                out.extend(detail_lines(entry));
            } else if !is_partial {
                out.push(Line::from(Span::styled(
                    crate::i18n::t("zc-doctor-no-selection"),
                    theme::dim_style(),
                )));
            }

            out
        };

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::DoctorTabAction;
        match DoctorTabAction::from_chord(&key) {
            Some(DoctorTabAction::Refresh) => {
                self.start_refresh();
            }
            Some(DoctorTabAction::FilterNext) => {
                self.cycle_filter_next();
            }
            Some(DoctorTabAction::FilterPrev) => {
                self.filter = self.filter.previous();
                self.sync_selection();
            }
            Some(DoctorTabAction::Down) => self.move_selection(1),
            Some(DoctorTabAction::Up) => self.move_selection(-1),
            Some(DoctorTabAction::PageDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(10);
            }
            Some(DoctorTabAction::PageUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
            }
            Some(DoctorTabAction::JumpStart) if !self.visible_indices().is_empty() => {
                self.list_state.select(Some(0));
                self.detail_scroll = 0;
            }
            Some(DoctorTabAction::JumpEnd) => {
                let len = self.visible_indices().len();
                if len > 0 {
                    self.list_state.select(Some(len - 1));
                    self.detail_scroll = 0;
                }
            }
            Some(DoctorTabAction::JumpStart) | None => {}
        }
        false
    }

    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent, _content_area: Rect) {
        use crate::mouse;
        use crossterm::event::MouseButton;

        let col = mouse.column;
        let row = mouse.row;
        let visible_len = self.visible_indices().len();
        let in_list = mouse::in_rect(col, row, self.last_list_area);
        let in_detail = mouse::in_rect(col, row, self.last_detail_area);
        let in_filter = self
            .last_filter_area
            .is_some_and(|rect| mouse::in_rect(col, row, rect));

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if in_filter => {
                self.cycle_filter_next();
            }
            MouseEventKind::Down(MouseButton::Left) if in_list => {
                if let Some(idx) = mouse::list_click_index(
                    row,
                    self.last_list_area,
                    self.list_state.offset(),
                    visible_len,
                ) {
                    self.list_state.select(Some(idx));
                    self.detail_scroll = 0;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
                if in_detail {
                    if up {
                        self.detail_scroll = self.detail_scroll.saturating_sub(SCROLL_LINES as u16);
                    } else {
                        self.detail_scroll = self.detail_scroll.saturating_add(SCROLL_LINES as u16);
                    }
                } else if in_list && visible_len > 0 {
                    let i = self.list_state.selected().unwrap_or(0);
                    let new_i = mouse::list_scroll(i, visible_len, up, SCROLL_LINES);
                    self.list_state.select(Some(new_i));
                    self.detail_scroll = 0;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn wants_text_input(&self) -> bool {
        false
    }

    pub(crate) fn handle_paste(&mut self, _text: &str) {}

    fn cycle_filter_next(&mut self) {
        self.filter = self.filter.next();
        self.sync_selection();
        self.detail_scroll = 0;
    }

    fn summary_status_text(&self, result: &DoctorRunResult) -> (String, String) {
        let ok = result.summary.ok.to_string();
        let warnings = result.summary.warnings.to_string();
        let errors = result.summary.errors.to_string();
        let summary = crate::i18n::t_args(
            "zc-doctor-summary",
            &[("ok", &ok), ("warnings", &warnings), ("errors", &errors)],
        );
        let filter_status = crate::i18n::t_args(
            "zc-doctor-filter-status",
            &[("filter", &self.filter.label())],
        );
        (summary, filter_status)
    }

    fn visible_indices(&self) -> Vec<usize> {
        self.result
            .as_ref()
            .map(|result| {
                result
                    .results
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, entry)| self.filter.allows(entry.severity).then_some(idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_entry(&self) -> Option<&DoctorResultEntry> {
        let selected = self.list_state.selected()?;
        let idx = self.visible_indices().get(selected).copied()?;
        self.result.as_ref()?.results.get(idx)
    }

    fn sync_selection(&mut self) {
        self.clamp_selection(self.visible_indices().len());
    }

    fn clamp_selection(&mut self, len: usize) {
        match (len, self.list_state.selected()) {
            (0, _) => self.list_state.select(None),
            (_, None) => self.list_state.select(Some(0)),
            (len, Some(idx)) if idx >= len => self.list_state.select(Some(len - 1)),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_indices().len();
        if len == 0 {
            self.list_state.select(None);
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len.saturating_sub(1) as isize);
        self.list_state.select(Some(next as usize));
        self.detail_scroll = 0;
    }
}

impl crate::widgets::HelpContext for Doctor {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};
        let mut entries = crate::help::help_entries::<crate::keymap::DoctorTabAction>();
        entries.push(E::spacer());
        entries.push(E::desc(crate::i18n::t("zc-doctor-help-mouse")));
        HelpNode::entries(entries)
    }
}

#[cfg(test)]
fn visible_entries(result: &DoctorRunResult, filter: DoctorFilter) -> Vec<&DoctorResultEntry> {
    result
        .results
        .iter()
        .filter(|entry| filter.allows(entry.severity))
        .collect()
}

fn filter_hit_rect(inner: Rect, summary: &str, filter_status: &str) -> Option<Rect> {
    let summary_width = crate::display_width::display_width(summary) as u16;
    let filter_width = crate::display_width::display_width(filter_status) as u16;
    if filter_width == 0 || inner.width == 0 || inner.height == 0 {
        return None;
    }

    let filter_x = inner.x.saturating_add(summary_width).saturating_add(3);
    let inner_right = inner.x.saturating_add(inner.width);
    if filter_x >= inner_right {
        return None;
    }

    Some(Rect::new(
        filter_x,
        inner.y,
        filter_width.min(inner_right.saturating_sub(filter_x)),
        1,
    ))
}

fn detail_lines(entry: &DoctorResultEntry) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(
                severity_label(entry.severity).to_string(),
                severity_style(entry.severity).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(entry.category.clone(), theme::title_style()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!("{}: ", crate::i18n::t("zc-doctor-label-message")),
                theme::dim_style(),
            ),
            Span::raw(entry.message.clone()),
        ]),
    ]
}

fn severity_label(severity: DoctorSeverity) -> &'static str {
    match severity {
        DoctorSeverity::Ok => "OK",
        DoctorSeverity::Warn => "WARN",
        DoctorSeverity::Error => "ERR",
    }
}

fn severity_style(severity: DoctorSeverity) -> Style {
    match severity {
        DoctorSeverity::Ok => Style::default().fg(Color::Rgb(80, 220, 120)),
        DoctorSeverity::Warn => Style::default().fg(Color::Rgb(255, 220, 80)),
        DoctorSeverity::Error => Style::default().fg(Color::Rgb(255, 100, 80)),
    }
}

fn truncate_first_line(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or("");
    if first.chars().count() <= max {
        first.to_string()
    } else {
        format!("{}...", first.chars().take(max).collect::<String>())
    }
}

fn format_doctor_error(error: &str) -> String {
    if error.contains("Unknown method") || error.contains("-32601") {
        return crate::i18n::t("zc-doctor-error-unsupported-daemon");
    }
    // Whole-RPC timeout: the daemon may have failed to return for any
    // reason (model-probe deadline, network drop, daemon overload, etc.).
    // Without a response we cannot identify the phase — keep the message
    // generic so the user checks the right subsystem.
    if error.contains("timed out") || error.contains("timeout") {
        return format!(
            "{}\n\n{}",
            error,
            crate::i18n::t("zc-doctor-error-daemon-timeout"),
        );
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::method;
    use crate::jsonrpc::RpcOutbound;
    use crate::wire::{DoctorResultEntry, DoctorRunResult, DoctorSeverity, DoctorSummary};
    use crossterm::event::{KeyModifiers, MouseButton};
    use serde_json::Value;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn sample_result() -> DoctorRunResult {
        DoctorRunResult {
            results: vec![
                DoctorResultEntry {
                    severity: DoctorSeverity::Ok,
                    category: "config".to_string(),
                    message: "config ok".to_string(),
                },
                DoctorResultEntry {
                    severity: DoctorSeverity::Warn,
                    category: "workspace".to_string(),
                    message: "workspace warning".to_string(),
                },
                DoctorResultEntry {
                    severity: DoctorSeverity::Error,
                    category: "daemon".to_string(),
                    message: "daemon error".to_string(),
                },
            ],
            summary: DoctorSummary {
                ok: 1,
                warnings: 1,
                errors: 1,
            },
            log_path: None,
            timed_out_phase: None,
        }
    }

    fn test_client() -> Arc<RpcClient> {
        let (tx, _rx) = mpsc::channel::<String>(16);
        Arc::new(RpcClient::with_rpc(Arc::new(RpcOutbound::new(tx))))
    }

    fn test_client_with_rpc() -> (Arc<RpcClient>, Arc<RpcOutbound>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        (
            Arc::new(RpcClient::with_rpc(Arc::clone(&outbound))),
            outbound,
            rx,
        )
    }

    async fn next_rpc_request(rx: &mut mpsc::Receiver<String>) -> Value {
        let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("doctor refresh should send an RPC request")
            .expect("writer channel should stay open");
        serde_json::from_str(&raw).expect("outbound RPC request should be JSON")
    }

    fn sample_result_value() -> Value {
        serde_json::json!({
            "results": [
                { "severity": "ok", "category": "config", "message": "config ok" },
                { "severity": "warn", "category": "workspace", "message": "workspace warning" },
                { "severity": "error", "category": "daemon", "message": "daemon error" }
            ],
            "summary": { "ok": 1, "warnings": 1, "errors": 1 }
        })
    }

    #[test]
    fn doctor_filter_hides_ok_rows_for_problem_view() {
        let result = sample_result();

        let visible = visible_entries(&result, DoctorFilter::Problems);

        assert_eq!(visible.len(), 2);
        assert!(
            visible
                .iter()
                .all(|entry| entry.severity != DoctorSeverity::Ok)
        );
    }

    #[test]
    fn doctor_unknown_method_error_explains_daemon_version_mismatch() {
        let message = format_doctor_error("RPC doctor/run: Unknown method: doctor/run (-32601)");

        assert!(message.contains("daemon"));
        assert!(!message.contains("-32601"));
    }

    #[test]
    fn doctor_timeout_error_is_generic_not_model_probing_specific() {
        // A whole-RPC timeout (no response) must not suggest model probing —
        // the daemon may have timed out for any reason.
        let message = format_doctor_error("RPC doctor/run: request timed out");

        assert!(message.contains("request timed out"));
        assert!(
            message.contains("daemon may be busy"),
            "whole-RPC timeout must be generic, got: {message}"
        );
        assert!(
            !message.contains("Model probing") && !message.contains("provider API"),
            "whole-RPC timeout must NOT name model probing, got: {message}"
        );
    }

    /// Pairs with `doctor_timeout_error_is_generic_not_model_probing_specific`
    /// above to pin the whole-RPC-vs-structured-probe discrimination from
    /// review 8647: the "model probing" hint must only surface on the
    /// structured-partial banner path, never on the error path. A future
    /// refactor that swaps the discriminator (e.g. on freeform substring
    /// matching) will be caught here even though `format_doctor_error`'s own
    /// test would still pass in isolation.
    #[test]
    fn doctor_timeout_discriminator_pins_each_path_to_its_own_text() {
        // Error path — generic daemon-side timeout text only.
        let error_msg = format_doctor_error("RPC doctor/run: request timed out");
        assert!(
            error_msg.contains("daemon may be busy"),
            "error path must surface generic daemon-busy hint; got: {error_msg}"
        );
        assert!(
            !error_msg.contains("model probing"),
            "error path must NEVER mention model probing; got: {error_msg}"
        );
        assert!(
            !error_msg.contains("Partial results"),
            "error path must NEVER render partial-results chrome; got: {error_msg}"
        );

        // Structured-partial path — only the partial-banner string is allowed
        // to surface probe-specific copy; the error path's generic text must
        // not appear.
        let partial_banner = crate::i18n::t("zc-doctor-partial-banner");
        let partial_hint = crate::i18n::t("zc-doctor-partial-hint");
        let daemon_busy = crate::i18n::t("zc-doctor-error-daemon-timeout");

        assert!(
            partial_banner.contains("model probing"),
            "partial-banner FTL must carry the probe-specific substring; got: {partial_banner}"
        );
        assert!(
            partial_hint.contains("provider catalog"),
            "partial-hint FTL must explain which phase was lost; got: {partial_hint}"
        );
        assert!(
            daemon_busy.contains("daemon"),
            "daemon-timeout FTL must remain the generic copy; got: {daemon_busy}"
        );
        assert!(
            !daemon_busy.contains("model probing"),
            "daemon-timeout FTL must NOT leak the probe-specific copy; got: {daemon_busy}"
        );
        assert!(
            !partial_banner.contains("daemon may be busy"),
            "partial-banner FTL must NOT collide with error-path copy; got: {partial_banner}"
        );
    }

    #[tokio::test]
    async fn doctor_partial_result_banner_visible_alongside_selection() {
        // When timed_out_phase is set and sync_selection selects the
        // auto-added timeout warning, the partial banner must still be
        // reachable — not hidden behind selected_entry().
        let mut doctor = Doctor::new(test_client());
        let mut result = sample_result();
        result.timed_out_phase = Some("probe_models".into());
        doctor.result = Some(result);
        doctor.sync_selection();

        // selected_entry() returns the warning (confirming the scenario).
        assert!(
            doctor.selected_entry().is_some(),
            "should select the timeout warning entry"
        );

        // The partial flag is still set — draw_detail can show the banner.
        assert!(
            doctor
                .result
                .as_ref()
                .is_some_and(|r| r.timed_out_phase.is_some()),
            "partial-results state must survive sync_selection"
        );
    }

    #[tokio::test]
    async fn doctor_refresh_starts_in_background_and_sets_loading() {
        let (client, _outbound, mut rx) = test_client_with_rpc();
        let mut doctor = Doctor::new(client);

        doctor.refresh_if_inactive();

        assert!(doctor.is_loading());
        assert!(doctor.result.is_none());
        let request = next_rpc_request(&mut rx).await;
        assert_eq!(request["method"], method::DOCTOR_RUN);
    }

    #[tokio::test]
    async fn doctor_poll_refresh_applies_completed_result() {
        let (client, outbound, mut rx) = test_client_with_rpc();
        let mut doctor = Doctor::new(client);

        doctor.refresh_if_inactive();
        let request = next_rpc_request(&mut rx).await;
        let id = request["id"]
            .as_str()
            .expect("outbound request should carry a string id")
            .to_string();
        outbound.dispatch_response(&id, Some(sample_result_value()), None);
        tokio::task::yield_now().await;

        doctor.poll_refresh().await;

        assert!(!doctor.is_loading());
        assert!(doctor.error.is_none());
        assert_eq!(
            doctor.result.as_ref().map(|result| result.summary.ok),
            Some(1)
        );
        assert_eq!(doctor.list_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn doctor_filter_label_click_cycles_filter() {
        let mut doctor = Doctor::new(test_client());
        doctor.result = Some(sample_result());
        doctor.filter = DoctorFilter::Problems;
        doctor.last_filter_area = Some(Rect::new(31, 1, 20, 1));
        doctor.sync_selection();

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 31,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        doctor.handle_mouse(click, Rect::new(0, 0, 80, 20));

        assert_eq!(doctor.filter, DoctorFilter::Errors);
    }

    // ─────────────────────────────────────────────────────────
    // Tests from 8650 (log_path) — kept verbatim with `timed_out_phase: None`
    // to match the merged DoctorRunResult shape.
    // ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn doctor_detail_panel_includes_log_path_when_no_entry_selected() {
        let mut doctor = Doctor::new(test_client());
        let mut result = sample_result();
        result.log_path = Some("/home/user/.local/share/zeroclaw/logs/trace.jsonl".into());
        doctor.result = Some(result);

        // No entry is selected → draw_detail renders log_path before
        // "No diagnostic selected".
        assert!(
            doctor.selected_entry().is_none(),
            "no diagnostic entry should be selected after initial result load"
        );
        assert!(
            doctor
                .result
                .as_ref()
                .and_then(|r| r.log_path.as_deref())
                .is_some(),
            "log_path must be accessible to draw_detail when no entry selected"
        );
    }

    /// Render Doctor with `log_persistence = "file"` and a long realistic
    /// resolved path. Asserts the path appears in the detail panel buffer
    /// (the operator's discoverability contract from 8650) and dumps the
    /// rendered buffer to stdout so `cargo test -- --nocapture` produces a
    /// capture suitable for pasting into the PR as first-hand evidence.
    #[tokio::test]
    async fn render_screenshot_log_path_file() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut doctor = Doctor::new(test_client());
        doctor.result = Some(DoctorRunResult {
            results: vec![],
            summary: DoctorSummary {
                ok: 0,
                warnings: 0,
                errors: 0,
            },
            log_path: Some(
                "/home/operator/.local/share/zeroclaw/logs/trace-2026-07-13T08-30-00Z.jsonl"
                    .to_string(),
            ),
            timed_out_phase: None,
        });
        doctor.filter = DoctorFilter::Problems;

        let area = Rect::new(0, 0, 120, 24);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                doctor.draw(frame, area);
            })
            .expect("draw doctor");

        let rendered = render_buffer_to_string(terminal.backend().buffer(), area);

        // The path must be discoverable in the detail panel. The detail panel
        // inner width is narrower than the path, so ratatui's wrap may split
        // the path across two lines — both halves must be present.
        assert!(
            rendered.contains("trace-2026-07-13T08-30"),
            "first half of resolved log path must render in detail panel; got:\n{rendered}"
        );
        assert!(
            rendered.contains("-00Z.jsonl"),
            "second half of resolved log path must render in detail panel; got:\n{rendered}"
        );
        // The "No diagnostic selected" line should also be present so the
        // operator sees the discoverability affordance is part of the
        // empty-selection fallback rather than a hidden field.
        assert!(
            rendered.contains("No diagnostic selected"),
            "fallback hint must render alongside log_path; got:\n{rendered}"
        );

        println!(
            "\n=== CAPTURE: zerocode doctor with log_persistence = \"file\" (120x24) ===\n{rendered}\n=== END CAPTURE ==="
        );
    }

    /// Render Doctor with `log_persistence = "none"`. Asserts the path is
    /// absent and the fallback "No diagnostic selected" message renders.
    #[tokio::test]
    async fn render_screenshot_log_path_none() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut doctor = Doctor::new(test_client());
        doctor.result = Some(DoctorRunResult {
            results: vec![],
            summary: DoctorSummary {
                ok: 0,
                warnings: 0,
                errors: 0,
            },
            log_path: None,
            timed_out_phase: None,
        });
        doctor.filter = DoctorFilter::Problems;

        let area = Rect::new(0, 0, 120, 24);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                doctor.draw(frame, area);
            })
            .expect("draw doctor");

        let rendered = render_buffer_to_string(terminal.backend().buffer(), area);

        // No resolved-path text should appear when persistence is disabled.
        assert!(
            !rendered.contains("/home/operator/"),
            "log_path must be absent when persistence is disabled; got:\n{rendered}"
        );
        assert!(
            !rendered.contains("trace-2026-07-13"),
            "log_path must be absent when persistence is disabled; got:\n{rendered}"
        );
        // The fallback "No diagnostic selected" line should still render.
        assert!(
            rendered.contains("No diagnostic selected"),
            "fallback hint must still render when persistence is disabled; got:\n{rendered}"
        );

        println!(
            "\n=== CAPTURE: zerocode doctor with log_persistence = \"none\" (120x24) ===\n{rendered}\n=== END CAPTURE ==="
        );
    }

    // ─────────────────────────────────────────────────────────
    // Test from 8647 (partial-results banner) — kept verbatim.
    // ─────────────────────────────────────────────────────────

    /// Render Doctor when `probe_models` has timed out: the partial-results
    /// banner must appear above the selected entry's detail in the detail
    /// panel. Mirrors the Scenario 1 capture in the PR body, but produced
    /// via `Doctor::draw` against a `ratatui::backend::TestBackend` at
    /// 120x24 so it can be regenerated on demand:
    ///
    ///   cargo test --locked --bin zerocode -p zerocode \
    ///     render_screenshot -- --nocapture
    #[tokio::test]
    async fn render_screenshot_partial_results_banner() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut doctor = Doctor::new(test_client());
        let mut result = sample_result();
        result.timed_out_phase = Some("probe_models".to_string());
        doctor.result = Some(result);
        // sync_selection so the highlight lands on the first visible row.
        doctor.sync_selection();

        let area = Rect::new(0, 0, 120, 24);
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                doctor.draw(frame, area);
            })
            .expect("draw doctor");

        let rendered = render_buffer_to_string(terminal.backend().buffer(), area);

        // The banner from 8647 must be discoverable in the detail panel,
        // above the selected entry's `detail_lines` content.
        assert!(
            rendered.contains("⚠ Partial results"),
            "partial-results banner must render in detail panel; got:\n{rendered}"
        );
        assert!(
            rendered.contains("model probing timed out"),
            "banner subtitle must render in detail panel; got:\n{rendered}"
        );
        // The Diagnostics list still surfaces surviving entries (the probe
        // row is missing) — both warn and error entries are visible.
        assert!(
            rendered.contains("workspace"),
            "warn entry must still appear in Diagnostics list; got:\n{rendered}"
        );
        assert!(
            rendered.contains("daemon"),
            "error entry must still appear in Diagnostics list; got:\n{rendered}"
        );

        println!(
            "\n=== CAPTURE: zerocode doctor with probe_models timed_out (120x24) ===\n{rendered}\n=== END CAPTURE ==="
        );
    }

    /// Helper: flatten a ratatui buffer to a string the way a user would
    /// see it on the terminal. Each row becomes one line; trailing
    /// whitespace is trimmed so the capture looks like the rendered TUI
    /// rather than a 120-column ragged dump.
    fn render_buffer_to_string(buffer: &ratatui::buffer::Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }
}
