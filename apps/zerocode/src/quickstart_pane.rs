//! Quickstart pane — modal-based checklist that produces one
//! `BuilderSubmission`, sent through `quickstart/apply` RPC.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, ListState, Padding, Paragraph, Wrap},
};
use std::sync::Arc;

/// Display placeholder the daemon emits for an unset `Option` field,
/// mirroring `zeroclaw_config::traits::UNSET_DISPLAY`. zerocode talks to
/// the daemon over RPC and mirrors config types on the wire rather than
/// depending on `zeroclaw-config`, so the sentinel is duplicated here.
/// It is a *display* value, never a real default — seeding a field
/// buffer with it or submitting it makes the daemon validate `<unset>`
/// against the field's true type (e.g. a bool), which fails with
/// "bool value with length 7".
const UNSET_DISPLAY: &str = "<unset>";

/// Upper bound on rendered secret-mask bullets. A pasted API key can be
/// 100+ chars; one bullet per character wraps the masked value across
/// rows and pushes later fields and the footer out of view. Beyond this
/// the mask is clipped and a `(+N)` suffix reports the hidden length.
const SECRET_MASK_MAX: usize = 24;

/// Render a bounded secret mask. One bullet per character lets a pasted
/// API key wrap across rows and shove later fields off-screen; past
/// `SECRET_MASK_MAX` the mask is clipped and the hidden length reported
/// as `(+N)` so the user still has feedback that input was captured.
fn masked_secret(buf: &str) -> String {
    let count = buf.chars().count();
    if count > SECRET_MASK_MAX {
        format!(
            "{} (+{})",
            "•".repeat(SECRET_MASK_MAX),
            count - SECRET_MASK_MAX
        )
    } else {
        "•".repeat(count)
    }
}

use crate::client::{
    QuickstartApplyResult, QuickstartError, QuickstartFieldDescriptor, QuickstartFieldSection,
    QuickstartStateResult, QuickstartStep, QuickstartSurface, RpcClient,
};
use crate::theme;
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
    PeerGroups,
    Agent,
    Submit,
}

impl Selector {
    const ALL: [Selector; 8] = [
        Selector::ModelProvider,
        Selector::RiskProfile,
        Selector::RuntimeProfile,
        Selector::Memory,
        Selector::Channels,
        Selector::PeerGroups,
        Selector::Agent,
        Selector::Submit,
    ];

    fn fluent_key(self) -> &'static str {
        match self {
            Selector::ModelProvider => "zc-quickstart-selector-model-provider",
            Selector::RiskProfile => "zc-quickstart-selector-risk-profile",
            Selector::RuntimeProfile => "zc-quickstart-selector-runtime-profile",
            Selector::Memory => "zc-quickstart-selector-memory",
            Selector::Channels => "zc-quickstart-selector-channels",
            Selector::PeerGroups => "zc-quickstart-selector-peer-groups",
            Selector::Agent => "zc-quickstart-selector-agent",
            Selector::Submit => "zc-quickstart-selector-submit",
        }
    }

    fn title(self) -> String {
        crate::i18n::t(self.fluent_key())
    }

    fn step(self) -> QuickstartStep {
        match self {
            Selector::ModelProvider => QuickstartStep::ModelProvider,
            Selector::RiskProfile => QuickstartStep::RiskProfile,
            Selector::RuntimeProfile => QuickstartStep::RuntimeProfile,
            Selector::Memory => QuickstartStep::Memory,
            Selector::Channels => QuickstartStep::Channels,
            Selector::PeerGroups => QuickstartStep::PeerGroups,
            Selector::Agent => QuickstartStep::Agent,
            Selector::Submit => QuickstartStep::Agent,
        }
    }

    /// Localised title for the selector that owns a validation step, so
    /// a field error can name where the problem lives (e.g.
    /// `Model provider / alias: …`) instead of only a count.
    fn title_for_step(step: QuickstartStep) -> String {
        let sel = match step {
            QuickstartStep::ModelProvider => Selector::ModelProvider,
            QuickstartStep::RiskProfile => Selector::RiskProfile,
            QuickstartStep::RuntimeProfile => Selector::RuntimeProfile,
            QuickstartStep::Memory => Selector::Memory,
            QuickstartStep::Channels => Selector::Channels,
            QuickstartStep::PeerGroups => Selector::PeerGroups,
            QuickstartStep::Agent => Selector::Agent,
        };
        sel.title()
    }
}

/// Drop validation errors for selectors the user hasn't filled yet.
///
/// `revalidate` runs after every selector commit, and the runtime
/// validates the *whole* submission, short-circuiting at the first
/// failing step. Mid-build that first failure is almost always a
/// selector the user simply hasn't reached — e.g. the empty risk
/// profile, surfacing the instant the model provider is committed. Shown
/// as a red "1 error(s) — fix selectors and resubmit", it reads as if the
/// step they just finished broke. Keep only errors for selectors the user
/// has actually filled; unfilled ones are already tracked as `[ ]` in the
/// checklist, and submit re-validates the full set with nothing empty to
/// short-circuit on.
fn retain_filled_selector_errors(
    form: &FormState,
    errors: Vec<QuickstartError>,
) -> Vec<QuickstartError> {
    errors
        .into_iter()
        .filter(|e| {
            Selector::ALL
                .iter()
                .any(|s| form.is_satisfied(*s) && s.step() == e.step)
        })
        .collect()
}

fn opt(value: &str, label: impl Into<String>, help: impl Into<String>) -> PickerOption {
    PickerOption {
        value: value.to_string(),
        label: label.into(),
        help: help.into(),
        use_existing: false,
    }
}

fn existing_opt(alias: String) -> PickerOption {
    PickerOption {
        label: format!("Use existing: {alias}"),
        value: alias,
        help: crate::i18n::t("zc-quickstart-reuse-alias-help"),
        use_existing: true,
    }
}

fn in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn synth_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE)
}

/// The character a key press contributes to a free-text buffer (the
/// agent-name field), or `None` for control chords and non-character
/// keys. Letters that double as modal hotkeys on file rows — `e` (edit
/// in $EDITOR), `t` (from template), `c` (clear), `d` (delete) — are
/// still plain text on a text row, so this deliberately ignores the
/// chord mapping: the hotkey arms are gated on `on_file` and never fire
/// while the cursor is on the name field.
fn typed_char(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => Some(c),
        _ => None,
    }
}

fn action_row_line(label: &str, is_cursor: bool) -> Line<'static> {
    let glyph = if is_cursor { " › " } else { "   " };
    let style = if is_cursor {
        theme::accent_style()
    } else {
        theme::body_style()
    };
    Line::from(vec![
        Span::styled(glyph, theme::accent_style()),
        Span::styled(label.to_string(), style),
    ])
}

fn risk_options() -> [PickerOption; 3] {
    [
        opt(
            "locked_down",
            crate::i18n::t("zc-quickstart-risk-locked-down"),
            crate::i18n::t("zc-quickstart-risk-locked-down-desc"),
        ),
        opt(
            "balanced",
            crate::i18n::t("zc-quickstart-risk-balanced"),
            crate::i18n::t("zc-quickstart-risk-balanced-desc"),
        ),
        opt(
            "yolo",
            crate::i18n::t("zc-quickstart-risk-yolo"),
            crate::i18n::t("zc-quickstart-risk-yolo-desc"),
        ),
    ]
}

