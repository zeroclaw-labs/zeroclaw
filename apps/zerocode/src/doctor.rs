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
        } else if let Some(entry) = self.selected_entry() {
            detail_lines(entry)
        } else {
            vec![Line::from(Span::styled(
                crate::i18n::t("zc-doctor-no-selection"),
                theme::dim_style(),
            ))]
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
        crate::i18n::t("zc-doctor-error-unsupported-daemon")
    } else {
        error.to_string()
    }
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
}
