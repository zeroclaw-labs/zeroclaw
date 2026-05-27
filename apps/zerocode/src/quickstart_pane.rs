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

use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice,
};

use crate::client::{
    QuickstartApplyResult, QuickstartError, QuickstartFieldDescriptor, QuickstartFieldSection,
    QuickstartStateResult, QuickstartStep, QuickstartSurface, RpcClient,
};
use crate::widgets::{HelpEntry, HelpNode};

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
        label,
        help,
    }
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

fn memory_options() -> [PickerOption; 2] {
    [
        opt(
            "sqlite",
            "SQLite",
            "On-disk persistent memory. Recommended.",
        ),
        opt(
            "none",
            "None",
            "No long-term recall — session history only.",
        ),
    ]
}

fn provider_type_options() -> [PickerOption; 5] {
    [
        opt(
            "anthropic",
            "Anthropic",
            "Claude models. Cloud. Needs an API key.",
        ),
        opt("openai", "OpenAI", "GPT models. Cloud. Needs an API key."),
        opt(
            "openrouter",
            "OpenRouter",
            "Multi-provider gateway. Cloud. Needs an API key.",
        ),
        opt(
            "ollama",
            "Ollama",
            "Local models on your machine. No API key needed.",
        ),
        opt(
            "openai_compatible",
            "OpenAI-compatible",
            "Any OpenAI-format endpoint. Base URL + key.",
        ),
    ]
}