fn runtime_options() -> [PickerOption; 3] {
    [
        opt(
            "tight",
            crate::i18n::t("zc-quickstart-runtime-tight"),
            crate::i18n::t("zc-quickstart-runtime-tight-desc"),
        ),
        opt(
            "balanced",
            crate::i18n::t("zc-quickstart-runtime-balanced"),
            crate::i18n::t("zc-quickstart-runtime-balanced-desc"),
        ),
        opt(
            "unbounded",
            crate::i18n::t("zc-quickstart-runtime-unbounded"),
            crate::i18n::t("zc-quickstart-runtime-unbounded-desc"),
        ),
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
                crate::i18n::t("zc-quickstart-provider-local")
            } else {
                crate::i18n::t("zc-quickstart-provider-cloud")
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
    /// Captured field-form values for the model_provider entry,
    /// keyed by `FieldDescriptor.key` (kebab-case schema identifier).
    /// Submitted verbatim via `ModelProviderChoice.fields`; the
    /// daemon writes each entry under `<prefix>.<key>`.
    provider_fields: std::collections::HashMap<String, String>,
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
    peer_groups_visited: bool,
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
            provider_fields: std::collections::HashMap::new(),
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
            peer_groups_visited: false,
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
            Selector::PeerGroups => self.peer_groups_visited,
            Selector::Agent => !self.agent_name.is_empty(),
            // Submit ticks when the daemon has accepted the submission;
            // until then it stays open so the user can tell it's the
            // active step.
            Selector::Submit => false,
        }
    }

    /// Whether every real form selector is satisfied. Excludes `Submit`
    /// — it's the action row, not a field, and `is_satisfied(Submit)`
    /// is always false until the daemon accepts the submission, so
    /// including it would make Create permanently unreachable.
    fn all_selectors_satisfied(&self) -> bool {
        Selector::ALL
            .iter()
            .filter(|s| !matches!(s, Selector::Submit))
            .all(|s| self.is_satisfied(*s))
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
            Selector::PeerGroups => {
                if self.peer_groups.is_empty() {
                    "0".to_string()
                } else {
                    format!("{} configured", self.peer_groups.len())
                }
            }
            Selector::Agent => {
                if self.agent_name.is_empty() {
                    "not yet named".to_string()
                } else {
                    self.agent_name.clone()
                }
            }
            Selector::Submit => crate::i18n::t("zc-quickstart-submit-create"),
        }
    }

    fn to_submission(&self) -> BuilderSubmission {
        let model_provider = match self.provider_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: self.provider_type.clone(),
                alias: self.provider_alias.clone(),
                model: self.model.clone(),
                fields: self.provider_fields.clone(),
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
    /// Single-field text input.
    TextInput(TextInputModal),
    /// Multi-field form sourced from `quickstart/fields`. Used by
    /// Model provider and Channels once the user has chosen a type.
    FieldForm(FieldFormModal),
    /// Channels list manager.
    ChannelList(ChannelListModal),
    /// Peer groups list manager.
    PeerGroupList(PeerGroupListModal),
    /// Agent name + personality files staging.
    Agent(AgentModal),
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
    /// Step 1 of the peer-group add flow: chose a channel ref. The
    /// next step opens a [`TextInputModal`] for the peers buffer.
    PeerGroupChannel,
}

struct TextInputModal {
    selector: Selector,
    label: &'static str,
    help: String,
    buf: String,
    is_secret: bool,
    /// When `Some`, this TextInput is the peers-buffer step of the
    /// peer-group add flow. The wrapped channel ref is consumed at
    /// commit time to build a [`wire::QuickstartPeerGroup`].
    peer_group_channel: Option<String>,
}

/// Lifecycle of the live model catalog for a ModelProvider FieldForm.
/// The form opens immediately in `Pending` so the modal paints a
/// loading row instead of the picker blocking on the catalog RPC; a
/// later `tick` resolves it to `Loaded` (model row upgraded to an
/// enum picker) or `Empty` (catalog unavailable → free-text fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelCatalogState {
    /// Section has no model row (channels) — nothing to load.
    NotApplicable,
    /// Catalog fetch not yet started or in flight.
    Pending,
    /// Catalog returned variants; model row is a picker.
    Loaded,
    /// Catalog was empty or unavailable; model row is free-text.
    Empty,
}

struct FieldFormModal {
    selector: Selector,
    /// Provider / channel type chosen in the preceding picker step.
    type_key: String,
    /// User-named alias for this entry. Pre-filled with `type_key`.
    alias: String,
    fields: Vec<FieldFormRow>,
    cursor: usize,
    model_catalog: ModelCatalogState,
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

struct PeerGroupListModal {
    /// Same layout as [`ChannelListModal`]: drafts, then "+ Add", then "Done".
    cursor: usize,
}

struct AgentModal {
    /// Row 0: name. Rows 1..=N: one per filename in
    /// `state_snapshot.personality_files`. Row N+1: Save & close.
    cursor: usize,
    name: String,
    /// Staged content per canonical filename. Empty string = unset.
    files: std::collections::BTreeMap<String, String>,
    /// Canonical filenames the daemon reported in `state.personality_files`.
    /// Captured at modal open so the row order is stable across re-draws.
    filenames: Vec<String>,
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
            HelpEntry::new(vec!["↑/↓"], crate::i18n::t("zc-quickstart-help-move")),
            HelpEntry::new(vec!["Enter"], crate::i18n::t("zc-quickstart-help-open")),
            HelpEntry::key(
                "c",
                crate::i18n::t_args("zc-quickstart-help-create", &[("enter", "Enter")]),
            ),
            HelpEntry::key("Esc", crate::i18n::t("zc-quickstart-help-leave")),
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
            let (rect, rows) = draw_modal(
                frame,
                area,
                modal,
                &self.form.channels,
                &self.form.peer_groups,
            );
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
        use crate::keymap::QuickstartTabAction;
        match QuickstartTabAction::from_chord(&key) {
            Some(QuickstartTabAction::Down) => {
                self.move_selection(1);
                false
            }
            Some(QuickstartTabAction::Up) => {
                self.move_selection(-1);
                false
            }
            Some(QuickstartTabAction::Enter) => {
                if let Some(idx) = self.list_state.selected()
                    && let Some(sel) = Selector::ALL.get(idx).copied()
                {
                    self.last_step = Some(sel.step());
                    if matches!(sel, Selector::Submit) {
                        if self.can_create() {
                            self.submit().await;
                        }
                    } else {
                        self.open_modal_for(sel);
                    }
                }
                false
            }
            Some(QuickstartTabAction::Create) => {
                if self.can_create() {
                    self.submit().await;
                }
                false
            }
            _ => false,
        }
    }

