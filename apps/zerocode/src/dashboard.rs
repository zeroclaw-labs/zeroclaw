use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use std::sync::Arc;

use crate::client::{
    AgentStatusEntry, CostSummaryResult, CronJobEntry, CronRunEntry, CronSchedule,
    CronTriggerResult, MemoryEntryResult, MessageEntry, OrgCost, RpcClient, SessionEntry,
    StatusResult, TuiListEntry,
};
use crate::mouse;
use crate::theme;

// ── Constants ────────────────────────────────────────────────────

const POLL_INTERVAL_SECS: u64 = 5;

/// Page size for `session/messages` on detail-open. Pulls the
/// most-recent page only; the right-side detail pane shows the tail
/// of the conversation. Long sessions never load the full history.
const SESSION_MESSAGES_PAGE_SIZE: usize = 100;

struct CronTriggerUpdate {
    job_id: String,
    result: Result<CronTriggerResult, String>,
    jobs: Option<Vec<CronJobEntry>>,
    runs: Option<Result<Vec<CronRunEntry>, String>>,
}

struct AgentRenameState {
    from: String,
    buf: String,
}

#[derive(Clone, Copy)]
enum DashboardMessageLevel {
    Info,
    Warn,
    Error,
}

struct DashboardMessage {
    text: String,
    level: DashboardMessageLevel,
}

// ── Tab enum ─────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Sessions,
    Agents,
    Memories,
    Health,
    Cost,
    Cron,
}

const TABS: [Tab; 7] = [
    Tab::Overview,
    Tab::Sessions,
    Tab::Agents,
    Tab::Memories,
    Tab::Health,
    Tab::Cost,
    Tab::Cron,
];

impl Tab {
    fn fluent_key(self) -> &'static str {
        match self {
            Self::Overview => "zc-dashboard-tab-overview",
            Self::Sessions => "zc-dashboard-tab-sessions",
            Self::Agents => "zc-dashboard-tab-agents",
            Self::Memories => "zc-dashboard-tab-memories",
            Self::Health => "zc-dashboard-tab-health",
            Self::Cost => "zc-dashboard-tab-cost",
            Self::Cron => "zc-dashboard-tab-cron",
        }
    }
}

// ── Dashboard ────────────────────────────────────────────────────

pub(crate) struct Dashboard {
    rpc: Arc<RpcClient>,
    connect_label: String,
    insecure_tls: bool,
    tab: Tab,
    last_poll: Option<Instant>,
    // Data
    sessions: Vec<SessionEntry>,
    agents: Vec<AgentStatusEntry>,
    cost: Option<CostSummaryResult>,
    /// Per-period (day / month / quarter / YTD) cost summaries for the user's
    /// account, fetched on the Cost tab so the dashboard mirrors a typical CLI
    /// report's period breakdown. `(label, summary)`.
    cost_periods: Vec<(String, CostSummaryResult)>,
    /// Optional org-level billed snapshot (`cost/org`). Present only when the
    /// daemon has an `org_cost.json` (an integrator's external sync); `None`
    /// otherwise, so the organization row is simply omitted.
    cost_org: Option<OrgCost>,
    cost_org_error: Option<String>,
    cron_jobs: Vec<CronJobEntry>,
    cron_runs: Vec<CronRunEntry>,
    cron_runs_job_id: Option<String>,
    cron_runs_error: Option<String>,
    cron_trigger_job_id: Option<String>,
    cron_trigger_message: Option<String>,
    cron_trigger_inflight_job_id: Option<String>,
    cron_trigger_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CronTriggerUpdate>>,
    memories: Vec<MemoryEntryResult>,
    memory_error: Option<String>,
    cost_error: Option<String>,
    sessions_loaded: bool,
    memory_detail: Option<MemoryEntryResult>,
    /// Key of the entry whose detail is currently being fetched or
    /// shown. Used to drop stale `memory/get` responses when the
    /// selection moves before the daemon answers.
    memory_detail_key: Option<String>,
    tuis: Vec<TuiListEntry>,
    // Session messages (loaded on demand)
    session_messages: Vec<MessageEntry>,
    session_messages_id: Option<String>,
    /// Total persisted messages for the currently-loaded session, as
    /// reported by `session/messages`. Pairs with
    /// `session_messages_start` to label the right-pane scrollback
    /// affordance once it lands.
    session_messages_total: usize,
    /// Index of `session_messages[0]` in the full persisted history.
    session_messages_start: usize,
    // List states
    session_state: ListState,
    agent_state: ListState,
    memory_state: ListState,
    cron_state: ListState,
    health_scroll: u16,
    cost_scroll: u16,
    // Detail pane
    detail_open: bool,
    detail_scroll: u16,
    detail_pct: u16,
    // Search / filter
    search_active: bool,
    search_buf: String,
    search_query: String,
    search_query_saved: String, // saved on search entry for Esc restore
    agent_rename: Option<AgentRenameState>,
    agent_rename_message: Option<DashboardMessage>,
    // Layout tracking for mouse
    tab_area: Rect,
    list_area: Rect,
    overview_agents_area: Rect,
    agent_alias_rename_area: Option<Rect>,
    detail_area: Option<Rect>,
    double_click: mouse::DoubleClickTracker,
}

impl Dashboard {
    pub(crate) fn new(rpc: Arc<RpcClient>, connect_label: &str, insecure_tls: bool) -> Self {
        Self {
            rpc,
            connect_label: connect_label.to_string(),
            insecure_tls,
            tab: Tab::Overview,
            last_poll: None,
            sessions: Vec::new(),
            agents: Vec::new(),
            cost: None,
            cost_periods: Vec::new(),
            cost_org: None,
            cost_org_error: None,
            cron_jobs: Vec::new(),
            cron_runs: Vec::new(),
            cron_runs_job_id: None,
            cron_runs_error: None,
            cron_trigger_job_id: None,
            cron_trigger_message: None,
            cron_trigger_inflight_job_id: None,
            cron_trigger_rx: None,
            memories: Vec::new(),
            memory_error: None,
            cost_error: None,
            sessions_loaded: false,
            memory_detail: None,
            memory_detail_key: None,
            tuis: Vec::new(),
            session_messages: Vec::new(),
            session_messages_id: None,
            session_messages_total: 0,
            session_messages_start: 0,
            session_state: ListState::default(),
            agent_state: ListState::default(),
            memory_state: ListState::default(),
            cron_state: ListState::default(),
            health_scroll: 0,
            cost_scroll: 0,
            detail_open: false,
            detail_scroll: 0,
            detail_pct: 50,
            search_active: false,
            search_buf: String::new(),
            search_query: String::new(),
            search_query_saved: String::new(),
            agent_rename: None,
            agent_rename_message: None,
            tab_area: Rect::default(),
            list_area: Rect::default(),
            overview_agents_area: Rect::default(),
            agent_alias_rename_area: None,
            detail_area: None,
            double_click: mouse::DoubleClickTracker::new(),
        }
    }

    pub(crate) async fn init(&mut self) -> anyhow::Result<()> {
        self.poll_data().await;
        Ok(())
    }

    /// Called on every tick from the app event loop.
    pub(crate) async fn tick(&mut self) {
        let should_poll = self
            .last_poll
            .map(|t| t.elapsed().as_secs() >= POLL_INTERVAL_SECS)
            .unwrap_or(true);
        if should_poll {
            self.poll_data().await;
        }
    }

    pub(crate) fn on_pane_blur(&mut self) {
        self.agent_rename = None;
        self.agent_rename_message = None;
        self.agent_alias_rename_area = None;
    }

    async fn poll_data(&mut self) {
        self.last_poll = Some(Instant::now());

        // Fetch tab-specific data
        match self.tab {
            Tab::Overview => {
                match self.rpc.cost_query(None).await {
                    Ok(c) => {
                        self.cost = Some(c);
                        self.cost_error = None;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("not available") {
                            self.cost_error =
                                Some(crate::i18n::t("zc-dashboard-cost-not-available"));
                        } else {
                            self.cost_error = Some(msg);
                        }
                    }
                }
                if let Ok(a) = self.rpc.agents_status().await {
                    self.agents = a.agents;
                }
                if let Ok(t) = self.rpc.tui_list().await {
                    self.tuis = t.tuis;
                }
            }
            Tab::Sessions => {
                // Pass search query for server-side FTS when active.
                let query = if self.search_query.is_empty() {
                    None
                } else {
                    Some(self.search_query.as_str())
                };
                if let Ok(s) = self.rpc.session_list(query).await {
                    self.sessions = s.sessions;
                    self.sessions_loaded = true;
                }
            }
            Tab::Agents => {
                if let Ok(a) = self.rpc.agents_status().await {
                    self.agents = a.agents;
                }
            }
            Tab::Memories => {
                // Use search endpoint when a query is active, list otherwise.
                let result = if !self.search_query.is_empty() {
                    self.rpc
                        .memory_search(&self.search_query, 200)
                        .await
                        .map(|r| r.entries)
                } else {
                    self.rpc.memory_list(None).await.map(|r| r.entries)
                };
                match result {
                    Ok(mut entries) => {
                        // Sort newest-first by timestamp.
                        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                        self.memories = entries;
                        self.memory_error = None;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("not available") {
                            self.memory_error =
                                Some(crate::i18n::t("zc-dashboard-memory-not-configured"));
                        } else {
                            self.memory_error = Some(msg);
                        }
                    }
                }
            }
            Tab::Health => {} // health already fetched above
            Tab::Cost => {
                match self.rpc.cost_query(None).await {
                    Ok(c) => {
                        self.cost = Some(c);
                        self.cost_error = None;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if msg.contains("not available") {
                            self.cost_error =
                                Some(crate::i18n::t("zc-dashboard-cost-not-available"));
                        } else {
                            self.cost_error = Some(msg);
                        }
                    }
                }
                // Day / month / quarter / YTD windows for the user's account,
                // mirroring a typical CLI report. Best-effort per window.
                let mut periods = Vec::new();
                for (label, from, to) in cost_period_windows() {
                    if let Ok(c) = self.rpc.cost_query_window(&from, &to, None).await {
                        periods.push((label, c));
                    }
                }
                self.cost_periods = periods;
                match self.rpc.cost_org().await {
                    Ok(org) => {
                        self.cost_org = org;
                        self.cost_org_error = None;
                    }
                    Err(_e) => {
                        self.cost_org = None;
                        self.cost_org_error = Some(crate::i18n::t("zc-dashboard-cost-org-error"));
                    }
                }
            }
            Tab::Cron => {
                if let Ok(c) = self.rpc.cron_list().await {
                    self.cron_jobs = c.jobs;
                    if self.detail_open {
                        self.load_cron_runs().await;
                    }
                }
            }
        }
    }

