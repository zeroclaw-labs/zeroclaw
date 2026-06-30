use std::sync::Arc;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::client::RpcClient;

/// SOP authoring pane: lists SOPs from the daemon and renders the selected
/// SOP's projected node graph as text. The graph text is produced by the
/// backend (`sops/graph` returns the projection); this pane only formats
/// what it receives, never inferring graph shape itself.
pub(crate) struct SopPane {
    rpc: Arc<RpcClient>,
    names: Vec<String>,
    list_state: ListState,
    graph_lines: Vec<String>,
    error: Option<String>,
}

impl SopPane {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            names: Vec::new(),
            list_state: ListState::default(),
            graph_lines: Vec::new(),
            error: None,
        }
    }

    pub(crate) fn selected_name(&self) -> Option<&str> {
        self.list_state
            .selected()
            .and_then(|i| self.names.get(i))
            .map(String::as_str)
    }

    pub(crate) async fn refresh(&mut self) {
        match self.rpc.sops_list().await {
            Ok(value) => {
                self.names = parse_sop_names(&value);
                self.error = None;
                if self.list_state.selected().is_none() && !self.names.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub(crate) async fn load_selected_graph(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_graph(&name).await {
            Ok(value) => {
                self.graph_lines = graph_to_lines(&value);
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub(crate) fn select_next(&mut self) {
        if self.names.is_empty() {
            return;
        }
        let next = self
            .list_state
            .selected()
            .map_or(0, |i| if i + 1 >= self.names.len() { 0 } else { i + 1 });
        self.list_state.select(Some(next));
    }

    pub(crate) fn select_prev(&mut self) {
        if self.names.is_empty() {
            return;
        }
        let prev = self
            .list_state
            .selected()
            .map_or(0, |i| if i == 0 { self.names.len() - 1 } else { i - 1 });
        self.list_state.select(Some(prev));
    }

    pub(crate) async fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crate::keymap::SopTabAction;
        match SopTabAction::from_chord(&key) {
            Some(SopTabAction::Up) => self.select_prev(),
            Some(SopTabAction::Down) => self.select_next(),
            Some(SopTabAction::Enter) => self.load_selected_graph().await,
            None => {}
        }
        false
    }

    pub(crate) fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::SopTabAction as S;
        crate::widgets::HelpNode::entries(crate::help::entries_for([S::Up, S::Down, S::Enter]))
    }

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(area);

        let items: Vec<ListItem> = self
            .names
            .iter()
            .map(|n| ListItem::new(Line::from(n.clone())))
            .collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("SOPs"))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, cols[0], &mut self.list_state);

        let body = if let Some(err) = &self.error {
            err.clone()
        } else {
            self.graph_lines.join("\n")
        };
        let para = Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title("Graph"))
            .wrap(Wrap { trim: false });
        f.render_widget(para, cols[1]);
    }
}

/// Extract SOP names from the `sops/list` array response.
fn parse_sop_names(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Format the `sops/graph` projection into display lines: one line per node
/// with its outbound flow targets, then a diagnostics block when present.
fn graph_to_lines(graph: &serde_json::Value) -> Vec<String> {
    let mut lines = Vec::new();
    let nodes = graph.get("nodes").and_then(|n| n.as_array());
    let wires = graph.get("wires").and_then(|w| w.as_array());

    if let Some(nodes) = nodes {
        for node in nodes {
            let step = node.get("step").and_then(serde_json::Value::as_u64);
            let title = node.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let outs: Vec<String> = wires
                .map(|ws| {
                    ws.iter()
                        .filter(|w| {
                            w.get("class").and_then(|c| c.as_str()) == Some("flow")
                                && w.get("from_step").and_then(serde_json::Value::as_u64) == step
                        })
                        .filter_map(|w| {
                            w.get("to_step")
                                .and_then(serde_json::Value::as_u64)
                                .map(|t| t.to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            match step {
                Some(s) if outs.is_empty() => lines.push(format!("{s}. {title}")),
                Some(s) => lines.push(format!("{s}. {title} -> {}", outs.join(", "))),
                None => {}
            }
        }
    }

    if let Some(diags) = graph.get("diagnostics").and_then(|d| d.as_array())
        && !diags.is_empty()
    {
        lines.push(String::new());
        lines.push("diagnostics:".to_string());
        for d in diags {
            let sev = d.get("severity").and_then(|s| s.as_str()).unwrap_or("");
            let step = d
                .get("step")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let msg = d.get("message").and_then(|m| m.as_str()).unwrap_or("");
            lines.push(format!("  [{sev}] step {step}: {msg}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::method;
    use crate::jsonrpc::RpcOutbound;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn test_client_with_rpc() -> (Arc<RpcClient>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>(16);
        let outbound = Arc::new(RpcOutbound::new(tx));
        (Arc::new(RpcClient::with_rpc(outbound)), rx)
    }

    async fn next_request(rx: &mut mpsc::Receiver<String>) -> serde_json::Value {
        let raw = tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("a request should be sent")
            .expect("writer channel open");
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn parse_names_from_list_response() {
        let v = serde_json::json!([
            { "name": "alpha" },
            { "name": "beta" }
        ]);
        assert_eq!(parse_sop_names(&v), vec!["alpha", "beta"]);
    }

    #[test]
    fn parse_names_empty_on_non_array() {
        assert!(parse_sop_names(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn graph_lines_show_nodes_and_flow_targets() {
        let g = serde_json::json!({
            "nodes": [
                { "step": 1, "title": "a" },
                { "step": 2, "title": "b" }
            ],
            "wires": [
                { "class": "flow", "from_step": 1, "to_step": 2, "flow_role": "sequence" }
            ],
            "diagnostics": []
        });
        let lines = graph_to_lines(&g);
        assert_eq!(lines[0], "1. a -> 2");
        assert_eq!(lines[1], "2. b");
    }

    #[test]
    fn graph_lines_append_diagnostics() {
        let g = serde_json::json!({
            "nodes": [{ "step": 1, "title": "a" }],
            "wires": [],
            "diagnostics": [
                { "severity": "error", "step": 1, "message": "boom" }
            ]
        });
        let lines = graph_to_lines(&g);
        assert!(lines.iter().any(|l| l == "diagnostics:"));
        assert!(lines.iter().any(|l| l.contains("[error] step 1: boom")));
    }

    #[tokio::test]
    async fn selection_wraps_both_directions() {
        let (client, _rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.names = vec!["a".into(), "b".into()];
        pane.list_state.select(Some(0));
        pane.select_prev();
        assert_eq!(pane.list_state.selected(), Some(1));
        pane.select_next();
        assert_eq!(pane.list_state.selected(), Some(0));
    }

    #[tokio::test]
    async fn refresh_calls_sops_list() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        let task = tokio::spawn(async move {
            pane.refresh().await;
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_LIST);
        task.abort();
    }
}
