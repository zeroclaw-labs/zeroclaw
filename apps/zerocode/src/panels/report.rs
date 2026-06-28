//! Cost / usage report panel.
//!
//! A pull-based [`Panel`] that queries the daemon's cost engine
//! (`cost/query`) and renders a period breakdown plus per-model and
//! per-agent rollups. This is the reference implementation of the panel
//! plugin API: it owns its own data, refresh cadence, and scroll state,
//! and touches no app internals beyond the shared [`RpcClient`].
//!
//! The daemon currently reports session / day / month totals
//! ([`CostSummaryResult`]); quarter and year-to-date rows are surfaced once
//! the cost RPC exposes them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::client::{CostSummaryResult, RpcClient};
use crate::panel::{Panel, PanelOutcome};
use crate::theme;
use crate::widgets::{HelpEntry, HelpNode};

/// Minimum gap between automatic refreshes driven by `tick`.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

pub struct ReportPanel {
    rpc: Arc<RpcClient>,
    cost: Option<CostSummaryResult>,
    error: Option<String>,
    scroll: u16,
    last_refresh: Option<Instant>,
}

impl ReportPanel {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            cost: None,
            error: None,
            scroll: 0,
            last_refresh: None,
        }
    }

    async fn refresh(&mut self) {
        match self.rpc.cost_query(None).await {
            Ok(c) => {
                self.cost = Some(c);
                self.error = None;
            }
            Err(e) => {
                let msg = e.to_string();
                // A daemon without a cost engine answers "method not found";
                // surface that as a friendly "not available" rather than a
                // raw RPC error.
                if msg.contains("-32601") || msg.to_lowercase().contains("method not found") {
                    self.error = Some(crate::i18n::t("zc-report-not-available"));
                } else {
                    self.error = Some(msg);
                }
            }
        }
        self.last_refresh = Some(Instant::now());
    }
}

#[async_trait::async_trait]
impl Panel for ReportPanel {
    fn id(&self) -> &'static str {
        "report"
    }

    fn title_key(&self) -> &'static str {
        "zc-pane-report"
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        self.refresh().await;
        Ok(())
    }

    async fn tick(&mut self) {
        let due = self
            .last_refresh
            .map(|t| t.elapsed() >= REFRESH_INTERVAL)
            .unwrap_or(true);
        if due {
            self.refresh().await;
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", crate::i18n::t("zc-pane-report")),
                theme::title_style(),
            ))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if let Some(ref err) = self.error {
            frame.render_widget(
                Paragraph::new(Span::styled(err.as_str(), theme::warn_style()))
                    .wrap(Wrap { trim: true }),
                inner,
            );
            return;
        }

        let Some(ref c) = self.cost else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-report-loading"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let mut lines = vec![
            Line::from(Span::styled(
                crate::i18n::t("zc-report-section-periods"),
                theme::heading_style(),
            )),
            kv(
                &crate::i18n::t("zc-report-period-session"),
                &format!("${:.4}", c.session_cost_usd),
            ),
            kv(
                &crate::i18n::t("zc-report-period-today"),
                &format!("${:.4}", c.daily_cost_usd),
            ),
            kv(
                &crate::i18n::t("zc-report-period-month"),
                &format!("${:.4}", c.monthly_cost_usd),
            ),
            Line::from(""),
            kv(
                &crate::i18n::t("zc-report-total-tokens"),
                &fmt_tokens(c.total_tokens),
            ),
            kv(
                &crate::i18n::t("zc-report-total-requests"),
                &c.request_count.to_string(),
            ),
        ];

        if !c.by_model.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-report-section-by-model"),
                theme::heading_style(),
            )));
            let mut models: Vec<_> = c.by_model.values().collect();
            models.sort_by(|a, b| {
                b.cost_usd
                    .partial_cmp(&a.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for m in models {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<36}", m.model), theme::body_style()),
                    Span::styled(format!("${:.4}", m.cost_usd), theme::accent_style()),
                    Span::styled(
                        format!(
                            "  {} reqs  {} tok",
                            m.request_count,
                            fmt_tokens(m.total_tokens)
                        ),
                        theme::dim_style(),
                    ),
                ]));
            }
        }

        if !c.by_agent.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-report-section-by-agent"),
                theme::heading_style(),
            )));
            let mut agents: Vec<_> = c.by_agent.values().collect();
            agents.sort_by(|a, b| {
                b.cost_usd
                    .partial_cmp(&a.cost_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for a in agents {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<20}", a.agent_alias), theme::body_style()),
                    Span::styled(format!("${:.4}", a.cost_usd), theme::accent_style()),
                    Span::styled(
                        format!(
                            "  {} reqs  {} tok",
                            a.request_count,
                            fmt_tokens(a.total_tokens)
                        ),
                        theme::dim_style(),
                    ),
                ]));
            }
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0));
        frame.render_widget(para, inner);
    }

    async fn handle_key(&mut self, key: KeyEvent) -> PanelOutcome {
        use crate::keymap::PanelAction;
        match PanelAction::from_chord(&key) {
            Some(PanelAction::Refresh) => self.refresh().await,
            Some(PanelAction::ScrollUp) => self.scroll = self.scroll.saturating_sub(1),
            Some(PanelAction::ScrollDown) => self.scroll = self.scroll.saturating_add(1),
            Some(PanelAction::PageUp) => self.scroll = self.scroll.saturating_sub(10),
            Some(PanelAction::PageDown) => self.scroll = self.scroll.saturating_add(10),
            Some(PanelAction::Top) => self.scroll = 0,
            None => {}
        }
        PanelOutcome::Continue
    }

    fn help_context(&self) -> HelpNode {
        HelpNode {
            title: Some(crate::i18n::t("zc-pane-report")),
            description: Some(crate::i18n::t("zc-report-help-desc")),
            entries: vec![
                HelpEntry::new(["r"], crate::i18n::t("zc-report-help-refresh")),
                HelpEntry::new(
                    ["↑", "↓", "k", "j"],
                    crate::i18n::t("zc-report-help-scroll"),
                ),
            ],
            children: vec![],
        }
    }
}

/// A right-padded `label   value` line, matching the dashboard's detail rows.
fn kv(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {label:<24}"), theme::dim_style()),
        Span::styled(value.to_string(), theme::body_style()),
    ])
}

/// Compact token formatting: 1234 -> "1.2k", 1_200_000 -> "1.2M".
fn fmt_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}