    /// Fetch session messages for the currently selected session.
    async fn load_session_messages(&mut self) {
        let Some(idx) = self.selected_session_index() else {
            return;
        };
        let sid = &self.sessions[idx].session_id;
        if self.session_messages_id.as_deref() == Some(sid) {
            return; // already loaded
        }
        let sid = sid.clone();
        // Load only the most-recent page on detail-open. Older
        // pages can be paged in if the session view ever grows a
        // scrollback affordance; for now the right-side detail
        // pane shows the tail of the conversation.
        if let Ok(result) = self
            .rpc
            .session_messages_page(&sid, Some(SESSION_MESSAGES_PAGE_SIZE), None)
            .await
        {
            self.session_messages = result.messages;
            self.session_messages_total = result.total;
            self.session_messages_start = result.start;
            self.session_messages_id = Some(sid);
        }
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
        status: Option<&StatusResult>,
        health: Option<&serde_json::Value>,
        code_cwd: Option<&str>,
        chat_cwd: Option<&str>,
    ) {
        self.drain_cron_trigger_updates();

        // Clear stale data when disconnected so panels don't show
        // ghost entries from a previous daemon lifetime.
        if matches!(
            self.rpc.connection_state(),
            crate::client::ConnectionState::Disconnected { .. }
        ) {
            self.tuis.clear();
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // tab bar
                Constraint::Length(1), // status line
                Constraint::Min(0),    // content
                Constraint::Length(1), // footer
            ])
            .split(area);

        self.tab_area = chunks[0];
        self.draw_tab_bar(frame, chunks[0]);
        self.draw_status_line(frame, chunks[1]);

        match self.tab {
            Tab::Overview => {
                self.draw_overview(frame, chunks[2], status, health, code_cwd, chat_cwd)
            }
            Tab::Sessions => self.draw_sessions(frame, chunks[2]),
            Tab::Agents => self.draw_agents(frame, chunks[2]),
            Tab::Memories => self.draw_memories(frame, chunks[2]),
            Tab::Health => self.draw_health(frame, chunks[2], health),
            Tab::Cost => self.draw_cost(frame, chunks[2]),
            Tab::Cron => self.draw_cron(frame, chunks[2]),
        }

