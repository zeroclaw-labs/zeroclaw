use std::sync::Arc;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::client::{
    FlowRole, GraphLayout, GraphNode, GraphPin, GraphWire, NodeKind, NodeRunState, PinClass,
    PlannedToolCall, RpcClient, SopDraft, SopGraphView, SopStep, SopStepKind, StepFailure,
    SwitchRule,
};

pub(crate) struct SopPane {
    rpc: Arc<RpcClient>,
    names: Vec<String>,
    list_state: ListState,
    graph: SopGraphView,
    layer: RenderLayer,
    run_input: Option<String>,
    run_payload_input: Option<String>,
    overlay: Option<RunOverlayView>,
    current_run_id: Option<String>,
    editor: Option<SopEditorState>,
    trigger_registry: crate::client::TriggerSourceRegistryView,
    error: Option<String>,
    status: Option<String>,
    animation_origin: std::time::Instant,
    list_row_rects: Vec<Rect>,
    node_rects: Vec<(u32, Rect)>,
    handle_rects: Vec<(u32, FlowRole, Option<usize>, Rect)>,
    add_rects: Vec<(u32, FlowRole, Option<usize>, Rect)>,
    wire_rects: Vec<(u32, u32, FlowRole, Option<usize>, Rect)>,
    link_from: Option<(u32, FlowRole, Option<usize>)>,
    editor_graph: SopGraphView,
    pan_x: u16,
    pan_y: u16,
    canvas_rect: Rect,
    pan_drag: Option<(u16, u16, u16, u16)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum RenderLayer {
    #[default]
    Visual,
    Fields,
}

impl RenderLayer {
    fn toggled(self) -> Self {
        match self {
            RenderLayer::Visual => RenderLayer::Fields,
            RenderLayer::Fields => RenderLayer::Visual,
        }
    }
}

struct SopEditorState {
    create: bool,
    original_name: Option<String>,
    draft: SopDraft,
    focus: EditorFocus,
    step_cursor: usize,
    trigger_cursor: usize,
    field: StepField,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum EditorFocus {
    Name,
    Triggers,
    Steps,
}

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
    Switch,
    Calls,
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
            Self::FailureArg => Self::Switch,
            Self::Switch => Self::Calls,
            Self::Calls => Self::Title,
        }
    }
}

impl SopEditorState {
    fn new_create() -> Self {
        Self {
            create: true,
            original_name: None,
            draft: SopDraft::default(),
            focus: EditorFocus::Name,
            step_cursor: 0,
            trigger_cursor: 0,
            field: StepField::Title,
        }
    }

