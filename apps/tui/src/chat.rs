use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::client::RpcClient;
use crate::theme;

// ── Legacy stub (used by app.rs tab switcher) ────────────────────

pub(crate) struct Chat<'a> {
    rpc: &'a RpcClient,
}

impl<'a> Chat<'a> {
    pub(crate) fn new(rpc: &'a RpcClient) -> Self {
        Self { rpc }
    }

    pub(crate) fn draw(&self, frame: &mut ratatui::Frame, area: Rect) {
        let block = Block::default()
            .title(Span::styled(" Chat ", theme::title_style()))
            .borders(Borders::ALL)
            .border_style(theme::dim_style());

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .split(inner);

        let hint = Paragraph::new(Line::from(vec![
            Span::styled("Coming soon ", theme::body_style()),
            Span::styled("— press ", theme::dim_style()),
            Span::styled("F1", theme::accent_style()),
            Span::styled(" to switch to Config", theme::dim_style()),
        ]))
        .alignment(Alignment::Center);

        let version = Paragraph::new(Line::from(Span::styled(
            format!("daemon v{}", self.rpc.server_version),
            theme::dim_style(),
        )))
        .alignment(Alignment::Center);

        frame.render_widget(hint, chunks[1]);
        frame.render_widget(version, chunks[2]);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
    }
}

// ── ChatState / ChatEntry ─────────────────────────────────────────

use crate::client::SessionUpdate;

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub arguments_summary: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone)]
pub enum ChatEntry {
    AgentMessage(String),
    AgentThought(String),
    UserMessage(String),
    Tool {
        tool_call_id: String,
        name: String,
        input: serde_json::Value,
        result: Option<String>,
    },
}

#[derive(Debug)]
pub struct ChatState {
    pub session_id: String,
    pub agent_alias: String,
    input: String,
    entries: Vec<ChatEntry>,
    streaming_text: String,
    streaming_thought: String,
    pending_approval: Option<PendingApproval>,
    pub turn_in_flight: bool,
}

impl ChatState {
    pub fn new(session_id: String, agent_alias: String) -> Self {
        Self {
            session_id,
            agent_alias,
            input: String::new(),
            entries: Vec::new(),
            streaming_text: String::new(),
            streaming_thought: String::new(),
            pending_approval: None,
            turn_in_flight: false,
        }
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn push_input_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn pop_input_char(&mut self) {
        self.input.pop();
    }

    pub fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input)
    }

    pub fn entries(&self) -> &[ChatEntry] {
        &self.entries
    }

    pub fn current_agent_text(&self) -> &str {
        &self.streaming_text
    }

    pub fn pending_approval(&self) -> Option<&PendingApproval> {
        self.pending_approval.as_ref()
    }

    pub fn take_pending_approval(&mut self) -> Option<PendingApproval> {
        self.pending_approval.take()
    }

    pub fn apply_update(&mut self, update: SessionUpdate) {
        match update {
            SessionUpdate::AgentMessageChunk { text, .. } => {
                self.streaming_text.push_str(&text);
            }
            SessionUpdate::AgentThoughtChunk { text, .. } => {
                self.streaming_thought.push_str(&text);
            }
            SessionUpdate::ToolCall {
                tool_call_id,
                name,
                raw_input,
                ..
            } => {
                self.entries.push(ChatEntry::Tool {
                    tool_call_id,
                    name,
                    input: raw_input,
                    result: None,
                });
            }
            SessionUpdate::ToolResult {
                tool_call_id,
                raw_output,
                ..
            } => {
                for entry in self.entries.iter_mut().rev() {
                    if let ChatEntry::Tool {
                        tool_call_id: id,
                        result,
                        ..
                    } = entry
                    {
                        if id == &tool_call_id {
                            *result = Some(raw_output);
                            break;
                        }
                    }
                }
            }
            SessionUpdate::ApprovalRequest {
                request_id,
                tool_name,
                arguments_summary,
                timeout_secs,
                ..
            } => {
                self.pending_approval = Some(PendingApproval {
                    request_id,
                    tool_name,
                    arguments_summary,
                    timeout_secs,
                });
            }
        }
    }

    pub fn commit_turn(&mut self, full_text: String) {
        self.streaming_text.clear();
        self.streaming_thought.clear();
        if !full_text.is_empty() {
            self.entries.push(ChatEntry::AgentMessage(full_text));
        }
        self.turn_in_flight = false;
    }

    pub fn push_user_message(&mut self, msg: String) {
        self.entries.push(ChatEntry::UserMessage(msg));
        self.turn_in_flight = true;
    }
}

// ── Interactive run() entry point ─────────────────────────────────

use std::io::Stdout;