        // Footer: ?=help hint at bottom-left.
        frame.render_widget(
            Paragraph::new(Span::styled(mouse::HELP_HINT, theme::dim_style())),
            chunks[3],
        );
    }

    fn draw_tab_bar(&self, frame: &mut ratatui::Frame, area: Rect) {
        let mut spans = Vec::new();
        for (i, tab) in TABS.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" \u{2502} ", theme::dim_style()));
            }
            let style = if *tab == self.tab {
                theme::selected_style().add_modifier(Modifier::BOLD)
            } else {
                theme::body_style()
            };
            spans.push(Span::styled(crate::i18n::t(tab.fluent_key()), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_status_line(&self, frame: &mut ratatui::Frame, area: Rect) {
        let help: String = if self.search_active || self.agent_rename.is_some() {
            format!(
                "Enter:{apply}  Esc:{cancel}",
                apply = if self.agent_rename.is_some() {
                    crate::i18n::t("zc-dashboard-agent-rename-action-apply")
                } else {
                    crate::i18n::t("zc-dashboard-search-action-apply")
                },
                cancel = if self.agent_rename.is_some() {
                    crate::i18n::t("zc-dashboard-agent-rename-action-cancel")
                } else {
                    crate::i18n::t("zc-dashboard-search-action-cancel")
                },
            )
        } else if self.tab == Tab::Agents {
            let key =
                crate::keymap::action_key_labels(crate::keymap::DashboardTabAction::RenameAgent)
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "e".to_string());
            format!("{key}:{}", crate::i18n::t("zc-dashboard-agent-rename-hint"))
        } else {
            String::new()
        };

        let line = if let Some(rename) = &self.agent_rename {
            let mut spans = vec![
                Span::styled(
                    crate::i18n::t("zc-dashboard-agent-rename-prefix"),
                    theme::accent_style(),
                ),
                Span::styled(" ", theme::dim_style()),
                Span::styled(&rename.from, theme::body_style()),
                Span::styled(" -> ", theme::dim_style()),
                Span::styled(&rename.buf, theme::input_style()),
                Span::styled("\u{2588} ", theme::accent_style()),
                Span::styled(help, theme::dim_style()),
            ];
            if let Some(message) = &self.agent_rename_message {
                let style = match message.level {
                    DashboardMessageLevel::Info => theme::dim_style(),
                    DashboardMessageLevel::Warn | DashboardMessageLevel::Error => {
                        theme::warn_style()
                    }
                };
                spans.push(Span::styled("  ", theme::dim_style()));
                spans.push(Span::styled(&message.text, style));
            }
            Line::from(spans)
        } else if self.search_active {
            Line::from(vec![
                Span::styled(" /", theme::accent_style()),
                Span::styled(&self.search_buf, theme::input_style()),
                Span::styled("\u{2588}", theme::accent_style()),
            ])
        } else {
            let mut spans = Vec::new();
            if !self.search_query.is_empty() {
                spans.push(Span::styled(
                    crate::i18n::t("zc-dashboard-search-prefix"),
                    theme::dim_style(),
                ));
                spans.push(Span::styled(&self.search_query, theme::accent_style()));
                spans.push(Span::styled(" ", theme::dim_style()));
            }
            if self.tab == Tab::Agents
                && let Some(message) = &self.agent_rename_message
            {
                let style = match message.level {
                    DashboardMessageLevel::Info => theme::dim_style(),
                    DashboardMessageLevel::Warn | DashboardMessageLevel::Error => {
                        theme::warn_style()
                    }
                };
                spans.push(Span::styled(&message.text, style));
                spans.push(Span::styled(" ", theme::dim_style()));
            }
            spans.push(Span::styled(help, theme::dim_style()));
            Line::from(spans)
        };

        frame.render_widget(Paragraph::new(line), area);
    }

    // ── Overview tab ─────────────────────────────────────────────

    fn draw_overview(
        &mut self,
        frame: &mut ratatui::Frame,
        area: Rect,
        status: Option<&StatusResult>,
        health: Option<&serde_json::Value>,
        code_cwd: Option<&str>,
        chat_cwd: Option<&str>,
    ) {
        let workspace_lines = workspace_lines(code_cwd, chat_cwd);
        let status_height = 12 + workspace_lines.len() as u16;
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(status_height),                // status box
                Constraint::Length(4 + self.agents.len() as u16), // agents
                Constraint::Min(0),                               // connected TUIs
            ])
            .split(area);

        // Status box
        let status_block = Block::default()
            .title(Span::styled(" Status ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = status_block.inner(chunks[0]);
        frame.render_widget(status_block, chunks[0]);

        if let Some(s) = status {
            let lines = overview_status_lines(
                &self.connect_label,
                self.insecure_tls,
                s,
                health,
                code_cwd,
                chat_cwd,
            );
            frame.render_widget(Paragraph::new(lines), inner);
        } else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-loading"),
                    theme::dim_style(),
                )),
                inner,
            );
        }

        // Agents
        let agents_block = Block::default()
            .title(Span::styled(" Agents ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let agents_inner = agents_block.inner(chunks[1]);
        self.overview_agents_area = chunks[1];
        frame.render_widget(agents_block, chunks[1]);

        let items: Vec<ListItem> = self
            .agents
            .iter()
            .map(|a| {
                let status_style = if a.enabled {
                    Style::default().fg(Color::Green)
                } else {
                    theme::dim_style()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if a.enabled { "\u{25cf} " } else { "\u{25cb} " },
                        status_style,
                    ),
                    Span::styled(&a.alias, theme::body_style()),
                    Span::styled(
                        format!(
                            "  ({} live, {} persisted)",
                            a.live_sessions, a.persisted_sessions
                        ),
                        theme::dim_style(),
                    ),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), agents_inner);

        // Connected TUIs
        self.draw_tuis_panel(frame, chunks[2]);
    }

    fn draw_tuis_panel(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(
                format!(" Connected TUIs ({}) ", self.tuis.len()),
                theme::title_style(),
            ))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.tuis.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-no-tuis"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        }

        let my_id = self.rpc.tui_id();
        let items: Vec<ListItem> = self
            .tuis
            .iter()
            .map(|t| {
                let is_me = my_id == Some(t.tui_id.as_str());
                let you_marker = if is_me { " (you)" } else { "" };
                let elapsed = format_relative_time(t.connected_at_unix);
                let id_style = if is_me {
                    theme::accent_style()
                } else {
                    theme::body_style()
                };
                let peer = if !t.peer_label.is_empty() {
                    format!(" [{}]", t.peer_label)
                } else if !t.transport.is_empty() {
                    format!(" [{}]", t.transport)
                } else {
                    String::new()
                };
                ListItem::new(Line::from(vec![
                    Span::styled("\u{25cf} ", Style::default().fg(Color::Green)),
                    Span::styled(&t.tui_id, id_style),
                    Span::styled(peer, theme::dim_style()),
                    Span::styled(you_marker, theme::accent_style()),
                    Span::styled(format!("  {elapsed}"), theme::dim_style()),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), inner);
    }

    // ── Sessions tab ─────────────────────────────────────────────

    fn draw_sessions(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let filtered = self.filtered_session_indices();

        if self.detail_open {
            let list_pct = 100u16.saturating_sub(self.detail_pct);
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(self.detail_pct),
                ])
                .split(area);
            self.draw_session_list(frame, hsplit[0], &filtered);
            self.draw_session_detail(frame, hsplit[1]);
            self.detail_area = Some(hsplit[1]);
        } else {
            self.detail_area = None;
            self.draw_session_list(frame, area, &filtered);
        }
    }

    fn draw_session_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        self.list_area = area;
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| {
                let s = &self.sessions[i];
                let agent = s.agent_alias.as_deref().unwrap_or("?");
                let name = s
                    .name
                    .as_deref()
                    .unwrap_or(&s.session_id[..8.min(s.session_id.len())]);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{agent:<12}"), theme::accent_style()),
                    Span::styled(name, theme::body_style()),
                    Span::styled(format!("  msgs:{} ", s.message_count), theme::dim_style()),
                    Span::styled(&s.last_activity, theme::dim_style()),
                ]))
            })
            .collect();

        let title = if self.sessions_loaded {
            format!(" Sessions ({}) ", filtered.len())
        } else {
            " Sessions ".to_string()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(title, theme::title_style()))
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.session_state);
    }

    fn draw_session_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Session Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(idx) = self.selected_session_index() else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-no-session"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let s = &self.sessions[idx];
        let mut lines = vec![
            detail_line("ID", &s.session_id),
            detail_line(&crate::i18n::t("zc-dashboard-detail-key"), &s.session_key),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-agent"),
                s.agent_alias.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-channel"),
                s.channel_id.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-name"),
                s.name.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-messages"),
                &s.message_count.to_string(),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-created"),
                &s.created_at,
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-activity"),
                &s.last_activity,
            ),
        ];

        // Show message history if loaded
        if self.session_messages_id.as_deref() == Some(&s.session_id) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t_args(
                    "zc-dashboard-message-history",
                    &[("count", &self.session_messages.len().to_string())],
                ),
                theme::heading_style(),
            )));
            lines.push(Line::from(""));
            for msg in &self.session_messages {
                let role_style = match msg.role() {
                    crate::client::MessageRole::User => theme::user_label_style(),
                    crate::client::MessageRole::Assistant => theme::agent_label_style(),
                    crate::client::MessageRole::System => {
                        theme::dim_style().add_modifier(Modifier::BOLD)
                    }
                    crate::client::MessageRole::Other => {
                        theme::body_style().add_modifier(Modifier::BOLD)
                    }
                };
                lines.push(Line::from(Span::styled(
                    format!("[{}]", msg.role),
                    role_style,
                )));
                for l in msg.content.lines() {
                    lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
                }
                lines.push(Line::from(""));
            }
            if self.session_messages.is_empty() {
                lines.push(Line::from(Span::styled(
                    "(no messages)",
                    theme::dim_style(),
                )));
            }
        } else {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-loading-messages"),
                theme::dim_style(),
            )));
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    fn filtered_session_indices(&self) -> Vec<usize> {
        // Sessions use server-side FTS — the list from the daemon is
        // already filtered when a search query is active.
        (0..self.sessions.len()).collect()
    }

    fn selected_session_index(&self) -> Option<usize> {
        let filtered = self.filtered_session_indices();
        let sel = self.session_state.selected()?;
        filtered.get(sel).copied()
    }

    // ── Agents tab ───────────────────────────────────────────────

    fn draw_agents(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let filtered = self.filtered_agent_indices();

        if self.detail_open {
            let list_pct = 100u16.saturating_sub(self.detail_pct);
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(self.detail_pct),
                ])
                .split(area);
            self.draw_agent_list(frame, hsplit[0], &filtered);
            self.draw_agent_detail(frame, hsplit[1]);
            self.detail_area = Some(hsplit[1]);
        } else {
            self.detail_area = None;
            self.agent_alias_rename_area = None;
            self.draw_agent_list(frame, area, &filtered);
        }
    }

    fn draw_agent_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        self.list_area = area;
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| {
                let a = &self.agents[i];
                let status_style = if a.enabled {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Red)
                };
                let dot = if a.enabled { "\u{25cf}" } else { "\u{25cb}" };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{dot} "), status_style),
                    Span::styled(format!("{:<20}", a.alias), theme::body_style()),
                    Span::styled(
                        if a.enabled {
                            crate::i18n::t("zc-dashboard-enabled")
                        } else {
                            crate::i18n::t("zc-dashboard-disabled")
                        },
                        status_style,
                    ),
                    Span::styled(
                        format!(
                            "  live: {}, persisted: {}",
                            a.live_sessions, a.persisted_sessions
                        ),
                        theme::dim_style(),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Agents ({}) ", filtered.len()),
                        theme::title_style(),
                    ))
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.agent_state);
    }

    fn draw_agent_detail(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        self.agent_alias_rename_area = None;
        let block = Block::default()
            .title(Span::styled(" Agent Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(idx) = self.selected_agent_index() else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-no-agent"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let a = &self.agents[idx];
        if self.detail_scroll == 0 && inner.height > 0 {
            self.agent_alias_rename_area = Some(Rect::new(inner.x, inner.y, inner.width, 1));
        }
        let mut lines = vec![
            detail_action_line(
                &crate::i18n::t("zc-dashboard-detail-alias"),
                &a.alias,
                &crate::i18n::t("zc-dashboard-agent-rename-hint"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-enabled"),
                &if a.enabled {
                    crate::i18n::t("zc-dashboard-yes")
                } else {
                    crate::i18n::t("zc-dashboard-no")
                },
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-sessions"),
                &format!(
                    "{} live, {} persisted",
                    a.live_sessions, a.persisted_sessions
                ),
            ),
        ];
        if a.persisted_sessions > 0 {
            lines.push(detail_line(
                &crate::i18n::t("zc-dashboard-detail-persisted-sessions"),
                &a.persisted_sessions.to_string(),
            ));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-section-channels"),
            theme::heading_style(),
        )));
        if a.channels.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (none configured)",
                theme::dim_style(),
            )));
        } else {
            for ch in &a.channels {
                lines.push(Line::from(vec![
                    Span::styled("  \u{2022} ", theme::accent_style()),
                    Span::styled(ch.to_string(), theme::body_style()),
                ]));
            }
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    fn filtered_agent_indices(&self) -> Vec<usize> {
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                if self.search_query.is_empty() {
                    return true;
                }
                let q = self.search_query.to_lowercase();
                a.alias.to_lowercase().contains(&q)
                    || a.channels.iter().any(|c| c.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_agent_index(&self) -> Option<usize> {
        let filtered = self.filtered_agent_indices();
        let sel = self.agent_state.selected()?;
        filtered.get(sel).copied()
    }

    // ── Memories tab ─────────────────────────────────────────────

    fn draw_memories(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        // Show error state when memory backend is unavailable.
        if let Some(ref err) = self.memory_error {
            let block = Block::default()
                .title(Span::styled(" Memories ", theme::title_style()))
                .borders(Borders::ALL)
                .border_style(theme::dim_style());
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(err.as_str())
                    .style(theme::warn_style())
                    .wrap(Wrap { trim: true }),
                inner,
            );
            return;
        }

        let filtered = self.filtered_memory_indices();

        if self.detail_open {
            let list_pct = 100u16.saturating_sub(self.detail_pct);
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(self.detail_pct),
                ])
                .split(area);
            self.draw_memory_list(frame, hsplit[0], &filtered);
            self.draw_memory_detail(frame, hsplit[1]);
            self.detail_area = Some(hsplit[1]);
        } else {
            self.detail_area = None;
            self.draw_memory_list(frame, area, &filtered);
        }
    }

    fn draw_memory_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        self.list_area = area;
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| {
                let m = &self.memories[i];
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<14}", m.category), theme::accent_style()),
                    Span::styled(&m.key, theme::body_style()),
                    Span::styled(
                        format!("  {}", truncate(&m.content, 40)),
                        theme::dim_style(),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Memories ({}) ", filtered.len()),
                        theme::title_style(),
                    ))
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.memory_state);
    }

    fn draw_memory_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Memory Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let m: &MemoryEntryResult = match (&self.memory_detail, self.selected_memory_index()) {
            (Some(detail), _) => detail,
            (None, Some(idx)) => &self.memories[idx],
            (None, None) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        crate::i18n::t("zc-dashboard-no-entry"),
                        theme::dim_style(),
                    )),
                    inner,
                );
                return;
            }
        };
        let mut lines = vec![
            detail_line(&crate::i18n::t("zc-dashboard-detail-key"), &m.key),
            detail_line(&crate::i18n::t("zc-dashboard-detail-category"), &m.category),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-namespace"),
                &m.namespace,
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-timestamp"),
                &m.timestamp,
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-agent"),
                m.agent_alias.as_deref().unwrap_or("\u{2014}"),
            ),
        ];
        if let Some(score) = m.score {
            lines.push(detail_line(
                &crate::i18n::t("zc-dashboard-detail-score"),
                &format!("{score:.3}"),
            ));
        }
        if let Some(imp) = m.importance {
            lines.push(detail_line(
                &crate::i18n::t("zc-dashboard-detail-importance"),
                &format!("{imp:.2}"),
            ));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-section-content"),
            theme::heading_style(),
        )));
        for l in m.content.lines() {
            lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    fn filtered_memory_indices(&self) -> Vec<usize> {
        self.memories
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                if self.search_query.is_empty() {
                    return true;
                }
                let q = self.search_query.to_lowercase();
                m.key.to_lowercase().contains(&q)
                    || m.content.to_lowercase().contains(&q)
                    || m.category.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_memory_index(&self) -> Option<usize> {
        let filtered = self.filtered_memory_indices();
        let sel = self.memory_state.selected()?;
        filtered.get(sel).copied()
    }

    // ── Health tab ───────────────────────────────────────────────

    fn draw_health(
        &self,
        frame: &mut ratatui::Frame,
        area: Rect,
        health: Option<&serde_json::Value>,
    ) {
        let block = Block::default()
            .title(Span::styled(" Health ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(h) = health else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-loading"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let mut lines = Vec::new();
        if let Some(obj) = h.as_object() {
            // Overall status
            if let Some(uptime) = obj.get("uptime_seconds").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<11}", crate::i18n::t("zc-dashboard-label-uptime")),
                        theme::dim_style(),
                    ),
                    Span::styled(format_uptime(uptime), theme::body_style()),
                ]));
            }
            if let Some(pid) = obj.get("pid").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:<11}", crate::i18n::t("zc-dashboard-label-pid")),
                        theme::dim_style(),
                    ),
                    Span::styled(pid.to_string(), theme::body_style()),
                ]));
            }

            // Process stats
            if let Some(process) = obj.get("process") {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    crate::i18n::t("zc-dashboard-section-process"),
                    theme::heading_style(),
                )));
                if let Some(rss) = process.get("rss_bytes").and_then(|v| v.as_u64())
                    && rss > 0
                {
                    let total = process
                        .get("system_ram_total_bytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let rss_str = format_bytes(rss);
                    let val = if total > 0 {
                        let pct = (rss as f64 / total as f64) * 100.0;
                        format!("{rss_str} / {} ({pct:.1}%)", format_bytes(total))
                    } else {
                        rss_str
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  RAM      ", theme::dim_style()),
                        Span::styled(val, theme::body_style()),
                    ]));
                }
                if let Some(cpu) = process.get("cpu_percent").and_then(|v| v.as_f64()) {
                    let ncpu = process
                        .get("num_cpus")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let val = if ncpu > 0 {
                        crate::i18n::t_args(
                            "zc-dashboard-cpu-with-cores",
                            &[("cpu", &format!("{cpu:.1}%")), ("cores", &ncpu.to_string())],
                        )
                    } else {
                        format!("{cpu:.1}%")
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  CPU      ", theme::dim_style()),
                        Span::styled(val, theme::body_style()),
                    ]));
                }
            }

            // Components
            if let Some(components) = obj.get("components").and_then(|v| v.as_object()) {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    crate::i18n::t("zc-dashboard-section-components"),
                    theme::heading_style(),
                )));
                for (name, val) in components {
                    let status = val
                        .as_object()
                        .and_then(|o| o.get("status"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let style = match status {
                        "healthy" | "ok" => Style::default().fg(Color::Green),
                        "degraded" => theme::warn_style(),
                        _ => Style::default().fg(Color::Red),
                    };
                    let dot = match status {
                        "healthy" | "ok" => "\u{25cf}",
                        _ => "\u{25cb}",
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {dot} "), style),
                        Span::styled(format!("{name:<24}"), theme::body_style()),
                        Span::styled(status, style),
                    ]));
                }
            }

            // Raw JSON fallback for any other fields
            let known = [
                "status",
                "uptime_seconds",
                "components",
                "pid",
                "updated_at",
                "process",
            ];
            let extras: Vec<_> = obj
                .keys()
                .filter(|k| !known.contains(&k.as_str()))
                .collect();
            if !extras.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    crate::i18n::t("zc-dashboard-section-details"),
                    theme::heading_style(),
                )));
                for key in extras {
                    if let Some(val) = obj.get(key) {
                        let val_str = if let Some(s) = val.as_str() {
                            s.to_string()
                        } else {
                            serde_json::to_string_pretty(val).unwrap_or_default()
                        };
                        for (i, line) in val_str.lines().enumerate() {
                            if i == 0 {
                                lines.push(Line::from(vec![
                                    Span::styled(format!("  {key:<22}"), theme::dim_style()),
                                    Span::styled(line.to_string(), theme::body_style()),
                                ]));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    format!("  {:<22}{line}", ""),
                                    theme::body_style(),
                                )));
                            }
                        }
                    }
                }
            }
        } else {
            // Non-object health response — dump as JSON
            let pretty = serde_json::to_string_pretty(h).unwrap_or_default();
            for line in pretty.lines() {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    theme::body_style(),
                )));
            }
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.health_scroll, 0));
        frame.render_widget(para, inner);
    }

    // ── Cost tab ─────────────────────────────────────────────────

    fn draw_cost(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Cost ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if let Some(ref err) = self.cost_error {
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
                    crate::i18n::t("zc-dashboard-loading"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let mut lines = vec![
            Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-summary"),
                theme::heading_style(),
            )),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-session"),
                &format!("${:.6}", c.session_cost_usd),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-daily"),
                &format!("${:.6}", c.daily_cost_usd),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-monthly"),
                &format!("${:.6}", c.monthly_cost_usd),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-tokens"),
                &format_tokens(c.total_tokens),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-requests"),
                &c.request_count.to_string(),
            ),
        ];

        // By-period breakdown (day / month / quarter / YTD) for the user's
        // account — mirrors a typical CLI report. Paid vs free tokens make the
        // split explicit (free models contribute $0).
        if !self.cost_periods.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-by-period"),
                theme::heading_style(),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {:<10} {:>12} {:>12} {:>12} {:>7}",
                    crate::i18n::t("zc-dashboard-col-period"),
                    crate::i18n::t("zc-dashboard-col-cost"),
                    crate::i18n::t("zc-dashboard-col-paid-tok"),
                    crate::i18n::t("zc-dashboard-col-free-tok"),
                    crate::i18n::t("zc-dashboard-col-reqs")
                ),
                theme::dim_style(),
            )));
            for (label, summary) in &self.cost_periods {
                let (paid_tok, free_tok) =
                    summary.by_model.values().fold((0u64, 0u64), |(p, f), m| {
                        if m.cost_usd > 0.0 {
                            (p + m.total_tokens, f)
                        } else {
                            (p, f + m.total_tokens)
                        }
                    });
                let cost_style = if summary.session_cost_usd > 0.0 {
                    theme::accent_style()
                } else {
                    theme::success_style()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {label:<10}"), theme::body_style()),
                    Span::styled(format!("{:>12.4}", summary.session_cost_usd), cost_style),
                    Span::styled(
                        format!("{:>12}", format_tokens(paid_tok)),
                        theme::accent_style(),
                    ),
                    Span::styled(
                        format!("{:>12}", format_tokens(free_tok)),
                        theme::success_style(),
                    ),
                    Span::styled(format!("{:>7}", summary.request_count), theme::dim_style()),
                ]));
            }
        }

        // Organization-level billed snapshot. Appends the org billing
        // section: a present snapshot renders the billed rows, a broken
        // snapshot (cost/org RPC error) renders a warning, and an absent
        // snapshot renders nothing. See `org_section_lines`.
        lines.extend(org_section_lines(
            self.cost_org.as_ref(),
            self.cost_org_error.as_deref(),
            frac_year_elapsed(),
        ));

        if !c.by_model.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-by-model"),
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
                    Span::styled(format!("${:.6}", m.cost_usd), theme::accent_style()),
                    Span::styled(
                        format!(
                            "  {} reqs  {} tok",
                            m.request_count,
                            format_tokens(m.total_tokens)
                        ),
                        theme::dim_style(),
                    ),
                ]));
            }
        }

        if !c.by_agent.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-by-agent"),
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
                    Span::styled(format!("${:.6}", a.cost_usd), theme::accent_style()),
                    Span::styled(
                        format!(
                            "  {} reqs  {} tok",
                            a.request_count,
                            format_tokens(a.total_tokens)
                        ),
                        theme::dim_style(),
                    ),
                ]));
            }
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.cost_scroll, 0));
        frame.render_widget(para, inner);
    }

    // ── Cron tab ─────────────────────────────────────────────────

    fn draw_cron(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let filtered = self.filtered_cron_indices();

        if self.detail_open {
            let list_pct = 100u16.saturating_sub(self.detail_pct);
            let hsplit = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(list_pct),
                    Constraint::Percentage(self.detail_pct),
                ])
                .split(area);
            self.draw_cron_list(frame, hsplit[0], &filtered);
            self.draw_cron_detail(frame, hsplit[1]);
            self.detail_area = Some(hsplit[1]);
        } else {
            self.detail_area = None;
            self.draw_cron_list(frame, area, &filtered);
        }
    }

    fn draw_cron_list(&mut self, frame: &mut ratatui::Frame, area: Rect, filtered: &[usize]) {
        self.list_area = area;
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|&i| {
                let j = &self.cron_jobs[i];
                let status_style = if j.enabled {
                    Style::default().fg(Color::Green)
                } else {
                    theme::dim_style()
                };
                let dot = if j.enabled { "\u{25cf}" } else { "\u{25cb}" };
                let label = j.name.as_deref().unwrap_or(&j.id);
                let sched = match &j.schedule {
                    CronSchedule::Cron { expr, .. } => expr.clone(),
                    CronSchedule::At { at } => format!("at {at}"),
                    CronSchedule::Every { every_ms } => format!("every {}s", every_ms / 1000),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{dot} "), status_style),
                    Span::styled(format!("{label:<20}"), theme::body_style()),
                    Span::styled(format!("{:<12}", j.agent_alias), theme::accent_style()),
                    Span::styled(sched, theme::dim_style()),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Cron Jobs ({}) ", filtered.len()),
                        theme::title_style(),
                    ))
                    .borders(Borders::ALL)
                    .border_style(theme::dim_style()),
            )
            .highlight_style(theme::selected_style());

        frame.render_stateful_widget(list, area, &mut self.cron_state);
    }

    fn draw_cron_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Cron Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(idx) = self.selected_cron_index() else {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    crate::i18n::t("zc-dashboard-no-job"),
                    theme::dim_style(),
                )),
                inner,
            );
            return;
        };

        let j = &self.cron_jobs[idx];
        let sched_str = match &j.schedule {
            CronSchedule::Cron { expr, tz } => {
                let tz_str = tz.as_deref().unwrap_or("UTC");
                format!("cron: {expr} ({tz_str})")
            }
            CronSchedule::At { at } => format!("at: {at}"),
            CronSchedule::Every { every_ms } => format!("every: {}s", every_ms / 1000),
        };

        let mut lines = vec![
            detail_line("ID", &j.id),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-name"),
                j.name.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(&crate::i18n::t("zc-dashboard-detail-agent"), &j.agent_alias),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-enabled"),
                &if j.enabled {
                    crate::i18n::t("zc-dashboard-yes")
                } else {
                    crate::i18n::t("zc-dashboard-no")
                },
            ),
            detail_line(&crate::i18n::t("zc-dashboard-detail-schedule"), &sched_str),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-created"),
                &j.created_at,
            ),
            detail_line(&crate::i18n::t("zc-dashboard-detail-next-run"), &j.next_run),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-last-run"),
                j.last_run.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-detail-last-status"),
                j.last_status.as_deref().unwrap_or("\u{2014}"),
            ),
            detail_line(
                &crate::i18n::t("zc-dashboard-actions"),
                &self.cron_action_hint(),
            ),
        ];

        if !j.command.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-command"),
                theme::heading_style(),
            )));
            for l in j.command.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }
        if let Some(ref prompt) = j.prompt {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-prompt"),
                theme::heading_style(),
            )));
            for l in prompt.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }
        if let Some(ref output) = j.last_output {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-last-output"),
                theme::heading_style(),
            )));
            for l in output.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }

        if self.cron_trigger_job_id.as_deref() == Some(j.id.as_str())
            && let Some(message) = &self.cron_trigger_message
        {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-section-manual-run"),
                theme::heading_style(),
            )));
            for l in message.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-section-recent-runs"),
            theme::heading_style(),
        )));
        if self.cron_runs_job_id.as_deref() != Some(j.id.as_str()) {
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-loading-runs"),
                theme::dim_style(),
            )));
        } else if let Some(error) = &self.cron_runs_error {
            lines.push(Line::from(Span::styled(
                format!("{}: {error}", crate::i18n::t("zc-dashboard-runs-error")),
                theme::error_style(),
            )));
        } else if self.cron_runs.is_empty()
            && self.cron_trigger_inflight_job_id.as_deref() != Some(j.id.as_str())
        {
            lines.push(Line::from(Span::styled(
                crate::i18n::t("zc-dashboard-no-runs"),
                theme::dim_style(),
            )));
        } else {
            if self.cron_trigger_inflight_job_id.as_deref() == Some(j.id.as_str()) {
                lines.push(Line::from(vec![
                    Span::styled("#... ", theme::dim_style()),
                    Span::styled(format!("{} ", truncate(&j.id, 24)), theme::dim_style()),
                    Span::styled(
                        format!("{:<10}", crate::i18n::t("zc-dashboard-run-pending-status")),
                        theme::warn_style(),
                    ),
                    Span::styled(
                        format!(
                            "{}  {}",
                            crate::i18n::t("zc-dashboard-run-pending-window"),
                            format_duration_ms(None)
                        ),
                        theme::dim_style(),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("  {}", crate::i18n::t("zc-dashboard-run-pending-output")),
                    theme::dim_style(),
                )));
            }
            for run in &self.cron_runs {
                let status_style = cron_run_status_style(&run.status);
                let run_window = if run.finished_at.trim().is_empty() {
                    run.started_at.clone()
                } else {
                    format!("{} -> {}", run.started_at, run.finished_at)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("#{} ", run.id), theme::dim_style()),
                    Span::styled(
                        format!("{} ", truncate(&run.job_id, 24)),
                        theme::dim_style(),
                    ),
                    Span::styled(format!("{:<10}", run.status), status_style),
                    Span::styled(
                        format!("{}  {}", run_window, format_duration_ms(run.duration_ms)),
                        theme::dim_style(),
                    ),
                ]));
                if let Some(output) = run.output.as_deref().filter(|s| !s.trim().is_empty()) {
                    for l in output.lines().take(3) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", truncate(l, 96)),
                            theme::body_style(),
                        )));
                    }
                }
            }
        }

        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));
        frame.render_widget(para, inner);
    }

    fn filtered_cron_indices(&self) -> Vec<usize> {
        self.cron_jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| {
                if self.search_query.is_empty() {
                    return true;
                }
                let q = self.search_query.to_lowercase();
                j.id.to_lowercase().contains(&q)
                    || j.name.as_deref().unwrap_or("").to_lowercase().contains(&q)
                    || j.agent_alias.to_lowercase().contains(&q)
                    || j.command.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_cron_index(&self) -> Option<usize> {
        let filtered = self.filtered_cron_indices();
        let sel = self.cron_state.selected()?;
        filtered.get(sel).copied()
    }

    // ── Key handling ─────────────────────────────────────────────

    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.agent_rename.is_some() {
            return self.handle_agent_rename_key(key).await;
        }
        if self.search_active {
            return self.handle_search_key(key);
        }
        if self.detail_open {
            return self.handle_detail_key(key).await;
        }
        self.handle_normal_key(key).await
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::SearchBoxAction;
        match SearchBoxAction::from_chord(&key) {
            Some(SearchBoxAction::Accept) => {
                self.search_query = self.search_buf.clone();
                self.search_active = false;
                // Force re-poll so server-side search (memories) picks up query.
                self.last_poll = None;
            }
            Some(SearchBoxAction::Cancel) => {
                // Restore the query from before search was activated.
                self.search_query = self.search_query_saved.clone();
                self.search_buf = self.search_query_saved.clone();
                self.search_active = false;
            }
            Some(SearchBoxAction::Backspace) => {
                self.search_buf.pop();
                // Live-filter for client-side tabs (agents, cron).
                // Server-side tabs (sessions, memories) wait for Enter.
                if !matches!(self.tab, Tab::Sessions | Tab::Memories) {
                    self.search_query = self.search_buf.clone();
                }
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.search_buf.push(c);
                    if !matches!(self.tab, Tab::Sessions | Tab::Memories) {
                        self.search_query = self.search_buf.clone();
                    }
                }
            }
        }
        false
    }

    async fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::DashboardTabAction;
        match DashboardTabAction::from_chord(&key) {
            Some(DashboardTabAction::CloseDetail) | Some(DashboardTabAction::OpenDetail) => {
                self.detail_open = false;
                self.detail_scroll = 0;
                self.memory_detail = None;
                self.memory_detail_key = None;
                self.session_messages.clear();
                self.session_messages_id = None;
                self.cron_runs.clear();
                self.cron_runs_job_id = None;
                self.cron_runs_error = None;
                self.cron_trigger_job_id = None;
                self.cron_trigger_message = None;
            }
            // Shift+J / Shift+K scroll the detail pane
            Some(DashboardTabAction::DetailScrollDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            Some(DashboardTabAction::DetailScrollUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            Some(DashboardTabAction::DetailWidenDown) => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            Some(DashboardTabAction::DetailWidenUp) => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            Some(DashboardTabAction::DetailWidenLeft) => {
                self.detail_pct = (self.detail_pct + 5).min(80);
            }
            Some(DashboardTabAction::DetailWidenRight) => {
                self.detail_pct = self.detail_pct.saturating_sub(5).max(20);
            }
            Some(DashboardTabAction::Down) => {
                self.move_list_down();
                self.detail_scroll = 0;
                self.on_selection_change().await;
            }
            Some(DashboardTabAction::Up) => {
                self.move_list_up();
                self.detail_scroll = 0;
                self.on_selection_change().await;
            }
            Some(DashboardTabAction::BeginSearch) => {
                self.search_query_saved = self.search_query.clone();
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            Some(DashboardTabAction::RenameAgent) if self.tab == Tab::Agents => {
                self.begin_agent_rename();
            }
            Some(DashboardTabAction::CopyDetail) => {
                self.search_query.clear();
                self.search_buf.clear();
                self.last_poll = None; // re-poll for server-side search
            }
            Some(DashboardTabAction::KillSession) if self.tab == Tab::Sessions => {
                if let Some(idx) = self.selected_session_index() {
                    let sid = self.sessions[idx].session_id.clone();
                    let _ = self.rpc.session_kill(&sid).await;
                    self.detail_open = false;
                    self.detail_scroll = 0;
                    self.session_messages.clear();
                    self.session_messages_id = None;
                    self.session_messages_total = 0;
                    self.session_messages_start = 0;
                    self.last_poll = None;
                }
            }
            Some(DashboardTabAction::TriggerCron) if self.tab == Tab::Cron => {
                self.trigger_selected_cron();
            }
            _ => {}
        }
        false
    }

    async fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::{DashboardTabAction, GlobalAction};
        if GlobalAction::from_chord(&key) == Some(GlobalAction::Quit) {
            return true;
        }
        match DashboardTabAction::from_chord(&key) {
            Some(DashboardTabAction::NextTab) => self.next_tab(),
            Some(DashboardTabAction::PrevTab) => self.prev_tab(),
            Some(DashboardTabAction::Tab1) => self.set_tab(Tab::Overview),
            Some(DashboardTabAction::Tab2) => self.set_tab(Tab::Sessions),
            Some(DashboardTabAction::Tab3) => self.set_tab(Tab::Agents),
            Some(DashboardTabAction::Tab4) => self.set_tab(Tab::Memories),
            Some(DashboardTabAction::Tab5) => self.set_tab(Tab::Health),
            Some(DashboardTabAction::Tab6) => self.set_tab(Tab::Cost),
            Some(DashboardTabAction::Tab7) => self.set_tab(Tab::Cron),
            Some(DashboardTabAction::Down) => self.move_list_down(),
            Some(DashboardTabAction::Up) => self.move_list_up(),
            Some(DashboardTabAction::OpenDetail) if self.has_detail_pane() => {
                self.detail_open = true;
                self.detail_scroll = 0;
                self.detail_pct = 50;
                self.on_selection_change().await;
            }
            Some(DashboardTabAction::BeginSearch) => {
                self.search_query_saved = self.search_query.clone();
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            Some(DashboardTabAction::RenameAgent) if self.tab == Tab::Agents => {
                self.begin_agent_rename();
            }
            Some(DashboardTabAction::CopyDetail) => {
                self.search_query.clear();
                self.search_buf.clear();
                self.last_poll = None; // re-poll for server-side search
            }
            Some(DashboardTabAction::Refresh) => {
                self.poll_data().await;
            }
            Some(DashboardTabAction::TriggerCron) if self.tab == Tab::Cron => {
                self.trigger_selected_cron();
            }
            Some(DashboardTabAction::JumpEnd) => self.jump_to_end(),
            Some(DashboardTabAction::JumpStart) => self.jump_to_start(),
            _ => {}
        }

        // Health / Cost tabs scroll on j/k too — resolve again so the
        // outer dashboard match (which consumed the chord into Up/Down)
        // doesn't shadow the scroll behaviour.
        let action = DashboardTabAction::from_chord(&key);
        match self.tab {
            Tab::Health => match action {
                Some(DashboardTabAction::Down) => {
                    self.health_scroll = self.health_scroll.saturating_add(1);
                }
                Some(DashboardTabAction::Up) => {
                    self.health_scroll = self.health_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Tab::Cost => match action {
                Some(DashboardTabAction::Down) => {
                    self.cost_scroll = self.cost_scroll.saturating_add(1);
                }
                Some(DashboardTabAction::Up) => {
                    self.cost_scroll = self.cost_scroll.saturating_sub(1);
                }
                _ => {}
            },
            _ => {}
        }

        false
    }

    fn begin_agent_rename(&mut self) {
        let Some(idx) = self.selected_agent_index() else {
            self.agent_rename_message = Some(DashboardMessage {
                text: crate::i18n::t("zc-dashboard-no-agent"),
                level: DashboardMessageLevel::Warn,
            });
            return;
        };
        let from = self.agents[idx].alias.clone();
        self.agent_rename = Some(AgentRenameState {
            from: from.clone(),
            buf: from,
        });
        self.agent_rename_message = None;
    }

    async fn handle_agent_rename_key(&mut self, key: KeyEvent) -> bool {
        use crate::keymap::ConfigEditorAction;
        let action = ConfigEditorAction::from_chord(&key);
        match action {
            Some(ConfigEditorAction::Cancel) => {
                self.agent_rename = None;
                self.agent_rename_message = None;
            }
            Some(ConfigEditorAction::Confirm) => {
                self.apply_agent_rename().await;
            }
            Some(ConfigEditorAction::Backspace) => {
                if let Some(rename) = self.agent_rename.as_mut() {
                    rename.buf.pop();
                }
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && let Some(rename) = self.agent_rename.as_mut()
                {
                    rename.buf.push(c);
                }
            }
        }
        false
    }

    async fn apply_agent_rename(&mut self) {
        let Some(rename) = &self.agent_rename else {
            return;
        };
        let from = rename.from.clone();
        let to = rename.buf.trim().to_string();
        if to.is_empty() {
            self.agent_rename_message = Some(DashboardMessage {
                text: crate::i18n::t("zc-dashboard-agent-rename-empty"),
                level: DashboardMessageLevel::Error,
            });
            return;
        }
        if from == to {
            self.agent_rename = None;
            self.agent_rename_message = Some(DashboardMessage {
                text: crate::i18n::t("zc-dashboard-agent-rename-unchanged"),
                level: DashboardMessageLevel::Info,
            });
            return;
        }

        match self.rpc.config_map_key_rename("agents", &from, &to).await {
            Ok(result) => {
                self.agent_rename = None;
                if let Ok(a) = self.rpc.agents_status().await {
                    self.agents = a.agents;
                    if !self.select_agent_alias(&to) {
                        self.search_query.clear();
                        self.search_buf.clear();
                        self.select_agent_alias(&to);
                    }
                }
                self.last_poll = None;
                if result.renamed {
                    if result.warnings.is_empty() {
                        self.agent_rename_message = Some(DashboardMessage {
                            text: crate::i18n::t_args(
                                "zc-dashboard-agent-rename-success",
                                &[("from", &from), ("to", &to)],
                            ),
                            level: DashboardMessageLevel::Info,
                        });
                    } else {
                        let warnings = result.warnings.join("; ");
                        self.agent_rename_message = Some(DashboardMessage {
                            text: crate::i18n::t_args(
                                "zc-dashboard-agent-rename-success-warnings",
                                &[("from", &from), ("to", &to), ("warnings", &warnings)],
                            ),
                            level: DashboardMessageLevel::Warn,
                        });
                    }
                } else {
                    self.agent_rename_message = Some(DashboardMessage {
                        text: crate::i18n::t("zc-dashboard-agent-rename-unchanged"),
                        level: DashboardMessageLevel::Info,
                    });
                }
            }
            Err(e) => {
                self.agent_rename_message = Some(DashboardMessage {
                    text: crate::i18n::t_args(
                        "zc-dashboard-agent-rename-failed",
                        &[("error", &e.to_string())],
                    ),
                    level: DashboardMessageLevel::Error,
                });
            }
        }
    }

    fn select_agent_alias(&mut self, alias: &str) -> bool {
        let Some(pos) = self
            .filtered_agent_indices()
            .iter()
            .position(|&idx| self.agents[idx].alias == alias)
        else {
            return false;
        };
        self.agent_state.select(Some(pos));
        self.detail_scroll = 0;
        true
    }

    /// Called when the list selection changes while the detail pane is open.
    async fn on_selection_change(&mut self) {
        if self.tab == Tab::Sessions && self.detail_open {
            self.load_session_messages().await;
        }
        if self.tab == Tab::Memories && self.detail_open {
            self.load_memory_detail().await;
        }
        if self.tab == Tab::Cron && self.detail_open {
            self.load_cron_runs().await;
        }
    }

    async fn load_cron_runs(&mut self) {
        let Some(idx) = self.selected_cron_index() else {
            self.cron_runs.clear();
            self.cron_runs_job_id = None;
            self.cron_runs_error = None;
            return;
        };
        let job_id = self.cron_jobs[idx].id.clone();
        self.cron_runs_job_id = Some(job_id.clone());
        match self.rpc.cron_runs(&job_id, Some(20)).await {
            Ok(res) => {
                if self.cron_runs_job_id.as_deref() == Some(job_id.as_str()) {
                    self.cron_runs = res.runs;
                    self.cron_runs_error = None;
                }
            }
            Err(e) => {
                if self.cron_runs_job_id.as_deref() == Some(job_id.as_str()) {
                    self.cron_runs.clear();
                    self.cron_runs_error = Some(e.to_string());
                }
            }
        }
    }

    fn trigger_selected_cron(&mut self) {
        let Some(idx) = self.selected_cron_index() else {
            self.cron_trigger_job_id = None;
            self.cron_trigger_message = Some(crate::i18n::t("zc-dashboard-no-job"));
            return;
        };
        let job_id = self.cron_jobs[idx].id.clone();
        if let Some(inflight_id) = self.cron_trigger_inflight_job_id.as_deref() {
            self.cron_trigger_message = Some(crate::i18n::t_args(
                "zc-dashboard-run-already-running",
                &[("id", inflight_id)],
            ));
            return;
        }

        self.cron_trigger_job_id = Some(job_id.clone());
        self.cron_trigger_message = Some(crate::i18n::t_args(
            "zc-dashboard-run-running",
            &[("id", &job_id)],
        ));
        self.cron_trigger_inflight_job_id = Some(job_id.clone());

        let rpc = Arc::clone(&self.rpc);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.cron_trigger_rx = Some(rx);
        tokio::spawn(async move {
            let result = rpc.cron_trigger(&job_id).await.map_err(|e| e.to_string());
            let jobs = rpc.cron_list().await.ok().map(|c| c.jobs);
            let runs = Some(
                rpc.cron_runs(&job_id, Some(20))
                    .await
                    .map(|r| r.runs)
                    .map_err(|e| e.to_string()),
            );
            let _ = tx.send(CronTriggerUpdate {
                job_id,
                result,
                jobs,
                runs,
            });
        });
        self.last_poll = None;
    }

    fn drain_cron_trigger_updates(&mut self) {
        let Some(mut rx) = self.cron_trigger_rx.take() else {
            return;
        };
        while let Ok(update) = rx.try_recv() {
            if self.cron_trigger_inflight_job_id.as_deref() == Some(update.job_id.as_str()) {
                self.cron_trigger_inflight_job_id = None;
            }
            self.cron_trigger_job_id = Some(update.job_id.clone());
            match update.result {
                Ok(result) => {
                    let status = if result.success {
                        crate::i18n::t("zc-dashboard-run-succeeded")
                    } else {
                        crate::i18n::t("zc-dashboard-run-failed")
                    };
                    let output = result.output.trim();
                    self.cron_trigger_message = Some(if output.is_empty() {
                        format!("{status}: {}", result.id)
                    } else {
                        format!("{status}: {}\n{}", result.id, output)
                    });
                }
                Err(e) => {
                    self.cron_trigger_message = Some(format!(
                        "{}: {e}",
                        crate::i18n::t("zc-dashboard-run-failed")
                    ));
                }
            }
            if let Some(jobs) = update.jobs {
                self.cron_jobs = jobs;
            }
            let selected_matches = self
                .selected_cron_index()
                .and_then(|idx| self.cron_jobs.get(idx))
                .is_some_and(|job| job.id == update.job_id);
            if selected_matches || self.cron_runs_job_id.as_deref() == Some(update.job_id.as_str())
            {
                self.cron_runs_job_id = Some(update.job_id.clone());
                match update.runs {
                    Some(Ok(runs)) => {
                        self.cron_runs = runs;
                        self.cron_runs_error = None;
                    }
                    Some(Err(e)) => {
                        self.cron_runs_error = Some(e);
                    }
                    None => {}
                }
            }
            self.last_poll = None;
        }
        if self.cron_trigger_inflight_job_id.is_some() {
            self.cron_trigger_rx = Some(rx);
        }
    }

    /// Lazy-load the full memory entry for the currently-selected row
    /// via `memory/get`. Stores the result in `self.memory_detail` for
    /// the detail pane to render. Called when the Memory detail pane
    /// opens and after the selection changes while it's still open.
    async fn load_memory_detail(&mut self) {
        let Some(idx) = self.selected_memory_index() else {
            self.memory_detail = None;
            self.memory_detail_key = None;
            return;
        };
        let key = self.memories[idx].key.clone();
        self.memory_detail_key = Some(key.clone());
        match self.rpc.memory_get(&key).await {
            Ok(res) => {
                // Drop stale responses if the user moved the
                // selection while the daemon was answering.
                if self.memory_detail_key.as_deref() == Some(key.as_str()) {
                    self.memory_detail = res.entry;
                }
            }
            Err(_) => {
                if self.memory_detail_key.as_deref() == Some(key.as_str()) {
                    self.memory_detail = None;
                }
            }
        }
    }

    // ── Mouse handling ───────────────────────────────────────────

    pub(crate) fn handle_mouse(&mut self, evt: MouseEvent, _content_area: Rect) {
        use crossterm::event::MouseButton;

        let col = evt.column;
        let row = evt.row;

        match evt.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.tab == Tab::Agents
                    && self.agent_rename.is_none()
                    && let Some(area) = self.agent_alias_rename_area
                    && mouse::in_rect(col, row, area)
                {
                    self.begin_agent_rename();
                    return;
                }

                if self.tab == Tab::Overview
                    && mouse::in_rect(col, row, self.overview_agents_area)
                    && let Some(idx) = mouse::list_click_index(
                        row,
                        self.overview_agents_area,
                        0,
                        self.agents.len(),
                    )
                    && let Some(agent) = self.agents.get(idx)
                {
                    let alias = agent.alias.clone();
                    self.focus_agent(&alias);
                    return;
                }

                // Tab bar clicks
                let labels: Vec<String> = TABS
                    .iter()
                    .map(|t| crate::i18n::t(t.fluent_key()))
                    .collect();
                let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                if let Some(idx) = mouse::tab_click_index(col, row, self.tab_area, &label_refs, 3) {
                    self.set_tab(TABS[idx]);
                    return;
                }

                // List clicks
                if mouse::in_rect(col, row, self.list_area) && self.has_detail_pane() {
                    let count = self.active_list_count();
                    let list_area = self.list_area;
                    let state = self.active_list_state_mut();
                    if let Some(idx) =
                        mouse::list_click_index(row, list_area, state.offset(), count)
                    {
                        state.select(Some(idx));
                        if self.detail_open {
                            self.detail_scroll = 0;
                        }
                        if self.double_click.click(col, row) {
                            self.detail_open = true;
                            self.detail_scroll = 0;
                            self.detail_pct = 50;
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(evt.kind, MouseEventKind::ScrollUp);
                if mouse::in_rect(col, row, self.list_area) {
                    let count = self.active_list_count();
                    let state = self.active_list_state_mut();
                    let i = state.selected().unwrap_or(0);
                    let new_i = mouse::list_scroll(i, count, up, 3);
                    state.select(Some(new_i));
                } else if let Some(detail) = self.detail_area
                    && mouse::in_rect(col, row, detail)
                {
                    if up {
                        self.detail_scroll = self.detail_scroll.saturating_sub(3);
                    } else {
                        self.detail_scroll = self.detail_scroll.saturating_add(3);
                    }
                }
            }
            _ => {}
        }
    }

    // ── Navigation helpers ───────────────────────────────────────

    fn next_tab(&mut self) {
        let idx = TABS.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(TABS[(idx + 1) % TABS.len()]);
    }

    fn prev_tab(&mut self) {
        let idx = TABS.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(TABS[(idx + TABS.len() - 1) % TABS.len()]);
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.on_tab_change();
    }

    fn on_tab_change(&mut self) {
        self.detail_open = false;
        self.detail_scroll = 0;
        self.health_scroll = 0;
        self.cost_scroll = 0;
        self.cron_runs.clear();
        self.cron_runs_job_id = None;
        self.cron_runs_error = None;
        self.cron_trigger_job_id = None;
        self.cron_trigger_message = None;
        self.agent_rename = None;
        self.agent_rename_message = None;
        self.agent_alias_rename_area = None;
        // Force immediate data fetch for new tab
        self.last_poll = None;
    }

    fn focus_agent(&mut self, alias: &str) {
        self.set_tab(Tab::Agents);
        self.search_query.clear();
        self.search_buf.clear();
        self.search_query_saved.clear();

        let filtered = self.filtered_agent_indices();
        if let Some(pos) = filtered
            .iter()
            .position(|&idx| self.agents[idx].alias == alias)
        {
            self.agent_state.select(Some(pos));
        }
        self.detail_open = true;
        self.detail_scroll = 0;
        self.detail_pct = 50;
    }

    fn has_detail_pane(&self) -> bool {
        matches!(
            self.tab,
            Tab::Sessions | Tab::Agents | Tab::Memories | Tab::Cron
        )
    }

    fn cron_action_hint(&self) -> String {
        use crate::keymap::{DashboardTabAction as D, action_key_labels};

        let run = action_key_labels(D::TriggerCron).join("/");
        let refresh = action_key_labels(D::Refresh).join("/");
        crate::i18n::t_args(
            "zc-dashboard-cron-action-hint",
            &[("run", &run), ("refresh", &refresh)],
        )
    }

    fn active_list_state_mut(&mut self) -> &mut ListState {
        match self.tab {
            Tab::Sessions => &mut self.session_state,
            Tab::Agents => &mut self.agent_state,
            Tab::Memories => &mut self.memory_state,
            Tab::Cron => &mut self.cron_state,
            _ => &mut self.session_state, // fallback
        }
    }

    fn active_list_count(&self) -> usize {
        match self.tab {
            Tab::Sessions => self.filtered_session_indices().len(),
            Tab::Agents => self.filtered_agent_indices().len(),
            Tab::Memories => self.filtered_memory_indices().len(),
            Tab::Cron => self.filtered_cron_indices().len(),
            _ => 0,
        }
    }

    fn move_list_down(&mut self) {
        let count = self.active_list_count();
        if count == 0 {
            return;
        }
        let state = self.active_list_state_mut();
        match state.selected() {
            None => state.select(Some(0)),
            Some(i) if i + 1 < count => state.select(Some(i + 1)),
            _ => {}
        }
    }

    fn move_list_up(&mut self) {
        let count = self.active_list_count();
        if count == 0 {
            return;
        }
        let state = self.active_list_state_mut();
        match state.selected() {
            None => state.select(Some(0)),
            Some(i) if i > 0 => state.select(Some(i - 1)),
            _ => {}
        }
    }

    fn jump_to_end(&mut self) {
        let count = self.active_list_count();
        if count > 0 {
            self.active_list_state_mut().select(Some(count - 1));
        }
    }

    fn jump_to_start(&mut self) {
        let count = self.active_list_count();
        if count > 0 {
            self.active_list_state_mut().select(Some(0));
        }
    }

    /// Whether the pane is in a text-input mode (search bar or rename prompt active).
    pub(crate) fn wants_text_input(&self) -> bool {
        self.search_active || self.agent_rename.is_some()
    }

    pub(crate) fn handle_paste(&mut self, text: &str) {
        if let Some(rename) = self.agent_rename.as_mut() {
            rename.buf.push_str(text);
            return;
        }
        if !self.search_active {
            return;
        }
        self.search_buf.push_str(text);
        if !matches!(self.tab, Tab::Sessions | Tab::Memories) {
            self.search_query = self.search_buf.clone();
        }
    }
}

impl crate::widgets::HelpContext for Dashboard {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::help::entries_for;
        use crate::keymap::DashboardTabAction as D;
        use crate::widgets::{HelpEntry as E, HelpNode};

        if self.search_active {
            return HelpNode::entries(entries_for([
                crate::keymap::SearchBoxAction::Accept,
                crate::keymap::SearchBoxAction::Cancel,
            ]));
        }

        if self.agent_rename.is_some() {
            return HelpNode::entries(entries_for([
                crate::keymap::SearchBoxAction::Accept,
                crate::keymap::SearchBoxAction::Cancel,
            ]));
        }

        // Global tab-switching always available.
        let tab_nav = entries_for([D::NextTab, D::PrevTab, D::Tab1, D::Refresh]);

        if self.detail_open {
            let mut detail = vec![
                D::CloseDetail,
                D::Up,
                D::Down,
                D::DetailScrollUp,
                D::DetailScrollDown,
                D::DetailWidenLeft,
                D::DetailWidenRight,
                D::Refresh,
                D::BeginSearch,
            ];
            if self.tab == Tab::Sessions {
                detail.push(D::KillSession);
            } else if self.tab == Tab::Cron {
                detail.push(D::TriggerCron);
            } else if self.tab == Tab::Agents {
                detail.push(D::RenameAgent);
            }
            return HelpNode::entries(entries_for(detail));
        }

        // Per-tab bindings — only show what actually works on this tab.
        let mut entries = tab_nav;
        match self.tab {
            Tab::Overview | Tab::Health | Tab::Cost => {
                // Read-only display tabs — no list, no detail, no search.
            }
            Tab::Sessions | Tab::Agents | Tab::Memories | Tab::Cron => {
                entries.push(E::spacer());
                let mut tab_actions = vec![
                    D::Up,
                    D::Down,
                    D::JumpEnd,
                    D::JumpStart,
                    D::OpenDetail,
                    D::BeginSearch,
                ];
                if self.tab == Tab::Cron {
                    tab_actions.push(D::TriggerCron);
                }
                if self.tab == Tab::Agents {
                    tab_actions.push(D::RenameAgent);
                }
                entries.extend(entries_for(tab_actions));
            }
        }
        HelpNode::entries(entries)
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn detail_line(label: &str, value: &str) -> Line<'static> {
    let pad = 12usize.saturating_sub(label.len());
    Line::from(vec![
        Span::styled(format!("{label}{}", " ".repeat(pad)), theme::dim_style()),
        Span::styled(value.to_string(), theme::body_style()),
    ])
}

fn cron_run_status_style(status: &str) -> Style {
    if status.eq_ignore_ascii_case("ok") || status.eq_ignore_ascii_case("success") {
        Style::default().fg(Color::Green)
    } else if status.eq_ignore_ascii_case("error") || status.eq_ignore_ascii_case("failed") {
        theme::error_style()
    } else if status.eq_ignore_ascii_case("degraded") || status.eq_ignore_ascii_case("warning") {
        theme::warn_style()
    } else {
        theme::body_style()
    }
}

fn format_duration_ms(duration_ms: Option<i64>) -> String {
    let Some(ms) = duration_ms else {
        return "-".to_string();
    };
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        format!("{:.1}m", ms as f64 / 60_000.0)
    }
}

fn detail_action_line(label: &str, value: &str, action: &str) -> Line<'static> {
    let pad = 12usize.saturating_sub(label.len());
    Line::from(vec![
        Span::styled(format!("{label}{}", " ".repeat(pad)), theme::dim_style()),
        Span::styled(value.to_string(), theme::body_style()),
        Span::styled("  ", theme::dim_style()),
        Span::styled(format!("[{action}]"), theme::dim_style()),
    ])
}

fn truncate(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.chars().count() > max {
        let truncated: String = first_line.chars().take(max).collect();
        format!("{truncated}...")
    } else {
        first_line.to_string()
    }
}

fn format_relative_time(epoch_secs: i64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let delta = (now - epoch_secs).max(0) as u64;
    if delta < 60 {
        "just now".to_string()
    } else if delta < 3600 {
        let m = delta / 60;
        format!("{m}m ago")
    } else if delta < 86400 {
        let h = delta / 3600;
        format!("{h}h ago")
    } else {
        let d = delta / 86400;
        format!("{d}d ago")
    }
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// `(label, from, to)` RFC3339 windows for day / month / quarter / YTD in local
/// time, period-to-date. Matches the conventional CLI report windows so the
/// dashboard and CLI agree on what each period means. The daemon parses these
/// bounds and converts to UTC.
fn cost_period_windows() -> Vec<(String, String, String)> {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    let to = now.to_rfc3339();
    let start = |month: u32, day: u32| -> String {
        Local
            .with_ymd_and_hms(now.year(), month, day, 0, 0, 0)
            .single()
            .unwrap_or(now)
            .to_rfc3339()
    };
    let quarter = (now.month() - 1) / 3; // 0..=3
    vec![
        (
            crate::i18n::t("zc-dashboard-period-today"),
            start(now.month(), now.day()),
            to.clone(),
        ),
        (
            crate::i18n::t("zc-dashboard-period-month"),
            start(now.month(), 1),
            to.clone(),
        ),
        (
            format!(
                "{}{}",
                crate::i18n::t("zc-dashboard-period-quarter-prefix"),
                quarter + 1
            ),
            start(quarter * 3 + 1, 1),
            to.clone(),
        ),
        (
            format!(
                "{} {}",
                crate::i18n::t("zc-dashboard-period-ytd"),
                now.year()
            ),
            start(1, 1),
            to,
        ),
    ]
}

fn org_section_lines(
    org: Option<&crate::client::OrgCost>,
    err: Option<&str>,
    frac: f64,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(err) = err {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-section-org"),
            theme::heading_style(),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            theme::warn_style(),
        )));
    } else if let Some(org) = org {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-section-org"),
            theme::heading_style(),
        )));
        let org_label = org
            .org_label
            .clone()
            .unwrap_or_else(|| crate::i18n::t("zc-dashboard-org-name"));
        if let Some(ref scope) = org.org {
            lines.push(org_scope_line(&org_label, scope, frac));
        }
        if let Some(ref scope) = org.personal {
            lines.push(org_scope_line(
                &crate::i18n::t("zc-dashboard-org-personal"),
                scope,
                frac,
            ));
        }
        if !org.generated.is_empty() || org.year != 0 {
            let mut note = String::from("  ");
            if org.year != 0 {
                note.push_str(&format!(
                    "{}{} ",
                    crate::i18n::t("zc-dashboard-org-fy-prefix"),
                    org.year
                ));
            }
            if !org.generated.is_empty() {
                note.push_str(&format!(
                    "{} {}",
                    crate::i18n::t("zc-dashboard-org-asof"),
                    org.generated
                ));
            }
            lines.push(Line::from(Span::styled(note, theme::dim_style())));
        }
    }
    lines
}

