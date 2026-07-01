use std::sync::Arc;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::client::{NodeRunState, RpcClient, SopDraft, SopStep, SopStepKind, StepFailure};

/// SOP authoring pane: lists SOPs from the daemon and renders the selected
/// SOP's projected node graph as text. The graph text is produced by the
/// backend (`sops/graph` returns the projection); this pane only formats
/// what it receives, never inferring graph shape itself.
pub(crate) struct SopPane {
    rpc: Arc<RpcClient>,
    names: Vec<String>,
    list_state: ListState,
    graph_lines: Vec<String>,
    run_input: Option<String>,
    overlay: Option<RunOverlayView>,
    editor: Option<SopEditorState>,
    error: Option<String>,
    status: Option<String>,
    animation_origin: std::time::Instant,
}

/// In-pane SOP authoring buffer. `create` distinguishes a new SOP (name still
/// editable, `sops/create` on save) from an edit of an existing one (`sops/save`
/// overwrite, name locked). Holds the canonical `SopDraft` directly; `field`
/// selects which attribute of the focused step the keyboard edits so routing,
/// failure policy, and kind are all authorable, not just titles.
struct SopEditorState {
    create: bool,
    draft: SopDraft,
    focus: EditorFocus,
    step_cursor: usize,
    field: StepField,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum EditorFocus {
    Name,
    Steps,
}

/// Which field of the focused step the keyboard edits. Cycled with Tab while
/// the Steps focus is active. Mirrors the web StepEditor's per-node controls:
/// title/body/tools/kind, plus routing (depends_on/next/when) and on_failure.
#[derive(PartialEq, Eq, Clone, Copy)]
enum StepField {
    Title,
    Body,
    Tools,
    Kind,
    DependsOn,
    Next,
    When,
    OnFailure,
    FailureArg,
}

impl StepField {
    fn cycle(self) -> Self {
        match self {
            Self::Title => Self::Body,
            Self::Body => Self::Tools,
            Self::Tools => Self::Kind,
            Self::Kind => Self::DependsOn,
            Self::DependsOn => Self::Next,
            Self::Next => Self::When,
            Self::When => Self::OnFailure,
            Self::OnFailure => Self::FailureArg,
            Self::FailureArg => Self::Title,
        }
    }
}

impl SopEditorState {
    fn new_create() -> Self {
        Self {
            create: true,
            draft: SopDraft::default(),
            focus: EditorFocus::Name,
            step_cursor: 0,
            field: StepField::Title,
        }
    }

    fn from_draft(create: bool, draft: SopDraft) -> Self {
        let draft = if draft.steps.is_empty() {
            SopDraft {
                steps: vec![SopStep {
                    number: 1,
                    ..SopStep::default()
                }],
                ..draft
            }
        } else {
            draft
        };
        Self {
            create,
            draft,
            focus: EditorFocus::Steps,
            step_cursor: 0,
            field: StepField::Title,
        }
    }

    /// Build the canonical `Sop` JSON. Steps with an empty title are dropped and
    /// the survivors renumbered 1-based; an empty body falls back to the title so
    /// `save_sop` strict validation passes. Routing `depends_on`/`next` are
    /// renumbered against the same index remap so wires stay consistent.
    fn to_sop_json(&self) -> serde_json::Value {
        let kept: Vec<&SopStep> = self
            .draft
            .steps
            .iter()
            .filter(|s| !s.title.trim().is_empty())
            .collect();
        // old step number -> new 1-based number
        let remap: std::collections::HashMap<u32, u32> = kept
            .iter()
            .enumerate()
            .map(|(i, s)| (s.number, i as u32 + 1))
            .collect();
        let steps: Vec<SopStep> = kept
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut out = (*s).clone();
                out.number = i as u32 + 1;
                if out.body.trim().is_empty() {
                    out.body = out.title.trim().to_string();
                }
                out.routing.depends_on = out
                    .routing
                    .depends_on
                    .iter()
                    .filter_map(|d| remap.get(d).copied())
                    .collect();
                out.routing.next = out.routing.next.and_then(|n| remap.get(&n).copied());
                if let StepFailure::Goto { step } = out.on_failure {
                    out.on_failure = remap
                        .get(&step)
                        .map(|s| StepFailure::Goto { step: *s })
                        .unwrap_or(StepFailure::Fail);
                }
                out
            })
            .collect();
        let draft = SopDraft {
            name: self.draft.name.trim().to_string(),
            steps,
            ..self.draft.clone()
        };
        serde_json::to_value(&draft).unwrap_or(serde_json::Value::Null)
    }
}

