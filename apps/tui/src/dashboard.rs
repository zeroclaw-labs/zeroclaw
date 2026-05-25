use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::client::{
    AgentStatusEntry, CostSummaryResult, CronJobEntry, CronSchedule, MemoryEntryResult,
    MessageEntry, RpcClient, SessionEntry, StatusResult, TuiListEntry,
};
use crate::mouse;
use crate::theme;

// ── Constants ────────────────────────────────────────────────────

const POLL_INTERVAL_SECS: u64 = 5;

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
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Sessions => "Sessions",
            Self::Agents => "Agents",
            Self::Memories => "Memories",
            Self::Health => "Health",
            Self::Cost => "Cost",
            Self::Cron => "Cron",
        }
    }
}

// ── Dashboard ────────────────────────────────────────────────────

pub(crate) struct Dashboard<'a> {
    rpc: &'a RpcClient,
    connect_label: String,
    tab: Tab,
    last_poll: Option<Instant>,
    // Data
    status: Option<StatusResult>,
    health: Option<serde_json::Value>,
    sessions: Vec<SessionEntry>,
    agents: Vec<AgentStatusEntry>,
    cost: Option<CostSummaryResult>,
    cron_jobs: Vec<CronJobEntry>,
    memories: Vec<MemoryEntryResult>,
    memory_error: Option<String>,
    tuis: Vec<TuiListEntry>,
    // Session messages (loaded on demand)
    session_messages: Vec<MessageEntry>,
    session_messages_id: Option<String>,
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
    // Layout tracking for mouse
    tab_area: Rect,
    list_area: Rect,
    detail_area: Option<Rect>,
    double_click: mouse::DoubleClickTracker,
}