fn org_scope_line(label: &str, scope: &crate::client::OrgScopeStat, frac: f64) -> Line<'static> {
    let runrate = if scope.monthly.len() >= 2 {
        Some(scope.monthly[scope.monthly.len() - 2].cost_usd * 12.0)
    } else {
        scope.monthly.last().map(|m| m.cost_usd * 12.0)
    };
    let proj = runrate.unwrap_or(if frac > 0.0 {
        scope.ytd_cost_usd / frac
    } else {
        0.0
    });
    Line::from(vec![
        Span::styled(format!("  {label:<14}"), theme::body_style()),
        Span::styled(
            format!(
                "{} ${:>14.2}",
                crate::i18n::t("zc-dashboard-period-ytd"),
                scope.ytd_cost_usd
            ),
            theme::accent_style(),
        ),
        Span::styled(
            format!(
                "  {:>10} {}",
                format_tokens(scope.ytd_tokens),
                crate::i18n::t("zc-dashboard-org-tok")
            ),
            theme::dim_style(),
        ),
        Span::styled(
            format!(
                "   {} ${proj:>14.2}",
                crate::i18n::t("zc-dashboard-org-projyr")
            ),
            theme::warn_style(),
        ),
    ])
}

/// Fraction of the current local year elapsed (leap-year aware). Used to scale
/// a billed YTD into a naive full-year projection, matching the CLI report.
fn frac_year_elapsed() -> f64 {
    use chrono::{Datelike, Local};
    let now = Local::now();
    let y = now.year();
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = if leap { 366.0 } else { 365.0 };
    now.ordinal() as f64 / days
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

fn overview_status_lines(
    connect_label: &str,
    insecure_tls: bool,
    status: &StatusResult,
    health: Option<&serde_json::Value>,
    code_cwd: Option<&str>,
    chat_cwd: Option<&str>,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{:<11}", crate::i18n::t("zc-dashboard-label-daemon")),
            theme::dim_style(),
        ),
        Span::styled(
            daemon_label(connect_label).to_string(),
            theme::accent_style(),
        ),
    ])];

    if is_local_connection(connect_label) {
        if let Some(socket) = status
            .local_ipc_endpoint
            .as_deref()
            .or_else(|| local_socket_label(connect_label))
        {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-dashboard-label-socket")),
                    theme::dim_style(),
                ),
                Span::styled(socket.to_string(), theme::body_style()),
            ]));
        }
    } else if is_remote_connection(connect_label) {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-dashboard-label-endpoint")),
                theme::dim_style(),
            ),
            Span::styled(remote_endpoint_label(connect_label), theme::body_style()),
        ]));
    }

    if insecure_tls {
        lines.push(Line::from(Span::styled(
            crate::i18n::t("zc-dashboard-label-insecure-tls"),
            theme::warn_style(),
        )));
    }

    lines.extend([
        Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-dashboard-label-server")),
                theme::dim_style(),
            ),
            Span::styled(format!("v{}", status.server_version), theme::body_style()),
        ]),
        Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-dashboard-label-protocol")),
                theme::dim_style(),
            ),
            Span::styled(format!("{}", status.protocol_version), theme::body_style()),
        ]),
    ]);

    if let Some(config_path) = status
        .config_file
        .as_deref()
        .or(status.config_dir.as_deref())
    {
        let mut spans = vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-dashboard-label-config")),
                theme::dim_style(),
            ),
            Span::styled(config_path.to_string(), theme::body_style()),
        ];
        if let Some(config_kind) = status.config_kind.as_ref() {
            spans.extend([
                Span::styled(" (", theme::body_style()),
                Span::styled(
                    config_kind_label(config_kind),
                    config_kind_style(config_kind),
                ),
                Span::styled(")", theme::body_style()),
            ]);
        }
        lines.push(Line::from(spans));
    }

    lines.extend(workspace_lines(code_cwd, chat_cwd));

    if let Some(h) = health
        && let Some(process) = h.get("process")
    {
        if let Some(rss) = process.get("rss_bytes").and_then(|v| v.as_u64())
            && rss > 0
        {
            let total = process
                .get("system_ram_total_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let rss_str = format_bytes(rss);
            let val = if total > 0 {
                let pct = (rss as f64 / total as f64) * 100.0;
                format!("{rss_str} / {} ({pct:.1}%)", format_bytes(total))
            } else {
                rss_str
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-dashboard-label-memory")),
                    theme::dim_style(),
                ),
                Span::styled(val, theme::body_style()),
            ]));
        }
        if let Some(cpu) = process.get("cpu_percent").and_then(|v| v.as_f64()) {
            let ncpu = process
                .get("num_cpus")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let val = if ncpu > 0 {
                crate::i18n::t_args(
                    "zc-dashboard-cpu-with-cores",
                    &[("cpu", &format!("{cpu:.1}%")), ("cores", &ncpu.to_string())],
                )
            } else {
                format!("{cpu:.1}%")
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-dashboard-label-cpu")),
                    theme::dim_style(),
                ),
                Span::styled(val, theme::body_style()),
            ]));
        } else {
            let ncpu = process
                .get("num_cpus")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let val = if ncpu > 0 {
                crate::i18n::t_args(
                    "zc-dashboard-cpu-with-cores",
                    &[
                        ("cpu", &crate::i18n::t("zc-dashboard-loading")),
                        ("cores", &ncpu.to_string()),
                    ],
                )
            } else {
                crate::i18n::t("zc-dashboard-loading")
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", crate::i18n::t("zc-dashboard-label-cpu")),
                    theme::dim_style(),
                ),
                Span::styled(val, theme::dim_style()),
            ]));
        }
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<11}", crate::i18n::t("zc-dashboard-label-cpu")),
                theme::dim_style(),
            ),
            Span::styled(crate::i18n::t("zc-dashboard-loading"), theme::dim_style()),
        ]));
    }

    lines
}

