//! Ephemeral presentation of the daemon ledger, bound to an exact save receipt.

use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::client::RpcClient;
use crate::i18n::{t, t_args};
use crate::keymap::{ConfigTabAction as Action, RebindableActions};
use crate::theme;
use crate::wire::{
    ConfigApplicationOutcome as Outcome, ConfigApplicationReason as Reason,
    ConfigApplicationRecord, ConfigApplicationStatus, ConfigApplicationTarget as Target,
    PublishedConfigRevision,
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct ApplicationFeedback {
    saved: Option<(String, Option<PublishedConfigRevision>)>,
    status: Option<ConfigApplicationStatus>,
    last_poll: Option<Instant>,
    poll: Option<JoinHandle<()>>,
    receiver: Option<oneshot::Receiver<Option<ConfigApplicationStatus>>>,
    details_open: bool,
    scroll: u16,
    viewport: Cell<(u16, u16)>,
}

impl ApplicationFeedback {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record_saved(
        &mut self,
        label: String,
        revision: Option<PublishedConfigRevision>,
    ) {
        self.clear_snapshot();
        self.saved = Some((label, revision));
    }

    pub(crate) fn deactivate(&mut self) {
        self.clear_snapshot();
        self.details_open = false;
    }

    fn clear_snapshot(&mut self) {
        if let Some(poll) = self.poll.take() {
            poll.abort();
        }
        self.receiver = None;
        self.status = None;
        self.last_poll = None;
        self.scroll = 0;
    }

    pub(crate) fn tick(&mut self, rpc: &Arc<RpcClient>) {
        if let Some(receiver) = self.receiver.as_mut() {
            match receiver.try_recv() {
                Ok(status) => {
                    self.status = status;
                    self.receiver = None;
                    self.poll = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.status = None;
                    self.receiver = None;
                    self.poll = None;
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.receiver.is_none()
            && self
                .last_poll
                .is_none_or(|time| time.elapsed() >= POLL_INTERVAL)
        {
            self.last_poll = Some(Instant::now());
            let (sender, receiver) = oneshot::channel();
            self.receiver = Some(receiver);
            let rpc = Arc::clone(rpc);
            self.poll = Some(tokio::spawn(async move {
                let status = rpc
                    .config_status()
                    .await
                    .ok()
                    .and_then(|result| result.application);
                let _ = sender.send(status);
            }));
        }
    }

    fn visible_records(&self) -> Vec<&ConfigApplicationRecord> {
        let Some(status) = self.status.as_ref() else {
            return Vec::new();
        };
        match self.saved.as_ref() {
            None => status.records.iter().collect(),
            Some((_, Some(revision)))
                if status.published_revision.epoch == revision.epoch
                    && status.published_revision.sequence >= revision.sequence =>
            {
                status
                    .records
                    .iter()
                    .filter(|record| record.revision == *revision)
                    .collect()
            }
            Some(_) => Vec::new(),
        }
    }

    fn missing_daemon_paths<'a>(records: &[&'a ConfigApplicationRecord]) -> Vec<&'a Vec<String>> {
        let mut missing = Vec::new();
        for record in records {
            if matches!(record.target, Target::Session { .. })
                && !records
                    .iter()
                    .any(|other| other.path == record.path && other.target == Target::Daemon)
                && !missing.contains(&&record.path)
            {
                missing.push(&record.path);
            }
        }
        missing
    }

    pub(crate) fn summary(&self) -> String {
        let Some((label, _)) = self.saved.as_ref() else {
            return t_args(
                "zc-config-application-no-save",
                &[("key", &key_label(Action::ApplicationStatus))],
            );
        };
        let mut parts = vec![t_args(
            "zc-config-application-saved",
            &[("label", &quoted(label))],
        )];
        let records = self.visible_records();
        if records.is_empty() {
            parts.push(t("zc-config-application-unavailable"));
        } else {
            let mut counts = [0usize; 5];
            for record in &records {
                counts[outcome_index(record)] += 1;
            }
            counts[4] += Self::missing_daemon_paths(&records).len();
            for (index, count) in counts.into_iter().enumerate() {
                if count > 0 {
                    parts.push(format!("{}: {count}", outcome_label(index)));
                }
            }
        }
        parts.push(t_args(
            "zc-config-application-details-hint",
            &[("key", &key_label(Action::ApplicationStatus))],
        ));
        parts.join("; ")
    }

    pub(crate) fn toggle_details(&mut self) {
        self.details_open = !self.details_open;
        self.scroll = 0;
    }

    pub(crate) fn details_open(&self) -> bool {
        self.details_open
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if !self.details_open {
            return false;
        }
        let (width, height) = self.viewport.get();
        let rows = Paragraph::new(self.detail_lines())
            .wrap(Wrap { trim: false })
            .line_count(width.max(1));
        let max_scroll = rows
            .saturating_sub(usize::from(height))
            .min(usize::from(u16::MAX)) as u16;
        let page = height.max(1);
        match Action::from_chord(&key) {
            Some(Action::Back | Action::ApplicationStatus) => self.details_open = false,
            Some(Action::Up) => self.scroll = self.scroll.saturating_sub(1),
            Some(Action::Down) => self.scroll = self.scroll.saturating_add(1).min(max_scroll),
            Some(Action::PageUp) => self.scroll = self.scroll.saturating_sub(page),
            Some(Action::PageDown) => {
                self.scroll = self.scroll.saturating_add(page).min(max_scroll)
            }
            Some(Action::JumpStart) => self.scroll = 0,
            Some(Action::JumpEnd) => self.scroll = max_scroll,
            _ => {}
        }
        true
    }

    fn detail_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(if self.saved.is_some() {
                self.summary()
            } else {
                t("zc-config-application-observed")
            }),
            Line::from(t_args(
                "zc-config-application-help",
                &[
                    ("up", &key_label(Action::Up)),
                    ("down", &key_label(Action::Down)),
                    ("back", &key_label(Action::Back)),
                    ("details", &key_label(Action::ApplicationStatus)),
                    ("pageup", &key_label(Action::PageUp)),
                    ("pagedown", &key_label(Action::PageDown)),
                ],
            )),
        ];
        if let Some((_, Some(revision))) = self.saved.as_ref() {
            lines.push(revision_line("zc-config-application-receipt", revision));
        }
        if let Some(status) = self.status.as_ref() {
            lines.push(revision_line(
                "zc-config-application-published",
                &status.published_revision,
            ));
            lines.push(Line::from(t_args(
                "zc-config-application-coverage",
                &[("limit", &status.record_limit.to_string())],
            )));
            if status.truncated {
                lines.push(Line::from(t("zc-config-application-truncated")));
            }
        }
        let records = self.visible_records();
        if records.is_empty() {
            lines.push(Line::from(t("zc-config-application-unavailable")));
        }
        for record in &records {
            let target = match &record.target {
                Target::Daemon => t("zc-config-application-daemon"),
                Target::Session { id, generation } => t_args(
                    "zc-config-application-session",
                    &[("id", &quoted(id)), ("generation", &generation.to_string())],
                ),
                Target::Unknown => t("zc-config-application-target-unknown"),
            };
            lines.push(Line::from(
                serde_json::to_string(&record.path).unwrap_or_default(),
            ));
            if record.path.is_empty() {
                lines.push(Line::from(t("zc-config-application-whole-config")));
            }
            lines.push(Line::from(format!(
                "  {target}: {}",
                outcome_label(outcome_index(record))
            )));
            lines.push(Line::from(format!("  {}", revision_text(&record.revision))));
            if let Some(reason) = record.reason {
                lines.push(Line::from(format!("  {}", reason_label(reason))));
            }
        }
        for path in Self::missing_daemon_paths(&records) {
            lines.push(Line::from(serde_json::to_string(path).unwrap_or_default()));
            lines.push(Line::from(t("zc-config-application-daemon-unavailable")));
        }
        lines
    }

    pub(crate) fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let block = theme::panel_block(&t("zc-config-application-title"));
        let inner = block.inner(area);
        self.viewport.set((inner.width, inner.height));
        let paragraph = Paragraph::new(self.detail_lines()).wrap(Wrap { trim: false });
        let max_scroll = paragraph
            .line_count(inner.width.max(1))
            .saturating_sub(usize::from(inner.height));
        let scroll = usize::from(self.scroll).min(max_scroll) as u16;
        frame.render_widget(
            paragraph
                .style(theme::body_style())
                .block(block)
                .scroll((scroll, 0)),
            area,
        );
    }
}

