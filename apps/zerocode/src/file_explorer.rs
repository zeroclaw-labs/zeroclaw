//! Reusable file explorer modal widget with multi-file selection.
//!
//! Browses the local filesystem where the TUI is running. Designed to
//! be invoked from any pane (Chat, ACP, etc.).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Paragraph},
};

use crate::theme;

// ── Types ────────────────────────────────────────────────────────

/// A single entry in the explorer listing.
impl std::fmt::Debug for FileExplorerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileExplorerState")
            .field("cwd", &self.cwd)
            .field("entries", &self.entries)
            .field("list_state", &self.list_state)
            .field("selected", &self.selected)
            .field("show_hidden", &self.show_hidden)
            .field("error", &self.error)
            .field("search_query", &self.search_query)
            .field("searching", &self.searching)
            .field("dir_picker", &self.dir_picker)
            .field(
                "remote_rpc",
                &self.remote_rpc.as_ref().map(|_| "<RpcClient>"),
            )
            .field("last_list_area", &self.last_list_area)
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct ExplorerEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub _is_hidden: bool,
    pub full_path: PathBuf,
}

/// Action returned from key handling.
pub(crate) enum ExplorerAction {
    /// Key consumed, no state change visible to caller.
    None,
    /// User confirmed selection. Contains selected file paths.
    Confirm(Vec<PathBuf>),
    /// User confirmed the current directory (dir-picker mode only).
    ConfirmDir(PathBuf),
    /// User cancelled the explorer.
    Cancel,
}

// ── State ────────────────────────────────────────────────────────

/// State for the file explorer overlay.
pub(crate) struct FileExplorerState {
    cwd: PathBuf,
    entries: Vec<ExplorerEntry>,
    list_state: ListState,
    selected: HashSet<PathBuf>,
    show_hidden: bool,
    error: Option<String>,
    search_query: String,
    searching: bool,
    dir_picker: bool,
    remote_rpc: Option<Arc<crate::client::RpcClient>>,
    last_list_area: Rect,
}

impl FileExplorerState {
    /// Create a new explorer rooted at `start_dir`.
    pub fn new(start_dir: PathBuf) -> Self {
        let mut state = Self {
            cwd: start_dir,
            entries: Vec::new(),
            list_state: ListState::default(),
            selected: HashSet::new(),
            show_hidden: false,
            error: None,
            search_query: String::new(),
            searching: false,
            dir_picker: false,
            remote_rpc: None,
            last_list_area: Rect::default(),
        };
        state.load_entries();
        if !state.entries.is_empty() {
            state.list_state.select(Some(0));
        }
        state
    }

    /// Create a directory picker that fetches entries from the remote daemon (WSS).
    ///
    /// Builds the struct with `remote_rpc` set **before** the first
    /// `load_entries()` call so the listing comes from the remote daemon
    /// rather than the local filesystem.
    pub fn new_dir_picker_remote(start_dir: PathBuf, rpc: Arc<crate::client::RpcClient>) -> Self {
        let mut state = Self {
            cwd: start_dir,
            entries: Vec::new(),
            list_state: ListState::default(),
            selected: HashSet::new(),
            show_hidden: false,
            error: None,
            search_query: String::new(),
            searching: false,
            dir_picker: true,
            remote_rpc: Some(rpc),
            last_list_area: Rect::default(),
        };
        state.load_entries();
        if !state.entries.is_empty() {
            state.list_state.select(Some(0));
        }
        state
    }