use crossterm::{
    event::{Event, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Margin,
    style::{Color, Modifier, Style},
    widgets::{Clear, Wrap},
    Frame, Terminal,
};

use crate::client::{ApprovalDecision, SessionPromptResult};

type Term = Terminal<CrosstermBackend<Stdout>>;

pub async fn run(rpc: &mut RpcClient, agent_alias: &str) -> anyhow::Result<()> {
    let session = rpc
        .session_new(agent_alias, None)
        .await
        .map_err(|e| anyhow::anyhow!("failed to create session: {e}"))?;

    let mut term = init_terminal()?;
    let result = chat_loop(rpc, session.session_id.clone(), agent_alias, &mut term).await;
    restore_terminal(&mut term)?;
    let _ = rpc.session_close(&session.session_id).await;
    result
}

fn init_terminal() -> anyhow::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(term: &mut Term) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

async fn chat_loop(
    rpc: &mut RpcClient,
    session_id: String,
    agent_alias: &str,
    term: &mut Term,
) -> anyhow::Result<()> {
    let mut state = ChatState::new(session_id.clone(), agent_alias.to_string());

    // We hold an optional in-flight prompt future using a channel to carry the
    // result back from a spawned task. Because RpcClient is not Clone/Send+
    // 'static, we use a channel where the spawned side just fires-and-forgets
    // a completion value.
    let (turn_result_tx, mut turn_result_rx) =
        tokio::sync::mpsc::channel::<anyhow::Result<SessionPromptResult>>(2);

    loop {
        term.draw(|f| render(f, &state))?;

        tokio::select! {
            maybe_event = async {
                if crossterm::event::poll(std::time::Duration::from_millis(50))? {
                    crossterm::event::read()
                } else {
                    Ok(Event::FocusLost)
                }
            } => {
                match maybe_event? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if state.turn_in_flight {
                                    let _ = rpc.session_cancel(&session_id).await;
                                }
                                break;
                            }
                            KeyCode::Enter => {
                                if let Some(pa) = state.take_pending_approval() {
                                    let _ = rpc
                                        .session_approve(
                                            &session_id,
                                            &pa.request_id,
                                            ApprovalDecision::AllowOnce,
                                        )
                                        .await;
                                } else if !state.turn_in_flight {
                                    let msg = state.take_input();
                                    if !msg.is_empty() {
                                        state.push_user_message(msg.clone());
                                        // Drive the prompt call; the future is polled
                                        // in the same select! loop via turn_result_rx.
                                        let sid = session_id.clone();
                                        let rpc_ref: *mut RpcClient = rpc;
                                        let tx = turn_result_tx.clone();
                                        // SAFETY: the loop borrows rpc exclusively and
                                        // we await inside a single-threaded context
                                        // so the raw pointer is valid for this call.
                                        //
                                        // Actually, we can't send a raw pointer across
                                        // threads. Use an inline call with a boxed future
                                        // stored on the stack instead.
                                        let _ = (sid, rpc_ref, tx); // drop the planned approach
                                        // Inline: call session_prompt directly and send result.
                                        let prompt_result = rpc.session_prompt(&session_id, &msg).await;
                                        let _ = turn_result_tx.send(prompt_result).await;
                                    }
                                }
                            }
                            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if let Some(pa) = state.take_pending_approval() {
                                    let _ = rpc
                                        .session_approve(
                                            &session_id,
                                            &pa.request_id,
                                            ApprovalDecision::Reject,
                                        )
                                        .await;
                                }
                            }
                            KeyCode::Char('a') => {
                                if let Some(pa) = state.take_pending_approval() {
                                    let _ = rpc
                                        .session_approve(
                                            &session_id,
                                            &pa.request_id,
                                            ApprovalDecision::AllowAlways,
                                        )
                                        .await;
                                } else if state.pending_approval().is_none() && !state.turn_in_flight {
                                    state.push_input_char('a');
                                }
                            }
                            KeyCode::Char(c) => {
                                if state.pending_approval().is_none() && !state.turn_in_flight {
                                    state.push_input_char(c);
                                }
                            }
                            KeyCode::Backspace => {
                                if state.pending_approval().is_none() {
                                    state.pop_input_char();
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::Resize(_, _) => {
                        term.autoresize()?;
                    }
                    _ => {}
                }
            }

            Some(update) = rpc.notifications.recv() => {
                state.apply_update(update);
            }

            Some(result) = turn_result_rx.recv() => {
                match result {
                    Ok(r) => state.commit_turn(r.content),
                    Err(e) => state.commit_turn(format!("[error: {e}]")),
                }
            }
        }
    }

    Ok(())
}