impl Drop for ApplicationFeedback {
    fn drop(&mut self) {
        if let Some(poll) = self.poll.take() {
            poll.abort();
        }
    }
}

fn quoted(value: &str) -> String {
    // Serializing strings and component arrays is infallible for these types.
    serde_json::to_string(value).unwrap_or_default()
}

fn key_label(action: Action) -> String {
    action
        .resolved()
        .iter()
        .map(crate::keymap::Chord::display)
        .collect::<Vec<_>>()
        .join("/")
}

fn revision_text(revision: &PublishedConfigRevision) -> String {
    format!("{} / {}", quoted(&revision.epoch), revision.sequence)
}

fn revision_line(key: &str, revision: &PublishedConfigRevision) -> Line<'static> {
    Line::from(t_args(key, &[("revision", &revision_text(revision))]))
}

fn outcome_index(record: &ConfigApplicationRecord) -> usize {
    if record.target == Target::Daemon
        && record.outcome == Outcome::QueuedForReload
        && record.reason != Some(Reason::Unknown)
    {
        return 3;
    }
    if record.path.is_empty()
        || record.target == Target::Unknown
        || matches!(
            record.reason,
            Some(Reason::Unknown | Reason::ChangeScopeUnavailable)
        )
    {
        return 4;
    }
    match record.outcome {
        Outcome::AppliedLive => 0,
        Outcome::Pending => 1,
        Outcome::Rejected => 2,
        Outcome::QueuedForReload => 3,
        Outcome::Unknown => 4,
    }
}