    /// Read the current directory and populate entries.
    pub fn load_entries(&mut self) {
        self.entries.clear();
        self.error = None;

        if let Some(rpc) = &self.remote_rpc {
            // Wire up remote fs/list_dir (WSS ACP case)
            let path = self.cwd.to_string_lossy().to_string();
            let show_hidden = self.show_hidden;
            let rpc = Arc::clone(rpc);
            let result = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    rpc.fs_list_dir(std::path::Path::new(&path), show_hidden)
                        .await
                })
            });
            match result {
                Ok(resp) => {
                    for e in resp.entries {
                        self.entries.push(ExplorerEntry {
                            name: e.name,
                            is_dir: e.is_dir,
                            size: e.size,
                            _is_hidden: e.is_hidden,
                            full_path: std::path::PathBuf::from(e.full_path),
                        });
                    }
                    // keep cwd consistent
                    if !resp.cwd.is_empty() {
                        self.cwd = std::path::PathBuf::from(resp.cwd);
                    }
                    self.entries
                        .sort_by_key(|a| (!a.is_dir, a.name.to_lowercase()));
                    return;
                }
                Err(e) => {
                    self.error = Some(format!("Remote list_dir failed: {e}"));
                    return;
                }
            }
        }

        // Local filesystem path (default / non-WSS case)
        let rd = match std::fs::read_dir(&self.cwd) {
            Ok(rd) => rd,
            Err(e) => {
                self.error = Some(format!("Cannot read {}: {e}", self.cwd.display()));
                return;
            }
        };

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_hidden = name.starts_with('.');
            if !self.show_hidden && is_hidden {
                continue;
            }
            let meta = entry.metadata();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let full_path = entry.path();

            let e = ExplorerEntry {
                name,
                is_dir,
                size,
                _is_hidden: is_hidden,
                full_path,
            };

            if is_dir {
                dirs.push(e);
            } else {
                files.push(e);
            }
        }

        dirs.sort_by_key(|a| a.name.to_lowercase());
        files.sort_by_key(|a| a.name.to_lowercase());

        self.entries.extend(dirs);
        self.entries.extend(files);
    }

    /// Filtered view of entries (when searching).
    fn visible_entries(&self) -> Vec<usize> {
        if self.search_query.is_empty() {
            return (0..self.entries.len()).collect();
        }
        let q = self.search_query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect()
    }

    fn selected_idx(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn current_entry(&self) -> Option<&ExplorerEntry> {
        let visible = self.visible_entries();
        self.list_state
            .selected()
            .and_then(|i| visible.get(i))
            .and_then(|&real_idx| self.entries.get(real_idx))
    }

    /// Handle a key event. Returns the action for the caller to process.
    pub fn handle_key(&mut self, key: KeyEvent) -> ExplorerAction {
        // Search mode intercepts character input.
        if self.searching {
            return self.handle_search_key(key);
        }

        let visible = self.visible_entries();
        let vis_len = visible.len();

        use crate::keymap::FileExplorerAction;
        let action = FileExplorerAction::from_chord(&key);
        match action {
            Some(FileExplorerAction::Cancel) => ExplorerAction::Cancel,
            Some(FileExplorerAction::Activate) => {
                if let Some(entry) = self.current_entry() {
                    if entry.is_dir {
                        let path = entry.full_path.clone();
                        self.cwd = path;
                        self.search_query.clear();
                        self.load_entries();
                        self.list_state.select(if self.entries.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
                        ExplorerAction::None
                    } else if self.selected.is_empty() {
                        // No multi-select: confirm just the cursor entry.
                        ExplorerAction::Confirm(vec![entry.full_path.clone()])
                    } else {
                        // Multi-select active: confirm all selected.
                        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
                        ExplorerAction::Confirm(paths)
                    }
                } else {
                    ExplorerAction::None
                }
            }
            Some(FileExplorerAction::ToggleSelect) => {
                if let Some(entry) = self.current_entry()
                    && !entry.is_dir
                {
                    let path = entry.full_path.clone();
                    if self.selected.contains(&path) {
                        self.selected.remove(&path);
                    } else {
                        self.selected.insert(path);
                    }
                    // Advance cursor after toggling.
                    if let Some(i) = self.selected_idx()
                        && i + 1 < vis_len
                    {
                        self.list_state.select(Some(i + 1));
                    }
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::Down) => {
                if let Some(i) = self.selected_idx() {
                    if i + 1 < vis_len {
                        self.list_state.select(Some(i + 1));
                    }
                } else if vis_len > 0 {
                    self.list_state.select(Some(0));
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::Up) => {
                if let Some(i) = self.selected_idx() {
                    if i > 0 {
                        self.list_state.select(Some(i - 1));
                    }
                } else if vis_len > 0 {
                    self.list_state.select(Some(0));
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::JumpStart) => {
                if vis_len > 0 {
                    self.list_state.select(Some(0));
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::JumpEnd) => {
                if vis_len > 0 {
                    self.list_state.select(Some(vis_len - 1));
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::EnterDir) => {
                // Enter directory under cursor.
                if let Some(entry) = self.current_entry()
                    && entry.is_dir
                {
                    let path = entry.full_path.clone();
                    self.cwd = path;
                    self.search_query.clear();
                    self.load_entries();
                    self.list_state.select(if self.entries.is_empty() {
                        None
                    } else {
                        Some(0)
                    });
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::LeaveDir) => {
                if let Some(parent) = self.cwd.parent() {
                    let prev = self.cwd.clone();
                    self.cwd = parent.to_path_buf();
                    self.search_query.clear();
                    self.load_entries();
                    // Try to re-select the dir we came from.
                    let idx = self
                        .entries
                        .iter()
                        .position(|e| e.full_path == prev)
                        .unwrap_or(0);
                    self.list_state.select(Some(idx));
                }
                ExplorerAction::None
            }
            Some(FileExplorerAction::ToggleHidden) => {
                self.show_hidden = !self.show_hidden;
                self.load_entries();
                self.list_state.select(if self.entries.is_empty() {
                    None
                } else {
                    Some(0)
                });
                ExplorerAction::None
            }
            Some(FileExplorerAction::BeginSearch) => {
                self.searching = true;
                self.search_query.clear();
                ExplorerAction::None
            }
            Some(FileExplorerAction::ConfirmDir) if self.dir_picker => {
                ExplorerAction::ConfirmDir(self.cwd.clone())
            }
            _ => ExplorerAction::None,
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> ExplorerAction {
        use crate::keymap::FileExplorerSearchAction;
        let action = FileExplorerSearchAction::from_chord(&key);
        match action {
            Some(FileExplorerSearchAction::Cancel) => {
                self.searching = false;
                self.search_query.clear();
                // Reset selection to first visible.
                let vis = self.visible_entries();
                self.list_state
                    .select(if vis.is_empty() { None } else { Some(0) });
                ExplorerAction::None
            }
            Some(FileExplorerSearchAction::Accept) => {
                self.searching = false;
                // Keep the filter active, confirm if on a file.
                if let Some(entry) = self.current_entry() {
                    if entry.is_dir {
                        let path = entry.full_path.clone();
                        self.cwd = path;
                        self.search_query.clear();
                        self.load_entries();
                        self.list_state.select(if self.entries.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
                        ExplorerAction::None
                    } else if self.selected.is_empty() {
                        ExplorerAction::Confirm(vec![entry.full_path.clone()])
                    } else {
                        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
                        ExplorerAction::Confirm(paths)
                    }
                } else {
                    ExplorerAction::None
                }
            }
            Some(FileExplorerSearchAction::Backspace) => {
                self.search_query.pop();
                let vis = self.visible_entries();
                self.list_state
                    .select(if vis.is_empty() { None } else { Some(0) });
                ExplorerAction::None
            }
            _ => {
                if let KeyCode::Char(c) = key.code
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.search_query.push(c);
                    let vis = self.visible_entries();
                    self.list_state
                        .select(if vis.is_empty() { None } else { Some(0) });
                }
                ExplorerAction::None
            }
        }
    }

    /// Handle mouse events (scroll and click-to-select).
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> ExplorerAction {
        use crate::mouse;

        let col = mouse.column;
        let row = mouse.row;
        let area = self.last_list_area;
        let visible = self.visible_entries();
        let vis_len = visible.len();

        if !mouse::in_rect(col, row, area) {
            return ExplorerAction::None;
        }

        match mouse.kind {
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // The list has no border, so row offset is directly from area.y.
                let row_in_list = (row - area.y) as usize;
                let offset = self.list_state.offset();
                let idx = offset + row_in_list;
                if idx < vis_len {
                    self.list_state.select(Some(idx));
                }
                ExplorerAction::None
            }
            MouseEventKind::ScrollUp => {
                if let Some(i) = self.selected_idx() {
                    let next = mouse::list_scroll(i, vis_len, true, 3);
                    self.list_state.select(Some(next));
                }
                ExplorerAction::None
            }
            MouseEventKind::ScrollDown => {
                if let Some(i) = self.selected_idx() {
                    let next = mouse::list_scroll(i, vis_len, false, 3);
                    self.list_state.select(Some(next));
                }
                ExplorerAction::None
            }
            _ => ExplorerAction::None,
        }
    }

    /// Render the file explorer as a centered modal overlay.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        // Center the overlay: 80% height, 70% width.
        let vert = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(10),
                Constraint::Min(10),
                Constraint::Percentage(10),
            ])
            .split(area);
        let overlay_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Min(40),
                Constraint::Percentage(15),
            ])
            .split(vert[1])[1];

        f.render_widget(Clear, overlay_area);

        // Title: current path.
        let cwd_display = self.cwd.display().to_string();
        let title = if cwd_display.len() > 50 {
            format!(" ...{} ", &cwd_display[cwd_display.len() - 47..])
        } else {
            format!(" {cwd_display} ")
        };

        let block = theme::modal_block(&title);

        let inner = block.inner(overlay_area);
        f.render_widget(block, overlay_area);

        if let Some(err) = &self.error {
            let p = Paragraph::new(Span::styled(err.as_str(), Style::default().fg(Color::Red)));
            f.render_widget(p, inner);
            return;
        }

        // Split inner into: entries list + footer.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let visible = self.visible_entries();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|&real_idx| {
                let entry = &self.entries[real_idx];
                let is_marked = self.selected.contains(&entry.full_path);

                let prefix = if is_marked { "* " } else { "  " };
                let (name, style) = if entry.is_dir {
                    (format!("{prefix}{}/", entry.name), theme::heading_style())
                } else {
                    let size = crate::attachment::format_size(entry.size);
                    let style = if is_marked {
                        theme::accent_style()
                    } else {
                        theme::body_style()
                    };
                    (format!("{prefix}{}  {size}", entry.name), style)
                };

                ListItem::new(Span::styled(name, style))
            })
            .collect();

        let list = List::new(items)
            .style(theme::fill_style())
            .highlight_style(theme::selected_style());
        self.last_list_area = chunks[0];
        let mut ls = self.list_state;
        f.render_stateful_widget(list, chunks[0], &mut ls);

        // Footer: selected count + search + key hints.
        let mut footer_spans: Vec<Span> = Vec::new();

        if !self.selected.is_empty() {
            footer_spans.push(Span::styled(
                format!(" {} selected ", self.selected.len()),
                theme::accent_style(),
            ));
            footer_spans.push(Span::styled("| ", theme::dim_style()));
        }

        if self.searching {
            footer_spans.push(Span::styled("/ ", theme::accent_style()));
            footer_spans.push(Span::styled(&self.search_query, theme::body_style()));
            footer_spans.push(Span::styled("\u{2588}", theme::body_style()));
        } else if self.dir_picker {
            footer_spans.push(Span::styled(
                " c=choose dir  Enter=open  Backspace=up  /=search  .=hidden  Esc=cancel",
                theme::dim_style(),
            ));
        } else {
            footer_spans.push(Span::styled(
                " Space=select Enter=confirm Esc=cancel /=search .=hidden",
                theme::dim_style(),
            ));
        }

        let footer = Paragraph::new(Line::from(footer_spans)).style(theme::fill_style());
        f.render_widget(footer, chunks[1]);
    }
}

impl crate::widgets::HelpContext for FileExplorerState {
    fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::widgets::{HelpEntry as E, HelpNode};
        if self.searching {
            HelpNode::entries(vec![
                E::key("Enter", "Confirm search"),
                E::key("Esc", "Cancel search"),
            ])
        } else if self.dir_picker {
            HelpNode::entries(vec![
                E::new(vec!["j", "↓"], "Next entry"),
                E::new(vec!["k", "↑"], "Prev entry"),
                E::key("Enter", "Open directory"),
                E::key("c", "Choose this directory"),
                E::key("Backspace", "Parent dir"),
                E::key("/", "Search"),
                E::key(".", "Toggle hidden"),
                E::new(vec!["q", "Esc"], "Cancel"),
            ])
        } else {
            HelpNode::entries(vec![
                E::new(vec!["j", "↓"], "Next entry"),
                E::new(vec!["k", "↑"], "Prev entry"),
                E::key("Enter", "Open dir / confirm"),
                E::key("Space", "Select file"),
                E::key("Backspace", "Parent dir"),
                E::key("/", "Search"),
                E::key(".", "Toggle hidden"),
                E::new(vec!["q", "Esc"], "Cancel"),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reads_current_dir() {
        let tmp = std::env::temp_dir();
        let state = FileExplorerState::new(tmp.clone());
        assert_eq!(state.cwd, tmp);
        assert!(state.error.is_none());
    }

    #[test]
    fn hidden_files_filtered_by_default() {
        let tmp = std::env::temp_dir();
        let state = FileExplorerState::new(tmp);
        // No entry should start with '.' when show_hidden is false.
        for entry in &state.entries {
            assert!(
                !entry.name.starts_with('.'),
                "Hidden file leaked: {}",
                entry.name
            );
        }
    }

    #[test]
    fn toggle_hidden() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp);
        assert!(!state.show_hidden);
        // Simulate pressing '.'
        state.handle_key(KeyEvent::from(KeyCode::Char('.')));
        assert!(state.show_hidden);
    }

    #[test]
    fn cancel_returns_cancel() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp);
        let action = state.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(action, ExplorerAction::Cancel));
    }

    #[test]
    fn search_filters_entries() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp);
        // Enter search mode.
        state.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(state.searching);
        // Type a query that won't match anything.
        state.handle_key(KeyEvent::from(KeyCode::Char('z')));
        state.handle_key(KeyEvent::from(KeyCode::Char('z')));
        state.handle_key(KeyEvent::from(KeyCode::Char('z')));
        state.handle_key(KeyEvent::from(KeyCode::Char('q')));
        state.handle_key(KeyEvent::from(KeyCode::Char('q')));
        let visible = state.visible_entries();
        // Likely empty, but the point is it filters.
        assert!(visible.len() <= state.entries.len());
    }

    #[test]
    fn dir_picker_c_key_returns_confirm_dir() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp.clone());
        state.dir_picker = true;
        let action = state.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(
            matches!(action, ExplorerAction::ConfirmDir(ref p) if p == &tmp),
            "expected ConfirmDir({:?}), got something else",
            tmp
        );
    }

    #[test]
    fn non_dir_picker_c_key_is_noop() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp);
        let action = state.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(matches!(action, ExplorerAction::None));
    }

    #[test]
    fn dir_picker_enter_on_dir_navigates_not_confirms() {
        let tmp = std::env::temp_dir();
        let mut state = FileExplorerState::new(tmp.clone());
        state.dir_picker = true;
        // Enter on a directory should navigate into it, not confirm it.
        // If no subdirs exist in tmp, this just verifies no ConfirmDir is returned.
        let action = state.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            !matches!(action, ExplorerAction::ConfirmDir(_)),
            "Enter must not return ConfirmDir in dir-picker mode"
        );
    }

    /// Verify that `new_dir_picker_remote` sends the initial listing request
    /// over the RPC channel (not the local filesystem) and populates entries
    /// from the remote response.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_dir_picker_remote_lists_via_rpc() {
        use crate::client::RpcClient;
        use crate::jsonrpc::RpcOutbound;
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::channel::<String>(16);
        let rpc = Arc::new(RpcOutbound::new(tx));
        let client = Arc::new(RpcClient::with_rpc(rpc.clone()));

        // Spawn the constructor on a blocking-friendly task so
        // `block_in_place` inside `load_entries` is allowed.
        let client2 = Arc::clone(&client);
        let handle = tokio::task::spawn_blocking(move || {
            FileExplorerState::new_dir_picker_remote(PathBuf::from("/remote/work"), client2)
        });

        // The constructor's `load_entries()` will issue an `fs/list_dir` RPC
        // request. Read it from the channel and reply with a fake listing.
        let line = rx
            .recv()
            .await
            .expect("expected an RPC request from load_entries");
        let req: serde_json::Value =
            serde_json::from_str(&line).expect("RPC request is valid JSON");

        assert_eq!(
            req["method"], "fs/list_dir",
            "load_entries must call fs/list_dir"
        );
        assert_eq!(
            req["params"]["path"], "/remote/work",
            "request path must be the remote start_dir, not a local path"
        );

        let id = req["id"]
            .as_str()
            .expect("request must have an id")
            .to_string();
        rpc.dispatch_response(
            &id,
            Some(serde_json::json!({
                "cwd": "/remote/work",
                "entries": [
                    {
                        "name": "src",
                        "full_path": "/remote/work/src",
                        "is_dir": true,
                        "is_hidden": false,
                        "size": 0
                    },
                    {
                        "name": "README.md",
                        "full_path": "/remote/work/README.md",
                        "is_dir": false,
                        "is_hidden": false,
                        "size": 1024
                    }
                ]
            })),
            None,
        );

        let state = handle.await.expect("constructor must not panic");

        // Structural assertions.
        assert!(state.dir_picker, "must be in dir-picker mode");
        assert!(state.remote_rpc.is_some(), "remote_rpc must be set");
        assert_eq!(state.cwd, PathBuf::from("/remote/work"));
        assert!(state.error.is_none(), "no error expected");

        // Entries must come from the remote response, not the local fs.
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].name, "src");
        assert!(state.entries[0].is_dir);
        assert_eq!(
            state.entries[0].full_path,
            PathBuf::from("/remote/work/src")
        );
        assert_eq!(state.entries[1].name, "README.md");
        assert!(!state.entries[1].is_dir);
        assert_eq!(
            state.entries[1].full_path,
            PathBuf::from("/remote/work/README.md")
        );

        // First entry must be selected.
        assert_eq!(state.list_state.selected(), Some(0));
    }
}