/// Append a char to a comma-separated string list. A comma finalizes the
/// current token and starts a new one; other chars extend the last token.
fn push_csv_char(list: &mut Vec<String>, c: char) {
    if c == ',' {
        list.push(String::new());
    } else if let Some(last) = list.last_mut() {
        last.push(c);
    } else {
        list.push(c.to_string());
    }
}

fn pop_csv_char(list: &mut Vec<String>) {
    if let Some(last) = list.last_mut() {
        last.pop();
        if last.is_empty() {
            list.pop();
        }
    }
}

/// Render a numeric list for display, joining with `, `.
fn num_csv(list: &[u32]) -> String {
    list.iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Edit a `Vec<u32>` as digits and commas. Non-digit, non-comma chars are
/// ignored. Trailing empty slots are represented as a 0 placeholder that the
/// display suppresses; on submit, zeros are filtered by the remap.
fn push_num_csv_char(list: &mut Vec<u32>, c: char) {
    if c == ',' {
        list.push(0);
        return;
    }
    let Some(d) = c.to_digit(10) else {
        return;
    };
    if let Some(last) = list.last_mut() {
        *last = last.saturating_mul(10).saturating_add(d);
    } else {
        list.push(d);
    }
}

fn pop_num_csv_char(list: &mut Vec<u32>) {
    if let Some(last) = list.last_mut() {
        if *last >= 10 {
            *last /= 10;
        } else {
            list.pop();
        }
    }
}

fn push_opt_u32_char(v: &mut Option<u32>, c: char) {
    let Some(d) = c.to_digit(10) else {
        return;
    };
    let cur = v.unwrap_or(0);
    *v = Some(cur.saturating_mul(10).saturating_add(d));
}

fn pop_opt_u32_char(v: &mut Option<u32>) {
    match v {
        Some(n) if *n >= 10 => *n /= 10,
        _ => *v = None,
    }
}

/// Edit the numeric argument of a `Retry{max}` / `Goto{step}` failure policy.
/// A no-op for `Fail` (it has no argument).
fn push_failure_arg_char(f: &mut StepFailure, c: char) {
    let Some(d) = c.to_digit(10) else {
        return;
    };
    match f {
        StepFailure::Retry { max } => *max = max.saturating_mul(10).saturating_add(d),
        StepFailure::Goto { step } => *step = step.saturating_mul(10).saturating_add(d),
        StepFailure::Fail => {}
    }
}

fn pop_failure_arg_char(f: &mut StepFailure) {
    match f {
        StepFailure::Retry { max } => *max /= 10,
        StepFailure::Goto { step } => *step /= 10,
        StepFailure::Fail => {}
    }
}

/// Human-readable label for a failure policy in the editor.
fn failure_label(f: &StepFailure) -> String {
    match f {
        StepFailure::Fail => "fail".to_string(),
        StepFailure::Retry { max } => format!("retry (max {max})"),
        StepFailure::Goto { step } => format!("goto step {step}"),
    }
}

/// Projected run state the pane overlays onto the graph: per-step states keyed
/// for marker lookup, plus the run-level status line. The backend produces the
/// projection (`sops/run-overlay`); this pane only formats what it receives.
struct RunOverlayView {
    status: String,
    current_step: u64,
    total_steps: u64,
    waiting: bool,
    paused: bool,
    states: std::collections::HashMap<u64, NodeRunState>,
}

impl SopPane {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            names: Vec::new(),
            list_state: ListState::default(),
            graph_lines: Vec::new(),
            run_input: None,
            overlay: None,
            editor: None,
            error: None,
            status: None,
            animation_origin: std::time::Instant::now(),
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
                self.overlay = None;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub(crate) async fn load_run_overlay(&mut self, run_id: &str) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_run_overlay(&name, run_id).await {
            Ok(value) => {
                self.overlay = Some(parse_overlay(&value));
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
        use crossterm::event::{KeyCode, KeyModifiers};
        if self.editor.is_some() {
            self.handle_editor_key(key).await;
            return false;
        }
        if self.run_input.is_some() {
            match key.code {
                KeyCode::Enter => self.submit_run_input().await, // keyguard: text-entry submit
                KeyCode::Esc => self.run_input = None,           // keyguard: text-entry cancel
                KeyCode::Backspace => self.run_input_backspace(), // keyguard: text-entry edit
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(buf) = self.run_input.as_mut() {
                        buf.push(c);
                    }
                }
                _ => {}
            }
            return false;
        }
        match SopTabAction::from_chord(&key) {
            Some(SopTabAction::Up) => self.select_prev(),
            Some(SopTabAction::Down) => self.select_next(),
            Some(SopTabAction::Enter) => self.load_selected_graph().await,
            Some(SopTabAction::Watch) => self.run_input = Some(String::new()),
            Some(SopTabAction::New) => self.editor = Some(SopEditorState::new_create()),
            Some(SopTabAction::Edit) => self.open_editor_for_selected().await,
            Some(SopTabAction::Delete) => self.delete_selected().await,
            None => {}
        }
        false
    }

    async fn open_editor_for_selected(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_get(&name).await {
            Ok(value) => match serde_json::from_value::<SopDraft>(value) {
                Ok(draft) => {
                    self.editor = Some(SopEditorState::from_draft(false, draft));
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("parse SOP '{name}': {e}")),
            },
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    async fn delete_selected(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_delete(&name).await {
            Ok(_) => {
                self.status = Some(format!("deleted {name}"));
                self.graph_lines.clear();
                self.overlay = None;
                self.list_state.select(None);
                self.refresh().await;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Route a key to the active editor. `Tab` advances focus: from the Name
    /// field into the step field cursor, then cycles through each step field
    /// (title/body/tools/kind/depends_on/next/when/on_failure/failure_arg) and
    /// wraps back to Name. `Up`/`Down` move between steps. `Ctrl+n` inserts a
    /// step, `Ctrl+s` saves, `Esc` cancels. Text fields take char/backspace;
    /// `kind` and `on_failure` toggle their enum with any char key.
    async fn handle_editor_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.editor = None, // keyguard: text-entry cancel
            KeyCode::Char('s') if ctrl => self.submit_editor().await, // keyguard: text-entry submit
            KeyCode::Char('n') if ctrl => self.editor_add_step(), // keyguard: text-entry add step
            KeyCode::Tab => self.editor_advance_focus(), // keyguard: text-entry focus advance
            KeyCode::Up => self.editor_step_up(), // keyguard: text-entry step cursor
            KeyCode::Down => self.editor_step_down(), // keyguard: text-entry step cursor
            KeyCode::Enter => self.editor_enter(), // keyguard: text-entry newline/advance
            KeyCode::Backspace => self.editor_backspace(), // keyguard: text-entry edit
            KeyCode::Char(c) if !ctrl => self.editor_push_char(c), // keyguard: text-entry char input
            _ => {}
        }
    }

    fn editor_advance_focus(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Name => {
                    ed.focus = EditorFocus::Steps;
                    ed.field = StepField::Title;
                }
                EditorFocus::Steps => {
                    let next = ed.field.cycle();
                    if next == StepField::Title {
                        ed.focus = EditorFocus::Name;
                    }
                    ed.field = next;
                }
            }
        }
    }

    fn editor_add_step(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            ed.step_cursor = (ed.step_cursor + 1).min(ed.draft.steps.len());
            let number = ed.draft.steps.len() as u32 + 1;
            ed.draft.steps.insert(
                ed.step_cursor,
                SopStep {
                    number,
                    ..SopStep::default()
                },
            );
            ed.focus = EditorFocus::Steps;
            ed.field = StepField::Title;
        }
    }

    fn editor_step_up(&mut self) {
        if let Some(ed) = self.editor.as_mut()
            && ed.focus == EditorFocus::Steps
            && ed.step_cursor > 0
        {
            ed.step_cursor -= 1;
        }
    }

    fn editor_step_down(&mut self) {
        if let Some(ed) = self.editor.as_mut()
            && ed.focus == EditorFocus::Steps
            && ed.step_cursor + 1 < ed.draft.steps.len()
        {
            ed.step_cursor += 1;
        }
    }

    fn editor_enter(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Name => {
                    ed.focus = EditorFocus::Steps;
                    ed.field = StepField::Title;
                }
                EditorFocus::Steps => self.editor_add_step(),
            }
        }
    }

    fn editor_push_char(&mut self, c: char) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        if ed.focus == EditorFocus::Name {
            ed.draft.name.push(c);
            return;
        }
        let cursor = ed.step_cursor;
        let field = ed.field;
        let Some(step) = ed.draft.steps.get_mut(cursor) else {
            return;
        };
        match field {
            StepField::Title => step.title.push(c),
            StepField::Body => step.body.push(c),
            StepField::Tools => push_csv_char(&mut step.suggested_tools, c),
            StepField::Kind => {
                step.kind = match step.kind {
                    SopStepKind::Execute => SopStepKind::Checkpoint,
                    SopStepKind::Checkpoint => SopStepKind::Execute,
                };
            }
            StepField::DependsOn => push_num_csv_char(&mut step.routing.depends_on, c),
            StepField::Next => push_opt_u32_char(&mut step.routing.next, c),
            StepField::When => {
                let mut w = step.routing.when.take().unwrap_or_default();
                w.push(c);
                step.routing.when = Some(w);
            }
            StepField::OnFailure => {
                step.on_failure = match step.on_failure {
                    StepFailure::Fail => StepFailure::Retry { max: 1 },
                    StepFailure::Retry { .. } => StepFailure::Goto { step: 1 },
                    StepFailure::Goto { .. } => StepFailure::Fail,
                };
            }
            StepField::FailureArg => push_failure_arg_char(&mut step.on_failure, c),
        }
    }

    fn editor_backspace(&mut self) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        if ed.focus == EditorFocus::Name {
            ed.draft.name.pop();
            return;
        }
        let cursor = ed.step_cursor;
        let field = ed.field;
        let len = ed.draft.steps.len();
        let Some(step) = ed.draft.steps.get_mut(cursor) else {
            return;
        };
        match field {
            StepField::Title => {
                let empty = step.title.is_empty();
                if empty && len > 1 {
                    ed.draft.steps.remove(cursor);
                    if ed.step_cursor > 0 {
                        ed.step_cursor -= 1;
                    }
                } else {
                    step.title.pop();
                }
            }
            StepField::Body => {
                step.body.pop();
            }
            StepField::Tools => pop_csv_char(&mut step.suggested_tools),
            StepField::Kind => {}
            StepField::DependsOn => pop_num_csv_char(&mut step.routing.depends_on),
            StepField::Next => pop_opt_u32_char(&mut step.routing.next),
            StepField::When => {
                if let Some(w) = step.routing.when.as_mut() {
                    w.pop();
                    if w.is_empty() {
                        step.routing.when = None;
                    }
                }
            }
            StepField::OnFailure => {}
            StepField::FailureArg => pop_failure_arg_char(&mut step.on_failure),
        }
    }

    async fn submit_editor(&mut self) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        if ed.draft.name.trim().is_empty() {
            self.error = Some("SOP name is required".to_string());
            return;
        }
        let create = ed.create;
        let name = ed.draft.name.trim().to_string();
        let sop = ed.to_sop_json();
        let result = if create {
            self.rpc.sops_create(sop).await
        } else {
            self.rpc.sops_save(sop).await
        };
        match result {
            Ok(_) => {
                self.editor = None;
                self.status = Some(format!("saved {name}"));
                self.error = None;
                self.refresh().await;
                if let Some(i) = self.names.iter().position(|n| n == &name) {
                    self.list_state.select(Some(i));
                    self.load_selected_graph().await;
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    async fn submit_run_input(&mut self) {
        let run_id = self
            .run_input
            .take()
            .map(|b| b.trim().to_string())
            .unwrap_or_default();
        if !run_id.is_empty() {
            self.load_run_overlay(&run_id).await;
        }
    }

    fn run_input_backspace(&mut self) {
        if let Some(buf) = self.run_input.as_mut() {
            buf.pop();
        }
    }

    pub(crate) fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::SopTabAction as S;
        crate::widgets::HelpNode::entries(crate::help::entries_for([
            S::Up,
            S::Down,
            S::Enter,
            S::Watch,
            S::New,
            S::Edit,
            S::Delete,
        ]))
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
        } else if self.editor.is_some() {
            self.editor_lines().join("\n")
        } else {
            self.body_lines().join("\n")
        };
        let title = if self.editor.is_some() {
            "Editor  [Tab field | ↑↓ step | Ctrl+n add | Ctrl+s save | Esc cancel]".to_string()
        } else {
            match &self.overlay {
                Some(o) => format!(
                    "Graph [{} {}/{}{}]",
                    o.status,
                    o.current_step,
                    o.total_steps,
                    if o.waiting {
                        " waiting"
                    } else if o.paused {
                        " paused"
                    } else {
                        ""
                    }
                ),
                None => match &self.status {
                    Some(s) => format!("Graph  ({s})"),
                    None => "Graph".to_string(),
                },
            }
        };
        let para = Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        f.render_widget(para, cols[1]);
    }

    /// Display lines: the graph lines, prefixed with per-step state markers when
    /// a run overlay is active, with the run-id prompt appended when entering it.
    /// The active node's marker cycles through a spinner phase (same cadence as
    /// `turn_status`) so the live step visibly pulses across the TUI surface.
    fn body_lines(&self) -> Vec<String> {
        let phase =
            (self.animation_origin.elapsed().as_millis() / 200) % ACTIVE_SPINNER.len() as u128;
        let active = ACTIVE_SPINNER[phase as usize];
        let mut lines: Vec<String> = self
            .graph_lines
            .iter()
            .map(|line| match &self.overlay {
                Some(o) => match leading_step(line).and_then(|s| o.states.get(&s)) {
                    Some(state) => format!("{} {line}", state_marker(*state, active)),
                    None => format!("  {line}"),
                },
                None => line.clone(),
            })
            .collect();
        if let Some(buf) = &self.run_input {
            lines.push(String::new());
            lines.push(format!("run id: {buf}_"));
        }
        lines
    }

    /// Render the editor buffer: the name field, then each step expanded into
    /// its editable fields (title/body/tools/kind/routing/on_failure) with a
    /// cursor marker on the focused field of the focused step.
    fn editor_lines(&self) -> Vec<String> {
        let Some(ed) = &self.editor else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let name_focus = ed.focus == EditorFocus::Name;
        lines.push(format!(
            "{} name: {}{}",
            if name_focus { ">" } else { " " },
            ed.draft.name,
            if name_focus { "_" } else { "" }
        ));
        lines.push(String::new());
        let steps_focus = ed.focus == EditorFocus::Steps;
        for (i, step) in ed.draft.steps.iter().enumerate() {
            let on_step = steps_focus && i == ed.step_cursor;
            let marker = |field: StepField| {
                if on_step && ed.field == field {
                    (">", "_")
                } else {
                    (" ", "")
                }
            };
            let kind = match step.kind {
                SopStepKind::Execute => "execute",
                SopStepKind::Checkpoint => "checkpoint",
            };
            lines.push(format!("── step {} ──", i + 1));
            let (m, cur) = marker(StepField::Title);
            lines.push(format!("{m} title: {}{cur}", step.title));
            let (m, cur) = marker(StepField::Body);
            lines.push(format!("{m} body: {}{cur}", step.body));
            let (m, cur) = marker(StepField::Tools);
            lines.push(format!(
                "{m} tools: {}{cur}",
                step.suggested_tools.join(", ")
            ));
            let (m, _) = marker(StepField::Kind);
            lines.push(format!("{m} kind: {kind}"));
            let (m, cur) = marker(StepField::DependsOn);
            lines.push(format!(
                "{m} depends_on: {}{cur}",
                num_csv(&step.routing.depends_on)
            ));
            let (m, cur) = marker(StepField::Next);
            lines.push(format!(
                "{m} next: {}{cur}",
                step.routing.next.map(|n| n.to_string()).unwrap_or_default()
            ));
            let (m, cur) = marker(StepField::When);
            lines.push(format!(
                "{m} when: {}{cur}",
                step.routing.when.clone().unwrap_or_default()
            ));
            let (m, _) = marker(StepField::OnFailure);
            lines.push(format!(
                "{m} on_failure: {}",
                failure_label(&step.on_failure)
            ));
            if !matches!(step.on_failure, StepFailure::Fail) {
                let (m, cur) = marker(StepField::FailureArg);
                let arg = match step.on_failure {
                    StepFailure::Retry { max } => max.to_string(),
                    StepFailure::Goto { step } => step.to_string(),
                    StepFailure::Fail => String::new(),
                };
                lines.push(format!("{m}   arg: {arg}{cur}"));
            }
            lines.push(String::new());
        }
        lines
    }
}