fn config_kind_label(kind: &str) -> String {
    match kind {
        "default" => crate::i18n::t("zc-dashboard-config-kind-default"),
        "custom" => crate::i18n::t("zc-dashboard-config-kind-custom"),
        "temporary" => crate::i18n::t("zc-dashboard-config-kind-temporary"),
        other => other.to_string(),
    }
}

fn config_kind_style(kind: &str) -> Style {
    match kind {
        "default" => theme::body_style(),
        "custom" => theme::accent_style(),
        "temporary" => theme::warn_style(),
        _ => theme::body_style(),
    }
}

fn daemon_label(connect_label: &str) -> String {
    if is_local_connection(connect_label) {
        crate::i18n::t("zc-dashboard-daemon-local")
    } else if is_remote_connection(connect_label) {
        crate::i18n::t("zc-dashboard-daemon-remote")
    } else {
        connect_label.to_string()
    }
}

fn is_local_connection(connect_label: &str) -> bool {
    connect_label.starts_with("local:")
}

fn is_remote_connection(connect_label: &str) -> bool {
    connect_label.starts_with("ws://") || connect_label.starts_with("wss://")
}

fn local_socket_label(connect_label: &str) -> Option<&str> {
    connect_label.strip_prefix("local:")
}

fn remote_endpoint_label(connect_label: &str) -> String {
    let Ok(url) = url::Url::parse(connect_label) else {
        return connect_label.to_string();
    };
    if !matches!(url.scheme(), "ws" | "wss") {
        return connect_label.to_string();
    }

    let mut safe = String::from(url.scheme());
    safe.push_str("://");
    if let Some(host) = url.host() {
        safe.push_str(&host.to_string());
    }
    if let Some(port) = url.port() {
        safe.push(':');
        safe.push_str(&port.to_string());
    }
    safe
}