    fn from_draft(create: bool, draft: SopDraft) -> Self {
        let original_name = if create {
            None
        } else {
            Some(draft.name.trim().to_string())
        };
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
            original_name,
            draft,
            focus: EditorFocus::Steps,
            step_cursor: 0,
            trigger_cursor: 0,
            field: StepField::Title,
        }
    }

    fn to_sop_json(&self) -> serde_json::Value {
        let steps: Vec<SopStep> = self
            .draft
            .steps
            .iter()
            .filter(|s| !s.title.trim().is_empty())
            .map(|s| {
                let mut out = s.clone();
                if out.body.trim().is_empty() {
                    out.body = out.title.trim().to_string();
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

fn num_csv(list: &[u32]) -> String {
    list.iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn switch_to_text(rules: &[SwitchRule]) -> String {
    rules
        .iter()
        .map(|r| {
            let goto = r.goto_buf.clone().or_else(|| r.goto.map(|g| g.to_string()));
            match (&r.when, goto) {
                (_, Some(g)) => {
                    let when = r.when.as_deref().unwrap_or("");
                    format!("{}>{}>{}", r.name, when, g)
                }
                (Some(w), None) => format!("{}>{}", r.name, w),
                (None, None) => r.name.clone(),
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn push_switch_char(rules: &mut Vec<SwitchRule>, c: char) {
    if c == ';' {
        rules.push(SwitchRule::default());
        return;
    }
    if rules.is_empty() {
        rules.push(SwitchRule::default());
    }
    let rule = rules.last_mut().expect("just pushed");
    if c == '>' {
        if rule.when.is_none() {
            rule.when = Some(String::new());
        } else if rule.goto_buf.is_none() {
            rule.goto_buf = Some(String::new());
        }
        return;
    }
    match (&mut rule.when, &mut rule.goto_buf) {
        (_, Some(g)) => {
            if c.is_ascii_digit() {
                g.push(c);
                rule.goto = g.parse::<u32>().ok();
            }
        }
        (Some(w), None) => w.push(c),
        (None, None) => rule.name.push(c),
    }
}

fn pop_switch_char(rules: &mut Vec<SwitchRule>) {
    let Some(rule) = rules.last_mut() else {
        return;
    };
    match (&mut rule.when, &mut rule.goto_buf) {
        (_, Some(g)) => {
            if g.pop().is_none() {
                rule.goto_buf = None;
                rule.goto = None;
            } else {
                rule.goto = g.parse::<u32>().ok();
            }
        }
        (Some(w), None) => {
            if w.pop().is_none() {
                rule.when = None;
            }
        }
        (None, None) => {
            if rule.name.pop().is_none() {
                rules.pop();
            }
        }
    }
}

/// Text codec for the calls field: the buffer holds raw JSON while the
/// operator types; a parseable buffer materializes into `calls` and an
/// unparseable one stays in the buffer so nothing is lost mid-edit.
fn calls_text(step: &SopStep) -> String {
    match &step.calls_buf {
        Some(buf) => buf.clone(),
        None if step.calls.is_empty() => String::new(),
        None => serde_json::to_string(&step.calls).unwrap_or_default(),
    }
}

fn sync_calls_from_buf(step: &mut SopStep) {
    let Some(buf) = &step.calls_buf else {
        return;
    };
    if buf.trim().is_empty() {
        step.calls.clear();
        return;
    }
    if let Ok(parsed) = serde_json::from_str::<Vec<PlannedToolCall>>(buf) {
        step.calls = parsed;
    }
}

fn push_calls_char(step: &mut SopStep, c: char) {
    let mut buf = step.calls_buf.take().unwrap_or_else(|| calls_text(step));
    buf.push(c);
    step.calls_buf = Some(buf);
    sync_calls_from_buf(step);
}

fn pop_calls_char(step: &mut SopStep) {
    let mut buf = step.calls_buf.take().unwrap_or_else(|| calls_text(step));
    buf.pop();
    step.calls_buf = if buf.is_empty() { None } else { Some(buf) };
    if step.calls_buf.is_none() {
        step.calls.clear();
    } else {
        sync_calls_from_buf(step);
    }
}

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

fn cycle_pick<T: Clone + PartialEq>(items: &[T], cur: &T, forward: bool) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let idx = items.iter().position(|s| s == cur).unwrap_or(0);
    let next = if forward {
        (idx + 1) % items.len()
    } else {
        (idx + items.len() - 1) % items.len()
    };
    items.get(next).cloned()
}

/// Ordered trigger source list rendered by the picker. Prefers the
/// backend-walked `sources` so ordering and membership match the runtime
/// `SopTriggerSource` enum exactly; falls back to reconstructing from `bound` +
/// `channel` only when an old or failed registry response omits `sources`, so
/// the picker still works against a stale daemon.
fn trigger_source_walk(registry: &crate::client::TriggerSourceRegistryView) -> Vec<String> {
    if !registry.sources.is_empty() {
        return registry.sources.clone();
    }
    let mut sources: Vec<String> = registry.bound.iter().map(|b| b.source.clone()).collect();
    sources.push("channel".to_string());
    sources
}

fn failure_label(f: &StepFailure) -> String {
    match f {
        StepFailure::Fail => "fail".to_string(),
        StepFailure::Retry { max } => format!("retry (max {max})"),
        StepFailure::Goto { step } => format!("goto step {step}"),
    }
}

#[derive(Default, serde::Deserialize)]
struct OverlayCallView {
    index: u64,
    tool: String,
    success: bool,
    duration_ms: u64,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct OverlayNodeView {
    step: u64,
    state: NodeRunState,
    tool_calls: Vec<OverlayCallView>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct RunOverlayView {
    status: String,
    current_step: u64,
    total_steps: u64,
    waiting: bool,
    paused: bool,
    #[serde(rename = "nodes")]
    node_states: Vec<OverlayNodeView>,
}

impl RunOverlayView {
    /// True when the run is parked awaiting an operator decision. Reads the
    /// backend-projected `waiting`/`paused` flags (derived server-side from the
    /// run status) rather than testing status strings here, so the set of
    /// awaiting states stays owned by the runtime.
    fn awaiting_decision(&self) -> bool {
        self.waiting || self.paused
    }

    fn state_of(&self, step: u64) -> Option<NodeRunState> {
        self.node_states
            .iter()
            .find(|n| n.step == step)
            .map(|n| n.state)
    }

    fn calls_of(&self, step: u64) -> &[OverlayCallView] {
        self.node_states
            .iter()
            .find(|n| n.step == step)
            .map(|n| n.tool_calls.as_slice())
            .unwrap_or(&[])
    }
}

impl SopPane {
    pub(crate) fn new(rpc: Arc<RpcClient>) -> Self {
        Self {
            rpc,
            names: Vec::new(),
            list_state: ListState::default(),
            graph: SopGraphView::default(),
            layer: RenderLayer::default(),
            run_input: None,
            run_payload_input: None,
            overlay: None,
            current_run_id: None,
            editor: None,
            trigger_registry: crate::client::TriggerSourceRegistryView::default(),
            error: None,
            status: None,
            animation_origin: std::time::Instant::now(),
            list_row_rects: Vec::new(),
            node_rects: Vec::new(),
            handle_rects: Vec::new(),
            add_rects: Vec::new(),
            wire_rects: Vec::new(),
            link_from: None,
            editor_graph: SopGraphView::default(),
            pan_x: 0,
            pan_y: 0,
            canvas_rect: Rect::new(0, 0, 0, 0),
            pan_drag: None,
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
                #[derive(serde::Deserialize)]
                struct Named {
                    name: String,
                }
                self.names = serde_json::from_value::<Vec<Named>>(value)
                    .map(|v| v.into_iter().map(|n| n.name).collect())
                    .unwrap_or_default();
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
        match self.rpc.sops_graph_view(&name).await {
            Ok(view) => {
                self.graph = view;
                self.overlay = None;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub(crate) fn toggle_layer(&mut self) {
        self.layer = self.layer.toggled();
    }

    pub(crate) async fn load_run_overlay(&mut self, run_id: &str) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_run_overlay(&name, run_id).await {
            Ok(value) => {
                self.overlay = serde_json::from_value(value).ok();
                self.current_run_id = Some(run_id.to_string());
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Resolve the current run's checkpoint. `approve` picks the success path;
    /// otherwise the failure path (`deny` with no reason). The decision is the
    /// `ApprovalDecision` wire shape; the daemon owns interpretation and routing.
    /// Refreshes the overlay so the post-decision state renders immediately.
    pub(crate) async fn decide_checkpoint(&mut self, approve: bool) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        let Some(run_id) = self.current_run_id.clone() else {
            return;
        };
        let awaiting = self
            .overlay
            .as_ref()
            .is_some_and(RunOverlayView::awaiting_decision);
        if !awaiting {
            self.status = Some("run is not awaiting a decision".into());
            return;
        }
        let decision = if approve {
            serde_json::json!("approve")
        } else {
            serde_json::json!({ "deny": {} })
        };
        match self.rpc.sops_decide(&name, &run_id, decision).await {
            Ok(value) => {
                self.overlay = serde_json::from_value(value).ok();
                self.status = Some(if approve {
                    "checkpoint approved".into()
                } else {
                    "checkpoint denied".into()
                });
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
        if self.run_payload_input.is_some() {
            match key.code {
                KeyCode::Enter => self.submit_run_payload().await, // keyguard: text-entry submit
                KeyCode::Esc => self.run_payload_input = None,     // keyguard: text-entry cancel
                KeyCode::Backspace => self.run_payload_backspace(), // keyguard: text-entry edit
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(buf) = self.run_payload_input.as_mut() {
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
            Some(SopTabAction::Run) => self.start_run_payload().await,
            Some(SopTabAction::Watch) => self.run_input = Some(String::new()),
            Some(SopTabAction::New) => {
                self.editor = Some(SopEditorState::new_create());
                self.refresh_trigger_registry().await;
            }
            Some(SopTabAction::Edit) => self.open_editor_for_selected().await,
            Some(SopTabAction::Delete) => self.delete_selected().await,
            Some(SopTabAction::Approve) => self.decide_checkpoint(true).await,
            Some(SopTabAction::Deny) => self.decide_checkpoint(false).await,
            Some(SopTabAction::Toggle) => self.toggle_layer(),
            Some(SopTabAction::PanLeft) => self.pan_x = self.pan_x.saturating_sub(4),
            Some(SopTabAction::PanRight) => self.pan_x = self.pan_x.saturating_add(4),
            Some(SopTabAction::PanUp) => self.pan_y = self.pan_y.saturating_sub(2),
            Some(SopTabAction::PanDown) => self.pan_y = self.pan_y.saturating_add(2),
            None => {}
        }
        false
    }

    pub(crate) async fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let (col, row) = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(idx) = self
                    .list_row_rects
                    .iter()
                    .position(|r| in_rect(col, row, *r))
                {
                    self.list_state.select(Some(idx));
                    self.load_selected_graph().await;
                    return;
                }
                if self.editor.is_some() {
                    if self.link_from.is_some() {
                        if let Some((step, _)) = self
                            .node_rects
                            .iter()
                            .find(|(_, r)| in_rect(col, row, *r))
                            .copied()
                        {
                            self.complete_link(step).await;
                            return;
                        }
                        self.link_from = None;
                        self.status = None;
                        return;
                    }
                    if let Some((step, role, port, _)) = self
                        .add_rects
                        .iter()
                        .find(|(_, _, _, r)| in_rect(col, row, *r))
                        .copied()
                    {
                        self.add_step_from(step, role, port).await;
                        return;
                    }
                    if let Some((step, role, port, _)) = self
                        .handle_rects
                        .iter()
                        .find(|(_, _, _, r)| in_rect(col, row, *r))
                        .copied()
                    {
                        self.start_link(step, role, port);
                        return;
                    }
                    if let Some((from, to, role, port, _)) = self
                        .wire_rects
                        .iter()
                        .find(|(_, _, _, _, r)| in_rect(col, row, *r))
                        .copied()
                    {
                        self.delete_wire(from, to, role, port).await;
                        return;
                    }
                }
                if let Some((step, _)) = self
                    .node_rects
                    .iter()
                    .find(|(_, r)| in_rect(col, row, *r))
                    .copied()
                {
                    if self.editor.is_some() {
                        self.focus_editor_step(step);
                    } else {
                        self.open_editor_for_step(step).await;
                    }
                    return;
                }
                // Empty canvas background: anchor a left-drag pan. Records the
                // press point and the pan offsets at press time; the Drag arm
                // walks pan_x/pan_y relative to this anchor.
                if in_rect(col, row, self.canvas_rect) {
                    self.pan_drag = Some((col, row, self.pan_x, self.pan_y));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((start_col, start_row, base_x, base_y)) = self.pan_drag {
                    let dx = col as i32 - start_col as i32;
                    let dy = row as i32 - start_row as i32;
                    self.pan_x = (base_x as i32 - dx).clamp(0, u16::MAX as i32) as u16;
                    self.pan_y = (base_y as i32 - dy).clamp(0, u16::MAX as i32) as u16;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.pan_drag = None,
            MouseEventKind::ScrollUp => self.select_prev(),
            MouseEventKind::ScrollDown => self.select_next(),
            _ => {}
        }
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
                    self.refresh_editor_graph().await;
                    self.refresh_trigger_registry().await;
                }
                Err(e) => self.error = Some(format!("parse SOP '{name}': {e}")),
            },
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn focus_editor_step(&mut self, step: u32) {
        if let Some(ed) = self.editor.as_mut()
            && let Some(idx) = ed.draft.steps.iter().position(|s| s.number == step)
        {
            ed.focus = EditorFocus::Steps;
            ed.step_cursor = idx;
        }
    }

    async fn open_editor_for_step(&mut self, step: u32) {
        self.open_editor_for_selected().await;
        self.focus_editor_step(step);
    }

    async fn refresh_editor_graph(&mut self) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let sop = editor.to_sop_json();
        if let Ok(view) = self.rpc.sops_graph_draft(sop).await {
            self.editor_graph = view;
        }
    }

    async fn refresh_trigger_registry(&mut self) {
        if let Ok(reg) = self.rpc.sops_trigger_sources().await {
            self.trigger_registry = reg;
        }
    }

    fn start_link(&mut self, step: u32, role: FlowRole, port: Option<usize>) {
        if self.editor.is_some() {
            self.link_from = Some((step, role, port));
            self.status = Some("wiring: click a target node (Esc to cancel)".into());
        }
    }

    async fn complete_link(&mut self, target: u32) {
        let Some((from, role, port)) = self.link_from.take() else {
            return;
        };
        self.status = None;
        if from == target {
            return;
        }
        self.apply_wire_edit("connect", from, target, role, port)
            .await;
    }

    async fn delete_wire(&mut self, from: u32, to: u32, role: FlowRole, port: Option<usize>) {
        self.apply_wire_edit("disconnect", from, to, role, port)
            .await;
    }

    async fn apply_wire_edit(
        &mut self,
        op: &str,
        from: u32,
        to: u32,
        role: FlowRole,
        port: Option<usize>,
    ) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let sop = editor.to_sop_json();
        let mut edit = serde_json::json!({
            "op": op,
            "from": from,
            "to": to,
            "role": role,
        });
        if let Some(port_index) = port {
            edit["port"] = serde_json::json!(port_index);
        }
        match self.rpc.sops_wire_draft(sop, edit).await {
            Ok(value) => {
                let Some(sop_value) = value.get("sop") else {
                    self.error = Some("wire: daemon returned no sop".into());
                    return;
                };
                match serde_json::from_value::<SopDraft>(sop_value.clone()) {
                    Ok(draft) => {
                        if let Some(editor) = self.editor.as_mut() {
                            let cursor =
                                editor.step_cursor.min(draft.steps.len().saturating_sub(1));
                            editor.draft = draft;
                            editor.step_cursor = cursor;
                        }
                        if let Some(graph_value) = value.get("graph")
                            && let Ok(view) =
                                serde_json::from_value::<SopGraphView>(graph_value.clone())
                        {
                            self.editor_graph = view;
                        }
                        self.error = None;
                    }
                    Err(parse_error) => {
                        self.error = Some(format!("wire: parse draft: {parse_error}"))
                    }
                }
            }
            Err(rpc_error) => self.error = Some(rpc_error.to_string()),
        }
    }

    async fn delete_selected(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        match self.rpc.sops_delete(&name).await {
            Ok(_) => {
                self.status = Some(format!("deleted {name}"));
                self.overlay = None;
                self.list_state.select(None);
                self.refresh().await;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    async fn handle_editor_key(&mut self, key: crossterm::event::KeyEvent) {
        use crate::keymap::SopEditorAction;
        use crossterm::event::{KeyCode, KeyModifiers};
        let in_triggers = self
            .editor
            .as_ref()
            .is_some_and(|ed| ed.focus == EditorFocus::Triggers);
        if in_triggers && let Some(action) = SopEditorAction::from_chord(&key) {
            match action {
                SopEditorAction::SourcePrev => self.editor_cycle_trigger_source(false),
                SopEditorAction::SourceNext => self.editor_cycle_trigger_source(true),
                SopEditorAction::ChannelNext => self.editor_cycle_trigger_channel(true),
                SopEditorAction::AliasNext => self.editor_cycle_trigger_alias(true),
                SopEditorAction::Add => self.editor_add_trigger(),
                SopEditorAction::Remove => self.editor_remove_trigger(),
            }
            if self.editor.is_some() {
                self.refresh_editor_graph().await;
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.editor = None, // keyguard: text-entry cancel
            KeyCode::Char('s') if ctrl => self.submit_editor().await, // keyguard: text-entry submit
            KeyCode::Char('n') if ctrl => self.editor_add_step(), // keyguard: text-entry add step
            KeyCode::Tab => self.editor_advance_focus(), // keyguard: text-entry focus advance
            KeyCode::Up => self.editor_step_up(), // keyguard: text-entry step cursor
            KeyCode::Down => self.editor_step_down(), // keyguard: text-entry step cursor
            KeyCode::Enter => self.editor_enter(), // keyguard: text-entry newline/advance
            KeyCode::Backspace if !in_triggers => self.editor_backspace(), // keyguard: text-entry edit
            KeyCode::Char(c) if !ctrl && !in_triggers => self.editor_push_char(c), // keyguard: text-entry char input
            _ => {}
        }
        if self.editor.is_some() {
            self.refresh_editor_graph().await;
        }
    }

    fn editor_advance_focus(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Name => {
                    ed.focus = EditorFocus::Triggers;
                    ed.trigger_cursor = ed
                        .trigger_cursor
                        .min(ed.draft.triggers.len().saturating_sub(1));
                }
                EditorFocus::Triggers => {
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
        if let Some(editor) = self.editor.as_mut() {
            editor.step_cursor = (editor.step_cursor + 1).min(editor.draft.steps.len());
            let number = editor.draft.steps.len() as u32 + 1;
            editor.draft.steps.insert(
                editor.step_cursor,
                SopStep {
                    number,
                    ..SopStep::default()
                },
            );
            editor.focus = EditorFocus::Steps;
            editor.field = StepField::Title;
        }
    }

    async fn add_step_from(&mut self, step: u32, role: FlowRole, port: Option<usize>) {
        let target = {
            let Some(editor) = self.editor.as_mut() else {
                return;
            };
            let number = editor.draft.steps.len() as u32 + 1;
            editor.draft.steps.push(SopStep {
                number,
                title: format!("Step {number}"),
                ..SopStep::default()
            });
            number
        };
        self.apply_wire_edit("connect", step, target, role, port)
            .await;
        if let Some(editor) = self.editor.as_mut() {
            editor.step_cursor = editor
                .draft
                .steps
                .iter()
                .position(|candidate| candidate.number == target)
                .unwrap_or(editor.step_cursor);
            editor.focus = EditorFocus::Steps;
            editor.field = StepField::Title;
        }
    }

    fn editor_step_up(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Steps if ed.step_cursor > 0 => ed.step_cursor -= 1,
                EditorFocus::Triggers if ed.trigger_cursor > 0 => ed.trigger_cursor -= 1,
                _ => {}
            }
        }
    }

    fn editor_step_down(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Steps if ed.step_cursor + 1 < ed.draft.steps.len() => {
                    ed.step_cursor += 1;
                }
                EditorFocus::Triggers if ed.trigger_cursor + 1 < ed.draft.triggers.len() => {
                    ed.trigger_cursor += 1;
                }
                _ => {}
            }
        }
    }

    fn editor_enter(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            match ed.focus {
                EditorFocus::Name => {
                    ed.focus = EditorFocus::Triggers;
                }
                EditorFocus::Triggers => self.editor_add_trigger(),
                EditorFocus::Steps => self.editor_add_step(),
            }
        }
    }

    fn editor_add_trigger(&mut self) {
        if let Some(ed) = self.editor.as_mut() {
            ed.draft
                .triggers
                .push(crate::client::SopTriggerDraft::default());
            ed.trigger_cursor = ed.draft.triggers.len() - 1;
        }
    }

    fn editor_remove_trigger(&mut self) {
        if let Some(ed) = self.editor.as_mut()
            && ed.draft.triggers.len() > 1
            && ed.trigger_cursor < ed.draft.triggers.len()
        {
            ed.draft.triggers.remove(ed.trigger_cursor);
            ed.trigger_cursor = ed.trigger_cursor.min(ed.draft.triggers.len() - 1);
        }
    }

    fn editor_cycle_trigger_source(&mut self, forward: bool) {
        let sources = self.trigger_source_walk();
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let Some(trigger) = ed.draft.triggers.get_mut(ed.trigger_cursor) else {
            return;
        };
        let cur = if trigger.channel.is_some() {
            "channel".to_string()
        } else {
            trigger.kind.clone()
        };
        let Some(picked) = cycle_pick(&sources, &cur, forward) else {
            return;
        };
        *trigger = crate::client::SopTriggerDraft::default();
        if picked == "channel" {
            trigger.kind = "channel".to_string();
            trigger.channel = self
                .trigger_registry
                .channels
                .first()
                .map(|c| c.channel.clone());
        } else {
            trigger.kind = picked;
        }
    }

    /// Ordered trigger source list rendered by the picker. Prefers the
    /// backend-walked `sources` so ordering and membership match the runtime
    /// `SopTriggerSource` enum exactly; falls back to reconstructing from
    /// `bound` + `channel` only when an old or failed registry response omits
    /// `sources`, so the picker still works against a stale daemon.
    fn trigger_source_walk(&self) -> Vec<String> {
        trigger_source_walk(&self.trigger_registry)
    }

    fn editor_cycle_trigger_channel(&mut self, forward: bool) {
        let kinds: Vec<String> = self
            .trigger_registry
            .channels
            .iter()
            .map(|c| c.channel.clone())
            .collect();
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let Some(trigger) = ed.draft.triggers.get_mut(ed.trigger_cursor) else {
            return;
        };
        if trigger.channel.is_none() {
            return;
        }
        let cur = trigger.channel.clone().unwrap_or_default();
        let Some(picked) = cycle_pick(&kinds, &cur, forward) else {
            return;
        };
        trigger.channel = Some(picked);
        trigger.alias = None;
    }

    fn editor_cycle_trigger_alias(&mut self, forward: bool) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        let Some(trigger) = ed.draft.triggers.get(ed.trigger_cursor) else {
            return;
        };
        let Some(kind_wire) = trigger.channel.clone() else {
            return;
        };
        let mut aliases: Vec<Option<String>> = vec![None];
        if let Some(entry) = self
            .trigger_registry
            .channels
            .iter()
            .find(|c| c.channel == kind_wire)
        {
            aliases.extend(entry.aliases.iter().map(|a| Some(a.alias.clone())));
        }
        let cur = trigger.alias.clone();
        let Some(picked) = cycle_pick(&aliases, &cur, forward) else {
            return;
        };
        if let Some(ed) = self.editor.as_mut()
            && let Some(trigger) = ed.draft.triggers.get_mut(ed.trigger_cursor)
        {
            trigger.alias = picked;
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
                if let Some(picked) = cycle_pick(&SopStepKind::ALL, &step.kind, true) {
                    step.kind = picked;
                }
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
            StepField::Switch => push_switch_char(&mut step.routing.switch, c),
            StepField::Calls => push_calls_char(step, c),
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
            StepField::Switch => pop_switch_char(&mut step.routing.switch),
            StepField::Calls => pop_calls_char(step),
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
        // Identity in edit mode is the name the edit started from. sops/save
        // persists under the draft's name and overwrites that directory, so a
        // rename would silently fork (new dir, old left behind) or clobber a
        // different SOP. Reject renames until an explicit rename flow exists.
        if let Some(original) = ed.original_name.as_deref()
            && name != original
        {
            self.error = Some(format!(
                "Renaming an SOP is not supported yet. Set the name back to \
                 '{original}', or create a new SOP and delete the old one."
            ));
            return;
        }
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

    /// Open the Manual-run payload prompt, but only when the selected SOP
    /// actually declares a manual trigger. Keys off the SOP definition, not a
    /// per-SOP check, matching the web affordance.
    async fn start_run_payload(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            return;
        };
        let is_manual = match self.rpc.sops_get(&name).await {
            Ok(value) => serde_json::from_value::<SopDraft>(value)
                .map(|d| d.triggers.iter().any(|t| t.kind == "manual"))
                .unwrap_or(false),
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        if is_manual {
            self.status = Some("manual run: type JSON payload, Enter to fire".into());
            self.run_payload_input = Some(String::new());
        } else {
            self.status = Some("selected SOP has no manual trigger".into());
        }
    }

    async fn submit_run_payload(&mut self) {
        let Some(name) = self.selected_name().map(String::from) else {
            self.run_payload_input = None;
            return;
        };
        let payload = self
            .run_payload_input
            .take()
            .map(|b| b.trim().to_string())
            .unwrap_or_default();
        if !payload.is_empty() && serde_json::from_str::<serde_json::Value>(&payload).is_err() {
            self.error = Some("payload is not valid JSON".into());
            return;
        }
        let arg = if payload.is_empty() {
            None
        } else {
            Some(payload.as_str())
        };
        match self.rpc.sops_run(&name, arg).await {
            Ok(run_id) => {
                self.status = Some(format!("started run {run_id}"));
                self.error = None;
                self.load_run_overlay(&run_id).await;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn run_input_backspace(&mut self) {
        if let Some(buf) = self.run_input.as_mut() {
            buf.pop();
        }
    }

    fn run_payload_backspace(&mut self) {
        if let Some(buf) = self.run_payload_input.as_mut() {
            buf.pop();
        }
    }

    pub(crate) fn help_context(&self) -> crate::widgets::HelpNode {
        use crate::keymap::SopTabAction as S;
        crate::widgets::HelpNode::entries(crate::help::entries_for([
            S::Up,
            S::Down,
            S::Enter,
            S::Run,
            S::Watch,
            S::New,
            S::Edit,
            S::Delete,
            S::Toggle,
            S::PanLeft,
            S::PanRight,
            S::PanUp,
            S::PanDown,
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
        self.list_row_rects = list_row_rects(cols[0], self.names.len());

        let editing = self.editor.is_some();
        let visual = self.layer == RenderLayer::Visual;
        let title = self.right_title();

        if visual && self.error.is_none() {
            self.render_canvas(f, cols[1], &title);
            return;
        }
        self.node_rects.clear();
        self.handle_rects.clear();
        self.wire_rects.clear();

        let body = if let Some(err) = &self.error {
            err.clone()
        } else if editing {
            self.editor_lines().join("\n")
        } else {
            self.body_lines().join("\n")
        };
        let para = Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        f.render_widget(para, cols[1]);
    }

    fn right_title(&self) -> String {
        if self.editor.is_some() {
            return "Editor  [Tab field | ↑↓ step | Ctrl+n add | Ctrl+s save | Esc cancel]"
                .to_string();
        }
        let layer = match self.layer {
            RenderLayer::Visual => "visual",
            RenderLayer::Fields => "fields",
        };
        let toggle = crate::keymap::SopTabAction::Toggle
            .default_chords()
            .first()
            .map(crate::keymap::Chord::display)
            .unwrap_or_default();
        match &self.overlay {
            Some(o) => format!(
                "Graph [{} {}/{}{}] ({layer}, {toggle} toggle)",
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
                Some(s) => format!("Graph ({s}) ({layer}, {toggle} toggle)"),
                None => format!("Graph ({layer}, {toggle} toggle)"),
            },
        }
    }

    fn render_canvas(&mut self, f: &mut Frame, area: Rect, title: &str) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title.to_string());
        let inner = block.inner(area);
        self.canvas_rect = inner;
        f.render_widget(block, area);
        self.node_rects.clear();
        self.handle_rects.clear();
        self.add_rects.clear();
        self.wire_rects.clear();

        let editor = self.editor.as_ref();
        let graph = if editor.is_some() {
            &self.editor_graph
        } else {
            &self.graph
        };

        let empty = match editor {
            Some(ed) => ed.draft.steps.is_empty(),
            None => graph.nodes.is_empty(),
        };
        if empty {
            let msg = if editor.is_some() {
                "(no steps; Ctrl+n to add, then click handles to wire)"
            } else {
                "(no nodes; press n to author, e to edit)"
            };
            f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: false }), inner);
            return;
        }

        let phase =
            (self.animation_origin.elapsed().as_millis() / 200) % ACTIVE_SPINNER.len() as u128;
        let active_frame = ACTIVE_SPINNER[phase as usize];
        let linking = self.link_from;
        let selected_number =
            editor.and_then(|ed| ed.draft.steps.get(ed.step_cursor).map(|s| s.number));
        let switch_by_step: std::collections::HashMap<u32, Vec<String>> = editor
            .map(|ed| {
                ed.draft
                    .steps
                    .iter()
                    .map(|s| {
                        (
                            s.number,
                            s.routing.switch.iter().map(|r| r.name.clone()).collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let slots = layout_slots(&graph.layout, inner, self.pan_x, self.pan_y);

        for wire in graph.wires.iter().filter(|w| w.class == PinClass::Flow) {
            let (Some(from), Some(to)) = (slots.get(&wire.from_step), slots.get(&wire.to_step))
            else {
                continue;
            };
            draw_wire_2d(f, inner, *from, *to, wire_color(wire));
            let (mx, my) = wire_midpoint(*from, *to);
            if editor.is_none() {
                if my > inner.y && in_rect(mx, my.saturating_sub(1), inner) {
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            wire_label(wire),
                            Style::default().fg(wire_color(wire)),
                        )),
                        Rect::new(mx, my.saturating_sub(1), inner.width.min(12), 1),
                    );
                }
                continue;
            }
            let Some(role) = wire.flow_role else { continue };
            if role == FlowRole::Trigger {
                continue;
            }
            let port = match role {
                FlowRole::Switch => wire.from_pin.as_ref().and_then(|label| {
                    switch_by_step
                        .get(&wire.from_step)
                        .and_then(|names| names.iter().position(|n| n == label))
                }),
                _ => None,
            };
            if in_rect(mx, my, inner) {
                let rect = Rect::new(mx, my, 1, 1);
                f.render_widget(
                    Paragraph::new(Span::styled("✕", Style::default().fg(Color::DarkGray))),
                    rect,
                );
                self.wire_rects
                    .push((wire.from_step, wire.to_step, role, port, rect));
            }
        }

        for node in &graph.nodes {
            let Some(rect) = slots.get(&node.step).copied() else {
                continue;
            };
            if !rects_overlap(rect, inner) {
                continue;
            }
            let clipped = clip_rect(rect, inner);
            let Some(ed) = editor else {
                let state = self
                    .overlay
                    .as_ref()
                    .and_then(|o| o.state_of(node.step as u64));
                render_node_card(f, clipped, node, state, active_frame);
                self.node_rects.push((node.step, clipped));
                continue;
            };

            if node.kind == NodeKind::Trigger {
                render_trigger_card(f, clipped, node);
                self.node_rects.push((node.step, clipped));
                continue;
            }

            let step = ed.draft.steps.iter().find(|s| s.number == node.step);
            render_editor_card(f, clipped, node, step, selected_number == Some(node.step));
            self.node_rects.push((node.step, clipped));

            let handle_x = rect.x.saturating_add(rect.width.saturating_sub(1));
            let handles: Vec<(FlowRole, Option<usize>, Color)> = match step {
                Some(s) if !s.routing.switch.is_empty() => s
                    .routing
                    .switch
                    .iter()
                    .enumerate()
                    .map(|(port, _)| (FlowRole::Switch, Some(port), Color::Magenta))
                    .collect(),
                _ => vec![
                    (FlowRole::Sequence, None, Color::Green),
                    (FlowRole::Failure, None, Color::Red),
                ],
            };
            let mut hy = rect.y.saturating_add(1);
            for (role, port, color) in handles {
                if in_rect(handle_x, hy, inner) {
                    let zone = Rect::new(handle_x, hy, 1, 1);
                    f.render_widget(
                        Paragraph::new(Span::styled("○", Style::default().fg(color))),
                        zone,
                    );
                    self.handle_rects.push((node.step, role, port, zone));
                    let add_x = handle_x.saturating_add(1);
                    if in_rect(add_x, hy, inner) {
                        let add_zone = Rect::new(add_x, hy, 1, 1);
                        f.render_widget(
                            Paragraph::new(Span::styled("+", Style::default().fg(Color::DarkGray))),
                            add_zone,
                        );
                        self.add_rects.push((node.step, role, port, add_zone));
                    }
                }
                hy = hy.saturating_add(1);
            }

            if let Some((from, role, _)) = linking
                && from == node.step
                && rect.y > inner.y
            {
                let hint = Rect::new(rect.x, rect.y.saturating_sub(1), inner.width, 1);
                f.render_widget(
                    Paragraph::new(Span::styled(
                        format!("wiring {role:?} → click target"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::ITALIC),
                    )),
                    hint,
                );
            }
        }
    }

    fn body_lines(&self) -> Vec<String> {
        let phase =
            (self.animation_origin.elapsed().as_millis() / 200) % ACTIVE_SPINNER.len() as u128;
        let active = ACTIVE_SPINNER[phase as usize];
        let mut lines: Vec<String> = self
            .graph
            .nodes
            .iter()
            .map(|node| {
                let outs: Vec<String> = self
                    .graph
                    .wires
                    .iter()
                    .filter(|w| w.class == PinClass::Flow && w.from_step == node.step)
                    .map(|w| w.to_step.to_string())
                    .collect();
                let line = if outs.is_empty() {
                    format!("{}. {}", node.step, node.title)
                } else {
                    format!("{}. {} -> {}", node.step, node.title, outs.join(", "))
                };
                let mut rendered = match self
                    .overlay
                    .as_ref()
                    .and_then(|o| o.state_of(node.step as u64))
                {
                    Some(state) => format!("{} {line}", state_marker(state, active)),
                    None if self.overlay.is_some() => format!("  {line}"),
                    None => line,
                };
                if let Some(overlay) = &self.overlay {
                    for call in overlay.calls_of(node.step as u64) {
                        rendered.push_str(&format!(
                            "\n     {} {} [{}] {}ms",
                            call.index,
                            call.tool,
                            if call.success { "ok" } else { "failed" },
                            call.duration_ms
                        ));
                    }
                }
                rendered
            })
            .collect();
        if !self.graph.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("diagnostics:".to_string());
            for d in &self.graph.diagnostics {
                lines.push(format!(
                    "  [{:?}] step {}: {}",
                    d.severity, d.step, d.message
                ));
            }
        }
        if let Some(buf) = &self.run_input {
            lines.push(String::new());
            lines.push(format!("run id: {buf}_"));
        }
        if let Some(buf) = &self.run_payload_input {
            lines.push(String::new());
            lines.push(format!("manual payload (JSON): {buf}_"));
        }
        lines
    }

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
        let triggers_focus = ed.focus == EditorFocus::Triggers;
        lines.push(format!(
            "{} triggers  [alt+←/→ source · alt+c channel · alt+a alias · alt+n add · alt+x remove]",
            if triggers_focus { ">" } else { " " }
        ));
        for (i, trigger) in ed.draft.triggers.iter().enumerate() {
            let on = triggers_focus && i == ed.trigger_cursor;
            let mark = if on { ">" } else { " " };
            let source = if trigger.channel.is_some() {
                "channel".to_string()
            } else {
                trigger.kind.clone()
            };
            let detail = match &trigger.channel {
                Some(kind_wire) => {
                    let alias = trigger.alias.clone().unwrap_or_else(|| "any".to_string());
                    let unconfigured = self
                        .trigger_registry
                        .channels
                        .iter()
                        .find(|c| &c.channel == kind_wire)
                        .is_some_and(|c| !c.configured);
                    let hint = if unconfigured {
                        "  (no alias configured; set up this channel first)"
                    } else {
                        ""
                    };
                    format!(" {kind_wire}/{alias}{hint}")
                }
                None => self.bound_trigger_detail(&source, trigger),
            };
            lines.push(format!("{mark}  {source}{detail}"));
        }
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
            let kind = step.kind.as_str();
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
            if on_step && ed.field == StepField::When && !self.trigger_registry.operators.is_empty()
            {
                let ops = self
                    .trigger_registry
                    .operators
                    .iter()
                    .map(|op| op.token.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                lines.push(format!("     ops: {ops}"));
            }
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
            let (m, cur) = marker(StepField::Switch);
            lines.push(format!(
                "{m} switch: {}{cur}",
                switch_to_text(&step.routing.switch)
            ));
            if on_step && ed.field == StepField::Switch {
                lines.push(
                    "      (name>when>goto; ... makes this an if-this-then-that node)".into(),
                );
            }
            let (m, cur) = marker(StepField::Calls);
            let calls_ok = step.calls_buf.is_none()
                || step
                    .calls_buf
                    .as_deref()
                    .is_some_and(|b| serde_json::from_str::<Vec<PlannedToolCall>>(b).is_ok());
            lines.push(format!(
                "{m} calls{}: {}{cur}",
                if calls_ok { "" } else { " (invalid JSON)" },
                calls_text(step)
            ));
            if on_step && ed.field == StepField::Calls {
                lines.push(
                    "      (JSON array of {tool, args, pinned?}; args strings may bind {{steps.N.path}} / {{calls.K.path}})"
                        .into(),
                );
            }
            lines.push(String::new());
        }
        lines
    }

    fn bound_trigger_detail(
        &self,
        source: &str,
        trigger: &crate::client::SopTriggerDraft,
    ) -> String {
        let Some(bound) = self
            .trigger_registry
            .bound
            .iter()
            .find(|b| b.source == source)
        else {
            return String::new();
        };
        let Ok(wire) = serde_json::to_value(trigger) else {
            return String::new();
        };
        let parts: Vec<String> = bound
            .fields
            .iter()
            .filter_map(|f| match wire.get(&f.name)? {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                serde_json::Value::Array(items) if !items.is_empty() => Some(
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => None,
            })
            .collect();
        if parts.is_empty() {
            String::new()
        } else {
            format!(" {}", parts.join(" "))
        }
    }
}

const ACTIVE_SPINNER: [&str; 4] = ["|>", "/>", "->", "\\>"];

fn state_marker(state: NodeRunState, active_frame: &str) -> String {
    match state {
        NodeRunState::Active => active_frame.to_string(),
        NodeRunState::Completed => "ok".to_string(),
        NodeRunState::Failed => "xx".to_string(),
        NodeRunState::Skipped => "--".to_string(),
        NodeRunState::Pending => "..".to_string(),
    }
}

fn in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn list_row_rects(area: Rect, count: usize) -> Vec<Rect> {
    let inner_x = area.x.saturating_add(1);
    let inner_y = area.y.saturating_add(1);
    let inner_w = area.width.saturating_sub(2);
    let inner_h = area.height.saturating_sub(2);
    (0..count)
        .map(|i| i as u16)
        .take_while(|i| *i < inner_h)
        .map(|i| Rect::new(inner_x, inner_y.saturating_add(i), inner_w, 1))
        .collect()
}

fn pin_color(pin: &GraphPin) -> Color {
    match pin.class {
        PinClass::Flow => Color::Green,
        PinClass::Data if pin.required => Color::Cyan,
        PinClass::Data => Color::DarkGray,
    }
}

fn pin_type_label(pin: &GraphPin) -> String {
    match pin.class {
        PinClass::Flow => "flow".to_string(),
        PinClass::Data => pin.data_type.clone().unwrap_or_else(|| "any".to_string()),
    }
}

fn wire_label(w: &GraphWire) -> String {
    match w.class {
        PinClass::Data => format!(
            "{} → {}",
            w.from_pin.as_deref().unwrap_or("?"),
            w.to_pin.as_deref().unwrap_or("?")
        ),
        PinClass::Flow => match w.flow_role {
            Some(FlowRole::Failure) => "failure".to_string(),
            Some(FlowRole::Dependency) => "dependency".to_string(),
            Some(FlowRole::Trigger) => "trigger".to_string(),
            Some(FlowRole::Switch) => match &w.from_pin {
                Some(port) => format!("switch:{port}"),
                None => "switch".to_string(),
            },
            _ => "sequence".to_string(),
        },
    }
}

fn wire_color(w: &GraphWire) -> Color {
    match w.class {
        PinClass::Data => Color::Cyan,
        PinClass::Flow => match w.flow_role {
            Some(FlowRole::Failure) => Color::Red,
            Some(FlowRole::Dependency) => Color::Yellow,
            Some(FlowRole::Switch) => Color::Magenta,
            Some(FlowRole::Trigger) => Color::LightBlue,
            _ => Color::Green,
        },
    }
}

fn node_border_color(state: Option<NodeRunState>) -> Color {
    match state {
        Some(NodeRunState::Active) => Color::Magenta,
        Some(NodeRunState::Completed) => Color::Green,
        Some(NodeRunState::Failed) => Color::Red,
        Some(NodeRunState::Skipped) => Color::Yellow,
        _ => Color::Gray,
    }
}

const CARD_W: u16 = 22;
const CARD_H: u16 = 5;
const COL_GAP: u16 = 6;
const ROW_GAP: u16 = 2;

/// Project a persisted web-canvas pixel coordinate onto a terminal slot cell.
/// `origin` and `web_pitch` come from the shared [`LayoutGeometry`] registry
/// carried on the graph, so zerocode never hardcodes the web's pixel geometry.
fn cell_from_web(coord: f64, origin: f64, web_pitch: f64, cell_pitch: u16) -> u16 {
    if web_pitch <= 0.0 {
        return 0;
    }
    let slots = ((coord - origin) / web_pitch).max(0.0);
    (slots * f64::from(cell_pitch))
        .round()
        .min(f64::from(u16::MAX)) as u16
}

fn layout_slots(
    layout: &GraphLayout,
    inner: Rect,
    pan_x: u16,
    pan_y: u16,
) -> std::collections::HashMap<u32, Rect> {
    let geometry = layout.geometry;
    let col_pitch = geometry.col_pitch();
    let row_pitch = geometry.row_pitch();
    let mut slots = std::collections::HashMap::new();
    for p in &layout.positions {
        let (col_cells, row_cells) = match (p.x, p.y) {
            (Some(px), Some(py)) => (
                cell_from_web(px, geometry.origin, col_pitch, CARD_W + COL_GAP),
                cell_from_web(py, geometry.origin, row_pitch, CARD_H + ROW_GAP),
            ),
            _ => (
                p.col as u16 * (CARD_W + COL_GAP),
                p.row as u16 * (CARD_H + ROW_GAP),
            ),
        };
        let x = inner.x.saturating_add(col_cells).saturating_sub(pan_x);
        let y = inner.y.saturating_add(row_cells).saturating_sub(pan_y);
        slots.insert(p.step, Rect::new(x, y, CARD_W, CARD_H));
    }
    slots
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn clip_rect(r: Rect, bounds: Rect) -> Rect {
    let x = r.x.max(bounds.x);
    let y = r.y.max(bounds.y);
    let right = (r.x + r.width).min(bounds.x + bounds.width);
    let bottom = (r.y + r.height).min(bounds.y + bounds.height);
    Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
}

fn wire_midpoint(from: Rect, to: Rect) -> (u16, u16) {
    let x1 = from.x + from.width.saturating_sub(1);
    let y1 = from.y + from.height / 2;
    let x2 = to.x;
    let y2 = to.y + to.height / 2;
    ((x1 + x2) / 2, (y1 + y2) / 2)
}

fn draw_wire_2d(f: &mut Frame, inner: Rect, from: Rect, to: Rect, color: Color) {
    let x1 = from.x.saturating_add(from.width.saturating_sub(1));
    let y1 = from.y.saturating_add(from.height / 2);
    let x2 = to.x;
    let y2 = to.y.saturating_add(to.height / 2);
    if x2 <= x1 {
        put_cell(f, inner, x1, y1, "─", color);
        return;
    }
    let midx = x1 + (x2 - x1) / 2;
    let style = Style::default().fg(color);
    for x in x1..=midx {
        put_cell(f, inner, x, y1, "─", color);
    }
    let (top, bot) = if y1 <= y2 { (y1, y2) } else { (y2, y1) };
    for y in top..=bot {
        put_cell(f, inner, midx, y, "│", color);
    }
    if y1 != y2 {
        let corner_top = if y1 < y2 { "╮" } else { "╯" };
        let corner_bot = if y1 < y2 { "╰" } else { "╭" };
        put_cell(f, inner, midx, y1, corner_top, color);
        put_cell(f, inner, midx, y2, corner_bot, color);
    }
    for x in midx..x2 {
        put_cell(f, inner, x, y2, "─", color);
    }
    let _ = style;
    put_cell(f, inner, x2.saturating_sub(1), y2, "▶", color);
}

fn put_cell(f: &mut Frame, inner: Rect, x: u16, y: u16, glyph: &str, color: Color) {
    if !in_rect(x, y, inner) {
        return;
    }
    f.render_widget(
        Paragraph::new(Span::styled(glyph.to_string(), Style::default().fg(color))),
        Rect::new(x, y, 1, 1),
    );
}

fn render_trigger_card(f: &mut Frame, area: Rect, node: &GraphNode) {
    let header = Line::from(vec![
        Span::styled("⚡ ", Style::default().fg(Color::LightBlue)),
        Span::styled(
            node.title.clone(),
            Style::default()
                .fg(Color::LightBlue)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let sub = Line::from(Span::styled(
        node.subtitle.clone().unwrap_or_default(),
        Style::default().fg(Color::DarkGray),
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::LightBlue));
    f.render_widget(
        Paragraph::new(vec![header, sub])
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_editor_card(
    f: &mut Frame,
    area: Rect,
    node: &GraphNode,
    step: Option<&SopStep>,
    selected: bool,
) {
    let title = if node.title.is_empty() {
        "(untitled)".to_string()
    } else {
        node.title.clone()
    };
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", node.step),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
    ]);
    let detail = match step {
        Some(s) if !s.calls.is_empty() => Line::from(Span::styled(
            format!("⚙ {} planned calls", s.calls.len()),
            Style::default().fg(Color::Cyan),
        )),
        Some(s) if !s.routing.switch.is_empty() => Line::from(Span::styled(
            format!("⋔ {} ports", s.routing.switch.len()),
            Style::default().fg(Color::Magenta),
        )),
        Some(s) if !s.suggested_tools.is_empty() => Line::from(Span::styled(
            s.suggested_tools.join(", "),
            Style::default().fg(Color::DarkGray),
        )),
        _ => Line::from(Span::styled(
            "no tools",
            Style::default().fg(Color::DarkGray),
        )),
    };
    let border = if selected { Color::Cyan } else { Color::Gray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    f.render_widget(
        Paragraph::new(vec![header, detail])
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_node_card(
    f: &mut Frame,
    area: Rect,
    node: &GraphNode,
    state: Option<NodeRunState>,
    active_frame: &str,
) {
    let badge = state.map(|s| format!(" [{}]", state_marker(s, active_frame)));
    let header = Line::from(vec![
        Span::styled(
            format!(" {} ", node.step),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(
            node.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            badge.unwrap_or_default(),
            Style::default().fg(node_border_color(state)),
        ),
    ]);

    let fmt_pins = |pins: &[GraphPin]| -> Vec<Span<'static>> {
        if pins.is_empty() {
            return vec![Span::styled("—", Style::default().fg(Color::DarkGray))];
        }
        let mut spans = Vec::new();
        for (i, p) in pins.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled("●", Style::default().fg(pin_color(p))));
            spans.push(Span::raw(format!(" {}:{}", p.name, pin_type_label(p))));
            if p.required && p.class == PinClass::Data {
                spans.push(Span::styled("*", Style::default().fg(Color::Red)));
            }
        }
        spans
    };

    let mut in_line = vec![Span::styled("in ", Style::default().fg(Color::DarkGray))];
    in_line.extend(fmt_pins(&node.inputs));
    let mut out_line = vec![Span::styled("out ", Style::default().fg(Color::DarkGray))];
    out_line.extend(fmt_pins(&node.outputs));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(node_border_color(state)));
    let para = Paragraph::new(vec![header, Line::from(in_line), Line::from(out_line)])
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

#[cfg(test)]
mod tests {
    use super::{CARD_H, CARD_W, COL_GAP, ROW_GAP, layout_slots, trigger_source_walk};
    use crate::client::NodePosition;
    use crate::client::{BoundTriggerSourceView, GraphLayout, TriggerSourceRegistryView};
    use ratatui::layout::Rect;

    #[test]
    fn honored_toml_coords_land_on_matching_slot() {
        // A node dragged to web grid slot (col 2, row 1) persists TOML
        // x = 24 + 2*340 = 704, y = 24 + 1*130 = 154. zerocode must place it
        // on the same slot: col_cells = 2*(CARD_W+COL_GAP), row_cells =
        // 1*(CARD_H+ROW_GAP), from an origin-cornered inner rect.
        let layout = GraphLayout {
            positions: vec![NodePosition {
                step: 1,
                col: 0,
                row: 0,
                x: Some(704.0),
                y: Some(154.0),
            }],
            columns: 1,
            rows: 1,
            ..GraphLayout::default()
        };
        let inner = Rect::new(0, 0, 200, 60);
        let slots = layout_slots(&layout, inner, 0, 0);
        let rect = slots.get(&1).copied().expect("step 1 placed");
        assert_eq!(rect.x, 2 * (CARD_W + COL_GAP));
        assert_eq!(rect.y, CARD_H + ROW_GAP);
    }

    #[test]
    fn absent_coords_fall_back_to_grid() {
        let layout = GraphLayout {
            positions: vec![NodePosition {
                step: 1,
                col: 3,
                row: 2,
                x: None,
                y: None,
            }],
            columns: 4,
            rows: 3,
            ..GraphLayout::default()
        };
        let inner = Rect::new(0, 0, 200, 60);
        let slots = layout_slots(&layout, inner, 0, 0);
        let rect = slots.get(&1).copied().expect("step 1 placed");
        assert_eq!(rect.x, 3 * (CARD_W + COL_GAP));
        assert_eq!(rect.y, 2 * (CARD_H + ROW_GAP));
    }

    #[test]
    fn below_origin_coords_clamp_to_first_slot() {
        let layout = GraphLayout {
            positions: vec![NodePosition {
                step: 1,
                col: 0,
                row: 0,
                x: Some(0.0),
                y: Some(-48.0),
            }],
            columns: 1,
            rows: 1,
            ..GraphLayout::default()
        };
        let inner = Rect::new(0, 0, 200, 60);
        let slots = layout_slots(&layout, inner, 0, 0);
        let rect = slots.get(&1).copied().expect("step 1 placed");
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
    }

    #[test]
    fn walk_uses_backend_sources_verbatim() {
        let registry = TriggerSourceRegistryView {
            sources: vec![
                "webhook".to_string(),
                "future_source".to_string(),
                "channel".to_string(),
            ],
            bound: vec![BoundTriggerSourceView {
                source: "webhook".to_string(),
                fields: vec![],
                condition: None,
            }],
            channels: vec![],
            operators: vec![],
        };
        assert_eq!(
            trigger_source_walk(&registry),
            vec!["webhook", "future_source", "channel"],
            "picker must render the backend walk verbatim, including a source \
             present only in `sources`, so zerocode cannot drift"
        );
    }

    #[test]
    fn walk_falls_back_only_when_sources_absent() {
        let registry = TriggerSourceRegistryView {
            sources: vec![],
            bound: vec![
                BoundTriggerSourceView {
                    source: "webhook".to_string(),
                    fields: vec![],
                    condition: None,
                },
                BoundTriggerSourceView {
                    source: "manual".to_string(),
                    fields: vec![],
                    condition: None,
                },
            ],
            channels: vec![],
            operators: vec![],
        };
        assert_eq!(
            trigger_source_walk(&registry),
            vec!["webhook", "manual", "channel"],
            "with no backend `sources` (old/failed response) the picker \
             reconstructs from bound + channel so it still works"
        );
    }
}
