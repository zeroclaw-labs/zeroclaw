//! Quickstart pane — modal-based checklist that produces one
//! `BuilderSubmission`, sent through `quickstart/apply` RPC.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap},
};
use std::sync::Arc;

use crate::client::{
    QuickstartApplyResult, QuickstartError, QuickstartFieldDescriptor, QuickstartFieldSection,
    QuickstartStateResult, QuickstartStep, QuickstartSurface, RpcClient,
};
use crate::widgets::{HelpEntry, HelpNode};
use crate::wire::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryBackendKind as MemoryKind,
    ModelProviderChoice, SelectorChoice,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selector {
    ModelProvider,
    RiskProfile,
    RuntimeProfile,
    Memory,
    Channels,
    AgentIdentity,
}

impl Selector {
    const ALL: [Selector; 6] = [
        Selector::ModelProvider,
        Selector::RiskProfile,
        Selector::RuntimeProfile,
        Selector::Memory,
        Selector::Channels,
        Selector::AgentIdentity,
    ];

    fn title(self) -> &'static str {
        match self {
            Selector::ModelProvider => "Model provider",
            Selector::RiskProfile => "Risk profile",
            Selector::RuntimeProfile => "Runtime profile",
            Selector::Memory => "Memory",
            Selector::Channels => "Channels (optional)",
            Selector::AgentIdentity => "Agent identity",
        }
    }

    fn step(self) -> QuickstartStep {
        match self {
            Selector::ModelProvider => QuickstartStep::ModelProvider,
            Selector::RiskProfile => QuickstartStep::RiskProfile,
            Selector::RuntimeProfile => QuickstartStep::RuntimeProfile,
            Selector::Memory => QuickstartStep::Memory,
            Selector::Channels => QuickstartStep::Channels,
            Selector::AgentIdentity => QuickstartStep::Agent,
        }
    }
}

fn opt(value: &str, label: &'static str, help: &'static str) -> PickerOption {
    PickerOption {
        value: value.to_string(),
        label: label.to_string(),
        help: help.to_string(),
        use_existing: false,
    }
}

fn existing_opt(alias: String) -> PickerOption {
    PickerOption {
        label: format!("Use existing: {alias}"),
        value: alias,
        help: "Reuse this alias instead of creating a new one.".to_string(),
        use_existing: true,
    }
}

fn in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn synth_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE)
}

fn action_row_line(label: &str, is_cursor: bool) -> Line<'static> {
    let glyph = if is_cursor { " › " } else { "   " };
    let style = if is_cursor {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    Line::from(vec![
        Span::styled(glyph, Style::default().fg(Color::Yellow)),
        Span::styled(label.to_string(), style),
    ])
}

fn risk_options() -> [PickerOption; 3] {
    [
        opt(
            "locked-down",
            "Locked Down",
            "Tight defaults. Workspace-only fs, approval on med/high risk.",
        ),
        opt(
            "balanced",
            "Balanced",
            "Day-to-day defaults. Approval on risky ops. Recommended.",
        ),
        opt(
            "yolo",
            "YOLO",
            "Full autonomy. No approval gates. Use on disposable machines only.",
        ),
    ]
}

fn runtime_options() -> [PickerOption; 3] {
    [
        opt("tight", "Tight", "Low ceilings on iterations and tokens."),
        opt("balanced", "Balanced", "Sensible ceilings. Recommended."),
        opt("unbounded", "Unbounded", "No artificial caps."),
    ]
}

fn memory_options() -> Vec<PickerOption> {
    // Walk every variant of the schema's canonical `MemoryBackendKind`.
    // `serde_json::to_value` returns the `#[serde(rename_all =
    // "snake_case")]` string for each variant — that string IS the
    // wire key written into `memory.backend`, so the picker carries
    // no parallel mapping. Variants come out in declaration order
    // because `enum-iterator`-style iteration is unnecessary for a
    // closed set: we list them once here against the schema and any
    // schema additions are caught at compile time because
    // `MemoryKind` is a public re-export and a `match` exhaustiveness
    // check below would fail to compile if a variant were dropped.
    let variants: [MemoryKind; 6] = [
        MemoryKind::Sqlite,
        MemoryKind::Markdown,
        MemoryKind::Postgres,
        MemoryKind::Qdrant,
        MemoryKind::Lucid,
        MemoryKind::None,
    ];
    // Compile-time exhaustiveness check: adding a new variant to
    // `MemoryBackendKind` triggers a non-exhaustive-match warning
    // here and forces the array above to grow alongside the schema.
    #[allow(clippy::no_effect_underscore_binding)]
    let _exhaustive = |k: MemoryKind| match k {
        MemoryKind::Sqlite
        | MemoryKind::Markdown
        | MemoryKind::Postgres
        | MemoryKind::Qdrant
        | MemoryKind::Lucid
        | MemoryKind::None => (),
    };
    variants
        .into_iter()
        .map(|kind| {
            let wire = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
            PickerOption {
                value: wire.clone(),
                label: wire,
                help: String::new(),
                use_existing: false,
            }
        })
        .collect()
}

fn provider_type_options(snapshot: Option<&QuickstartStateResult>) -> Vec<PickerOption> {
    // Source of truth is the daemon-side
    // `zeroclaw_runtime::quickstart::snapshot_state`, which maps the
    // canonical `zeroclaw_providers::list_model_providers()` registry
    // into wire rows. Adding a model provider in
    // `zeroclaw-providers` lights up here automatically — Quickstart
    // never maintains its own list.
    let Some(snap) = snapshot else {
        return Vec::new();
    };
    snap.model_provider_types
        .iter()
        .map(|t| PickerOption {
            value: t.kind.clone(),
            label: t.display_name.clone(),
            help: if t.local {
                "Local. No credential required.".to_string()
            } else {
                "Cloud. Provide an API key when prompted.".to_string()
            },
            use_existing: false,
        })
        .collect()
}