fn outcome_label(index: usize) -> String {
    t(match index {
        0 => "zc-config-application-applied",
        1 => "zc-config-application-pending",
        2 => "zc-config-application-rejected",
        3 => "zc-config-application-reload",
        _ => "zc-config-application-unavailable",
    })
}

fn reason_label(reason: Reason) -> String {
    t(match reason {
        Reason::AwaitingAcknowledgement => "zc-config-application-awaiting",
        Reason::DaemonReloadRequired => "zc-config-application-reload",
        Reason::ChangeScopeUnavailable => "zc-config-application-scope-unavailable",
        Reason::TargetRetired => "zc-config-application-retired",
        Reason::Unknown => "zc-config-application-unavailable",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{JsonRpcError, RpcOutbound};
    use crossterm::event::{KeyCode, KeyModifiers};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    struct Peer {
        client: Arc<RpcClient>,
        outbound: Arc<RpcOutbound>,
        requests: mpsc::Receiver<String>,
    }

    impl Peer {
        fn new() -> Self {
            let (sender, requests) = mpsc::channel(16);
            let outbound = Arc::new(RpcOutbound::new(sender));
            let client = Arc::new(RpcClient::with_rpc(Arc::clone(&outbound)));
            Self {
                client,
                outbound,
                requests,
            }
        }

        async fn request(&mut self, method: &str) -> String {
            let raw = self.requests.recv().await.unwrap();
            let request: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(request["method"], method);
            request["id"].as_str().unwrap().to_owned()
        }

        async fn save(&mut self, feedback: &mut ApplicationFeedback, sequence: Option<u64>) {
            let client = Arc::clone(&self.client);
            let call =
                tokio::spawn(
                    async move { client.config_set("agents.a.b.model", json!("x")).await },
                );
            let id = self.request("config/set").await;
            let result = sequence.map_or(
                json!({}),
                |sequence| json!({"revision": revision(sequence)}),
            );
            self.outbound.dispatch_response(&id, Some(result), None);
            feedback.record_saved("agents.a.b.model".into(), call.await.unwrap().unwrap());
        }

        async fn status(&mut self, feedback: &mut ApplicationFeedback, result: Value) {
            feedback.last_poll = None;
            feedback.tick(&self.client);
            let id = self.request("config/status").await;
            self.outbound.dispatch_response(&id, Some(result), None);
            self.finish(feedback).await;
        }

        async fn finish(&self, feedback: &mut ApplicationFeedback) {
            for _ in 0..100 {
                tokio::task::yield_now().await;
                feedback.tick(&self.client);
                if feedback.receiver.is_none() {
                    return;
                }
            }
            panic!("scripted status response did not settle");
        }
    }

    fn revision(sequence: u64) -> Value {
        json!({"epoch": "epoch", "sequence": sequence})
    }

    fn record(sequence: u64, target: Value, outcome: &str) -> Value {
        json!({"path": ["agents", "a.b", "model"], "revision": revision(sequence),
            "target": target, "outcome": outcome})
    }

    fn session(generation: u64) -> Value {
        json!({"kind": "session", "id": "s\n1", "generation": generation})
    }

    fn snapshot(sequence: u64, records: Vec<Value>) -> Value {
        json!({"application": {"published_revision": revision(sequence), "records": records,
            "record_limit": 512, "truncated": false}})
    }

    fn details(feedback: &ApplicationFeedback) -> String {
        feedback
            .detail_lines()
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn exact_save_rejects_later_writer_but_retains_unrelated_revision() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        peer.save(&mut feedback, Some(7)).await;
        peer.status(
            &mut feedback,
            snapshot(8, vec![record(8, session(1), "applied_live")]),
        )
        .await;
        assert!(feedback.summary().contains("Saved"));
        assert!(feedback.summary().contains("unavailable"));
        assert!(!feedback.summary().contains("Applied live"));
        peer.status(
            &mut feedback,
            snapshot(
                8,
                vec![
                    record(7, session(1), "applied_live"),
                    record(7, json!({"kind": "daemon"}), "queued_for_reload"),
                ],
            ),
        )
        .await;
        assert!(feedback.summary().contains("Applied live: 1"));
        assert!(feedback.summary().contains("Reload required: 1"));
    }

    #[tokio::test]
    async fn daemon_coverage_is_separate_from_sessions_and_dotted_paths_are_quoted() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        peer.save(&mut feedback, Some(7)).await;
        peer.status(
            &mut feedback,
            snapshot(
                7,
                vec![
                    record(7, session(42), "applied_live"),
                    record(6, json!({"kind": "daemon"}), "queued_for_reload"),
                ],
            ),
        )
        .await;
        assert!(feedback.summary().contains("Applied live: 1"));
        assert!(feedback.summary().contains("unavailable: 1"));
        assert!(!feedback.summary().contains("Reload required"));
        let text = details(&feedback);
        assert!(text.contains("[\"agents\",\"a.b\",\"model\"]"));
        assert!(text.contains("\"s\\n1\""));
        assert!(!text.contains("s\n1"));
        assert!(text.contains("generation 42"));
        assert!(text.contains("Daemon: application status unavailable"));
        peer.status(
            &mut feedback,
            snapshot(
                7,
                vec![
                    record(7, session(42), "rejected"),
                    record(7, session(43), "pending"),
                    record(7, json!({"kind": "daemon"}), "queued_for_reload"),
                ],
            ),
        )
        .await;
        let text = details(&feedback);
        assert!(text.contains("generation 42: Rejected"));
        assert!(text.contains("generation 43: Pending"));
        assert!(text.contains("Daemon: Reload required"));
    }

    #[tokio::test]
    async fn pending_settles_and_failed_unsupported_or_replaced_reads_clear_success() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        peer.save(&mut feedback, Some(7)).await;
        peer.status(
            &mut feedback,
            snapshot(7, vec![record(7, session(1), "pending")]),
        )
        .await;
        assert!(feedback.summary().contains("Pending: 1"));
        peer.status(
            &mut feedback,
            snapshot(7, vec![record(7, session(1), "applied_live")]),
        )
        .await;
        assert!(feedback.summary().contains("Applied live: 1"));
        feedback.last_poll = None;
        feedback.tick(&peer.client);
        let id = peer.request("config/status").await;
        peer.outbound.dispatch_response(
            &id,
            None,
            Some(JsonRpcError {
                code: -32601,
                message: "private provider failure".into(),
                data: None,
            }),
        );
        peer.finish(&mut feedback).await;
        assert!(feedback.status.is_none());
        assert!(!details(&feedback).contains("private provider failure"));
        for value in [
            json!({}),
            json!({"application": null}),
            json!({"application": "unsupported"}),
        ] {
            peer.status(&mut feedback, value).await;
            assert!(!feedback.summary().contains("Applied live"));
        }
        let mut replacement = snapshot(7, vec![record(7, session(1), "applied_live")]);
        replacement["application"]["published_revision"]["epoch"] = json!("replacement");
        peer.status(&mut feedback, replacement).await;
        assert!(!feedback.summary().contains("Applied live"));
        peer.save(&mut feedback, None).await;
        peer.status(
            &mut feedback,
            snapshot(7, vec![record(7, session(1), "applied_live")]),
        )
        .await;
        assert!(feedback.summary().contains("unavailable"));
        assert!(!feedback.summary().contains("Applied live"));
    }

    #[tokio::test]
    async fn one_inflight_poll_and_new_save_discard_old_response() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        peer.save(&mut feedback, Some(7)).await;
        feedback.tick(&peer.client);
        let old_id = peer.request("config/status").await;
        feedback.tick(&peer.client);
        assert!(peer.requests.try_recv().is_err());
        peer.save(&mut feedback, Some(8)).await;
        peer.outbound.dispatch_response(
            &old_id,
            Some(snapshot(7, vec![record(7, session(1), "applied_live")])),
            None,
        );
        assert!(feedback.status.is_none());
        assert!(!feedback.summary().contains("Applied live"));
        peer.status(
            &mut feedback,
            snapshot(8, vec![record(8, session(1), "pending")]),
        )
        .await;
        assert!(feedback.summary().contains("Pending: 1"));
        feedback.deactivate();
        assert!(feedback.summary().contains("Saved"));
        assert!(feedback.summary().contains("unavailable"));
        assert!(feedback.receiver.is_none());
    }

    #[tokio::test]
    async fn unknown_target_outcome_and_reason_never_report_applied() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        peer.save(&mut feedback, Some(7)).await;
        for field in ["target", "outcome", "reason"] {
            let mut row = record(7, json!({"kind": "daemon"}), "applied_live");
            row[field] = if field == "target" {
                json!({"kind": "future_target"})
            } else {
                json!("future_private_detail")
            };
            peer.status(&mut feedback, snapshot(7, vec![row])).await;
            assert!(feedback.summary().contains("unavailable"));
            assert!(!feedback.summary().contains("Applied live"));
            assert!(!details(&feedback).contains("future_private_detail"));
        }
        let mut whole_config = record(7, json!({"kind": "daemon"}), "queued_for_reload");
        whole_config["path"] = json!([]);
        whole_config["reason"] = json!("change_scope_unavailable");
        peer.status(&mut feedback, snapshot(7, vec![whole_config]))
            .await;
        assert!(feedback.summary().contains("Reload required: 1"));
        assert!(!feedback.summary().contains("Applied live"));
        assert!(details(&feedback).contains("Whole configuration"));
    }

    #[tokio::test]
    async fn observed_details_scroll_and_consume_editor_keys() {
        let mut peer = Peer::new();
        let mut feedback = ApplicationFeedback::new();
        let mut value = snapshot(
            7,
            (0..20)
                .map(|generation| record(7, session(generation), "applied_live"))
                .collect(),
        );
        value["application"]["truncated"] = json!(true);
        peer.status(&mut feedback, value).await;
        assert!(!feedback.summary().contains("Applied live"));
        assert!(details(&feedback).contains("Observed application status"));
        assert!(details(&feedback).contains("Record limit: 512"));
        assert!(details(&feedback).contains("evicted records"));
        feedback.toggle_details();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|frame| feedback.draw_details(frame, frame.area()))
            .unwrap();
        for code in [KeyCode::Enter, KeyCode::Char('d'), KeyCode::Delete] {
            assert!(feedback.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
            assert!(feedback.details_open());
        }
        assert!(feedback.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)));
        assert!(feedback.scroll > 0);
        assert!(feedback.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!feedback.details_open());
        assert!(!feedback.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }
}