impl<'a> Dashboard<'a> {
    pub(crate) fn new(rpc: &'a RpcClient, connect_label: &str) -> Self {
        Self {
            rpc,
            connect_label: connect_label.to_string(),
            tab: Tab::Overview,
            last_poll: None,
            status: None,
            health: None,
            sessions: Vec::new(),
            agents: Vec::new(),
            cost: None,
            cron_jobs: Vec::new(),
            memories: Vec::new(),
            memory_error: None,
            tuis: Vec::new(),
            session_messages: Vec::new(),
            session_messages_id: None,
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
            tab_area: Rect::default(),
            list_area: Rect::default(),
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

    async fn poll_data(&mut self) {
        self.last_poll = Some(Instant::now());

        // Always fetch status and health (health feeds the status line
        // on every tab — RAM/CPU display).
        if let Ok(s) = self.rpc.status().await {
            self.status = Some(s);
        }
        if let Ok(h) = self.rpc.health().await {
            self.health = Some(h);
        }

        // Fetch tab-specific data
        match self.tab {
            Tab::Overview => {
                if let Ok(c) = self.rpc.cost_query(None).await {
                    self.cost = Some(c);
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
                            self.memory_error = Some("Memory subsystem not configured".to_string());
                        } else {
                            self.memory_error = Some(msg);
                        }
                    }
                }
            }
            Tab::Health => {} // health already fetched above
            Tab::Cost => {
                if let Ok(c) = self.rpc.cost_query(None).await {
                    self.cost = Some(c);
                }
            }
            Tab::Cron => {
                if let Ok(c) = self.rpc.cron_list().await {
                    self.cron_jobs = c.jobs;
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
        if let Ok(result) = self.rpc.session_messages(&sid).await {
            self.session_messages = result.messages;
            self.session_messages_id = Some(sid);
        }
    }

    // ── Drawing ──────────────────────────────────────────────────

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame, area: Rect) {
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
            Tab::Overview => self.draw_overview(frame, chunks[2]),
            Tab::Sessions => self.draw_sessions(frame, chunks[2]),
            Tab::Agents => self.draw_agents(frame, chunks[2]),
            Tab::Memories => self.draw_memories(frame, chunks[2]),
            Tab::Health => self.draw_health(frame, chunks[2]),
            Tab::Cost => self.draw_cost(frame, chunks[2]),
            Tab::Cron => self.draw_cron(frame, chunks[2]),
        }

        // Footer: ?=help hint at bottom-left.
        frame.render_widget(
            Paragraph::new(Span::styled(" ?=help", theme::dim_style())),
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
            spans.push(Span::styled(tab.label(), style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_status_line(&self, frame: &mut ratatui::Frame, area: Rect) {
        let version = self
            .status
            .as_ref()
            .map(|s| s.server_version.as_str())
            .unwrap_or("?");
        let active = self.status.as_ref().map(|s| s.active_sessions).unwrap_or(0);
        let help = if self.search_active {
            "Enter:apply  Esc:cancel"
        } else {
            ""
        };

        // Process stats from health
        let process_info = self.process_stats_line();

        let line = if self.search_active {
            Line::from(vec![
                Span::styled(
                    format!(" v{version} sessions:{active}{process_info} "),
                    theme::dim_style(),
                ),
                Span::styled(" /", theme::accent_style()),
                Span::styled(&self.search_buf, theme::input_style()),
                Span::styled("\u{2588}", theme::accent_style()),
            ])
        } else {
            let mut spans = vec![Span::styled(
                format!(" v{version} sessions:{active}{process_info} "),
                theme::dim_style(),
            )];
            if !self.search_query.is_empty() {
                spans.push(Span::styled("search:", theme::dim_style()));
                spans.push(Span::styled(&self.search_query, theme::accent_style()));
                spans.push(Span::styled(" ", theme::dim_style()));
            }
            spans.push(Span::styled(help, theme::dim_style()));
            Line::from(spans)
        };

        frame.render_widget(Paragraph::new(line), area);
    }

    /// Build a compact process stats string from the health data.
    fn process_stats_line(&self) -> String {
        let Some(ref h) = self.health else {
            return String::new();
        };
        let Some(process) = h.get("process") else {
            return String::new();
        };
        let mut parts = Vec::new();
        if let Some(rss) = process.get("rss_bytes").and_then(|v| v.as_u64())
            && rss > 0
        {
            let total = process
                .get("system_ram_total_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let rss_str = format_bytes(rss);
            if total > 0 {
                let pct = (rss as f64 / total as f64) * 100.0;
                parts.push(format!(" ram:{rss_str}({pct:.0}%)"));
            } else {
                parts.push(format!(" ram:{rss_str}"));
            }
        }
        if let Some(cpu) = process.get("cpu_percent").and_then(|v| v.as_f64()) {
            parts.push(format!(" cpu:{cpu:.1}%"));
        }
        parts.join("")
    }

    // ── Overview tab ─────────────────────────────────────────────

    fn draw_overview(&self, frame: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),                            // status box
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

        if let Some(ref s) = self.status {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("Connected  ", theme::dim_style()),
                    Span::styled(&self.connect_label, theme::accent_style()),
                ]),
                Line::from(vec![
                    Span::styled("Server     ", theme::dim_style()),
                    Span::styled(format!("v{}", s.server_version), theme::body_style()),
                ]),
                Line::from(vec![
                    Span::styled("Protocol   ", theme::dim_style()),
                    Span::styled(format!("{}", s.protocol_version), theme::body_style()),
                ]),
                Line::from(vec![
                    Span::styled("Sessions   ", theme::dim_style()),
                    Span::styled(format!("{}", s.active_sessions), theme::accent_style()),
                ]),
            ];

            // Process stats from health
            if let Some(ref h) = self.health
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
                        Span::styled("Memory     ", theme::dim_style()),
                        Span::styled(val, theme::body_style()),
                    ]));
                }
                if let Some(cpu) = process.get("cpu_percent").and_then(|v| v.as_f64()) {
                    let ncpu = process
                        .get("num_cpus")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let val = if ncpu > 0 {
                        format!("{cpu:.1}% ({ncpu} cores)")
                    } else {
                        format!("{cpu:.1}%")
                    };
                    lines.push(Line::from(vec![
                        Span::styled("CPU        ", theme::dim_style()),
                        Span::styled(val, theme::body_style()),
                    ]));
                }
            }

            frame.render_widget(Paragraph::new(lines), inner);
        }

        // Agents
        let agents_block = Block::default()
            .title(Span::styled(" Agents ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let agents_inner = agents_block.inner(chunks[1]);
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
                        format!("  ({} active)", a.active_sessions),
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
                Paragraph::new(Span::styled("No TUIs connected", theme::dim_style())),
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

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(
                        format!(" Sessions ({}) ", filtered.len()),
                        theme::title_style(),
                    ))
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
                Paragraph::new(Span::styled("No session selected", theme::dim_style())),
                inner,
            );
            return;
        };

        let s = &self.sessions[idx];
        let mut lines = vec![
            detail_line("ID", &s.session_id),
            detail_line("Key", &s.session_key),
            detail_line("Agent", s.agent_alias.as_deref().unwrap_or("\u{2014}")),
            detail_line("Channel", s.channel_id.as_deref().unwrap_or("\u{2014}")),
            detail_line("Name", s.name.as_deref().unwrap_or("\u{2014}")),
            detail_line("Messages", &s.message_count.to_string()),
            detail_line("Created", &s.created_at),
            detail_line("Activity", &s.last_activity),
        ];

        // Show message history if loaded
        if self.session_messages_id.as_deref() == Some(&s.session_id) {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("Message History ({})", self.session_messages.len()),
                theme::heading_style(),
            )));
            lines.push(Line::from(""));
            for msg in &self.session_messages {
                let role_style = match msg.role.as_str() {
                    "user" => Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                    "assistant" => Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                    "system" => theme::dim_style().add_modifier(Modifier::BOLD),
                    _ => theme::body_style().add_modifier(Modifier::BOLD),
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
                "Loading messages...",
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
                        if a.enabled { "enabled " } else { "disabled" },
                        status_style,
                    ),
                    Span::styled(
                        format!("  sessions: {}", a.active_sessions),
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

    fn draw_agent_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Agent Detail ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(idx) = self.selected_agent_index() else {
            frame.render_widget(
                Paragraph::new(Span::styled("No agent selected", theme::dim_style())),
                inner,
            );
            return;
        };

        let a = &self.agents[idx];
        let mut lines = vec![
            detail_line("Alias", &a.alias),
            detail_line("Enabled", if a.enabled { "yes" } else { "no" }),
            detail_line("Sessions", &a.active_sessions.to_string()),
        ];

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Channels", theme::heading_style())));
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
                Paragraph::new(Span::styled(err.clone(), theme::warn_style())),
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

        let Some(idx) = self.selected_memory_index() else {
            frame.render_widget(
                Paragraph::new(Span::styled("No entry selected", theme::dim_style())),
                inner,
            );
            return;
        };

        let m = &self.memories[idx];
        let mut lines = vec![
            detail_line("Key", &m.key),
            detail_line("Category", &m.category),
            detail_line("Namespace", &m.namespace),
            detail_line("Timestamp", &m.timestamp),
            detail_line("Agent", m.agent_alias.as_deref().unwrap_or("\u{2014}")),
        ];
        if let Some(score) = m.score {
            lines.push(detail_line("Score", &format!("{score:.3}")));
        }
        if let Some(imp) = m.importance {
            lines.push(detail_line("Importance", &format!("{imp:.2}")));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Content", theme::heading_style())));
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

    fn draw_health(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Health ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(ref h) = self.health else {
            frame.render_widget(
                Paragraph::new(Span::styled("Loading...", theme::dim_style())),
                inner,
            );
            return;
        };

        let mut lines = Vec::new();
        if let Some(obj) = h.as_object() {
            // Overall status
            if let Some(uptime) = obj.get("uptime_seconds").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![
                    Span::styled("Uptime     ", theme::dim_style()),
                    Span::styled(format_uptime(uptime), theme::body_style()),
                ]));
            }
            if let Some(pid) = obj.get("pid").and_then(|v| v.as_u64()) {
                lines.push(Line::from(vec![
                    Span::styled("PID        ", theme::dim_style()),
                    Span::styled(pid.to_string(), theme::body_style()),
                ]));
            }

            // Process stats
            if let Some(process) = obj.get("process") {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Process", theme::heading_style())));
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
                        format!("{cpu:.1}% ({ncpu} cores)")
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
                    "Components",
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
                lines.push(Line::from(Span::styled("Details", theme::heading_style())));
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

        let Some(ref c) = self.cost else {
            frame.render_widget(
                Paragraph::new(Span::styled("Loading...", theme::dim_style())),
                inner,
            );
            return;
        };

        let mut lines = vec![
            Line::from(Span::styled("Summary", theme::heading_style())),
            detail_line("Session", &format!("${:.6}", c.session_cost_usd)),
            detail_line("Daily", &format!("${:.6}", c.daily_cost_usd)),
            detail_line("Monthly", &format!("${:.6}", c.monthly_cost_usd)),
            detail_line("Tokens", &format_tokens(c.total_tokens)),
            detail_line("Requests", &c.request_count.to_string()),
        ];

        if !c.by_model.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("By Model", theme::heading_style())));
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
            lines.push(Line::from(Span::styled("By Agent", theme::heading_style())));
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
                Paragraph::new(Span::styled("No job selected", theme::dim_style())),
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
            detail_line("Name", j.name.as_deref().unwrap_or("\u{2014}")),
            detail_line("Agent", &j.agent_alias),
            detail_line("Enabled", if j.enabled { "yes" } else { "no" }),
            detail_line("Schedule", &sched_str),
            detail_line("Created", &j.created_at),
            detail_line("Next Run", &j.next_run),
            detail_line("Last Run", j.last_run.as_deref().unwrap_or("\u{2014}")),
            detail_line(
                "Last Status",
                j.last_status.as_deref().unwrap_or("\u{2014}"),
            ),
        ];

        if !j.command.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Command", theme::heading_style())));
            for l in j.command.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }
        if let Some(ref prompt) = j.prompt {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Prompt", theme::heading_style())));
            for l in prompt.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
            }
        }
        if let Some(ref output) = j.last_output {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Last Output",
                theme::heading_style(),
            )));
            for l in output.lines() {
                lines.push(Line::from(Span::styled(l.to_string(), theme::body_style())));
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
        if self.search_active {
            return self.handle_search_key(key);
        }
        if self.detail_open {
            return self.handle_detail_key(key).await;
        }
        self.handle_normal_key(key).await
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter => {
                self.search_query = self.search_buf.clone();
                self.search_active = false;
                // Force re-poll so server-side search (memories) picks up query.
                self.last_poll = None;
            }
            KeyCode::Esc => {
                // Restore the query from before search was activated.
                self.search_query = self.search_query_saved.clone();
                self.search_buf = self.search_query_saved.clone();
                self.search_active = false;
            }
            KeyCode::Backspace => {
                self.search_buf.pop();
                // Live-filter for client-side tabs (agents, cron).
                // Server-side tabs (sessions, memories) wait for Enter.
                if !matches!(self.tab, Tab::Sessions | Tab::Memories) {
                    self.search_query = self.search_buf.clone();
                }
            }
            KeyCode::Char(c) => {
                self.search_buf.push(c);
                if !matches!(self.tab, Tab::Sessions | Tab::Memories) {
                    self.search_query = self.search_buf.clone();
                }
            }
            _ => {}
        }
        false
    }

    async fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.detail_open = false;
                self.detail_scroll = 0;
            }
            // Shift+J / Shift+K scroll the detail pane
            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Down if shift => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
            KeyCode::Up if shift => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
            // Shift+Left / Shift+Right resize the detail pane
            KeyCode::Left if shift => {
                self.detail_pct = (self.detail_pct + 5).min(80);
            }
            KeyCode::Right if shift => {
                self.detail_pct = self.detail_pct.saturating_sub(5).max(20);
            }
            // j/k / plain arrows move the list cursor
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_list_down();
                self.detail_scroll = 0;
                self.on_selection_change().await;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_list_up();
                self.detail_scroll = 0;
                self.on_selection_change().await;
            }
            KeyCode::Char('/') => {
                self.search_query_saved = self.search_query.clone();
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            KeyCode::Char('c') => {
                self.search_query.clear();
                self.search_buf.clear();
                self.last_poll = None; // re-poll for server-side search
            }
            _ => {}
        }
        false
    }

    async fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => self.next_tab(),
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left => self.prev_tab(),
            KeyCode::Char('1') => self.tab = Tab::Overview,
            KeyCode::Char('2') => self.tab = Tab::Sessions,
            KeyCode::Char('3') => self.tab = Tab::Agents,
            KeyCode::Char('4') => self.tab = Tab::Memories,
            KeyCode::Char('5') => self.tab = Tab::Health,
            KeyCode::Char('6') => self.tab = Tab::Cost,
            KeyCode::Char('7') => self.tab = Tab::Cron,
            KeyCode::Char('j') | KeyCode::Down => self.move_list_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_list_up(),
            KeyCode::Enter if self.has_detail_pane() => {
                self.detail_open = true;
                self.detail_scroll = 0;
                self.detail_pct = 50;
                self.on_selection_change().await;
            }
            KeyCode::Char('/') => {
                self.search_query_saved = self.search_query.clone();
                self.search_active = true;
                self.search_buf = self.search_query.clone();
            }
            KeyCode::Char('c') => {
                self.search_query.clear();
                self.search_buf.clear();
                self.last_poll = None; // re-poll for server-side search
            }
            KeyCode::Char('r') => {
                self.poll_data().await;
            }
            KeyCode::Char('G') | KeyCode::End => self.jump_to_end(),
            KeyCode::Char('g') | KeyCode::Home => self.jump_to_start(),
            _ => {}
        }

        // Scrollable tabs
        match self.tab {
            Tab::Health => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.health_scroll = self.health_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.health_scroll = self.health_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Tab::Cost => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.cost_scroll = self.cost_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.cost_scroll = self.cost_scroll.saturating_sub(1);
                }
                _ => {}
            },
            _ => {}
        }

        false
    }

    /// Called when the list selection changes while the detail pane is open.
    async fn on_selection_change(&mut self) {
        if self.tab == Tab::Sessions && self.detail_open {
            self.load_session_messages().await;
        }
    }

    // ── Mouse handling ───────────────────────────────────────────

    pub(crate) fn handle_mouse(&mut self, evt: MouseEvent, _content_area: Rect) {
        use crossterm::event::MouseButton;

        let col = evt.column;
        let row = evt.row;

        match evt.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Tab bar clicks
                let labels: Vec<&str> = TABS.iter().map(|t| t.label()).collect();
                if let Some(idx) = mouse::tab_click_index(col, row, self.tab_area, &labels, 3) {
                    self.tab = TABS[idx];
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
        self.tab = TABS[(idx + 1) % TABS.len()];
        self.on_tab_change();
    }

    fn prev_tab(&mut self) {
        let idx = TABS.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = TABS[(idx + TABS.len() - 1) % TABS.len()];
        self.on_tab_change();
    }

    fn on_tab_change(&mut self) {
        self.detail_open = false;
        self.detail_scroll = 0;
        self.health_scroll = 0;
        self.cost_scroll = 0;
        // Force immediate data fetch for new tab
        self.last_poll = None;
    }

    fn has_detail_pane(&self) -> bool {
        matches!(
            self.tab,
            Tab::Sessions | Tab::Agents | Tab::Memories | Tab::Cron
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

    /// Whether the pane is in a text-input mode (search bar active).
    pub(crate) fn wants_text_input(&self) -> bool {
        self.search_active
    }

}

impl crate::widgets::HelpContext for Dashboard<'_> {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};

        // Global tab-switching always available.
        let tab_nav = vec![
            E::new(vec!["Tab", "l", "→"], "Next tab"),
            E::new(vec!["Shift+Tab", "h", "←"], "Previous tab"),
            E::key("1–7", "Jump to tab"),
            E::key("r", "Refresh now"),
            E::key("q", "Quit TUI"),
            E::key("?", "This help"),
        ];

        if self.search_active {
            return HelpNode::entries(vec![
                E::key("Enter", "Apply search"),
                E::key("Esc", "Cancel search"),
            ]);
        }

        if self.detail_open {
            return HelpNode::entries(vec![
                E::new(vec!["Esc", "Enter"], "Close detail"),
                E::new(vec!["j", "k"], "Move list cursor"),
                E::new(vec!["J", "K"], "Scroll detail"),
                E::new(vec!["Shift+↑", "Shift+↓"], "Scroll detail"),
                E::key("Shift+←/→", "Resize detail pane"),
                E::key("r", "Refresh"),
                E::key("/", "Search"),
                E::key("c", "Clear search"),
                E::key("q", "Quit TUI"),
                E::key("?", "This help"),
            ]);
        }

        // Per-tab bindings — only show what actually works on this tab.
        let mut entries = tab_nav;
        match self.tab {
            Tab::Overview | Tab::Health | Tab::Cost => {
                // Read-only display tabs — no list, no detail, no search.
            }
            Tab::Sessions | Tab::Agents | Tab::Memories | Tab::Cron => {
                entries.push(E::spacer());
                entries.push(E::new(vec!["j", "k", "↑↓"], "Move cursor"));
                entries.push(E::new(vec!["G", "End"], "Jump to bottom"));
                entries.push(E::new(vec!["g", "Home"], "Jump to top"));
                entries.push(E::key("Enter", "Open detail pane"));
                entries.push(E::key("/", "Search / filter"));
                entries.push(E::key("c", "Clear search"));
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

fn truncate(s: &str, max: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() > max {
        format!("{}...", &first_line[..max])
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