fn channel_type_options(snapshot: Option<&QuickstartStateResult>) -> Vec<PickerOption> {
    // Same shape as `provider_type_options`: rows come from the
    // schema-driven `ChannelsConfig` inventory the daemon walks at
    // request time. The TUI carries no channel list of its own.
    let Some(snap) = snapshot else {
        return Vec::new();
    };
    snap.channel_types
        .iter()
        .map(|t| PickerOption {
            value: t.kind.clone(),
            label: t.display_name.clone(),
            help: format!("Configure a new {} channel.", t.display_name),
            use_existing: false,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct ChannelDraft {
    channel_type: String,
    alias: String,
    token: Option<String>,
    mode: SelectorMode,
}

/// Per-selector choice mode. Maps to `SelectorChoice<T>` at submit
/// time: `Mode::Fresh` → `SelectorChoice::Fresh(...)`,
/// `Mode::Existing` → `SelectorChoice::Existing(alias)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SelectorMode {
    #[default]
    Fresh,
    Existing,
}

#[derive(Debug, Clone)]
struct FormState {
    provider_type: String,
    provider_alias: String,
    provider_mode: SelectorMode,
    model: String,
    api_key: Option<String>,
    risk: String,
    risk_mode: SelectorMode,
    runtime: String,
    runtime_mode: SelectorMode,
    memory: MemoryKind,
    memory_mode: SelectorMode,
    /// `true` once the user has explicitly committed a Memory
    /// choice in the modal. The form starts `false` so the
    /// selector shows `[ ]` instead of a pre-checked default
    /// the user never picked.
    memory_chosen: bool,
    /// When `memory_mode == Existing`, this carries the alias the user
    /// picked (e.g. `sqlite-laptop`). Ignored when `memory_mode` is
    /// `Fresh`.
    memory_existing_alias: String,
    channels: Vec<ChannelDraft>,
    /// `true` once the user has opened the Channels modal and
    /// hit Done (channels are optional, but the user has to say
    /// "I considered this and chose 0 / N" before the selector
    /// counts as `[✓]`).
    channels_visited: bool,
    peer_groups: Vec<crate::wire::QuickstartPeerGroup>,
    agent_name: String,
    personality_files: Vec<crate::wire::QuickstartPersonalityFile>,
}

impl FormState {
    fn default_form() -> Self {
        Self {
            provider_type: String::new(),
            provider_alias: String::new(),
            provider_mode: SelectorMode::Fresh,
            model: String::new(),
            api_key: None,
            risk: String::new(),
            risk_mode: SelectorMode::Fresh,
            runtime: String::new(),
            runtime_mode: SelectorMode::Fresh,
            memory: MemoryKind::Sqlite,
            memory_mode: SelectorMode::Fresh,
            memory_chosen: false,
            memory_existing_alias: String::new(),
            channels: Vec::new(),
            channels_visited: false,
            peer_groups: Vec::new(),
            agent_name: String::new(),
            personality_files: Vec::new(),
        }
    }

    fn is_satisfied(&self, sel: Selector) -> bool {
        match sel {
            Selector::ModelProvider => match self.provider_mode {
                SelectorMode::Fresh => {
                    !self.provider_type.is_empty()
                        && !self.provider_alias.is_empty()
                        && !self.model.is_empty()
                }
                SelectorMode::Existing => {
                    !self.provider_type.is_empty() && !self.provider_alias.is_empty()
                }
            },
            Selector::RiskProfile => !self.risk.is_empty(),
            Selector::RuntimeProfile => !self.runtime.is_empty(),
            Selector::Memory => self.memory_chosen,
            Selector::Channels => self.channels_visited,
            Selector::AgentIdentity => !self.agent_name.is_empty(),
        }
    }

    fn summary(&self, sel: Selector) -> String {
        match sel {
            Selector::ModelProvider => {
                if self.provider_type.is_empty() {
                    "not yet chosen".to_string()
                } else {
                    format!(
                        "{} ({}) — {}",
                        self.provider_type, self.provider_alias, self.model
                    )
                }
            }
            Selector::RiskProfile => self.risk.clone(),
            Selector::RuntimeProfile => self.runtime.clone(),
            Selector::Memory => serde_json::to_value(self.memory)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{:?}", self.memory).to_lowercase()),
            Selector::Channels => {
                if self.channels.is_empty() {
                    "0 (CLI only)".to_string()
                } else {
                    format!("{} configured", self.channels.len())
                }
            }
            Selector::AgentIdentity => {
                if self.agent_name.is_empty() {
                    "not yet named".to_string()
                } else {
                    self.agent_name.clone()
                }
            }
        }
    }

    fn to_submission(&self) -> BuilderSubmission {
        let model_provider = match self.provider_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: self.provider_type.clone(),
                alias: self.provider_alias.clone(),
                model: self.model.clone(),
                api_key: self.api_key.clone(),
                base_url: None,
            }),
            SelectorMode::Existing => {
                SelectorChoice::Existing(format!("{}.{}", self.provider_type, self.provider_alias))
            }
        };
        let risk_profile = match self.risk_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(self.risk.clone()),
            SelectorMode::Existing => SelectorChoice::Existing(self.risk.clone()),
        };
        let runtime_profile = match self.runtime_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(self.runtime.clone()),
            SelectorMode::Existing => SelectorChoice::Existing(self.runtime.clone()),
        };
        let memory = match self.memory_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(self.memory),
            SelectorMode::Existing => SelectorChoice::Existing(self.memory_existing_alias.clone()),
        };
        BuilderSubmission {
            model_provider,
            risk_profile,
            runtime_profile,
            memory,
            channels: self
                .channels
                .iter()
                .map(|c| match c.mode {
                    SelectorMode::Fresh => SelectorChoice::Fresh(ChannelQuickStart {
                        channel_type: c.channel_type.clone(),
                        alias: c.alias.clone(),
                        token: c.token.clone(),
                    }),
                    SelectorMode::Existing => {
                        SelectorChoice::Existing(format!("{}.{}", c.channel_type, c.alias))
                    }
                })
                .collect(),
            peer_groups: self.peer_groups.clone(),
            agent: AgentIdentity {
                name: self.agent_name.clone(),
                system_prompt: String::new(),
                personality_file: None,
                personality_files: self.personality_files.clone(),
            },
        }
    }
}