fn workspace_lines(code_cwd: Option<&str>, chat_cwd: Option<&str>) -> Vec<Line<'static>> {
    match (code_cwd, chat_cwd) {
        (Some(code), Some(chat)) if code != chat => vec![
            status_row("zc-dashboard-label-code-cwd", code, theme::body_style()),
            status_row("zc-dashboard-label-chat-cwd", chat, theme::body_style()),
        ],
        (Some(cwd), _) | (_, Some(cwd)) => {
            vec![status_row(
                "zc-dashboard-label-workspace",
                cwd,
                theme::body_style(),
            )]
        }
        _ => Vec::new(),
    }
}

fn status_row(label_key: &str, value: &str, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{:<11}", crate::i18n::t(label_key)),
            theme::dim_style(),
        ),
        Span::styled(value.to_string(), style),
    ])
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}G", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}M", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("|")
    }

    fn status_fixture() -> StatusResult {
        StatusResult {
            server_version: "0.8.2".into(),
            protocol_version: 1,
            active_sessions: 2,
            config_dir: Some("/tmp/zc-profile".into()),
            config_file: Some("/tmp/zc-profile/config.toml".into()),
            config_kind: Some("temporary".into()),
            local_ipc_endpoint: Some("/tmp/zc-profile/data/daemon.sock".into()),
        }
    }

    #[test]
    fn overview_status_lines_render_local_socket_not_endpoint() {
        let status = status_fixture();
        let text = lines_text(&overview_status_lines(
            "local:/fallback.sock",
            false,
            &status,
            None,
            None,
            None,
        ));

        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-daemon-local")),
            "daemon label: {text}"
        );
        assert!(
            text.contains("/tmp/zc-profile/data/daemon.sock"),
            "local socket: {text}"
        );
        assert!(
            !text.contains("/fallback.sock"),
            "daemon socket wins: {text}"
        );
        assert!(
            !text.contains("wss://"),
            "must not render remote endpoint: {text}"
        );
    }

    #[test]
    fn overview_status_lines_render_remote_endpoint_and_insecure_tls() {
        let status = status_fixture();
        let text = lines_text(&overview_status_lines(
            "wss://zero.example.test:9781",
            true,
            &status,
            None,
            None,
            None,
        ));

        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-daemon-remote")),
            "daemon label: {text}"
        );
        assert!(
            text.contains("wss://zero.example.test:9781"),
            "remote endpoint: {text}"
        );
        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-label-insecure-tls")),
            "insecure TLS warning: {text}"
        );
        assert!(
            !text.contains("/tmp/zc-profile/data/daemon.sock"),
            "remote view must not label socket as endpoint: {text}"
        );
    }

    #[test]
    fn overview_status_lines_redacts_remote_endpoint_credentials() {
        let status = status_fixture();
        let text = lines_text(&overview_status_lines(
            "wss://user:secret@zero.example.test:9781/rpc?token=abc#frag",
            false,
            &status,
            None,
            None,
            None,
        ));

        assert!(
            text.contains("wss://zero.example.test:9781"),
            "safe endpoint: {text}"
        );
        assert!(!text.contains("/rpc"), "must redact path: {text}");
        assert!(!text.contains("user"), "must redact userinfo: {text}");
        assert!(!text.contains("secret"), "must redact password: {text}");
        assert!(!text.contains("token"), "must redact query: {text}");
        assert!(!text.contains("frag"), "must redact fragment: {text}");
    }

    #[test]
    fn overview_status_lines_collapse_shared_workspace() {
        let status = status_fixture();
        let text = lines_text(&overview_status_lines(
            "local:/daemon.sock",
            false,
            &status,
            None,
            Some("/work/shared"),
            Some("/work/shared"),
        ));

        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-label-workspace")),
            "shared workspace row: {text}"
        );
        assert!(!text.contains(&crate::i18n::t("zc-dashboard-label-code-cwd")));
        assert!(!text.contains(&crate::i18n::t("zc-dashboard-label-chat-cwd")));
        assert!(text.contains("/work/shared"), "workspace value: {text}");
    }

    #[test]
    fn overview_status_lines_split_different_workspaces() {
        let status = status_fixture();
        let text = lines_text(&overview_status_lines(
            "local:/daemon.sock",
            false,
            &status,
            None,
            Some("/work/code"),
            Some("/work/chat"),
        ));

        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-label-code-cwd")),
            "code cwd row: {text}"
        );
        assert!(
            text.contains(&crate::i18n::t("zc-dashboard-label-chat-cwd")),
            "chat cwd row: {text}"
        );
        assert!(text.contains("/work/code"), "code cwd value: {text}");
        assert!(text.contains("/work/chat"), "chat cwd value: {text}");
    }

    #[test]
    fn overview_status_lines_render_cpu_loading_before_sample() {
        let status = status_fixture();
        let health = serde_json::json!({
            "process": {
                "rss_bytes": 1_048_576_u64,
                "system_ram_total_bytes": 4_194_304_u64,
                "num_cpus": 8_u64
            }
        });
        let text = lines_text(&overview_status_lines(
            "local:/daemon.sock",
            false,
            &status,
            Some(&health),
            None,
            None,
        ));

        assert!(text.contains("1.0M / 4.0M (25.0%)"), "memory: {text}");
        assert!(
            text.contains(&crate::i18n::t_args(
                "zc-dashboard-cpu-with-cores",
                &[
                    ("cpu", &crate::i18n::t("zc-dashboard-loading")),
                    ("cores", "8"),
                ],
            )),
            "cpu loading: {text}"
        );
    }

    #[test]
    fn overview_status_lines_render_cpu_value_with_core_count() {
        let status = status_fixture();
        let health = serde_json::json!({
            "process": {
                "cpu_percent": 12.345_f64,
                "num_cpus": 8_u64
            }
        });
        let text = lines_text(&overview_status_lines(
            "local:/daemon.sock",
            false,
            &status,
            Some(&health),
            None,
            None,
        ));

        assert!(
            text.contains(&crate::i18n::t_args(
                "zc-dashboard-cpu-with-cores",
                &[("cpu", "12.3%"), ("cores", "8")],
            )),
            "cpu value: {text}"
        );
    }

    #[test]
    fn org_section_absent_renders_nothing() {
        // Absent snapshot (cost/org -> Ok(None), no error): the org section
        // is omitted entirely so the Cost tab shows only local usage.
        assert!(org_section_lines(None, None, 0.5).is_empty());
    }

    #[test]
    fn org_section_broken_surfaces_error_not_silence() {
        // Present-but-broken snapshot (cost/org RPC error): must surface a
        // visible warning rather than render identically to an absent one.
        let lines = org_section_lines(None, Some("snapshot unreadable"), 0.5);
        assert!(!lines.is_empty(), "broken snapshot must render a section");
        let text = lines_text(&lines);
        assert!(text.contains("snapshot unreadable"), "got: {text}");
    }

    #[test]
    fn org_section_present_renders_billed_rows() {
        let org = crate::client::OrgCost {
            year: 2026,
            generated: "2026-06-29".into(),
            org_label: Some("Acme".into()),
            org: Some(crate::client::OrgScopeStat {
                ytd_cost_usd: 1234.0,
                ytd_tokens: 5_000_000,
                monthly: vec![],
            }),
            personal: None,
        };
        let text = lines_text(&org_section_lines(Some(&org), None, 0.5));
        assert!(text.contains("Acme"), "org label: {text}");
        assert!(text.contains("1234"), "YTD cost: {text}");
    }

    #[test]
    fn truncate_does_not_panic_on_multibyte_boundary() {
        // Regression: byte-index slicing panicked when the byte length exceeded
        // `max` but `max` landed inside a multi-byte char. This 35-char CJK
        // string is 105 bytes, so `&s[..40]` used to panic mid-character even
        // though the string is well under the 40-*character* budget.
        let s = "用户询问桌面文件列表，助手列出了桌面上的文件夹和文件，包括名称和大小。";
        assert_eq!(s.chars().count(), 35);
        assert!(s.len() > 40);
        // Under the character budget -> returned unchanged, no panic.
        assert_eq!(truncate(s, 40), s);
    }

    #[test]
    fn truncate_multibyte_at_char_boundary() {
        // Over the character budget: truncates on a char boundary and appends
        // the ellipsis without panicking.
        let s = "一二三四五六七八九十甲乙丙丁";
        let result = truncate(s, 10);
        assert_eq!(result, "一二三四五六七八九十...");
        assert_eq!(result.chars().count(), 13);
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // 10 CJK chars (30 bytes) must not be truncated at a max of 20 chars.
        let s = "一二三四五六七八九十";
        assert_eq!(truncate(s, 20), s);
    }

    #[test]
    fn truncate_short_ascii_unchanged() {
        assert_eq!(truncate("hello", 40), "hello");
    }

    #[test]
    fn truncate_long_ascii() {
        let s = "a".repeat(50);
        let result = truncate(&s, 40);
        assert_eq!(result, format!("{}...", "a".repeat(40)));
    }

    #[test]
    fn truncate_uses_first_line_only() {
        assert_eq!(truncate("first\nsecond", 40), "first");
    }

    #[test]
    fn format_duration_ms_scales_units() {
        assert_eq!(format_duration_ms(None), "-");
        assert_eq!(format_duration_ms(Some(42)), "42ms");
        assert_eq!(format_duration_ms(Some(1_500)), "1.5s");
        assert_eq!(format_duration_ms(Some(90_000)), "1.5m");
    }
}