    /// Route a bracketed-paste payload into the active modal's text
    /// field. Mirrors the per-modal char-insertion rules in
    /// `handle_modal_key` so paste lands in exactly the same buffer a
    /// keystroke would: the TextInput buffer, the focused non-enum
    /// FieldForm row (e.g. an `api_key`), or the Agent name row. Panes
    /// without an active text target ignore the paste. Without this,
    /// `app`'s `Event::Paste` had no Quickstart arm, so paste was
    /// silently dropped on every Quickstart widget.
    pub fn handle_paste(&mut self, text: &str) {
        let Some(modal) = self.active_modal.as_mut() else {
            return;
        };
        match modal {
            Modal::TextInput(t) => t.buf.push_str(text),
            Modal::FieldForm(f) => {
                if let Some(row) = f.fields.get_mut(f.cursor)
                    && row.descriptor.enum_variants.is_none()
                {
                    row.buf.push_str(text);
                }
            }
            Modal::Agent(a) => {
                if a.cursor == 0 {
                    a.name.push_str(text);
                }
            }
            Modal::Picker(_) | Modal::ChannelList(_) | Modal::PeerGroupList(_) => {}
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
                        if matches!(sel, Selector::Submit) {
                            if self.can_create() {
                                self.submit().await;
                            }
                        } else {
                            self.open_modal_for(sel);
                        }
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
            Modal::PeerGroupList(pl) => (&mut pl.cursor, self.modal_row_rects.len()),
            Modal::Agent(a) => (&mut a.cursor, self.modal_row_rects.len()),
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
            Modal::PeerGroupList(pl) => {
                pl.cursor = idx;
            }
            Modal::Agent(a) => {
                a.cursor = idx;
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
            Selector::Agent => {
                let filenames: Vec<String> = self
                    .state_snapshot
                    .as_ref()
                    .map(|s| s.personality_files.iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default();
                let mut files: std::collections::BTreeMap<String, String> =
                    std::collections::BTreeMap::new();
                for pf in &self.form.personality_files {
                    files.insert(pf.filename.clone(), pf.content.clone());
                }
                for f in &filenames {
                    files.entry(f.clone()).or_default();
                }
                self.active_modal = Some(Modal::Agent(AgentModal {
                    cursor: 0,
                    name: self.form.agent_name.clone(),
                    files,
                    filenames,
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
            Selector::PeerGroups => {
                self.active_modal = Some(Modal::PeerGroupList(PeerGroupListModal { cursor: 0 }));
            }
            // Submit is handled by the caller (async submit/validate
            // flow); reaching this arm means a bug somewhere upstream.
            Selector::Submit => {}
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
        use crate::keymap::QuickstartModalAction;
        let action = QuickstartModalAction::from_chord(&key);
        match modal {
            Modal::Picker(p) => match action {
                Some(QuickstartModalAction::Cancel) => {
                    self.active_modal = None;
                }
                Some(QuickstartModalAction::Up) if p.cursor > 0 => {
                    p.cursor -= 1;
                }
                Some(QuickstartModalAction::Down) if p.cursor + 1 < p.options.len() => {
                    p.cursor += 1;
                }
                Some(QuickstartModalAction::Confirm) => {
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
                        (PickerPurpose::PeerGroupChannel, _) => {
                            self.active_modal = Some(Modal::TextInput(TextInputModal {
                                selector: Selector::PeerGroups,
                                label: "external_peers",
                                help: crate::i18n::t("zc-quickstart-help-external-peers"),
                                buf: String::new(),
                                is_secret: false,
                                peer_group_channel: Some(chosen),
                            }));
                        }
                    }
                }
                _ => {}
            },
            Modal::TextInput(t) => match action {
                Some(QuickstartModalAction::Cancel) => {
                    self.active_modal = None;
                }
                Some(QuickstartModalAction::Confirm) => {
                    let value = t.buf.trim().to_string();
                    let selector = t.selector;
                    if let Some(channel) = t.peer_group_channel.clone() {
                        let peers: Vec<String> = value
                            .split([',', '\n'])
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        let (ty, alias) = channel
                            .split_once('.')
                            .map(|(t, a)| (t.to_string(), a.to_string()))
                            .unwrap_or_else(|| (channel.clone(), "default".into()));
                        let name = format!("{ty}_{alias}_default");
                        self.form
                            .peer_groups
                            .push(crate::wire::QuickstartPeerGroup {
                                name,
                                channel,
                                external_peers: peers,
                                ignore: Vec::new(),
                            });
                        let cursor = self.form.peer_groups.len().saturating_sub(1);
                        self.active_modal =
                            Some(Modal::PeerGroupList(PeerGroupListModal { cursor }));
                        self.revalidate().await;
                    } else if !value.is_empty() {
                        self.apply_text_choice(selector, value);
                        self.active_modal = None;
                        self.revalidate().await;
                    }
                }
                Some(QuickstartModalAction::Backspace) => {
                    t.buf.pop();
                }
                _ => {
                    if let Some(c) = typed_char(&key) {
                        t.buf.push(c);
                    }
                }
            },
            Modal::FieldForm(f) => match action {
                Some(QuickstartModalAction::Cancel) => {
                    self.active_modal = None;
                }
                Some(QuickstartModalAction::NextField) | Some(QuickstartModalAction::Down) => {
                    if f.cursor + 1 < f.fields.len() {
                        f.cursor += 1;
                    } else {
                        f.cursor = 0;
                    }
                }
                Some(QuickstartModalAction::PrevField) | Some(QuickstartModalAction::Up) => {
                    if f.cursor == 0 {
                        f.cursor = f.fields.len().saturating_sub(1);
                    } else {
                        f.cursor -= 1;
                    }
                }
                Some(QuickstartModalAction::Confirm) => {
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
                Some(QuickstartModalAction::Left) => {
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
                Some(QuickstartModalAction::Right) => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && let Some(variants) = row.descriptor.enum_variants.as_deref()
                        && !variants.is_empty()
                    {
                        let cur = variants.iter().position(|v| v == &row.buf).unwrap_or(0);
                        let next = (cur + 1) % variants.len();
                        row.buf = variants[next].clone();
                    }
                }
                Some(QuickstartModalAction::Backspace) => {
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && row.descriptor.enum_variants.is_none()
                    {
                        row.buf.pop();
                    }
                }
                _ => {
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && let Some(row) = f.fields.get_mut(f.cursor)
                        && row.descriptor.enum_variants.is_none()
                    {
                        row.buf.push(c);
                    }
                }
            },
            Modal::ChannelList(cl) => {
                let drafts = self.form.channels.len();
                let row_count = drafts + 2; // drafts + Add + Done
                match action {
                    Some(QuickstartModalAction::Cancel) => {
                        self.active_modal = None;
                    }
                    Some(QuickstartModalAction::Up) if cl.cursor > 0 => {
                        cl.cursor -= 1;
                    }
                    Some(QuickstartModalAction::Down) if cl.cursor + 1 < row_count => {
                        cl.cursor += 1;
                    }
                    Some(QuickstartModalAction::DeleteRow) if cl.cursor < drafts => {
                        self.form.channels.remove(cl.cursor);
                        if cl.cursor >= self.form.channels.len() {
                            cl.cursor = self.form.channels.len();
                        }
                    }
                    Some(QuickstartModalAction::Confirm) => {
                        if cl.cursor == drafts {
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
                            self.form.channels_visited = true;
                            self.active_modal = None;
                        }
                    }
                    _ => {}
                }
            }
            Modal::PeerGroupList(pl) => {
                let drafts = self.form.peer_groups.len();
                let row_count = drafts + 2;
                match action {
                    Some(QuickstartModalAction::Cancel) => {
                        self.active_modal = None;
                    }
                    Some(QuickstartModalAction::Up) if pl.cursor > 0 => {
                        pl.cursor -= 1;
                    }
                    Some(QuickstartModalAction::Down) if pl.cursor + 1 < row_count => {
                        pl.cursor += 1;
                    }
                    Some(QuickstartModalAction::DeleteRow) if pl.cursor < drafts => {
                        self.form.peer_groups.remove(pl.cursor);
                        if pl.cursor >= self.form.peer_groups.len() {
                            pl.cursor = self.form.peer_groups.len();
                        }
                    }
                    Some(QuickstartModalAction::Confirm) => {
                        if pl.cursor == drafts {
                            let options = self.peer_group_channel_options();
                            if options.is_empty() {
                            } else {
                                self.active_modal = Some(Modal::Picker(PickerModal {
                                    selector: Selector::PeerGroups,
                                    purpose: PickerPurpose::PeerGroupChannel,
                                    options,
                                    cursor: 0,
                                }));
                            }
                        } else if pl.cursor == drafts + 1 {
                            self.form.peer_groups_visited = true;
                            self.active_modal = None;
                        }
                    }
                    _ => {}
                }
            }
            Modal::Agent(a) => {
                let row_count = a.filenames.len() + 2;
                let last_row = row_count - 1;
                let on_name = a.cursor == 0;
                let on_save = a.cursor == last_row;
                let on_file = !on_name && !on_save;
                match action {
                    Some(QuickstartModalAction::Cancel) => {
                        self.commit_agent_modal();
                        self.active_modal = None;
                        self.revalidate().await;
                    }
                    Some(QuickstartModalAction::Confirm) if on_save => {
                        self.commit_agent_modal();
                        self.active_modal = None;
                        self.revalidate().await;
                    }
                    Some(QuickstartModalAction::NextField) | Some(QuickstartModalAction::Down)
                        if a.cursor + 1 < row_count =>
                    {
                        a.cursor += 1;
                    }
                    Some(QuickstartModalAction::PrevField) | Some(QuickstartModalAction::Up)
                        if a.cursor > 0 =>
                    {
                        a.cursor -= 1;
                    }
                    Some(QuickstartModalAction::Backspace) if on_name => {
                        a.name.pop();
                    }
                    Some(QuickstartModalAction::EditWithEditor) if on_file => {
                        let filename = a.filenames[a.cursor - 1].clone();
                        let seed = a.files.get(&filename).cloned().unwrap_or_default();
                        let edited = crate::chat::open_editor_for_content(&seed).await;
                        if let Some(Modal::Agent(a)) = self.active_modal.as_mut() {
                            a.files.insert(filename, edited);
                        }
                    }
                    Some(QuickstartModalAction::EditTemplate) if on_file => {
                        let filename = a.filenames[a.cursor - 1].clone();
                        let templated = self.fetch_personality_template(&filename).await;
                        if let (Some(content), Some(Modal::Agent(a))) =
                            (templated, self.active_modal.as_mut())
                        {
                            a.files.insert(filename, content);
                        }
                    }
                    Some(QuickstartModalAction::EditCopy) if on_file => {
                        let filename = a.filenames[a.cursor - 1].clone();
                        a.files.insert(filename, String::new());
                    }
                    _ => {
                        if on_name && let Some(c) = typed_char(&key) {
                            a.name.push(c);
                        }
                    }
                }
            }
        }
    }

    fn apply_text_choice(&mut self, _sel: Selector, _value: String) {
        // Agent name is now committed via `commit_agent_modal`. No other
        // selector lands here today, but the function stays so adding a
        // new TextInput flow doesn't need to re-thread the call path.
    }

    /// Pull staged name and non-empty personality files out of the active
    /// AgentModal into `FormState`. No-op when the active modal isn't an
    /// AgentModal.
    fn commit_agent_modal(&mut self) {
        let Some(Modal::Agent(a)) = self.active_modal.as_ref() else {
            return;
        };
        self.form.agent_name = a.name.trim().to_string();
        self.form.personality_files = a
            .files
            .iter()
            .filter(|(_, content)| !content.trim().is_empty())
            .map(
                |(filename, content)| crate::wire::QuickstartPersonalityFile {
                    filename: filename.clone(),
                    content: content.clone(),
                },
            )
            .collect();
    }

    async fn fetch_personality_template(&self, filename: &str) -> Option<String> {
        let res = self.rpc.personality_templates(None).await.ok()?;
        res.files
            .into_iter()
            .find(|f| f.filename == filename)
            .map(|f| f.content)
    }

    fn adopt_existing_provider(&mut self, dotted_ref: String) {
        if let Some((ty, alias)) = dotted_ref.split_once('.') {
            self.form.provider_type = ty.to_string();
            self.form.provider_alias = alias.to_string();
            self.form.provider_mode = SelectorMode::Existing;
            // Default model / field values aren't carried in the
            // "existing" path — the runtime resolves the alias against
            // the live config at apply time. Leave them empty so they
            // don't overwrite the existing alias's values.
            self.form.model.clear();
            self.form.provider_fields.clear();
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

    /// Channel refs available for a new peer group: staged channel
    /// drafts from this run plus any unassigned existing channels the
    /// daemon reported, minus refs already claimed by a staged peer
    /// group. Matches the CLI and web flows.
    fn peer_group_channel_options(&self) -> Vec<PickerOption> {
        let staged: Vec<String> = self
            .form
            .channels
            .iter()
            .map(|c| format!("{}.{}", c.channel_type, c.alias))
            .collect();
        let claimed: std::collections::HashSet<String> = self
            .form
            .peer_groups
            .iter()
            .map(|pg| pg.channel.clone())
            .collect();
        let unassigned: &[String] = self
            .state_snapshot
            .as_ref()
            .map(|s| s.unassigned_channels.as_slice())
            .unwrap_or(&[]);
        let mut refs: Vec<String> = staged
            .into_iter()
            .chain(unassigned.iter().cloned())
            .filter(|r| !claimed.contains(r))
            .collect();
        refs.sort();
        refs.dedup();
        refs.into_iter()
            .map(|r| PickerOption {
                label: r.clone(),
                value: r,
                help: String::new(),
                use_existing: false,
            })
            .collect()
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
                self.last_errors = retain_filled_selector_errors(&self.form, errors);
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
        // The model catalog is fetched lazily in `tick` so the modal
        // paints a visible "Loading models…" row instead of freezing
        // the picker while the catalog RPC is in flight. Bool fields
        // are seeded here as two-value toggles so the runtime's
        // `kind: bool` metadata renders as `‹ false › / ‹ true ›`
        // rather than a raw text input the user has to type into.
        let mut rows: Vec<FieldFormRow> = fields
            .into_iter()
            .map(|mut d| {
                if matches!(d.kind, crate::client::QuickstartFieldKind::Bool) {
                    d.enum_variants = Some(vec!["false".to_string(), "true".to_string()]);
                }
                // For enum fields, default the buffer to the first
                // variant so the user lands on a valid value. ←/→
                // cycles through the list. The daemon's `<unset>`
                // placeholder for optional fields is treated as no
                // value — seeding or submitting it would fail
                // validation against the field's real type.
                let default = d
                    .default
                    .clone()
                    .filter(|v| v != UNSET_DISPLAY && !v.is_empty());
                let buf = if let Some(variants) = d.enum_variants.as_deref()
                    && !variants.is_empty()
                {
                    default
                        .filter(|v| variants.contains(v))
                        .unwrap_or_else(|| variants[0].clone())
                } else {
                    default.unwrap_or_default()
                };
                FieldFormRow { descriptor: d, buf }
            })
            .collect();
        let model_catalog = if matches!(section, QuickstartFieldSection::ModelProvider) {
            ModelCatalogState::Pending
        } else {
            ModelCatalogState::NotApplicable
        };
        // Prepend an editable alias row for ModelProvider so users can
        // choose a custom alias instead of the hardcoded "default".
        if matches!(section, QuickstartFieldSection::ModelProvider) {
            let default_alias = "default".to_string();
            rows.insert(
                0,
                FieldFormRow {
                    descriptor: QuickstartFieldDescriptor {
                        key: "alias".to_string(),
                        label: crate::i18n::t("zc-quickstart-field-label-alias"),
                        help: crate::i18n::t("zc-quickstart-field-help-alias"),
                        kind: crate::client::QuickstartFieldKind::String,
                        is_secret: false,
                        enum_variants: None,
                        required: true,
                        default: Some(default_alias.clone()),
                    },
                    buf: default_alias,
                },
            );
        }
        let alias = match section {
            QuickstartFieldSection::ModelProvider => "default".to_string(),
            _ => type_key.clone(),
        };
        self.active_modal = Some(Modal::FieldForm(FieldFormModal {
            selector: sel,
            type_key,
            alias,
            fields: rows,
            cursor: 0,
            model_catalog,
        }));
    }

    /// Per-frame poll. Resolves a `Pending` model catalog for an open
    /// ModelProvider FieldForm: the modal opens immediately so the
    /// loading row paints, then this fetch upgrades the `model` row to
    /// an enum picker (`Loaded`) or leaves it as free text (`Empty`)
    /// when no live catalog is available. Driven from the app idle
    /// loop the same way Dashboard/Logs panes refresh.
    pub async fn tick(&mut self) {
        let pending_type = match self.active_modal.as_ref() {
            Some(Modal::FieldForm(f)) if f.model_catalog == ModelCatalogState::Pending => {
                Some(f.type_key.clone())
            }
            _ => None,
        };
        let Some(type_key) = pending_type else {
            return;
        };
        let models: Option<Vec<String>> = match self.rpc.catalog_models(&type_key).await {
            Ok(res) if res.live && !res.models.is_empty() => Some(res.models),
            _ => None,
        };
        let Some(Modal::FieldForm(f)) = self.active_modal.as_mut() else {
            return;
        };
        // Re-check identity: the user may have cancelled and reopened a
        // different type while the catalog RPC was in flight.
        if f.type_key != type_key || f.model_catalog != ModelCatalogState::Pending {
            return;
        }
        match models {
            Some(models) => {
                if let Some(row) = f
                    .fields
                    .iter_mut()
                    .find(|r| r.descriptor.key.eq_ignore_ascii_case("model"))
                {
                    row.descriptor.kind = crate::client::QuickstartFieldKind::Enum;
                    row.descriptor.enum_variants = Some(models.clone());
                    if !models.contains(&row.buf) {
                        row.buf = models[0].clone();
                    }
                }
                f.model_catalog = ModelCatalogState::Loaded;
            }
            None => f.model_catalog = ModelCatalogState::Empty,
        }
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
                let mut provider_fields: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for row in &f.fields {
                    // `model` and `alias` are hoisted to FormState
                    // fields; every other descriptor flows through
                    // `provider_fields` keyed by its schema identifier
                    // (kebab-case).
                    if row.descriptor.key == "model" || row.descriptor.key == "alias" {
                        continue;
                    }
                    let value = row.buf.trim();
                    if !value.is_empty() && value != UNSET_DISPLAY {
                        provider_fields.insert(row.descriptor.key.clone(), value.to_string());
                    }
                }
                self.form.provider_type = f.type_key.clone();
                // Read alias from the editable field row; fall back to
                // `f.alias` for backward compatibility (non-ModelProvider
                // sections keep the auto-generated alias path).
                let alias_value = pick("alias");
                self.form.provider_alias = if alias_value.is_empty() {
                    f.alias.clone()
                } else {
                    alias_value
                };
                self.form.provider_mode = SelectorMode::Fresh;
                self.form.model = pick("model");
                self.form.provider_fields = provider_fields;
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
        self.form.all_selectors_satisfied() && !self.busy
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
            Span::styled(crate::i18n::t("zc-quickstart-title"), theme::accent_style()),
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
                    theme::dim_style()
                };
                let glyph = if satisfied { "[✓]" } else { "[ ]" };
                let title_style = theme::heading_style();
                let summary_style = theme::dim_style();
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {glyph}  "), glyph_style),
                    Span::styled(format!("{:18}", sel.title()), title_style),
                    Span::styled("  ", summary_style),
                    Span::styled(self.form.summary(*sel), summary_style),
                ]))
            })
            .collect();

        let block = theme::panel_block(" Selectors ").padding(Padding::horizontal(1));
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
            .highlight_style(theme::selected_style())
            .highlight_symbol(" › ");
        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_status_strip(&self, frame: &mut Frame, area: Rect) {
        let can_create = self.can_create();
        let label = if self.busy {
            crate::i18n::t("zc-quickstart-status-submitting")
        } else if let Some(alias) = &self.applied_alias {
            crate::i18n::t_args("zc-quickstart-status-created", &[("alias", alias.as_str())])
        } else if let Some(first) = self.last_errors.first() {
            // Name the first actionable field error so the user knows
            // which field is invalid, instead of only a count. The
            // daemon's message often already carries the specifics
            // (e.g. "alias openai.default already exists").
            let where_ = Selector::title_for_step(first.step);
            let field_part = if first.field.is_empty() {
                String::new()
            } else {
                format!(" / {}", first.field)
            };
            let more = self.last_errors.len().saturating_sub(1);
            let suffix = if more > 0 {
                crate::i18n::t_args(
                    "zc-quickstart-status-more-errors",
                    &[("count", &more.to_string())],
                )
            } else {
                String::new()
            };
            crate::i18n::t_args(
                "zc-quickstart-status-first-error",
                &[
                    ("where", where_.trim()),
                    ("field", &field_part),
                    ("message", first.message.trim()),
                    ("more", &suffix),
                ],
            )
        } else if can_create {
            crate::i18n::t_args("zc-quickstart-status-can-create", &[("chord", "c")])
        } else {
            crate::i18n::t_args("zc-quickstart-status-hint", &[("chord", "c")])
        };
        let style = if self.applied_alias.is_some() {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if !self.last_errors.is_empty() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if can_create {
            theme::accent_style()
        } else {
            theme::dim_style()
        };
        let block = theme::panel_block("").padding(Padding::horizontal(1));
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
    peer_groups: &[crate::wire::QuickstartPeerGroup],
) -> (Rect, Vec<Rect>) {
    let (title, header_lines, body_lines, footer, cursor_lines): (
        String,
        Vec<Line>,
        Vec<Line>,
        String,
        Vec<usize>,
    ) = match modal {
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
                        theme::accent_style()
                    } else {
                        theme::body_style()
                    };
                    Line::from(vec![
                        Span::styled(glyph, theme::accent_style()),
                        Span::styled(opt.label.as_str(), label_style),
                        Span::raw("  "),
                        Span::styled(opt.help.as_str(), theme::dim_style()),
                    ])
                })
                .collect();
            (
                format!(" {} ", p.selector.title()),
                Vec::new(),
                lines,
                format!(
                    "↑/↓ {move_v}   Enter {pick}   Esc {cancel}",
                    move_v = crate::i18n::t("zc-quickstart-modal-action-move"),
                    pick = crate::i18n::t("zc-quickstart-modal-action-pick"),
                    cancel = crate::i18n::t("zc-quickstart-modal-action-cancel"),
                ),
                cursor_lines,
            )
        }
        Modal::TextInput(t) => {
            let display = if t.is_secret {
                masked_secret(&t.buf)
            } else {
                t.buf.clone()
            };
            let lines = vec![
                Line::from(Span::styled(t.help.clone(), theme::dim_style())),
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{}: ", t.label), theme::accent_style()),
                    Span::styled(display, theme::body_style()),
                    Span::styled("█", theme::accent_style()),
                ]),
            ];
            (
                format!(" {} ", t.selector.title()),
                Vec::new(),
                lines,
                format!(
                    "Enter {accept}   Esc {cancel}",
                    accept = crate::i18n::t("zc-quickstart-modal-action-accept"),
                    cancel = crate::i18n::t("zc-quickstart-modal-action-cancel"),
                ),
                Vec::new(),
            )
        }
        Modal::FieldForm(f) => {
            let mut lines: Vec<Line> = Vec::new();
            let mut cursor_lines = Vec::with_capacity(f.fields.len());
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", crate::i18n::t("zc-quickstart-modal-type-prefix")),
                    theme::dim_style(),
                ),
                Span::styled(f.type_key.as_str(), theme::accent_style()),
            ]));
            lines.push(Line::from(""));
            for (i, row) in f.fields.iter().enumerate() {
                cursor_lines.push(lines.len());
                let is_cursor = i == f.cursor;
                let glyph = if is_cursor { " › " } else { "   " };
                let label_style = if is_cursor {
                    theme::accent_style()
                } else {
                    theme::body_style()
                };
                let is_model_row = row.descriptor.key.eq_ignore_ascii_case("model");
                // Secret fields render a bounded mask so a pasted,
                // realistic-length API key cannot wrap across rows and
                // push later fields and the footer out of view.
                let raw_display = if row.descriptor.is_secret {
                    masked_secret(&row.buf)
                } else {
                    row.buf.clone()
                };
                let is_empty_buf = raw_display.is_empty();
                let is_enum = row.descriptor.enum_variants.is_some();
                // Ghost text (the field default) is a placeholder for an
                // empty buffer, but only when the row is NOT focused.
                // Showing it on the focused row makes the default look
                // like real, editable text the user cannot Backspace
                // away — the alias `default` ghost-state defect. The
                // focused empty row renders empty so the cursor sits
                // where typing lands.
                let show_ghost = is_empty_buf && !is_cursor && !is_enum;
                let (display, value_style) =
                    if is_model_row && f.model_catalog == ModelCatalogState::Pending {
                        (
                            crate::i18n::t_args(
                                "zc-quickstart-model-loading",
                                &[("provider", f.type_key.as_str())],
                            ),
                            theme::dim_style().add_modifier(Modifier::ITALIC),
                        )
                    } else if is_model_row
                        && f.model_catalog == ModelCatalogState::Empty
                        && is_empty_buf
                    {
                        (
                            crate::i18n::t("zc-quickstart-model-catalog-empty"),
                            theme::dim_style().add_modifier(Modifier::ITALIC),
                        )
                    } else if show_ghost {
                        (
                            row.descriptor.default.clone().unwrap_or_default(),
                            theme::dim_style().add_modifier(Modifier::ITALIC),
                        )
                    } else {
                        (raw_display, theme::dim_style())
                    };
                lines.push(Line::from(vec![
                    Span::styled(glyph, theme::accent_style()),
                    Span::styled(format!("{:14}", row.descriptor.label), label_style),
                    Span::styled("  ", Style::default()),
                    Span::styled(if is_enum { "‹ " } else { "" }, theme::accent_style()),
                    Span::styled(display, value_style),
                    Span::styled(if is_enum { " ›" } else { "" }, theme::accent_style()),
                    if is_cursor && !is_enum {
                        Span::styled("█", theme::accent_style())
                    } else {
                        Span::raw("")
                    },
                ]));
            }
            // Help band for the highlighted field, rendered above
            // the form rows in its own region so it can't wrap into
            // and obscure later rows.
            let header_lines: Vec<Line> = f
                .fields
                .get(f.cursor)
                .map(|row| row.descriptor.help.as_str())
                .filter(|h| !h.is_empty())
                .map(|h| {
                    vec![
                        Line::from(Span::styled(
                            h.to_string(),
                            theme::dim_style().add_modifier(Modifier::ITALIC),
                        )),
                        Line::from(""),
                    ]
                })
                .unwrap_or_default();
            (
                format!(" {} ", f.selector.title()),
                header_lines,
                lines,
                format!(
                    "Tab/↑/↓ {move_v}   ←/→ {pick_enum}   Enter {accept}   Esc {cancel}",
                    move_v = crate::i18n::t("zc-quickstart-modal-action-move"),
                    pick_enum = crate::i18n::t("zc-quickstart-modal-action-pick-on-enum"),
                    accept = crate::i18n::t("zc-quickstart-modal-action-accept"),
                    cancel = crate::i18n::t("zc-quickstart-modal-action-cancel"),
                ),
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
                    crate::i18n::t("zc-quickstart-channels-empty"),
                    theme::dim_style(),
                )));
                lines.push(Line::from(""));
            } else {
                for (i, c) in channels.iter().enumerate() {
                    cursor_lines.push(lines.len());
                    let is_cursor = i == cl.cursor;
                    let glyph = if is_cursor { " › " } else { "   " };
                    let style = if is_cursor {
                        theme::accent_style()
                    } else {
                        theme::body_style()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(glyph, theme::accent_style()),
                        Span::styled(format!("{}.{}", c.channel_type, c.alias), style),
                        Span::styled(
                            if c.token.is_some() {
                                "  (token set)"
                            } else {
                                ""
                            },
                            theme::dim_style(),
                        ),
                    ]));
                }
                lines.push(Line::from(""));
            }
            let add_idx = drafts;
            let done_idx = drafts + 1;
            cursor_lines.push(lines.len());
            lines.push(action_row_line(
                &crate::i18n::t("zc-quickstart-channels-add"),
                cl.cursor == add_idx,
            ));
            cursor_lines.push(lines.len());
            lines.push(action_row_line(
                &crate::i18n::t("zc-quickstart-action-done"),
                cl.cursor == done_idx,
            ));
            let _ = row_count; // already encoded by the cursor styling above.
            (
                format!(" {} ", crate::i18n::t("zc-quickstart-block-channels")),
                Vec::new(),
                lines,
                format!(
                    "↑/↓ {move_v}   Enter {activate}   d {delete}   Esc {close}",
                    move_v = crate::i18n::t("zc-quickstart-modal-action-move"),
                    activate = crate::i18n::t("zc-quickstart-modal-action-activate"),
                    delete = crate::i18n::t("zc-quickstart-modal-action-delete"),
                    close = crate::i18n::t("zc-quickstart-modal-action-close"),
                ),
                cursor_lines,
            )
        }
        Modal::PeerGroupList(pl) => {
            let mut lines: Vec<Line> = Vec::new();
            let mut cursor_lines: Vec<usize> = Vec::new();
            let drafts = peer_groups.len();
            let row_count = drafts + 2;
            if drafts == 0 {
                lines.push(Line::from(Span::styled(
                    crate::i18n::t("zc-quickstart-no-peer-groups"),
                    theme::dim_style(),
                )));
                lines.push(Line::from(""));
            } else {
                for (i, pg) in peer_groups.iter().enumerate() {
                    cursor_lines.push(lines.len());
                    let is_cursor = i == pl.cursor;
                    let glyph = if is_cursor { " › " } else { "   " };
                    let style = if is_cursor {
                        theme::accent_style()
                    } else {
                        theme::body_style()
                    };
                    let peers = if pg.external_peers.is_empty() {
                        "no peers".to_string()
                    } else {
                        format!("{} peers", pg.external_peers.len())
                    };
                    lines.push(Line::from(vec![
                        Span::styled(glyph, theme::accent_style()),
                        Span::styled(format!("{} → {}", pg.channel, pg.name), style),
                        Span::styled(format!("  ({peers})"), theme::dim_style()),
                    ]));
                }
                lines.push(Line::from(""));
            }
            let add_idx = drafts;
            let done_idx = drafts + 1;
            cursor_lines.push(lines.len());
            lines.push(action_row_line(
                &crate::i18n::t("zc-quickstart-peers-add"),
                pl.cursor == add_idx,
            ));
            cursor_lines.push(lines.len());
            lines.push(action_row_line(
                &crate::i18n::t("zc-quickstart-action-done"),
                pl.cursor == done_idx,
            ));
            let _ = row_count;
            (
                format!(" {} ", crate::i18n::t("zc-quickstart-block-peers")),
                Vec::new(),
                lines,
                format!(
                    "↑/↓ {move_v}   Enter {activate}   d {delete}   Esc {close}",
                    move_v = crate::i18n::t("zc-quickstart-modal-action-move"),
                    activate = crate::i18n::t("zc-quickstart-modal-action-activate"),
                    delete = crate::i18n::t("zc-quickstart-modal-action-delete"),
                    close = crate::i18n::t("zc-quickstart-modal-action-close"),
                ),
                cursor_lines,
            )
        }
        Modal::Agent(a) => {
            let mut lines: Vec<Line> = Vec::new();
            let mut cursor_lines: Vec<usize> = Vec::new();

            // Row 0: agent name.
            cursor_lines.push(lines.len());
            let on_name = a.cursor == 0;
            let name_style = if on_name {
                theme::accent_style()
            } else {
                theme::body_style()
            };
            let glyph = if on_name { " › " } else { "   " };
            let display = if a.name.is_empty() {
                "<unset>".to_string()
            } else {
                a.name.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(glyph, theme::accent_style()),
                Span::styled(format!("{:14}", "name"), name_style),
                Span::styled("  ", Style::default()),
                Span::styled(display, theme::dim_style()),
                if on_name {
                    Span::styled("█", theme::accent_style())
                } else {
                    Span::raw("")
                },
            ]));

            if !a.filenames.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    crate::i18n::t("zc-quickstart-personality-help"),
                    theme::dim_style(),
                )));
            }

            for (i, filename) in a.filenames.iter().enumerate() {
                cursor_lines.push(lines.len());
                let row_cursor = i + 1;
                let is_cursor = a.cursor == row_cursor;
                let glyph = if is_cursor { " › " } else { "   " };
                let label_style = if is_cursor {
                    theme::accent_style()
                } else {
                    theme::body_style()
                };
                let content = a.files.get(filename).cloned().unwrap_or_default();
                let status = if content.trim().is_empty() {
                    "—".to_string()
                } else {
                    format!("{} bytes", content.len())
                };
                lines.push(Line::from(vec![
                    Span::styled(glyph, theme::accent_style()),
                    Span::styled(format!("{filename:14}"), label_style),
                    Span::styled("  ", Style::default()),
                    Span::styled(status, theme::dim_style()),
                ]));
            }

            lines.push(Line::from(""));
            cursor_lines.push(lines.len());
            let last_row = a.filenames.len() + 1;
            let on_save = a.cursor == last_row;
            lines.push(action_row_line(
                &crate::i18n::t("zc-quickstart-save-and-close"),
                on_save,
            ));

            (
                format!(" {} ", crate::i18n::t("zc-quickstart-block-agent")),
                Vec::new(),
                lines,
                format!(
                    "↑/↓ {move_v}   {edit_name}   e/t/c {on_files}   Esc {save}",
                    move_v = crate::i18n::t("zc-quickstart-modal-action-move"),
                    edit_name = crate::i18n::t("zc-quickstart-modal-action-edit-name"),
                    on_files = crate::i18n::t("zc-quickstart-modal-action-on-file-rows"),
                    save = crate::i18n::t("zc-quickstart-modal-action-save"),
                ),
                cursor_lines,
            )
        }
    };

    let box_w = area.width.saturating_sub(8).min(80);
    let header_h = header_lines.len() as u16;
    let total_content = header_h + body_lines.len() as u16;
    let box_h = (total_content + 4).min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let block = theme::modal_block(&title).padding(Padding::horizontal(1));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Footer occupies the last line of `inner`. The remaining vertical
    // space is split between an optional header band (per-field help)
    // and the body (form rows / picker entries).
    let inner_content_h = inner.height.saturating_sub(1);
    let effective_header_h = header_h.min(inner_content_h);
    let header_rect = Rect::new(inner.x, inner.y, inner.width, effective_header_h);
    let body_rect = Rect::new(
        inner.x,
        inner.y + effective_header_h,
        inner.width,
        inner_content_h.saturating_sub(effective_header_h),
    );

    let body_h = body_rect.height as usize;
    let body_len = body_lines.len();
    let scroll_offset: u16 = if body_len > body_h && body_h > 0 {
        // Pick the cursor line that should stay visible. Modals without
        // a row cursor (TextInput) leave this as None and the body just
        // top-aligns; everything else (Picker, FieldForm, ChannelList)
        // keeps the selected row inside the viewport.
        let selected_line = match modal {
            Modal::Picker(p) => cursor_lines.get(p.cursor).copied(),
            Modal::FieldForm(f) => cursor_lines.get(f.cursor).copied(),
            Modal::ChannelList(cl) => cursor_lines.get(cl.cursor).copied(),
            Modal::PeerGroupList(pl) => cursor_lines.get(pl.cursor).copied(),
            Modal::Agent(a) => cursor_lines.get(a.cursor).copied(),
            Modal::TextInput(_) => None,
        };
        match selected_line {
            Some(sel) if sel >= body_h => (sel + 1 - body_h) as u16,
            _ => 0,
        }
    } else {
        0
    };

    if effective_header_h > 0 {
        frame.render_widget(
            Paragraph::new(header_lines)
                .style(theme::fill_style())
                .wrap(Wrap { trim: false }),
            header_rect,
        );
    }

    let body = Paragraph::new(body_lines)
        .style(theme::fill_style())
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));
    frame.render_widget(body, body_rect);

    let footer_rect = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(footer, theme::dim_style())).style(theme::fill_style()),
        footer_rect,
    );

    // Translate cursor → body-line indices into screen-row hit-rects.
    // Lines outside the visible viewport (clipped by `body_rect.height`
    // or scrolled past) get a zero-sized rect so a click can't hit
    // them accidentally.
    let row_rects: Vec<Rect> = cursor_lines
        .into_iter()
        .map(|line_idx| {
            let scrolled = (line_idx as u16).checked_sub(scroll_offset);
            match scrolled {
                Some(dy) if dy < body_rect.height => {
                    Rect::new(body_rect.x, body_rect.y + dy, body_rect.width, 1)
                }
                _ => Rect::new(0, 0, 0, 0),
            }
        })
        .collect();
    (rect, row_rects)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FormState with every real selector satisfied.
    fn complete_form() -> FormState {
        let mut f = FormState::default_form();
        f.provider_type = "anthropic".into();
        f.provider_alias = "default".into();
        f.model = "claude-3-5-haiku-20241022".into();
        f.risk = "balanced".into();
        f.runtime = "balanced".into();
        f.memory_chosen = true;
        f.channels_visited = true;
        f.peer_groups_visited = true;
        f.agent_name = "bob".into();
        f
    }

    #[test]
    fn submit_is_excluded_from_completeness() {
        // Regression: can_create walked Selector::ALL including Submit,
        // and is_satisfied(Submit) is always false, so Create could
        // never enable even with every field filled.
        let f = complete_form();
        assert!(!f.is_satisfied(Selector::Submit));
        assert!(f.all_selectors_satisfied());
    }

    #[test]
    fn incomplete_form_is_not_satisfied() {
        let f = FormState::default_form();
        assert!(!f.all_selectors_satisfied());
    }

    #[test]
    fn missing_one_field_blocks_completeness() {
        let mut f = complete_form();
        f.agent_name.clear();
        assert!(!f.all_selectors_satisfied());
    }

    fn err(step: QuickstartStep) -> QuickstartError {
        QuickstartError {
            step,
            field: String::new(),
            message: "boom".into(),
        }
    }

    #[test]
    fn revalidate_hides_errors_for_unfilled_selectors() {
        // Regression: committing the model provider triggered a full
        // re-validate. The runtime short-circuits at the first failing
        // step, so the still-empty risk profile came back as a single
        // error and the status strip flashed "1 error(s) — fix selectors
        // and resubmit", as if the provider step had failed.
        let mut f = FormState::default_form();
        f.provider_type = "anthropic".into();
        f.provider_alias = "default".into();
        f.model = "claude-3-5-haiku-20241022".into();
        assert!(f.is_satisfied(Selector::ModelProvider));
        assert!(!f.is_satisfied(Selector::RiskProfile));

        let kept = retain_filled_selector_errors(&f, vec![err(QuickstartStep::RiskProfile)]);
        assert!(
            kept.is_empty(),
            "an unfilled selector's error must not surface mid-build: {kept:?}"
        );
    }

    #[test]
    fn revalidate_keeps_errors_for_filled_selectors() {
        // A real problem with a selector the user *has* filled (e.g. an
        // alias collision on the model provider) must still surface.
        let mut f = FormState::default_form();
        f.provider_type = "anthropic".into();
        f.provider_alias = "default".into();
        f.model = "claude-3-5-haiku-20241022".into();

        let kept = retain_filled_selector_errors(&f, vec![err(QuickstartStep::ModelProvider)]);
        assert_eq!(kept.len(), 1, "filled-selector errors must be retained");
    }

    #[test]
    fn name_field_accepts_hotkey_letters() {
        // Regression: e/t/c/d double as Agent-modal hotkeys (edit in
        // $EDITOR, from template, clear, delete) on file rows. On the
        // name row they are plain text, but the old handler routed every
        // keypress through the chord mapping and dropped any char that
        // resolved to an action — so agent names could not contain those
        // letters. `typed_char` is the text-buffer path; assert it keeps
        // them, and that they really are bound actions (bug was reachable).
        use crate::keymap::QuickstartModalAction;
        for ch in ['e', 'c', 't', 'd', 'E', 'C', 'T', 'D'] {
            let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
            assert_eq!(typed_char(&key), Some(ch), "name field must accept '{ch}'");
            assert!(
                QuickstartModalAction::from_chord(&key).is_some(),
                "'{ch}' must be a modal hotkey for this regression to be real"
            );
        }
    }

    #[test]
    fn typed_char_ignores_control_and_non_char_keys() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(typed_char(&ctrl_c), None);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(typed_char(&enter), None);
    }

    #[test]
    fn unset_placeholder_is_not_a_real_default() {
        // The daemon emits `<unset>` as a display placeholder for
        // optional fields. Seeding a buffer with it (or submitting it)
        // made the daemon validate `<unset>` against the field's real
        // type, failing e.g. a bool with "length 7". Confirm the
        // sentinel matches the daemon's UNSET_DISPLAY wire value.
        assert_eq!(UNSET_DISPLAY, "<unset>");
        let seeded = Some(UNSET_DISPLAY.to_string())
            .filter(|v| v != UNSET_DISPLAY && !v.is_empty())
            .unwrap_or_default();
        assert!(seeded.is_empty());
    }

    #[test]
    fn secret_mask_is_bounded() {
        // A short secret masks one bullet per char; a realistic-length
        // key clips at the cap and reports the hidden remainder so it
        // can never wrap across rows and hide later fields/footer.
        assert_eq!(masked_secret("abc"), "•••");
        assert_eq!(masked_secret(""), "");
        let long = "x".repeat(100);
        let masked = masked_secret(&long);
        assert_eq!(
            masked.chars().filter(|&c| c == '•').count(),
            SECRET_MASK_MAX
        );
        assert!(masked.ends_with(&format!("(+{})", 100 - SECRET_MASK_MAX)));
    }

    #[test]
    fn step_titles_round_trip_through_selector() {
        // Every validation step must resolve to its owning selector's
        // title so a field error can name where the problem lives. A
        // dropped arm would panic the title lookup or mislabel an error.
        for step in [
            QuickstartStep::ModelProvider,
            QuickstartStep::RiskProfile,
            QuickstartStep::RuntimeProfile,
            QuickstartStep::Memory,
            QuickstartStep::Channels,
            QuickstartStep::PeerGroups,
            QuickstartStep::Agent,
        ] {
            assert!(!Selector::title_for_step(step).is_empty());
        }
    }
}