fn render(f: &mut Frame, state: &ChatState) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    render_conversation(f, state, chunks[0]);
    render_input(f, state, chunks[1]);

    if state.pending_approval().is_some() {
        render_approval_overlay(f, state, area);
    }
}

fn render_conversation(f: &mut Frame, state: &ChatState, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for entry in state.entries() {
        match entry {
            ChatEntry::UserMessage(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "You: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.as_str()),
                ]));
            }
            ChatEntry::AgentMessage(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Agent: ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text.as_str()),
                ]));
            }
            ChatEntry::AgentThought(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "(thinking) ",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(text.as_str(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            ChatEntry::Tool { name, input, result, .. } => {
                let input_str = serde_json::to_string(input).unwrap_or_default();
                let truncated: String = input_str.chars().take(60).collect();
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("[tool: {name}] "),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(truncated),
                ]));
                if let Some(r) = result {
                    let result_truncated: String = r.chars().take(60).collect();
                    lines.push(Line::from(vec![
                        Span::styled("  \u{2514}\u{2500} ", Style::default().fg(Color::DarkGray)),
                        Span::raw(result_truncated),
                    ]));
                }
            }
        }
    }

    if !state.current_agent_text().is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "Agent: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(state.current_agent_text()),
        ]));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} ", state.agent_alias)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, area);
}

fn render_input(f: &mut Frame, state: &ChatState, area: Rect) {
    let label = if state.turn_in_flight {
        " (thinking\u{2026}) "
    } else {
        " > "
    };
    let p = Paragraph::new(state.input())
        .block(Block::default().borders(Borders::ALL).title(label));
    f.render_widget(p, area);
}

fn render_approval_overlay(f: &mut Frame, state: &ChatState, area: Rect) {
    let pa = match state.pending_approval() {
        Some(p) => p,
        None => return,
    };

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Length(12),
            Constraint::Min(0),
        ])
        .split(area);
    let overlay_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(10),
            Constraint::Min(60),
            Constraint::Percentage(10),
        ])
        .split(vert[1])[1];

    f.render_widget(Clear, overlay_area);

    let text = format!(
        "Approve tool call: {}\n\n  {}\n\n  Enter=Allow  a=Always  Ctrl+D=Reject  e=Edit",
        pa.tool_name, pa.arguments_summary
    );
    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Approval Required ")
                .style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: true });
    f.render_widget(p, overlay_area);
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ChatState {
        ChatState::new("sess-1".to_string(), "myagent".to_string())
    }

    #[test]
    fn input_append_and_clear() {
        let mut s = state();
        s.push_input_char('h');
        s.push_input_char('i');
        assert_eq!(s.input(), "hi");
        let taken = s.take_input();
        assert_eq!(taken, "hi");
        assert_eq!(s.input(), "");
    }

    #[test]
    fn text_chunk_accumulates() {
        let mut s = state();
        s.apply_update(crate::client::SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Hello".to_string(),
        });
        s.apply_update(crate::client::SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: " world".to_string(),
        });
        assert_eq!(s.current_agent_text(), "Hello world");
    }

    #[test]
    fn tool_call_followed_by_result_is_one_entry() {
        let mut s = state();
        s.apply_update(crate::client::SessionUpdate::ToolCall {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_input: serde_json::json!({"command":"ls"}),
        });
        s.apply_update(crate::client::SessionUpdate::ToolResult {
            session_id: "sess-1".to_string(),
            tool_call_id: "tc1".to_string(),
            name: "shell".to_string(),
            raw_output: "file.txt\n".to_string(),
        });
        let entries = s.entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(&entries[0], ChatEntry::Tool { result: Some(_), .. }));
    }

    #[test]
    fn approval_request_sets_pending_approval() {
        let mut s = state();
        s.apply_update(crate::client::SessionUpdate::ApprovalRequest {
            session_id: "sess-1".to_string(),
            request_id: "req-1".to_string(),
            tool_name: "shell".to_string(),
            arguments_summary: "rm -rf /".to_string(),
            timeout_secs: 30,
        });
        assert!(s.pending_approval().is_some());
        let pa = s.pending_approval().unwrap();
        assert_eq!(pa.request_id, "req-1");
        assert_eq!(pa.tool_name, "shell");
    }

    #[test]
    fn turn_commit_flushes_streaming_buffer() {
        let mut s = state();
        s.apply_update(crate::client::SessionUpdate::AgentMessageChunk {
            session_id: "sess-1".to_string(),
            text: "Done".to_string(),
        });
        s.commit_turn("Done".to_string());
        assert_eq!(s.current_agent_text(), "");
        assert!(s
            .entries()
            .iter()
            .any(|e| matches!(e, ChatEntry::AgentMessage(t) if t == "Done")));
    }
}