/// Spinner frames for the active node, cycled by `body_lines` at the TUI redraw
/// cadence to give the live step a visible pulse.
const ACTIVE_SPINNER: [&str; 4] = ["|>", "/>", "->", "\\>"];

/// State glyph for an overlaid node. The active node is animated: its glyph is
/// supplied by the caller from the spinner phase. All other states are static.
fn state_marker(state: NodeRunState, active_frame: &str) -> String {
    match state {
        NodeRunState::Active => active_frame.to_string(),
        NodeRunState::Completed => "ok".to_string(),
        NodeRunState::Failed => "xx".to_string(),
        NodeRunState::Skipped => "--".to_string(),
        NodeRunState::Pending => "..".to_string(),
    }
}

/// The leading step number of a graph line formatted as `N. title ...`.
fn leading_step(line: &str) -> Option<u64> {
    line.split_once('.')
        .and_then(|(head, _)| head.trim().parse().ok())
}

/// Parse the `sops/run-overlay` projection into the pane's overlay view.
fn parse_overlay(value: &serde_json::Value) -> RunOverlayView {
    let states = value
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|node| {
                    let step = node.get("step").and_then(serde_json::Value::as_u64)?;
                    let state: NodeRunState =
                        serde_json::from_value(node.get("state")?.clone()).ok()?;
                    Some((step, state))
                })
                .collect()
        })
        .unwrap_or_default();
    RunOverlayView {
        status: value
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
        current_step: value
            .get("current_step")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        total_steps: value
            .get("total_steps")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        waiting: value
            .get("waiting")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        paused: value
            .get("paused")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        states,
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
    use crate::client::StepRouting;
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

    #[test]
    fn leading_step_parses_graph_line() {
        assert_eq!(leading_step("3. do thing -> 4"), Some(3));
        assert_eq!(leading_step("  diagnostics:"), None);
    }

    #[test]
    fn parse_overlay_extracts_states_and_status() {
        let v = serde_json::json!({
            "run_id": "run-1",
            "sop_name": "alpha",
            "status": "running",
            "current_step": 2,
            "total_steps": 3,
            "waiting": false,
            "paused": false,
            "nodes": [
                { "step": 1, "state": "completed" },
                { "step": 2, "state": "active" }
            ]
        });
        let o = parse_overlay(&v);
        assert_eq!(o.status, "running");
        assert_eq!(o.current_step, 2);
        assert_eq!(o.total_steps, 3);
        assert_eq!(o.states.get(&1).copied(), Some(NodeRunState::Completed));
        assert_eq!(o.states.get(&2).copied(), Some(NodeRunState::Active));
    }

    #[tokio::test]
    async fn body_lines_prefix_state_markers_when_overlaid() {
        let (client, _rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.graph_lines = vec!["1. a -> 2".into(), "2. b".into()];
        let v = serde_json::json!({
            "status": "running", "current_step": 2, "total_steps": 2,
            "waiting": false, "paused": false,
            "nodes": [
                { "step": 1, "state": "completed" },
                { "step": 2, "state": "active" }
            ]
        });
        pane.overlay = Some(parse_overlay(&v));
        let lines = pane.body_lines();
        assert_eq!(lines[0], "ok 1. a -> 2");
        // Active node's marker is the first spinner frame right after construction.
        assert_eq!(lines[1], format!("{} 2. b", ACTIVE_SPINNER[0]));
    }

    #[tokio::test]
    async fn load_run_overlay_calls_rpc() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.names = vec!["alpha".into()];
        pane.list_state.select(Some(0));
        let task = tokio::spawn(async move {
            pane.load_run_overlay("run-1").await;
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_RUN_OVERLAY);
        assert_eq!(req["params"]["name"], "alpha");
        assert_eq!(req["params"]["run_id"], "run-1");
        task.abort();
    }

    #[test]
    fn editor_to_sop_json_numbers_steps_and_drops_blanks() {
        let mut ed = SopEditorState::new_create();
        ed.draft.name = "demo".into();
        ed.draft.steps = vec![
            SopStep {
                number: 1,
                title: "first".into(),
                ..SopStep::default()
            },
            SopStep {
                number: 2,
                title: "  ".into(),
                ..SopStep::default()
            },
            SopStep {
                number: 3,
                title: "third".into(),
                ..SopStep::default()
            },
        ];
        let sop = ed.to_sop_json();
        assert_eq!(sop["name"], "demo");
        let steps = sop["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["number"], 1);
        assert_eq!(steps[0]["title"], "first");
        assert_eq!(steps[1]["number"], 2);
        assert_eq!(steps[1]["title"], "third");
        assert_eq!(sop["triggers"][0]["type"], "manual");
    }

    #[test]
    fn editor_to_sop_json_remaps_routing_and_failure_after_drop() {
        // step 2 is blank and dropped; a depends_on/goto that referenced the
        // surviving steps must renumber, and a reference to the dropped step is
        // pruned (depends_on) or reset to Fail (goto).
        let mut ed = SopEditorState::new_create();
        ed.draft.name = "r".into();
        ed.draft.steps = vec![
            SopStep {
                number: 1,
                title: "a".into(),
                routing: StepRouting {
                    depends_on: vec![3],
                    ..StepRouting::default()
                },
                ..SopStep::default()
            },
            SopStep {
                number: 2,
                title: "  ".into(),
                ..SopStep::default()
            },
            SopStep {
                number: 3,
                title: "c".into(),
                on_failure: StepFailure::Goto { step: 1 },
                ..SopStep::default()
            },
        ];
        let sop = ed.to_sop_json();
        let steps = sop["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 2);
        // old step 3 -> new step 2; step 1 depends_on old 3 -> new 2
        assert_eq!(steps[0]["routing"]["depends_on"][0], 2);
        // old step 3's goto old 1 -> new 1
        assert_eq!(steps[1]["on_failure"]["goto"]["step"], 1);
    }

    #[test]
    fn editor_edits_routing_and_failure_fields() {
        let mut ed = SopEditorState::new_create();
        ed.draft.name = "e".into();
        ed.draft.steps[0].title = "s".into();
        push_num_csv_char(&mut ed.draft.steps[0].routing.depends_on, '2');
        assert_eq!(ed.draft.steps[0].routing.depends_on, vec![2]);
        ed.draft.steps[0].on_failure = StepFailure::Retry { max: 0 };
        push_failure_arg_char(&mut ed.draft.steps[0].on_failure, '5');
        assert_eq!(ed.draft.steps[0].on_failure, StepFailure::Retry { max: 5 });
        ed.draft.steps[0].routing.when = Some("$.x".into());
        let sop = ed.to_sop_json();
        assert_eq!(sop["steps"][0]["on_failure"]["retry"]["max"], 5);
        assert_eq!(sop["steps"][0]["routing"]["when"], "$.x");
    }

    #[tokio::test]
    async fn submit_editor_create_calls_sops_create() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        let mut ed = SopEditorState::new_create();
        ed.draft.name = "brandnew".into();
        ed.draft.steps = vec![SopStep {
            number: 1,
            title: "do".into(),
            ..SopStep::default()
        }];
        pane.editor = Some(ed);
        let task = tokio::spawn(async move {
            pane.submit_editor().await;
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_CREATE);
        assert_eq!(req["params"]["sop"]["name"], "brandnew");
        task.abort();
    }

    #[tokio::test]
    async fn submit_editor_edit_calls_sops_save() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        let draft = SopDraft {
            name: "existing".into(),
            steps: vec![SopStep {
                number: 1,
                title: "step one".into(),
                ..SopStep::default()
            }],
            ..SopDraft::default()
        };
        pane.editor = Some(SopEditorState::from_draft(false, draft));
        let task = tokio::spawn(async move {
            pane.submit_editor().await;
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_SAVE);
        assert_eq!(req["params"]["sop"]["name"], "existing");
        task.abort();
    }

    #[tokio::test]
    async fn delete_selected_calls_sops_delete() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.names = vec!["gone".into()];
        pane.list_state.select(Some(0));
        let task = tokio::spawn(async move {
            pane.delete_selected().await;
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_DELETE);
        assert_eq!(req["params"]["name"], "gone");
        task.abort();
    }

    #[tokio::test]
    async fn open_editor_for_selected_calls_sops_get() {
        let (client, mut rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.names = vec!["alpha".into()];
        pane.list_state.select(Some(0));
        let task = tokio::spawn(async move {
            pane.open_editor_for_selected().await;
            pane
        });
        let req = next_request(&mut rx).await;
        assert_eq!(req["method"], method::SOPS_GET);
        assert_eq!(req["params"]["name"], "alpha");
        task.abort();
    }

    #[tokio::test]
    async fn submit_editor_rejects_blank_name() {
        let (client, _rx) = test_client_with_rpc();
        let mut pane = SopPane::new(client);
        pane.editor = Some(SopEditorState::new_create());
        pane.submit_editor().await;
        assert!(pane.editor.is_some());
        assert!(pane.error.is_some());
    }
}