/// Modal kinds the pane can put up over the main checklist. Each
/// kind holds its own state: which selector triggered it, the
/// current cursor / draft buffers, etc. The modal owns input until
/// dismissed.
enum Modal {
    /// Single-select picker. Used by Risk, Runtime, Memory, and the
    /// provider-type / channel-type pre-step.
    Picker(PickerModal),
    /// Single-field text input. Used by Agent identity and alias
    /// prompts that take one short string.
    TextInput(TextInputModal),
    /// Multi-field form sourced from `quickstart/fields`. Used by
    /// Model provider and Channels once the user has chosen a type.
    FieldForm(FieldFormModal),
    /// Channels list manager — shows current drafts and offers
    /// Add / Done. Add opens a Picker (channel type) → FieldForm.
    ChannelList(ChannelListModal),
}

struct PickerModal {
    selector: Selector,
    purpose: PickerPurpose,
    options: Vec<PickerOption>,
    cursor: usize,
}

/// What does the picker collect? Drives what happens on Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerPurpose {
    /// Direct write into [`FormState`] via [`apply_picker_choice`].
    DirectChoice,
    /// Step 1 of the provider flow: chose a provider type. The next
    /// step opens a [`FieldFormModal`] with shape from the daemon.
    ProviderType,
    /// Step 1 of the channels flow: chose a channel type. The next
    /// step opens a [`FieldFormModal`] with shape from the daemon.
    ChannelType,
}

struct TextInputModal {
    selector: Selector,
    label: &'static str,
    help: &'static str,
    buf: String,
    is_secret: bool,
}

struct FieldFormModal {
    selector: Selector,
    /// Provider / channel type chosen in the preceding picker step.
    type_key: String,
    /// User-named alias for this entry. Pre-filled with `type_key`.
    alias: String,
    fields: Vec<FieldFormRow>,
    cursor: usize,
}

struct FieldFormRow {
    descriptor: QuickstartFieldDescriptor,
    /// User-typed buffer. Pre-filled from `descriptor.default`.
    buf: String,
}

struct ChannelListModal {
    /// `cursor < channels.len()`  → highlight that draft (Enter = delete).
    /// `cursor == channels.len()` → "+ Add channel" row.
    /// `cursor == channels.len()+1` → "Done" row.
    cursor: usize,
}

#[derive(Clone)]
struct PickerOption {
    /// Wire-side value written back into [`FormState`].
    value: String,
    /// Display label.
    label: String,
    /// One-line help / blurb.
    help: String,
    /// `true` when this option points at an already-configured alias
    /// (`SelectorChoice::Existing`). `false` for fresh presets / type
    /// rows that build a `SelectorChoice::Fresh`.
    use_existing: bool,
}

pub struct QuickstartPane {
    rpc: Arc<RpcClient>,
    /// Shared state that survives the daemon-reload reconnect. Used
    /// by Stage 2 to hand the new agent's alias to the next
    /// `app::run` iteration so the user lands directly in Chat.
    reconnect_state: crate::app::SharedReconnectState,
    form: FormState,
    list_state: ListState,
    run_id: String,
    last_step: Option<QuickstartStep>,
    state_snapshot: Option<QuickstartStateResult>,
    last_errors: Vec<QuickstartError>,
    applied_alias: Option<String>,
    busy: bool,
    active_modal: Option<Modal>,
    /// Rect of the modal body painted by the most recent `draw` call.
    /// `None` when no modal is up. Used by `handle_mouse` to detect
    /// clicks inside vs. outside the modal.
    modal_rect: Option<Rect>,
    /// Per-row hit-rects inside the modal body, in cursor order. Empty
    /// for text-input modals (no row cursor) and channel-list modals
    /// (cursor maps to entries the mouse handler computes lazily).
    modal_row_rects: Vec<Rect>,
    /// Hit-rect of the main selector list, populated each draw so
    /// clicks on selector rows route through `move_selection` /
    /// `open_modal_for`.
    selector_list_rect: Option<Rect>,
    selector_row_rects: Vec<Rect>,
}