fn channel_type_options() -> [PickerOption; 3] {
    [
        opt(
            "telegram",
            "Telegram",
            "Telegram bot. Needs a bot token from @BotFather.",
        ),
        opt(
            "discord",
            "Discord",
            "Discord bot. Needs a bot token from the Developer Portal.",
        ),
        opt("web", "Web", "Built-in web chat at the gateway URL."),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryKind {
    Sqlite,
    None,
}

impl MemoryKind {
    const WIRE_SQLITE: &'static str = "sqlite";
    const WIRE_NONE: &'static str = "none";

    fn from_wire(s: &str) -> Option<Self> {
        if s == Self::WIRE_SQLITE {
            Some(Self::Sqlite)
        } else if s == Self::WIRE_NONE {
            Some(Self::None)
        } else {
            None
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Self::Sqlite => Self::WIRE_SQLITE,
            Self::None => Self::WIRE_NONE,
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelDraft {
    channel_type: String,
    alias: String,
    token: Option<String>,
}

#[derive(Debug, Clone)]
struct FormState {
    provider_type: String,
    provider_alias: String,
    default_model: String,
    api_key: Option<String>,
    risk: String,
    runtime: String,
    memory: MemoryKind,
    channels: Vec<ChannelDraft>,
    agent_name: String,
}

impl FormState {
    fn default_form() -> Self {
        Self {
            provider_type: String::new(),
            provider_alias: String::new(),
            default_model: String::new(),
            api_key: None,
            risk: "balanced".to_string(),
            runtime: "balanced".to_string(),
            memory: MemoryKind::Sqlite,
            channels: Vec::new(),
            agent_name: String::new(),
        }
    }

    fn is_satisfied(&self, sel: Selector) -> bool {
        match sel {
            Selector::ModelProvider => {
                !self.provider_type.is_empty()
                    && !self.provider_alias.is_empty()
                    && !self.default_model.is_empty()
            }
            Selector::RiskProfile => !self.risk.is_empty(),
            Selector::RuntimeProfile => !self.runtime.is_empty(),
            Selector::Memory => true,
            Selector::Channels => true,
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
                        self.provider_type, self.provider_alias, self.default_model
                    )
                }
            }
            Selector::RiskProfile => self.risk.clone(),
            Selector::RuntimeProfile => self.runtime.clone(),
            Selector::Memory => match self.memory {
                MemoryKind::Sqlite => "sqlite (recommended)".to_string(),
                MemoryKind::None => "none".to_string(),
            },
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
        BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: self.provider_type.clone(),
                alias: self.provider_alias.clone(),
                default_model: self.default_model.clone(),
                api_key: self.api_key.clone(),
                base_url: None,
            }),
            risk_profile: SelectorChoice::Fresh(self.risk.clone()),
            runtime_profile: SelectorChoice::Fresh(self.runtime.clone()),
            memory: SelectorChoice::Fresh(match self.memory {
                MemoryKind::Sqlite => MemoryChoice::Sqlite,
                MemoryKind::None => MemoryChoice::None,
            }),
            channels: self
                .channels
                .iter()
                .map(|c| {
                    SelectorChoice::Fresh(ChannelQuickStart {
                        channel_type: c.channel_type.clone(),
                        alias: c.alias.clone(),
                        token: c.token.clone(),
                    })
                })
                .collect(),
            agent: AgentIdentity {
                name: self.agent_name.clone(),
                system_prompt: String::new(),
                personality_file: None,
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
    label: &'static str,
    /// One-line help / blurb.
    help: &'static str,
}

pub struct QuickstartPane {
    rpc: Arc<RpcClient>,
    form: FormState,
    list_state: ListState,
    run_id: String,
    last_step: Option<QuickstartStep>,
    state_snapshot: Option<QuickstartStateResult>,
    last_errors: Vec<QuickstartError>,
    applied_alias: Option<String>,
    busy: bool,
    active_modal: Option<Modal>,
}

impl QuickstartPane {
    pub fn new(rpc: Arc<RpcClient>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            rpc,
            form: FormState::default_form(),
            list_state,
            run_id: generate_run_id(),
            last_step: None,
            state_snapshot: None,
            last_errors: Vec::new(),
            applied_alias: None,
            busy: false,
            active_modal: None,
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
            draw_modal(frame, area, modal, &self.form.channels);
        }
    }

    pub async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.active_modal.is_some() {
            self.handle_modal_key(key).await;
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
                self.active_modal = Some(Modal::Picker(PickerModal {
                    selector: sel,
                    purpose: PickerPurpose::ProviderType,
                    options: provider_type_options().to_vec(),
                    cursor: 0,
                }));
            }
            Selector::Channels => {
                self.active_modal = Some(Modal::ChannelList(ChannelListModal { cursor: 0 }));
            }
        }
    }

    fn open_picker_modal(&mut self, sel: Selector) {
        let options: Vec<PickerOption> = match sel {
            Selector::RiskProfile => risk_options().to_vec(),
            Selector::RuntimeProfile => runtime_options().to_vec(),
            Selector::Memory => memory_options().to_vec(),
            _ => return,
        };
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
                let v = self.form.memory.as_wire();
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
                    let selector = p.selector;
                    let purpose = p.purpose;
                    match purpose {
                        PickerPurpose::DirectChoice => {
                            self.apply_picker_choice(selector, chosen);
                            self.active_modal = None;
                            self.last_errors.clear();
                        }
                        PickerPurpose::ProviderType => {
                            self.active_modal = None;
                            self.open_field_form(
                                selector,
                                QuickstartFieldSection::ModelProvider,
                                chosen,
                            )
                            .await;
                        }
                        PickerPurpose::ChannelType => {
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
                        self.last_errors.clear();
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
                    self.last_errors.clear();
                }
                KeyCode::Backspace => {
                    if let Some(row) = f.fields.get_mut(f.cursor) {
                        row.buf.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(row) = f.fields.get_mut(f.cursor) {
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
                            self.active_modal = Some(Modal::Picker(PickerModal {
                                selector: Selector::Channels,
                                purpose: PickerPurpose::ChannelType,
                                options: channel_type_options().to_vec(),
                                cursor: 0,
                            }));
                        } else if cl.cursor == drafts + 1 {
                            // "Done" row → close.
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
        let rows: Vec<FieldFormRow> = fields
            .into_iter()
            .map(|d| {
                let buf = d.default.clone().unwrap_or_default();
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
                self.form.default_model = pick("model");
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
                });
            }
            _ => {}
        }
        true
    }

    fn apply_picker_choice(&mut self, sel: Selector, value: String) {
        match sel {
            Selector::RiskProfile => self.form.risk = value,
            Selector::RuntimeProfile => self.form.runtime = value,
            Selector::Memory => {
                if let Some(m) = MemoryKind::from_wire(&value) {
                    self.form.memory = m;
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
            format!("Created `{alias}`.")
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

fn draw_modal(frame: &mut Frame, area: Rect, modal: &Modal, channels: &[ChannelDraft]) {
    let (title, body_lines, footer) = match modal {
        Modal::Picker(p) => {
            let lines: Vec<Line> = p
                .options
                .iter()
                .enumerate()
                .map(|(i, opt)| {
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
                        Span::styled(opt.label, label_style),
                        Span::raw("  "),
                        Span::styled(opt.help, Style::default().fg(Color::DarkGray)),
                    ])
                })
                .collect();
            (
                format!(" {} ", p.selector.title()),
                lines,
                "↑/↓ move   Enter pick   Esc cancel",
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
            )
        }
        Modal::FieldForm(f) => {
            let mut lines: Vec<Line> = Vec::new();
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
                lines.push(Line::from(vec![
                    Span::styled(glyph, Style::default().fg(Color::Yellow)),
                    Span::styled(format!("{:14}", row.descriptor.label), label_style),
                    Span::styled("  ", Style::default()),
                    Span::styled(display, Style::default().fg(Color::Gray)),
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
                "Tab/↑/↓ move   Enter accept   Esc cancel",
            )
        }
        Modal::ChannelList(cl) => {
            let mut lines: Vec<Line> = Vec::new();
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
            let action_row = |label: &str, idx: usize| {
                let is_cursor = idx == cl.cursor;
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
            };
            lines.push(action_row("+ Add channel", add_idx));
            lines.push(action_row("Done", done_idx));
            let _ = row_count; // already encoded by the cursor styling above.
            (
                " Channels ".to_string(),
                lines,
                "↑/↓ move   Enter activate   d delete   Esc close",
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
}