impl QuickstartPane {
    pub fn new(rpc: Arc<RpcClient>, reconnect_state: crate::app::SharedReconnectState) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            rpc,
            reconnect_state,
            form: FormState::default_form(),
            list_state,
            run_id: generate_run_id(),
            last_step: None,
            state_snapshot: None,
            last_errors: Vec::new(),
            applied_alias: None,
            busy: false,
            active_modal: None,
            modal_rect: None,
            modal_row_rects: Vec::new(),
            selector_list_rect: None,
            selector_row_rects: Vec::new(),
        }
    }

    pub async fn init(&mut self) -> anyhow::Result<()> {
        if let Ok(s) = self.rpc.quickstart_state().await {
            self.state_snapshot = Some(s);
        }
        Ok(())
    }

    pub fn help_context(&self) -> HelpNode {
        HelpNode::entries(vec![
            HelpEntry::new(vec!["↑/↓"], "Move between selectors"),
            HelpEntry::new(vec!["Enter"], "Open the highlighted selector"),
            HelpEntry::key("c", "Create the agent (when all selectors ✓)"),
            HelpEntry::key("Esc", "Leave (no config written)"),
        ])
    }

    pub fn wants_text_input(&self) -> bool {
        false
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        self.draw_title(frame, chunks[0]);
        self.draw_selector_list(frame, chunks[1]);
        self.draw_status_strip(frame, chunks[2]);

        if let Some(modal) = &self.active_modal {
            let (rect, rows) = draw_modal(frame, area, modal, &self.form.channels);
            self.modal_rect = Some(rect);
            self.modal_row_rects = rows;
        } else {
            self.modal_rect = None;
            self.modal_row_rects.clear();
        }
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.active_modal.is_some() {
            self.handle_modal_key(key).await;
            return false;
        }
        // After Apply, `applied_alias` is set and the daemon is in the
        // middle of reloading. Suppress all main-list key handling
        // until the connection drops and the next `app::run`
        // iteration consumes the armed Stage-2 intent. Pressing Enter
        // here does nothing — there's no reachable RPC to act on.
        if self.applied_alias.is_some() {
            return false;
        }
        match key.code {
            KeyCode::Down => {
                self.move_selection(1);
                false
            }
            KeyCode::Up => {
                self.move_selection(-1);
                false
            }
            KeyCode::Enter => {
                if let Some(idx) = self.list_state.selected()
                    && let Some(sel) = Selector::ALL.get(idx).copied()
                {
                    self.last_step = Some(sel.step());
                    self.open_modal_for(sel);
                }
                false
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if self.can_create() {
                    self.submit().await;
                }
                false
            }
            _ => false,
        }
    }

    pub async fn dismiss_beacon(&self) {
        if self.applied_alias.is_some() {
            return;
        }
        let _ = self
            .rpc
            .quickstart_dismiss(&self.run_id, QuickstartSurface::Tui, self.last_step)
            .await;
    }

    /// Mouse handler. Recognises:
    ///   - left-click on a modal row → moves modal cursor + synthesises
    ///     Enter (committing that row);
    ///   - left-click outside an active modal → closes the modal;
    ///   - left-click on a selector row → moves the selector cursor +
    ///     opens that selector's modal;
    ///   - scroll up/down → moves the cursor on whichever surface is
    ///     active (modal if open, otherwise selector list).
    pub async fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent, _content: Rect) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let col = mouse.column;
        let row = mouse.row;

        if self.active_modal.is_some() {
            let modal_rect = self.modal_rect;
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click on a tracked row → set cursor + activate.
                    if let Some((idx, _r)) = self
                        .modal_row_rects
                        .iter()
                        .enumerate()
                        .find(|(_, r)| in_rect(col, row, **r))
                    {
                        self.set_modal_cursor(idx);
                        // Synthesise the same Enter behaviour the
                        // keyboard takes.
                        self.handle_modal_key(synth_enter()).await;
                        return;
                    }
                    // Click anywhere outside the modal body → close.
                    if let Some(mr) = modal_rect
                        && !in_rect(col, row, mr)
                    {
                        self.active_modal = None;
                        self.modal_rect = None;
                        self.modal_row_rects.clear();
                    }
                }
                MouseEventKind::ScrollUp => self.nudge_modal_cursor(-1),
                MouseEventKind::ScrollDown => self.nudge_modal_cursor(1),
                _ => {}
            }
            return;
        }

        // No modal: selector list + status strip clicks.
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((idx, _r)) = self
                    .selector_row_rects
                    .iter()
                    .enumerate()
                    .find(|(_, r)| in_rect(col, row, **r))
                {
                    self.list_state.select(Some(idx));
                    if let Some(sel) = Selector::ALL.get(idx).copied() {
                        self.last_step = Some(sel.step());
                        self.open_modal_for(sel);
                    }
                }
            }
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::ScrollDown => self.move_selection(1),
            _ => {}
        }
    }

    /// Move the cursor of the currently active modal by `delta`. No-op
    /// for modals that don't have a row cursor (TextInput).
    fn nudge_modal_cursor(&mut self, delta: i32) {
        let Some(modal) = self.active_modal.as_mut() else {
            return;
        };
        let (cur, len) = match modal {
            Modal::Picker(p) => (&mut p.cursor, p.options.len()),
            Modal::FieldForm(f) => (&mut f.cursor, f.fields.len()),
            Modal::ChannelList(cl) => (&mut cl.cursor, self.modal_row_rects.len()),
            Modal::TextInput(_) => return,
        };
        if len == 0 {
            return;
        }
        let next = (*cur as i32 + delta).rem_euclid(len as i32);
        *cur = next as usize;
    }

    /// Directly set the cursor of the currently active modal. No-op
    /// for TextInput. Out-of-range indices are clamped.
    fn set_modal_cursor(&mut self, idx: usize) {
        let Some(modal) = self.active_modal.as_mut() else {
            return;
        };
        match modal {
            Modal::Picker(p) => {
                if idx < p.options.len() {
                    p.cursor = idx;
                }
            }
            Modal::FieldForm(f) => {
                if idx < f.fields.len() {
                    f.cursor = idx;
                }
            }
            Modal::ChannelList(cl) => {
                cl.cursor = idx;
            }
            Modal::TextInput(_) => {}
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = Selector::ALL.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn open_modal_for(&mut self, sel: Selector) {
        match sel {
            Selector::RiskProfile | Selector::RuntimeProfile | Selector::Memory => {
                self.open_picker_modal(sel)
            }
            Selector::AgentIdentity => {
                self.active_modal = Some(Modal::TextInput(TextInputModal {
                    selector: sel,
                    label: "Agent name",
                    help: "Short identifier (e.g. `helper`, `coder`). Used in `zeroclaw agent <name>`.",
                    buf: self.form.agent_name.clone(),
                    is_secret: false,
                }));
            }
            Selector::ModelProvider => {
                let mut options: Vec<PickerOption> =
                    provider_type_options(self.state_snapshot.as_ref());
                if let Some(snap) = &self.state_snapshot {
                    for alias in &snap.model_providers {
                        options.push(existing_opt(alias.clone()));
                    }
                }
                self.active_modal = Some(Modal::Picker(PickerModal {
                    selector: sel,
                    purpose: PickerPurpose::ProviderType,
                    options,
                    cursor: 0,
                }));
            }
            Selector::Channels => {
                self.active_modal = Some(Modal::ChannelList(ChannelListModal { cursor: 0 }));
            }
        }
    }

    fn open_picker_modal(&mut self, sel: Selector) {
        let mut options: Vec<PickerOption> = match sel {
            Selector::RiskProfile => risk_options().to_vec(),
            Selector::RuntimeProfile => runtime_options().to_vec(),
            Selector::Memory => memory_options(),
            _ => return,
        };
        // Append "Use existing" rows for any aliases the daemon
        // reported under this selector's section. Preset rows always
        // come first; existing rows sit underneath so users who just
        // want the recommended default never have to scroll.
        if let Some(snap) = &self.state_snapshot {
            let existing: &[String] = match sel {
                Selector::RiskProfile => &snap.risk_profiles,
                Selector::RuntimeProfile => &snap.runtime_profiles,
                Selector::Memory => &snap.storage,
                _ => &[],
            };
            for alias in existing {
                // Skip aliases that match a preset row — re-applying
                // the same preset is overwrite-by-design, so listing
                // it twice adds noise.
                if options.iter().any(|o| o.value == *alias) {
                    continue;
                }
                options.push(existing_opt(alias.clone()));
            }
        }
        let cursor = match sel {
            Selector::RiskProfile => options
                .iter()
                .position(|o| o.value == self.form.risk)
                .unwrap_or(0),
            Selector::RuntimeProfile => options
                .iter()
                .position(|o| o.value == self.form.runtime)
                .unwrap_or(0),
            Selector::Memory => {
                let v = serde_json::to_value(self.form.memory)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                options.iter().position(|o| o.value == v).unwrap_or(0)
            }
            _ => 0,
        };
        self.active_modal = Some(Modal::Picker(PickerModal {
            selector: sel,
            purpose: PickerPurpose::DirectChoice,
            options,
            cursor,
        }));
    }

    async fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.active_modal.as_mut() else {
            return;
        };
        match modal {
            Modal::Picker(p) => match key.code {
                KeyCode::Esc => self.active_modal = None,
                KeyCode::Up if p.cursor > 0 => {
                    p.cursor -= 1;
                }
                KeyCode::Down if p.cursor + 1 < p.options.len() => {
                    p.cursor += 1;
                }
                KeyCode::Enter => {
                    let chosen = p.options[p.cursor].value.clone();
                    let use_existing = p.options[p.cursor].use_existing;
                    let selector = p.selector;
                    let purpose = p.purpose;
                    match (purpose, use_existing) {
                        (PickerPurpose::DirectChoice, _) => {
                            self.apply_picker_choice(selector, chosen, use_existing);
                            self.active_modal = None;
                            self.revalidate().await;
                        }
                        (PickerPurpose::ProviderType, true) => {
                            // chosen is `<type>.<alias>`. Adopt the
                            // alias ref; skip the field form.
                            self.adopt_existing_provider(chosen);
                            self.active_modal = None;
                            self.revalidate().await;
                        }
                        (PickerPurpose::ProviderType, false) => {
                            self.active_modal = None;
                            self.open_field_form(
                                selector,
                                QuickstartFieldSection::ModelProvider,
                                chosen,
                            )
                            .await;
                        }
                        (PickerPurpose::ChannelType, true) => {
                            self.adopt_existing_channel(chosen);
                            self.active_modal = None;
                            self.revalidate().await;
                        }
                        (PickerPurpose::ChannelType, false) => {
                            self.active_modal = None;
                            self.open_field_form(selector, QuickstartFieldSection::Channel, chosen)
                                .await;
                        }
                    }
                }
                _ => {}
            },
            Modal::TextInput(t) => match key.code {
                KeyCode::Esc => self.active_modal = None,
                KeyCode::Enter => {
                    let value = t.buf.trim().to_string();
                    let selector = t.selector;
                    if !value.is_empty() {
                        self.apply_text_choice(selector, value);
                        self.active_modal = None;
                        self.revalidate().await;
                    }
                }
                KeyCode::Backspace => {
                    t.buf.pop();
                }
                KeyCode::Char(c) => {
                    t.buf.push(c);
                }
                _ => {}
            },
            Modal::FieldForm(f) => match key.code {
                KeyCode::Esc => self.active_modal = None,
                KeyCode::Tab | KeyCode::Down => {
                    if f.cursor + 1 < f.fields.len() {
                        f.cursor += 1;
                    } else {
                        f.cursor = 0;
                    }
                }
                KeyCode::BackTab | KeyCode::Up => {
                    if f.cursor == 0 {
                        f.cursor = f.fields.len().saturating_sub(1);
                    } else {
                        f.cursor -= 1;
                    }
                }
                KeyCode::Enter => {
                    // Take f out so we can re-borrow `self` for the
                    // commit. The active_modal stays as FieldForm
                    // until the commit succeeds; on failure we
                    // restore it.
                    if !self.commit_field_form() {
                        return;
                    }
                    let from_channel = matches!(
                        self.active_modal.as_ref(),
                        Some(Modal::FieldForm(f)) if f.selector == Selector::Channels
                    );
                    if from_channel {
                        self.active_modal =
                            Some(Modal::ChannelList(ChannelListModal { cursor: 0 }));
                    } else {
                        self.active_modal = None;
                    }
                    self.revalidate().await;
                }
                KeyCode::Left => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && let Some(variants) = row.descriptor.enum_variants.as_deref()
                        && !variants.is_empty()
                    {
                        let cur = variants.iter().position(|v| v == &row.buf).unwrap_or(0);
                        let next = if cur == 0 {
                            variants.len() - 1
                        } else {
                            cur - 1
                        };
                        row.buf = variants[next].clone();
                    }
                }
                KeyCode::Right => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && let Some(variants) = row.descriptor.enum_variants.as_deref()
                        && !variants.is_empty()
                    {
                        let cur = variants.iter().position(|v| v == &row.buf).unwrap_or(0);
                        let next = (cur + 1) % variants.len();
                        row.buf = variants[next].clone();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && row.descriptor.enum_variants.is_none()
                    {
                        row.buf.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && row.descriptor.enum_variants.is_none()
                    {
                        row.buf.push(c);
                    }
                }
                _ => {}
            },
            Modal::ChannelList(cl) => {
                let drafts = self.form.channels.len();
                let row_count = drafts + 2; // drafts + Add + Done
                match key.code {
                    KeyCode::Esc => self.active_modal = None,
                    KeyCode::Up if cl.cursor > 0 => {
                        cl.cursor -= 1;
                    }
                    KeyCode::Down if cl.cursor + 1 < row_count => {
                        cl.cursor += 1;
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') if cl.cursor < drafts => {
                        self.form.channels.remove(cl.cursor);
                        if cl.cursor >= self.form.channels.len() {
                            cl.cursor = self.form.channels.len();
                        }
                    }
                    KeyCode::Enter => {
                        if cl.cursor == drafts {
                            // "+ Add channel" row → open channel-type picker.
                            let mut options: Vec<PickerOption> =
                                channel_type_options(self.state_snapshot.as_ref());
                            if let Some(snap) = &self.state_snapshot {
                                for alias in &snap.unassigned_channels {
                                    options.push(existing_opt(alias.clone()));
                                }
                            }
                            self.active_modal = Some(Modal::Picker(PickerModal {
                                selector: Selector::Channels,
                                purpose: PickerPurpose::ChannelType,
                                options,
                                cursor: 0,
                            }));
                        } else if cl.cursor == drafts + 1 {
                            // "Done" row → close. Mark the
                            // Channels selector as visited so the
                            // checklist shows `[✓]` regardless of
                            // whether the user added any drafts
                            // (channels are optional, but the user
                            // has to acknowledge that explicitly).
                            self.form.channels_visited = true;
                            self.active_modal = None;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn apply_text_choice(&mut self, sel: Selector, value: String) {
        // Currently only the agent identity selector lands here.
        // The match form keeps the room for future text selectors
        // (provider alias, channel alias) without a churn-y rewrite.
        if sel == Selector::AgentIdentity {
            self.form.agent_name = value;
        }
    }

    fn adopt_existing_provider(&mut self, dotted_ref: String) {
        if let Some((ty, alias)) = dotted_ref.split_once('.') {
            self.form.provider_type = ty.to_string();
            self.form.provider_alias = alias.to_string();
            self.form.provider_mode = SelectorMode::Existing;
            // Default model / api-key aren't carried in the "existing"
            // path — the runtime resolves the alias against the live
            // config at apply time. Leave them empty so they don't
            // overwrite the existing alias's values.
            self.form.model.clear();
            self.form.api_key = None;
        }
    }

    fn adopt_existing_channel(&mut self, dotted_ref: String) {
        if let Some((ty, alias)) = dotted_ref.split_once('.') {
            self.form.channels.push(ChannelDraft {
                channel_type: ty.to_string(),
                alias: alias.to_string(),
                token: None,
                mode: SelectorMode::Existing,
            });
        }
    }

    /// Debounced-ish validation: after a selector commit, ask the
    /// runtime whether the assembled submission would pass. Errors
    /// land in `last_errors` and surface in the status strip. The
    /// `quickstart/validate` path is read-only and cheap; we run it
    /// once per commit rather than per keystroke.
    async fn revalidate(&mut self) {
        let submission = self.form.to_submission();
        match self.rpc.quickstart_validate(&submission).await {
            Ok(crate::client::QuickstartValidateResult::Ok) => {
                self.last_errors.clear();
            }
            Ok(crate::client::QuickstartValidateResult::Errors { errors }) => {
                self.last_errors = errors;
            }
            Err(_) => {
                // Validation failures on the wire are non-fatal —
                // the user can still Create and let the apply path
                // surface real errors. Leave `last_errors` alone.
            }
        }
    }

    async fn open_field_form(
        &mut self,
        sel: Selector,
        section: QuickstartFieldSection,
        type_key: String,
    ) {
        let fields = match self.rpc.quickstart_fields(section, &type_key).await {
            Ok(res) => res.fields,
            Err(err) => {
                self.last_errors = vec![QuickstartError {
                    step: sel.step(),
                    field: String::new(),
                    message: format!("Failed to fetch field shape: {err}"),
                }];
                return;
            }
        };
        // For the model-provider section, upgrade the `model` row with
        // live catalog options so it renders as a picker. Empty catalog
        // → free-text fallback (descriptor unchanged).
        let model_catalog: Option<Vec<String>> =
            if matches!(section, QuickstartFieldSection::ModelProvider) {
                match self.rpc.catalog_models(&type_key).await {
                    Ok(res) if res.live && !res.models.is_empty() => Some(res.models),
                    _ => None,
                }
            } else {
                None
            };
        let rows: Vec<FieldFormRow> = fields
            .into_iter()
            .map(|mut d| {
                if let Some(ref models) = model_catalog
                    && d.key.eq_ignore_ascii_case("model")
                {
                    d.kind = crate::client::QuickstartFieldKind::Enum;
                    d.enum_variants = Some(models.clone());
                }
                // For enum fields, default the buffer to the first
                // variant so the user lands on a valid value. ←/→
                // cycles through the list.
                let buf = if let Some(variants) = d.enum_variants.as_deref()
                    && !variants.is_empty()
                {
                    d.default
                        .clone()
                        .filter(|v| variants.contains(v))
                        .unwrap_or_else(|| variants[0].clone())
                } else {
                    d.default.clone().unwrap_or_default()
                };
                FieldFormRow { descriptor: d, buf }
            })
            .collect();
        let alias = type_key.clone();
        self.active_modal = Some(Modal::FieldForm(FieldFormModal {
            selector: sel,
            type_key,
            alias,
            fields: rows,
            cursor: 0,
        }));
    }

    /// Commit the active FieldFormModal into [`FormState`]. Returns
    /// `true` when the form was valid and consumed; `false` keeps the
    /// modal open so the user can fix missing required fields.
    fn commit_field_form(&mut self) -> bool {
        let Some(Modal::FieldForm(f)) = self.active_modal.as_ref() else {
            return false;
        };
        let missing: Vec<&str> = f
            .fields
            .iter()
            .filter(|r| r.descriptor.required && r.buf.trim().is_empty())
            .map(|r| r.descriptor.key.as_str())
            .collect();
        if !missing.is_empty() {
            self.last_errors = missing
                .iter()
                .map(|k| QuickstartError {
                    step: f.selector.step(),
                    field: (*k).to_string(),
                    message: format!("Required field `{k}` is empty"),
                })
                .collect();
            return false;
        }
        match f.selector {
            Selector::ModelProvider => {
                let pick = |key: &str| {
                    f.fields
                        .iter()
                        .find(|r| r.descriptor.key == key)
                        .map(|r| r.buf.trim().to_string())
                        .unwrap_or_default()
                };
                let api_key = {
                    let v = pick("api-key");
                    if v.is_empty() { None } else { Some(v) }
                };
                self.form.provider_type = f.type_key.clone();
                self.form.provider_alias = f.alias.clone();
                self.form.provider_mode = SelectorMode::Fresh;
                self.form.model = pick("model");
                self.form.api_key = api_key;
            }
            Selector::Channels => {
                let pick = |key: &str| {
                    f.fields
                        .iter()
                        .find(|r| r.descriptor.key == key)
                        .map(|r| r.buf.trim().to_string())
                        .unwrap_or_default()
                };
                // `bot-token` covers Telegram / Discord; `token` is the
                // generic fallback for any channel kind that just needs
                // one secret.
                let token = {
                    let v = pick("bot-token");
                    if v.is_empty() {
                        let alt = pick("token");
                        if alt.is_empty() { None } else { Some(alt) }
                    } else {
                        Some(v)
                    }
                };
                self.form.channels.push(ChannelDraft {
                    channel_type: f.type_key.clone(),
                    alias: f.alias.clone(),
                    token,
                    mode: SelectorMode::Fresh,
                });
            }
            _ => {}
        }
        true
    }

    fn apply_picker_choice(&mut self, sel: Selector, value: String, use_existing: bool) {
        let mode = if use_existing {
            SelectorMode::Existing
        } else {
            SelectorMode::Fresh
        };
        match sel {
            Selector::RiskProfile => {
                self.form.risk = value;
                self.form.risk_mode = mode;
            }
            Selector::RuntimeProfile => {
                self.form.runtime = value;
                self.form.runtime_mode = mode;
            }
            Selector::Memory => {
                if use_existing {
                    // Existing memory alias — keep the displayed
                    // backend kind as-is (it's only used for the
                    // status-line summary) but record the alias the
                    // user picked so to_submission emits Existing.
                    self.form.memory_mode = SelectorMode::Existing;
                    self.form.memory_existing_alias = value;
                    self.form.memory_chosen = true;
                } else if let Ok(m) =
                    serde_json::from_value::<MemoryKind>(serde_json::Value::String(value.clone()))
                {
                    self.form.memory = m;
                    self.form.memory_mode = SelectorMode::Fresh;
                    self.form.memory_existing_alias.clear();
                    self.form.memory_chosen = true;
                }
            }
            _ => {}
        }
    }

    fn can_create(&self) -> bool {
        Selector::ALL.iter().all(|s| self.form.is_satisfied(*s)) && !self.busy
    }

    async fn submit(&mut self) {
        self.busy = true;
        self.last_errors.clear();
        let submission = self.form.to_submission();
        match self.rpc.quickstart_apply(&submission).await {
            Ok(QuickstartApplyResult::Applied { agent, .. }) => {
                // Arm the Stage-2 hand-off **before** the daemon reload
                // kicks in. The socket dies shortly after this returns,
                // the TUI freezes during the disconnect, and the next
                // `app::run` iteration reads this back to route the
                // user into the new agent's Chat tab automatically.
                if let Ok(mut guard) = self.reconnect_state.lock() {
                    guard.start_chat_with = Some(agent.alias.clone());
                }
                self.applied_alias = Some(agent.alias);
                self.last_errors.clear();
            }
            Ok(QuickstartApplyResult::Errors { errors }) => {
                self.last_errors = errors;
            }
            Err(err) => {
                self.last_errors = vec![QuickstartError {
                    step: QuickstartStep::Agent,
                    field: String::new(),
                    message: format!("RPC error: {err}"),
                }];
            }
        }
        self.busy = false;
    }

    fn draw_title(&self, frame: &mut Frame, area: Rect) {
        let title = Paragraph::new(Line::from(vec![
            Span::styled(
                "Quickstart",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  — create one working agent end-to-end."),
        ]));
        frame.render_widget(title, area);
    }

    fn draw_selector_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = Selector::ALL
            .iter()
            .map(|sel| {
                let satisfied = self.form.is_satisfied(*sel);
                let glyph_style = if satisfied {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let glyph = if satisfied { "[✓]" } else { "[ ]" };
                let title_style = Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
                let summary_style = Style::default().fg(Color::Gray);
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {glyph}  "), glyph_style),
                    Span::styled(format!("{:18}", sel.title()), title_style),
                    Span::styled("  ", summary_style),
                    Span::styled(self.form.summary(*sel), summary_style),
                ]))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1))
            .title(" Selectors ");
        let inner = block.inner(area);
        // Record per-row rects for mouse hit testing. Each ListItem is
        // one row; clipping at `inner.height` lines up with what the
        // List widget will actually paint.
        self.selector_list_rect = Some(inner);
        self.selector_row_rects = (0..Selector::ALL.len())
            .map(|i| {
                let y = inner.y.saturating_add(i as u16);
                Rect::new(inner.x, y, inner.width, 1)
            })
            .collect();
        let list = List::default()
            .items(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(40, 60, 90))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" › ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_status_strip(&self, frame: &mut Frame, area: Rect) {
        let can_create = self.can_create();
        let label = if self.busy {
            "Submitting…".to_string()
        } else if let Some(alias) = &self.applied_alias {
            format!("Created `{alias}`. Reloading daemon — Chat will open when reconnected…")
        } else if !self.last_errors.is_empty() {
            format!(
                "{} error(s) — fix selectors and resubmit",
                self.last_errors.len()
            )
        } else if can_create {
            "All selectors ✓. Press `c` to Create.".to_string()
        } else {
            "↑/↓ to move, Enter to open. `c` enables when every selector is ✓.".to_string()
        };
        let style = if self.applied_alias.is_some() {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if !self.last_errors.is_empty() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if can_create {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::horizontal(1));
        let p = Paragraph::new(label)
            .style(style)
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
    }
}

fn generate_run_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{now:x}-{pid:x}")
}

/// Paint the modal and return `(inner_rect, row_to_cursor)` so the
/// pane's mouse handler can resolve a click to a cursor index. The
/// `row_to_cursor` vec maps each body row (top → bottom) to either
/// `Some(cursor_index)` for clickable rows or `None` for help /
/// blank lines.
fn draw_modal(
    frame: &mut Frame,
    area: Rect,
    modal: &Modal,
    channels: &[ChannelDraft],
) -> (Rect, Vec<Rect>) {
    let (title, body_lines, footer, cursor_lines): (String, Vec<Line>, &str, Vec<usize>) =
        match modal {
            Modal::Picker(p) => {
                let mut cursor_lines = Vec::with_capacity(p.options.len());
                let lines: Vec<Line> = p
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, opt)| {
                        cursor_lines.push(i);
                        let is_cursor = i == p.cursor;
                        let glyph = if is_cursor { " › " } else { "   " };
                        let label_style = if is_cursor {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        Line::from(vec![
                            Span::styled(glyph, Style::default().fg(Color::Yellow)),
                            Span::styled(opt.label.as_str(), label_style),
                            Span::raw("  "),
                            Span::styled(opt.help.as_str(), Style::default().fg(Color::DarkGray)),
                        ])
                    })
                    .collect();
                (
                    format!(" {} ", p.selector.title()),
                    lines,
                    "↑/↓ move   Enter pick   Esc cancel",
                    cursor_lines,
                )
            }
            Modal::TextInput(t) => {
                let display = if t.is_secret {
                    "•".repeat(t.buf.chars().count())
                } else {
                    t.buf.clone()
                };
                let lines = vec![
                    Line::from(Span::styled(t.help, Style::default().fg(Color::DarkGray))),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!("{}: ", t.label),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(display, Style::default().fg(Color::White)),
                        Span::styled("█", Style::default().fg(Color::Yellow)),
                    ]),
                ];
                (
                    format!(" {} ", t.selector.title()),
                    lines,
                    "Enter accept   Esc cancel",
                    Vec::new(),
                )
            }
            Modal::FieldForm(f) => {
                let mut lines: Vec<Line> = Vec::new();
                let mut cursor_lines = Vec::with_capacity(f.fields.len());
                lines.push(Line::from(vec![
                    Span::styled("Type: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        f.type_key.as_str(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("    Alias: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(f.alias.as_str(), Style::default().fg(Color::White)),
                ]));
                lines.push(Line::from(""));
                for (i, row) in f.fields.iter().enumerate() {
                    cursor_lines.push(lines.len());
                    let is_cursor = i == f.cursor;
                    let glyph = if is_cursor { " › " } else { "   " };
                    let label_style = if is_cursor {
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    let display = if row.descriptor.is_secret {
                        "•".repeat(row.buf.chars().count())
                    } else {
                        row.buf.clone()
                    };
                    let display = if display.is_empty() {
                        row.descriptor
                            .default
                            .as_deref()
                            .map(|d| format!("(default: {d})"))
                            .unwrap_or_else(|| "<empty>".to_string())
                    } else {
                        display
                    };
                    let is_enum = row.descriptor.enum_variants.is_some();
                    lines.push(Line::from(vec![
                        Span::styled(glyph, Style::default().fg(Color::Yellow)),
                        Span::styled(format!("{:14}", row.descriptor.label), label_style),
                        Span::styled("  ", Style::default()),
                        Span::styled(
                            if is_enum { "‹ " } else { "" },
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::styled(display, Style::default().fg(Color::Gray)),
                        Span::styled(
                            if is_enum { " ›" } else { "" },
                            Style::default().fg(Color::Yellow),
                        ),
                        if is_cursor {
                            Span::styled("█", Style::default().fg(Color::Yellow))
                        } else {
                            Span::raw("")
                        },
                    ]));
                    if is_cursor && !row.descriptor.help.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("    {}", row.descriptor.help),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }
                (
                    format!(" {} ", f.selector.title()),
                    lines,
                    "Tab/↑/↓ move   ←/→ pick on ‹enum›   Enter accept   Esc cancel",
                    cursor_lines,
                )
            }
            Modal::ChannelList(cl) => {
                let mut lines: Vec<Line> = Vec::new();
                let mut cursor_lines: Vec<usize> = Vec::new();
                let drafts = channels.len();
                let row_count = drafts + 2;
                if drafts == 0 {
                    lines.push(Line::from(Span::styled(
                    "No channels configured. An agent without channels still works via `zeroclaw agent <name>` from the CLI.",
                    Style::default().fg(Color::DarkGray),
                )));
                    lines.push(Line::from(""));
                } else {
                    for (i, c) in channels.iter().enumerate() {
                        cursor_lines.push(lines.len());
                        let is_cursor = i == cl.cursor;
                        let glyph = if is_cursor { " › " } else { "   " };
                        let style = if is_cursor {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(glyph, Style::default().fg(Color::Yellow)),
                            Span::styled(format!("{}.{}", c.channel_type, c.alias), style),
                            Span::styled(
                                if c.token.is_some() {
                                    "  (token set)"
                                } else {
                                    ""
                                },
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                    lines.push(Line::from(""));
                }
                let add_idx = drafts;
                let done_idx = drafts + 1;
                cursor_lines.push(lines.len());
                lines.push(action_row_line("+ Add channel", cl.cursor == add_idx));
                cursor_lines.push(lines.len());
                lines.push(action_row_line("Done", cl.cursor == done_idx));
                let _ = row_count; // already encoded by the cursor styling above.
                (
                    " Channels ".to_string(),
                    lines,
                    "↑/↓ move   Enter activate   d delete   Esc close",
                    cursor_lines,
                )
            }
        };

    let box_w = area.width.saturating_sub(8).min(80);
    let box_h = (body_lines.len() as u16 + 4).min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    let body_rect = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    frame.render_widget(body, body_rect);

    let footer_rect = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(Color::DarkGray))),
        footer_rect,
    );

    // Translate cursor → body-line indices into screen-row hit-rects.
    // Body lines past `body_rect.height` got clipped, so anything off
    // the painted area gets a zero-sized rect (so a click can't hit
    // it accidentally).
    let row_rects: Vec<Rect> = cursor_lines
        .into_iter()
        .map(|line_idx| {
            let dy = line_idx as u16;
            if dy >= body_rect.height {
                Rect::new(0, 0, 0, 0)
            } else {
                Rect::new(body_rect.x, body_rect.y + dy, body_rect.width, 1)
            }
        })
        .collect();
    (rect, row_rects)
}
