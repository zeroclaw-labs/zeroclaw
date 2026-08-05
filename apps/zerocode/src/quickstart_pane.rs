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

const UNSET_DISPLAY: &str = "<unset>";
const MODEL_CATALOG_MAX_ATTEMPTS: u8 = 2;
const MODEL_PROVIDER_EXISTING_COLLAPSED_LIMIT: usize = 5;

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
    AppliedAgent, QuickstartApplyResult, QuickstartError, QuickstartFieldDescriptor,
    QuickstartFieldSection, QuickstartStateResult, QuickstartStep, QuickstartSurface, RpcClient,
};
use crate::theme;
use crate::widgets::HelpNode;
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

    fn from_step(step: QuickstartStep) -> Option<Self> {
        match step {
            QuickstartStep::ModelProvider => Some(Selector::ModelProvider),
            QuickstartStep::RiskProfile => Some(Selector::RiskProfile),
            QuickstartStep::RuntimeProfile => Some(Selector::RuntimeProfile),
            QuickstartStep::Memory => Some(Selector::Memory),
            QuickstartStep::Channels => Some(Selector::Channels),
            QuickstartStep::PeerGroups => Some(Selector::PeerGroups),
            QuickstartStep::Agent => Some(Selector::Agent),
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

fn first_incomplete_required_selector(form: &FormState) -> Option<Selector> {
    Selector::ALL.iter().copied().find(|sel| {
        !matches!(
            sel,
            Selector::Submit | Selector::Channels | Selector::PeerGroups
        ) && !form.is_satisfied(*sel)
    })
}

fn incomplete_submit_error(form: &FormState) -> Option<QuickstartError> {
    let sel = first_incomplete_required_selector(form)?;
    let message_key = match sel {
        Selector::ModelProvider => "zc-quickstart-missing-model-provider",
        Selector::RiskProfile => "zc-quickstart-missing-risk-profile",
        Selector::RuntimeProfile => "zc-quickstart-missing-runtime-profile",
        Selector::Memory => "zc-quickstart-missing-memory",
        Selector::Agent => "zc-quickstart-missing-agent",
        Selector::Channels | Selector::PeerGroups | Selector::Submit => return None,
    };
    Some(QuickstartError {
        step: sel.step(),
        field: String::new(),
        message: crate::i18n::t(message_key),
    })
}

fn errors_include_selector(errors: &[QuickstartError], sel: Selector) -> bool {
    let step = sel.step();
    errors.iter().any(|e| e.step == step)
}

fn next_selector_index_after(sel: Selector, form: &FormState) -> Option<usize> {
    let current = Selector::ALL.iter().position(|s| *s == sel)?;
    let next = (current + 1).min(Selector::ALL.len().saturating_sub(1));

    Selector::ALL
        .iter()
        .enumerate()
        .skip(next)
        .find_map(|(idx, candidate)| {
            if selector_needs_attention(*candidate, form) {
                Some(idx)
            } else {
                None
            }
        })
        .or(Some(next))
}

fn selector_needs_attention(sel: Selector, form: &FormState) -> bool {
    match sel {
        Selector::Submit => form.all_selectors_satisfied(),
        _ => !form.is_satisfied(sel),
    }
}

fn apply_model_catalog_result(form: &mut FieldFormModal, models: Option<Vec<String>>) {
    form.model_catalog_attempts = form.model_catalog_attempts.saturating_add(1);
    match models {
        Some(models) => {
            apply_model_catalog_to_rows(&mut form.fields, Some(&models));
            form.model_catalog_state = ModelCatalogState::Loaded;
        }
        None if form.model_catalog_attempts < MODEL_CATALOG_MAX_ATTEMPTS => {
            form.model_catalog_state = ModelCatalogState::Retrying;
        }
        None => {
            form.model_catalog_state = ModelCatalogState::Empty;
        }
    }
}

fn opt(value: &str, label: impl Into<String>, help: impl Into<String>) -> PickerOption {
    PickerOption {
        value: value.to_string(),
        label: label.into(),
        help: help.into(),
        kind: PickerOptionKind::Choice {
            use_existing: false,
        },
    }
}

fn existing_opt(alias: String) -> PickerOption {
    PickerOption {
        label: format!("Use existing: {alias}"),
        value: alias,
        help: crate::i18n::t("zc-quickstart-reuse-alias-help"),
        kind: PickerOptionKind::Choice { use_existing: true },
    }
}

fn existing_provider_opt(alias: String) -> PickerOption {
    PickerOption {
        label: alias.clone(),
        value: alias,
        help: crate::i18n::t("zc-quickstart-reuse-alias-help"),
        kind: PickerOptionKind::Choice { use_existing: true },
    }
}

fn header_opt(label: impl Into<String>) -> PickerOption {
    PickerOption {
        value: String::new(),
        label: label.into(),
        help: String::new(),
        kind: PickerOptionKind::Header,
    }
}

fn existing_provider_toggle_opt(hidden_count: usize, expanded: bool) -> PickerOption {
    let label = if expanded {
        format!(
            "  {}",
            crate::i18n::t("zc-quickstart-existing-providers-show-fewer")
        )
    } else {
        let count = hidden_count.to_string();
        format!(
            "  {}",
            crate::i18n::t_args(
                "zc-quickstart-existing-providers-show-more",
                &[("count", count.as_str())],
            )
        )
    };
    PickerOption {
        value: String::new(),
        label,
        help: String::new(),
        kind: PickerOptionKind::ExistingProviderToggle { expanded },
    }
}

fn in_rect(col: u16, row: u16, r: Rect) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn synth_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE) // keyguard: bridges a mouse click to the canonical submit key for replay
}

fn queue_apply_handoff(
    reconnect_state: &crate::app::SharedReconnectState,
    alias: String,
    daemon_restarted: bool,
) -> Option<String> {
    let Ok(mut guard) = reconnect_state.lock() else {
        return None;
    };
    if daemon_restarted {
        guard.pending_quickstart_chat = Some(crate::app::PendingQuickstartChat::AfterReconnect(
            alias.clone(),
        ));
        Some(alias)
    } else {
        guard.pending_quickstart_chat = Some(crate::app::PendingQuickstartChat::Immediate(alias));
        None
    }
}

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

fn runtime_picker_options(snapshot: Option<&QuickstartStateResult>) -> Vec<PickerOption> {
    snapshot
        .into_iter()
        .flat_map(|snapshot| &snapshot.runtime_presets)
        .map(|preset| {
            let localized = match preset.preset_name.as_str() {
                "tight" => Some((
                    "zc-quickstart-runtime-tight",
                    "zc-quickstart-runtime-tight-desc",
                )),
                "local_small" => Some((
                    "zc-quickstart-runtime-local-small",
                    "zc-quickstart-runtime-local-small-desc",
                )),
                "balanced" => Some((
                    "zc-quickstart-runtime-balanced",
                    "zc-quickstart-runtime-balanced-desc",
                )),
                "unbounded" => Some((
                    "zc-quickstart-runtime-unbounded",
                    "zc-quickstart-runtime-unbounded-desc",
                )),
                _ => None,
            };
            let (label, help) = localized
                .map(|(label_key, help_key)| {
                    (
                        crate::i18n::try_t(label_key).unwrap_or_else(|| preset.label.clone()),
                        crate::i18n::try_t(help_key).unwrap_or_else(|| preset.help.clone()),
                    )
                })
                .unwrap_or_else(|| (preset.label.clone(), preset.help.clone()));
            opt(&preset.preset_name, label, help)
        })
        .collect()
}

fn memory_options() -> Vec<PickerOption> {
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
            opt(&wire, wire.clone(), String::new())
        })
        .collect()
}

fn provider_type_options(snapshot: Option<&QuickstartStateResult>) -> Vec<PickerOption> {
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
            kind: PickerOptionKind::Choice {
                use_existing: false,
            },
        })
        .collect()
}

fn ordered_existing_provider_refs(existing: &[String]) -> Vec<String> {
    let mut refs: Vec<String> = existing.to_vec();
    refs.sort_by(|a, b| {
        let rank = |value: &str| {
            if value == "openrouter.default" {
                0
            } else if value.ends_with(".default") {
                1
            } else {
                2
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    refs
}

fn model_provider_picker_options(
    snapshot: Option<&QuickstartStateResult>,
    existing_expanded: bool,
) -> Vec<PickerOption> {
    let Some(snap) = snapshot else {
        return Vec::new();
    };

    let mut options = Vec::new();
    let existing = ordered_existing_provider_refs(&snap.model_providers);
    if !existing.is_empty() {
        options.push(header_opt(crate::i18n::t(
            "zc-quickstart-existing-providers-heading",
        )));
        let visible_count = if existing_expanded {
            existing.len()
        } else {
            existing.len().min(MODEL_PROVIDER_EXISTING_COLLAPSED_LIMIT)
        };
        for alias in existing.iter().take(visible_count) {
            options.push(existing_provider_opt(alias.clone()));
        }
        if existing.len() > MODEL_PROVIDER_EXISTING_COLLAPSED_LIMIT {
            options.push(existing_provider_toggle_opt(
                existing.len() - MODEL_PROVIDER_EXISTING_COLLAPSED_LIMIT,
                existing_expanded,
            ));
        }
    }

    options.push(header_opt(crate::i18n::t(
        "zc-quickstart-new-provider-heading",
    )));
    options.extend(provider_type_options(snapshot));
    options
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
            kind: PickerOptionKind::Choice {
                use_existing: false,
            },
        })
        .collect()
}

#[derive(Debug, Clone)]
struct ChannelDraft {
    channel_type: String,
    alias: String,
    fields: std::collections::HashMap<String, String>,
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
    runtime_auto_defaulted: bool,
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
            provider_fields: std::collections::HashMap::new(),
            risk: String::new(),
            risk_mode: SelectorMode::Fresh,
            runtime: String::new(),
            runtime_mode: SelectorMode::Fresh,
            runtime_auto_defaulted: false,
            memory: MemoryKind::Sqlite,
            memory_mode: SelectorMode::Fresh,
            memory_chosen: false,
            memory_existing_alias: String::new(),
            channels: Vec::new(),
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
            // Optional rows should not block Submit when left empty,
            // but they also should not render as completed unless the
            // user actually configured something there.
            Selector::Channels => !self.channels.is_empty(),
            Selector::PeerGroups => !self.peer_groups.is_empty(),
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
            .filter(|s| {
                !matches!(
                    s,
                    Selector::Submit | Selector::Channels | Selector::PeerGroups
                )
            })
            .all(|s| self.is_satisfied(*s))
    }

    fn selector_is_complete(&self, sel: Selector, errors: &[QuickstartError]) -> bool {
        self.is_satisfied(sel) && !errors_include_selector(errors, sel)
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
        let runtime_profile = self.runtime_profile_choice();
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
                        fields: c.fields.clone(),
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

    fn runtime_profile_choice(&self) -> SelectorChoice<String> {
        match self.runtime_mode {
            SelectorMode::Fresh => SelectorChoice::Fresh(self.runtime.clone()),
            SelectorMode::Existing => SelectorChoice::Existing(self.runtime.clone()),
        }
    }

    fn apply_provider_runtime_default(&mut self, default: Option<&str>) {
        let Some(next) = default else { return };
        if self.runtime.is_empty() || self.runtime_auto_defaulted {
            self.runtime = next.to_string();
            self.runtime_mode = SelectorMode::Fresh;
            self.runtime_auto_defaulted = true;
        }
    }
}

fn provider_runtime_default<'a>(
    snapshot: Option<&'a QuickstartStateResult>,
    type_key: &str,
) -> Option<&'a str> {
    let snapshot = snapshot?;
    snapshot
        .model_provider_types
        .iter()
        .find(|provider| provider.kind == type_key)
        .and_then(|provider| provider.default_runtime_profile.as_deref())
        .or(snapshot.default_runtime_profile.as_deref())
}

fn apply_existing_provider_choice(
    form: &mut FormState,
    snapshot: Option<&QuickstartStateResult>,
    dotted_ref: &str,
) {
    let Some((provider_type, alias)) = dotted_ref.split_once('.') else {
        return;
    };
    form.provider_type = provider_type.to_string();
    form.provider_alias = alias.to_string();
    form.provider_mode = SelectorMode::Existing;
    form.model.clear();
    form.provider_fields.clear();
    form.apply_provider_runtime_default(provider_runtime_default(snapshot, provider_type));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelCatalogState {
    /// Section has no model row (channels) — nothing to load.
    NotApplicable,
    /// Catalog fetch not yet started or in flight.
    Pending,
    /// Catalog fetch failed once and is retrying before falling back.
    Retrying,
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
    model_catalog_state: ModelCatalogState,
    model_catalog_attempts: u8,
    fields: Vec<FieldFormRow>,
    cursor: usize,
}

struct ModelCatalogFetchResult {
    type_key: String,
    models: Option<Vec<String>>,
}

struct FieldFormRow {
    descriptor: QuickstartFieldDescriptor,
    /// User-typed buffer. Pre-filled from `descriptor.default`.
    buf: String,
}

fn channel_fields_from_rows(rows: &[FieldFormRow]) -> std::collections::HashMap<String, String> {
    rows.iter()
        .filter_map(|row| {
            let value = row.buf.trim();
            (!value.is_empty() && value != UNSET_DISPLAY)
                .then(|| (row.descriptor.key.clone(), value.to_string()))
        })
        .collect()
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
    /// In-pane file editor. Kept inside the Agent modal so Quickstart
    /// never has to leave raw/alternate-screen mode for `$EDITOR`.
    editor: Option<FileEditorState>,
}

struct FileEditorState {
    filename: String,
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

fn personality_file_needs_template(agent: &AgentModal, filename: &str) -> bool {
    agent
        .files
        .get(filename)
        .map(|content| content.trim().is_empty())
        .unwrap_or(true)
}

fn advance_agent_modal_after_file(agent: &mut AgentModal) {
    let row_count = agent.filenames.len() + 2;
    if agent.cursor + 1 < row_count {
        agent.cursor += 1;
    }
}

impl FileEditorState {
    fn new(filename: String, content: String) -> Self {
        let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            filename,
            lines,
            cursor_row: 0,
            cursor_col: 0,
        }
    }

    fn content(&self) -> String {
        self.lines.join("\n")
    }

    fn insert_text(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\r' => {}
                '\n' => self.insert_newline(),
                c => self.insert_char(c),
            }
        }
    }

    fn insert_char(&mut self, c: char) {
        self.ensure_cursor_in_bounds();
        let idx = byte_index_at_char(&self.lines[self.cursor_row], self.cursor_col);
        self.lines[self.cursor_row].insert(idx, c);
        self.cursor_col += 1;
    }

    fn insert_newline(&mut self) {
        self.ensure_cursor_in_bounds();
        let idx = byte_index_at_char(&self.lines[self.cursor_row], self.cursor_col);
        let tail = self.lines[self.cursor_row].split_off(idx);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_row, tail);
    }

    fn backspace(&mut self) {
        self.ensure_cursor_in_bounds();
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let end = byte_index_at_char(line, self.cursor_col);
            let start = byte_index_at_char(line, self.cursor_col - 1);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&current);
        }
    }

    fn move_left(&mut self) {
        self.ensure_cursor_in_bounds();
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
        }
    }

    fn move_right(&mut self) {
        self.ensure_cursor_in_bounds();
        if self.cursor_col < self.lines[self.cursor_row].chars().count() {
            self.cursor_col += 1;
        } else if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.cursor_col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cursor_row > 0 {
            self.cursor_row -= 1;
            self.clamp_cursor_col();
        }
    }

    fn move_down(&mut self) {
        if self.cursor_row + 1 < self.lines.len() {
            self.cursor_row += 1;
            self.clamp_cursor_col();
        }
    }

    fn scroll_lines(&mut self, delta: i32) {
        if self.lines.is_empty() {
            self.ensure_cursor_in_bounds();
            return;
        }
        let max_row = self.lines.len().saturating_sub(1) as i32;
        self.cursor_row = (self.cursor_row as i32 + delta).clamp(0, max_row) as usize;
        self.clamp_cursor_col();
    }

    fn ensure_cursor_in_bounds(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = self.cursor_row.min(self.lines.len().saturating_sub(1));
        self.clamp_cursor_col();
    }

    fn clamp_cursor_col(&mut self) {
        self.cursor_col = self
            .cursor_col
            .min(self.lines[self.cursor_row].chars().count());
    }
}

fn byte_index_at_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerOptionKind {
    Choice { use_existing: bool },
    Header,
    ExistingProviderToggle { expanded: bool },
}

#[derive(Clone)]
struct PickerOption {
    /// Wire-side value written back into [`FormState`].
    value: String,
    /// Display label.
    label: String,
    /// One-line help / blurb.
    help: String,
    kind: PickerOptionKind,
}

impl PickerOption {
    fn is_selectable(&self) -> bool {
        !matches!(self.kind, PickerOptionKind::Header)
    }

    fn use_existing(&self) -> bool {
        matches!(self.kind, PickerOptionKind::Choice { use_existing: true })
    }

    fn existing_provider_toggle_expanded(&self) -> Option<bool> {
        match self.kind {
            PickerOptionKind::ExistingProviderToggle { expanded } => Some(expanded),
            _ => None,
        }
    }
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
    /// Source of truth for an in-flight model-catalog fetch. The
    /// fetched model list itself is not cached here; successful results
    /// are applied into the model field descriptor so the picker has one
    /// canonical source.
    model_catalog_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ModelCatalogFetchResult>>,
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
    leave_requested: bool,
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
            model_catalog_rx: None,
            modal_rect: None,
            modal_row_rects: Vec::new(),
            selector_list_rect: None,
            selector_row_rects: Vec::new(),
            leave_requested: false,
        }
    }

    pub fn take_leave_request(&mut self) -> bool {
        std::mem::take(&mut self.leave_requested)
    }

    pub async fn init(&mut self) -> anyhow::Result<()> {
        if let Ok(s) = self.rpc.quickstart_state().await {
            self.state_snapshot = Some(s);
        }
        Ok(())
    }

    pub fn help_context(&self) -> HelpNode {
        use crate::keymap::QuickstartTabAction as Q;
        HelpNode::entries(crate::help::entries_for([
            Q::Up,
            Q::Down,
            Q::Enter,
            Q::Create,
            Q::Back,
        ]))
    }

    pub fn wants_text_input(&self) -> bool {
        match self.active_modal.as_ref() {
            Some(Modal::TextInput(_)) => true,
            Some(Modal::FieldForm(f)) => f.fields.get(f.cursor).is_some_and(|row| {
                field_form_row_visible(f, f.cursor) && field_row_variants(row).is_none()
            }),
            Some(Modal::Agent(a)) => a.editor.is_some() || a.cursor == 0,
            _ => false,
        }
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
                        } else {
                            self.report_incomplete_submit();
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
                } else {
                    self.report_incomplete_submit();
                }
                false
            }
            Some(QuickstartTabAction::Back) => {
                self.leave_requested = true;
                false
            }
            _ => false,
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        let Some(modal) = self.active_modal.as_mut() else {
            return;
        };
        match modal {
            Modal::TextInput(t) => t.buf.push_str(text),
            Modal::FieldForm(f) => {
                if field_form_row_visible(f, f.cursor)
                    && let Some(row) = f.fields.get_mut(f.cursor)
                    && row.descriptor.enum_variants.is_none()
                {
                    row.buf.push_str(text);
                }
            }
            Modal::Agent(a) => {
                if let Some(editor) = a.editor.as_mut() {
                    editor.insert_text(text);
                } else if a.cursor == 0 {
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
                        if !self.modal_row_is_selectable(idx) {
                            return;
                        }
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
                            } else {
                                self.report_incomplete_submit();
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
        if let Modal::Agent(a) = modal
            && let Some(editor) = a.editor.as_mut()
        {
            editor.scroll_lines(delta);
            return;
        }
        if let Modal::Picker(p) = modal {
            p.cursor = wrapped_selectable_option_index(&p.options, p.cursor, delta);
            return;
        }
        if let Modal::FieldForm(f) = modal {
            if delta >= 0 {
                move_field_form_cursor(f, 1);
            } else {
                move_field_form_cursor(f, -1);
            }
            return;
        }
        let (cur, len) = match modal {
            Modal::ChannelList(cl) => (&mut cl.cursor, self.modal_row_rects.len()),
            Modal::PeerGroupList(pl) => (&mut pl.cursor, self.modal_row_rects.len()),
            Modal::Agent(a) => (&mut a.cursor, self.modal_row_rects.len()),
            Modal::Picker(_) | Modal::FieldForm(_) | Modal::TextInput(_) => return,
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
                if p.options.get(idx).is_some_and(PickerOption::is_selectable) {
                    p.cursor = idx;
                }
            }
            Modal::FieldForm(f) => {
                if idx < f.fields.len() {
                    f.cursor = idx;
                    normalize_field_form_cursor(f);
                }
            }
            Modal::ChannelList(cl) => {
                cl.cursor = idx;
            }
            Modal::PeerGroupList(pl) => {
                pl.cursor = idx;
            }
            Modal::Agent(a) => {
                if let Some(editor) = a.editor.as_mut() {
                    if idx < editor.lines.len() {
                        editor.cursor_row = idx;
                        editor.clamp_cursor_col();
                    }
                } else {
                    a.cursor = idx;
                }
            }
            Modal::TextInput(_) => {}
        }
    }

    fn modal_row_is_selectable(&self, idx: usize) -> bool {
        match self.active_modal.as_ref() {
            Some(Modal::Picker(p)) => p.options.get(idx).is_some_and(PickerOption::is_selectable),
            Some(Modal::TextInput(_)) | None => false,
            _ => true,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = Selector::ALL.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).rem_euclid(len);
        self.list_state.select(Some(next as usize));
    }

    fn advance_after_completed(&mut self, sel: Selector) {
        if let Some(next) = next_selector_index_after(sel, &self.form) {
            self.list_state.select(Some(next));
        }
    }

    fn keep_selector_selected(&mut self, sel: Selector) {
        if let Some(idx) = Selector::ALL.iter().position(|candidate| *candidate == sel) {
            self.list_state.select(Some(idx));
        }
    }

    fn advance_after_validated(&mut self, sel: Selector) {
        if errors_include_selector(&self.last_errors, sel) {
            self.keep_selector_selected(sel);
        } else {
            self.advance_after_completed(sel);
        }
    }

    fn report_incomplete_submit(&mut self) {
        if let Some(error) = incomplete_submit_error(&self.form) {
            if let Some(sel) = Selector::from_step(error.step) {
                self.keep_selector_selected(sel);
            }
            self.last_errors = vec![error];
        }
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
                    editor: None,
                }));
            }
            Selector::ModelProvider => {
                let options = model_provider_picker_options(self.state_snapshot.as_ref(), false);
                let cursor = current_provider_picker_cursor(&options, &self.form);
                self.active_modal = Some(Modal::Picker(PickerModal {
                    selector: sel,
                    purpose: PickerPurpose::ProviderType,
                    options,
                    cursor,
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
            Selector::RuntimeProfile => runtime_picker_options(self.state_snapshot.as_ref()),
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
        if options.is_empty() {
            return;
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
        if let Modal::FieldForm(form) = modal {
            normalize_field_form_cursor(form);
        }
        match modal {
            Modal::Picker(p) => match action {
                Some(QuickstartModalAction::Cancel) => {
                    self.active_modal = None;
                }
                Some(QuickstartModalAction::Up) => {
                    if let Some(next) = adjacent_selectable_option_index(&p.options, p.cursor, -1) {
                        p.cursor = next;
                    }
                }
                Some(QuickstartModalAction::Down) => {
                    if let Some(next) = adjacent_selectable_option_index(&p.options, p.cursor, 1) {
                        p.cursor = next;
                    }
                }
                Some(QuickstartModalAction::Confirm) => {
                    let Some(option) = p.options.get(p.cursor) else {
                        return;
                    };
                    if let Some(expanded) = option.existing_provider_toggle_expanded() {
                        p.options =
                            model_provider_picker_options(self.state_snapshot.as_ref(), !expanded);
                        p.cursor = p
                            .options
                            .iter()
                            .position(|option| {
                                option.existing_provider_toggle_expanded() == Some(!expanded)
                            })
                            .unwrap_or_else(|| {
                                nearest_selectable_option_index(&p.options, p.cursor)
                            });
                        return;
                    }
                    let Some(option) = p.options.get(p.cursor) else {
                        return;
                    };
                    if !option.is_selectable() {
                        return;
                    }
                    let chosen = option.value.clone();
                    let use_existing = option.use_existing();
                    let selector = p.selector;
                    let purpose = p.purpose;
                    match (purpose, use_existing) {
                        (PickerPurpose::DirectChoice, _) => {
                            self.apply_picker_choice(selector, chosen, use_existing);
                            self.active_modal = None;
                            self.revalidate().await;
                            self.advance_after_validated(selector);
                        }
                        (PickerPurpose::ProviderType, true) => {
                            self.adopt_existing_provider(chosen);
                            self.active_modal = None;
                            self.revalidate().await;
                            self.advance_after_validated(selector);
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
                            self.advance_after_validated(selector);
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
                    if let Some(error) =
                        current_field_form_duplicate_error(self.state_snapshot.as_ref(), f)
                    {
                        self.last_errors = vec![error];
                        return;
                    }
                    if current_field_form_row_is_model_provider_alias(f) {
                        self.last_errors.retain(|e| {
                            e.step != QuickstartStep::ModelProvider || e.field != "alias"
                        });
                    }
                    move_field_form_cursor(f, 1);
                }
                Some(QuickstartModalAction::PrevField) | Some(QuickstartModalAction::Up) => {
                    move_field_form_cursor(f, -1);
                }
                Some(QuickstartModalAction::Confirm) => {
                    if !field_form_cursor_is_last_visible(f) {
                        if let Some(error) =
                            current_field_form_duplicate_error(self.state_snapshot.as_ref(), f)
                        {
                            self.last_errors = vec![error];
                            return;
                        }
                        if current_field_form_row_is_model_provider_alias(f) {
                            self.last_errors.retain(|e| {
                                e.step != QuickstartStep::ModelProvider || e.field != "alias"
                            });
                        }
                        move_field_form_cursor(f, 1);
                        return;
                    }
                    let selector = f.selector;
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
                    if !from_channel {
                        self.advance_after_validated(selector);
                    }
                }
                Some(QuickstartModalAction::Left) => {
                    let variants = f
                        .fields
                        .get(f.cursor)
                        .and_then(field_row_variants)
                        .map(|v| v.to_vec());
                    if let (Some(row), Some(variants)) = (f.fields.get_mut(f.cursor), variants)
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
                    let variants = f
                        .fields
                        .get(f.cursor)
                        .and_then(field_row_variants)
                        .map(|v| v.to_vec());
                    if let (Some(row), Some(variants)) = (f.fields.get_mut(f.cursor), variants)
                        && !variants.is_empty()
                    {
                        let cur = variants.iter().position(|v| v == &row.buf).unwrap_or(0);
                        let next = (cur + 1) % variants.len();
                        row.buf = variants[next].clone();
                    }
                }
                Some(QuickstartModalAction::Backspace) => {
                    let is_enum = f
                        .fields
                        .get(f.cursor)
                        .and_then(field_row_variants)
                        .is_some();
                    if let Some(row) = f.fields.get_mut(f.cursor)
                        && !is_enum
                    {
                        row.buf.pop();
                    }
                }
                _ => {
                    let is_enum = f
                        .fields
                        .get(f.cursor)
                        .and_then(field_row_variants)
                        .is_some();
                    if let KeyCode::Char(c) = key.code
                        && !key.modifiers.contains(KeyModifiers::CONTROL)
                        && let Some(row) = f.fields.get_mut(f.cursor)
                        && !is_enum
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
                            self.active_modal = None;
                            self.revalidate().await;
                            self.advance_after_validated(Selector::Channels);
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
                            self.active_modal = None;
                            self.revalidate().await;
                            self.advance_after_validated(Selector::PeerGroups);
                        }
                    }
                    _ => {}
                }
            }
            Modal::Agent(a) => {
                if let Some(editor) = a.editor.as_mut() {
                    match action {
                        Some(QuickstartModalAction::Save) => {
                            let filename = editor.filename.clone();
                            let content = editor.content();
                            a.files.insert(filename, content);
                            a.editor = None;
                        }
                        Some(QuickstartModalAction::Cancel) => {
                            a.editor = None;
                        }
                        Some(QuickstartModalAction::Backspace) => editor.backspace(),
                        Some(QuickstartModalAction::Confirm) => editor.insert_newline(),
                        Some(QuickstartModalAction::Up) => editor.move_up(),
                        Some(QuickstartModalAction::Down) => editor.move_down(),
                        Some(QuickstartModalAction::Left) => editor.move_left(),
                        Some(QuickstartModalAction::Right) => editor.move_right(),
                        _ => {
                            if let Some(c) = typed_char(&key) {
                                editor.insert_char(c);
                            }
                        }
                    }
                    return;
                }
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
                        self.advance_after_validated(Selector::Agent);
                    }
                    Some(QuickstartModalAction::Confirm) if on_file => {
                        let filename = a.filenames[a.cursor - 1].clone();
                        if !personality_file_needs_template(a, &filename) {
                            advance_agent_modal_after_file(a);
                            return;
                        }
                        let agent_name = a.name.trim().to_string();
                        let templated = self
                            .fetch_personality_template(&filename, Some(agent_name.as_str()))
                            .await;
                        match templated {
                            Some(content) => {
                                if let Some(Modal::Agent(a)) = self.active_modal.as_mut() {
                                    a.files.insert(filename, content);
                                    advance_agent_modal_after_file(a);
                                }
                                self.last_errors.clear();
                            }
                            None => {
                                self.last_errors = vec![missing_template_error(&filename)];
                            }
                        }
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
                        a.editor = Some(FileEditorState::new(filename, seed));
                    }
                    Some(QuickstartModalAction::EditTemplate) if on_file => {
                        let filename = a.filenames[a.cursor - 1].clone();
                        let agent_name = a.name.trim().to_string();
                        let templated = self
                            .fetch_personality_template(&filename, Some(agent_name.as_str()))
                            .await;
                        match templated {
                            Some(content) => {
                                if let Some(Modal::Agent(a)) = self.active_modal.as_mut() {
                                    a.files.insert(filename, content);
                                }
                                self.last_errors.clear();
                            }
                            None => {
                                self.last_errors = vec![missing_template_error(&filename)];
                            }
                        }
                    }
                    Some(QuickstartModalAction::ClearFile) if on_file => {
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

    async fn fetch_personality_template(
        &self,
        filename: &str,
        agent: Option<&str>,
    ) -> Option<String> {
        let res = self.rpc.personality_templates(agent).await.ok()?;
        res.files
            .into_iter()
            .find(|f| f.filename == filename)
            .map(|f| f.content)
    }

    fn adopt_existing_provider(&mut self, dotted_ref: String) {
        apply_existing_provider_choice(&mut self.form, self.state_snapshot.as_ref(), &dotted_ref);
    }

    fn adopt_existing_channel(&mut self, dotted_ref: String) {
        if let Some((ty, alias)) = dotted_ref.split_once('.') {
            self.form.channels.push(ChannelDraft {
                channel_type: ty.to_string(),
                alias: alias.to_string(),
                fields: std::collections::HashMap::new(),
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
            .map(|r| opt(&r, r.clone(), String::new()))
            .collect()
    }

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
        let is_model_provider = matches!(section, QuickstartFieldSection::ModelProvider);
        let mut rows = build_field_form_rows(section, fields, None);
        restore_field_form_rows_from_form(section, &type_key, &mut rows, &self.form);
        let model_catalog_state = if is_model_provider {
            ModelCatalogState::Pending
        } else {
            ModelCatalogState::NotApplicable
        };
        let alias = match section {
            QuickstartFieldSection::ModelProvider => "default".to_string(),
            _ => type_key.clone(),
        };
        self.active_modal = Some(Modal::FieldForm(FieldFormModal {
            selector: sel,
            type_key,
            alias,
            model_catalog_state,
            model_catalog_attempts: 0,
            fields: rows,
            cursor: 0,
        }));
        self.model_catalog_rx = None;
    }

    pub async fn tick(&mut self) {
        let mut clear_rx = false;
        let mut fetched = None;
        if let Some(rx) = self.model_catalog_rx.as_mut() {
            match rx.try_recv() {
                Ok(result) => {
                    fetched = Some(result);
                    clear_rx = true;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    clear_rx = true;
                }
            }
        }
        if clear_rx {
            self.model_catalog_rx = None;
        }
        if let Some(result) = fetched
            && let Some(Modal::FieldForm(form)) = self.active_modal.as_mut()
            && form.type_key == result.type_key
            && matches!(
                form.model_catalog_state,
                ModelCatalogState::Pending | ModelCatalogState::Retrying
            )
        {
            apply_model_catalog_result(form, result.models);
        }

        let pending_type = match self.active_modal.as_ref() {
            Some(Modal::FieldForm(form))
                if matches!(
                    form.model_catalog_state,
                    ModelCatalogState::Pending | ModelCatalogState::Retrying
                ) =>
            {
                Some(form.type_key.clone())
            }
            _ => None,
        };
        let Some(type_key) = pending_type else {
            return;
        };
        if self.model_catalog_rx.is_some() {
            return;
        }

        let rpc = Arc::clone(&self.rpc);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.model_catalog_rx = Some(rx);
        tokio::spawn(async move {
            let models = match rpc.catalog_models(&type_key).await {
                Ok(res) if res.live && !res.models.is_empty() => Some(res.models),
                _ => None,
            };
            let _ = tx.send(ModelCatalogFetchResult { type_key, models });
        });
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
            .enumerate()
            .filter(|(index, r)| {
                field_form_row_visible(f, *index)
                    && r.descriptor.required
                    && r.buf.trim().is_empty()
            })
            .map(|(_, r)| r.descriptor.key.as_str())
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
        if let Some(error) = model_provider_alias_duplicate_error(self.state_snapshot.as_ref(), f) {
            let alias_idx = f
                .fields
                .iter()
                .position(|row| row.descriptor.key == "alias")
                .unwrap_or(0);
            self.last_errors = vec![error];
            if let Some(Modal::FieldForm(form)) = self.active_modal.as_mut() {
                form.cursor = alias_idx;
            }
            return false;
        }
        match f.selector {
            Selector::ModelProvider => {
                let runtime_default =
                    provider_runtime_default(self.state_snapshot.as_ref(), &f.type_key)
                        .map(str::to_string);
                let pick = |key: &str| {
                    f.fields
                        .iter()
                        .find(|r| r.descriptor.key == key)
                        .map(|r| r.buf.trim().to_string())
                        .unwrap_or_default()
                };
                let mut provider_fields: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                for (index, row) in f.fields.iter().enumerate() {
                    if !field_form_row_visible(f, index) {
                        continue;
                    }
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
                self.form
                    .apply_provider_runtime_default(runtime_default.as_deref());
            }
            Selector::Channels => {
                self.form.channels.push(ChannelDraft {
                    channel_type: f.type_key.clone(),
                    alias: f.alias.clone(),
                    fields: channel_fields_from_rows(&f.fields),
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
                self.form.runtime_auto_defaulted = false;
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
        self.form.all_selectors_satisfied() && self.last_errors.is_empty() && !self.busy
    }

    async fn submit(&mut self) {
        self.busy = true;
        self.last_errors.clear();
        let submission = self.form.to_submission();
        match self.rpc.quickstart_apply(&submission).await {
            Ok(QuickstartApplyResult::Applied {
                agent,
                daemon_restarted,
            }) => {
                self.handle_apply_success(agent, daemon_restarted);
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

    fn handle_apply_success(&mut self, agent: AppliedAgent, daemon_restarted: bool) {
        // A real daemon reload must survive the old connection closing:
        // use the immediate handoff only when the daemon reports that no
        // reconnect is needed.
        self.applied_alias =
            queue_apply_handoff(&self.reconnect_state, agent.alias, daemon_restarted);
        self.last_errors.clear();
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
                let satisfied = self.form.selector_is_complete(*sel, &self.last_errors);
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
        let style = if self.applied_alias.is_some() || can_create {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else if !self.last_errors.is_empty() {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
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

fn wrapped_row_heights(lines: &[Line], width: u16) -> Vec<u16> {
    lines
        .iter()
        .map(|line| {
            Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1) as u16
        })
        .collect()
}

/// Total wrapped rows a block of lines occupies at `width`.
fn wrapped_total(lines: &[Line], width: u16) -> u16 {
    wrapped_row_heights(lines, width).iter().copied().sum()
}

fn apply_model_catalog_to_rows(rows: &mut [FieldFormRow], model_catalog: Option<&[String]>) {
    let Some(models) = model_catalog else {
        return;
    };
    if models.is_empty() {
        return;
    }
    for row in rows {
        if is_model_field(&row.descriptor) {
            row.descriptor.kind = crate::client::QuickstartFieldKind::Enum;
            row.descriptor.enum_variants = Some(models.to_vec());
            if !models.contains(&row.buf) {
                row.buf = models[0].clone();
            }
        }
    }
}

fn first_selectable_option_index(options: &[PickerOption]) -> usize {
    options
        .iter()
        .position(PickerOption::is_selectable)
        .unwrap_or(0)
}

fn nearest_selectable_option_index(options: &[PickerOption], preferred: usize) -> usize {
    if options
        .get(preferred)
        .is_some_and(PickerOption::is_selectable)
    {
        return preferred;
    }
    options
        .iter()
        .enumerate()
        .skip(preferred.saturating_add(1))
        .find(|(_, option)| option.is_selectable())
        .map(|(idx, _)| idx)
        .or_else(|| {
            options
                .iter()
                .enumerate()
                .take(preferred)
                .rev()
                .find(|(_, option)| option.is_selectable())
                .map(|(idx, _)| idx)
        })
        .unwrap_or(0)
}

fn adjacent_selectable_option_index(
    options: &[PickerOption],
    current: usize,
    delta: i32,
) -> Option<usize> {
    if delta < 0 {
        options
            .iter()
            .enumerate()
            .take(current)
            .rev()
            .find(|(_, option)| option.is_selectable())
            .map(|(idx, _)| idx)
    } else {
        options
            .iter()
            .enumerate()
            .skip(current.saturating_add(1))
            .find(|(_, option)| option.is_selectable())
            .map(|(idx, _)| idx)
    }
}

fn wrapped_selectable_option_index(options: &[PickerOption], current: usize, delta: i32) -> usize {
    if options.is_empty() {
        return 0;
    }
    let mut next = current.min(options.len().saturating_sub(1));
    for _ in 0..options.len() {
        next = (next as i32 + delta).rem_euclid(options.len() as i32) as usize;
        if options[next].is_selectable() {
            return next;
        }
    }
    current.min(options.len().saturating_sub(1))
}

fn current_provider_picker_cursor(options: &[PickerOption], form: &FormState) -> usize {
    if form.provider_type.is_empty() {
        return first_selectable_option_index(options);
    }

    let target = if form.provider_mode == SelectorMode::Existing && !form.provider_alias.is_empty()
    {
        format!("{}.{}", form.provider_type, form.provider_alias)
    } else {
        form.provider_type.clone()
    };

    options
        .iter()
        .position(|o| o.value == target && o.is_selectable())
        .unwrap_or_else(|| first_selectable_option_index(options))
}

fn model_provider_alias_value(form: &FieldFormModal) -> String {
    form.fields
        .iter()
        .find(|row| row.descriptor.key == "alias")
        .map(|row| row.buf.trim().to_string())
        .filter(|alias| !alias.is_empty())
        .unwrap_or_else(|| form.alias.clone())
}

fn model_provider_alias_duplicate_error(
    snapshot: Option<&QuickstartStateResult>,
    form: &FieldFormModal,
) -> Option<QuickstartError> {
    if form.selector != Selector::ModelProvider {
        return None;
    }
    let alias = model_provider_alias_value(form);
    if alias.trim().is_empty() {
        return None;
    }
    let dotted = format!("{}.{}", form.type_key, alias.trim());
    let exists = snapshot?
        .model_providers
        .iter()
        .any(|existing| existing == &dotted);
    if !exists {
        return None;
    }
    Some(QuickstartError {
        step: QuickstartStep::ModelProvider,
        field: "alias".into(),
        message: crate::i18n::t_args(
            "zc-quickstart-provider-alias-exists",
            &[("alias", dotted.as_str())],
        ),
    })
}

fn current_field_form_duplicate_error(
    snapshot: Option<&QuickstartStateResult>,
    form: &FieldFormModal,
) -> Option<QuickstartError> {
    let row = form.fields.get(form.cursor)?;
    if row.descriptor.key != "alias" {
        return None;
    }
    model_provider_alias_duplicate_error(snapshot, form)
}

fn current_field_form_row_is_model_provider_alias(form: &FieldFormModal) -> bool {
    form.selector == Selector::ModelProvider
        && form
            .fields
            .get(form.cursor)
            .is_some_and(|row| row.descriptor.key == "alias")
}

fn restore_field_form_rows_from_form(
    section: QuickstartFieldSection,
    type_key: &str,
    rows: &mut [FieldFormRow],
    form: &FormState,
) {
    if !matches!(section, QuickstartFieldSection::ModelProvider)
        || form.provider_type != type_key
        || form.provider_mode != SelectorMode::Fresh
    {
        return;
    }

    for row in rows {
        match row.descriptor.key.as_str() {
            "alias" if !form.provider_alias.is_empty() => {
                row.buf = form.provider_alias.clone();
            }
            "model" if !form.model.is_empty() => {
                row.buf = form.model.clone();
            }
            key => {
                if let Some(value) = form.provider_fields.get(key) {
                    row.buf = value.clone();
                }
            }
        }
    }
}

fn is_model_field(field: &QuickstartFieldDescriptor) -> bool {
    field.key.eq_ignore_ascii_case("model") || field.label.eq_ignore_ascii_case("model")
}

fn build_field_form_rows(
    section: QuickstartFieldSection,
    fields: Vec<QuickstartFieldDescriptor>,
    model_catalog: Option<&[String]>,
) -> Vec<FieldFormRow> {
    let mut rows: Vec<FieldFormRow> = fields
        .into_iter()
        .map(|mut d| {
            if matches!(d.kind, crate::client::QuickstartFieldKind::Bool) {
                d.enum_variants = Some(vec!["false".to_string(), "true".to_string()]);
            }
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
    // Prepend an editable alias row for ModelProvider so users can
    // choose a custom alias instead of the hardcoded "default".
    if matches!(section, QuickstartFieldSection::ModelProvider) {
        rows.insert(0, model_provider_alias_row());
    }
    apply_model_catalog_to_rows(&mut rows, model_catalog);
    rows
}

fn model_provider_alias_row() -> FieldFormRow {
    let default_alias = "default".to_string();
    FieldFormRow {
        descriptor: QuickstartFieldDescriptor {
            key: "alias".to_string(),
            label: crate::i18n::t("zc-quickstart-field-label-alias"),
            help: crate::i18n::t("zc-quickstart-field-help-alias"),
            kind: crate::client::QuickstartFieldKind::String,
            is_secret: false,
            enum_variants: None,
            required: true,
            // The edit buffer starts as `default`, but this synthetic
            // row must not also use `default` as ghost text. Otherwise
            // Backspace clears the buffer and the same word immediately
            // reappears as non-editable placeholder text.
            default: None,
        },
        buf: default_alias,
    }
}

fn field_row_variants(row: &FieldFormRow) -> Option<&[String]> {
    if let Some(variants) = row.descriptor.enum_variants.as_deref()
        && !variants.is_empty()
    {
        return Some(variants);
    }
    None
}

fn field_form_uses_openai_codex_auth(form: &FieldFormModal) -> bool {
    matches!(form.selector, Selector::ModelProvider)
        && form.type_key.trim().eq_ignore_ascii_case("openai")
        && form.fields.iter().any(|row| {
            row.descriptor.key == "auth_mode" && row.buf.trim().eq_ignore_ascii_case("codex")
        })
}

fn field_form_row_visible(form: &FieldFormModal, index: usize) -> bool {
    let Some(row) = form.fields.get(index) else {
        return false;
    };
    !(field_form_uses_openai_codex_auth(form) && row.descriptor.key == "api_key")
}

fn visible_field_form_indices(
    form: &FieldFormModal,
) -> impl DoubleEndedIterator<Item = usize> + '_ {
    (0..form.fields.len()).filter(|index| field_form_row_visible(form, *index))
}

fn normalize_field_form_cursor(form: &mut FieldFormModal) {
    if field_form_row_visible(form, form.cursor) {
        return;
    }
    let first_visible = visible_field_form_indices(form).next();
    if let Some(index) = first_visible {
        form.cursor = index;
    }
}

fn move_field_form_cursor(form: &mut FieldFormModal, delta: i32) {
    let visible: Vec<usize> = visible_field_form_indices(form).collect();
    if visible.is_empty() {
        return;
    }
    let current_pos = visible
        .iter()
        .position(|index| *index == form.cursor)
        .unwrap_or(0);
    let next_pos = (current_pos as i32 + delta).rem_euclid(visible.len() as i32);
    form.cursor = visible[next_pos as usize];
}

fn field_form_cursor_is_last_visible(form: &FieldFormModal) -> bool {
    visible_field_form_indices(form)
        .next_back()
        .is_none_or(|index| index == form.cursor)
}

fn missing_template_error(filename: &str) -> QuickstartError {
    QuickstartError {
        step: QuickstartStep::Agent,
        field: filename.to_string(),
        message: crate::i18n::t_args("zc-quickstart-no-template", &[("filename", filename)]),
    }
}

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
                if !field_form_row_visible(f, i) {
                    cursor_lines.push(usize::MAX);
                    continue;
                }
                cursor_lines.push(lines.len());
                let is_cursor = i == f.cursor;
                let glyph = if is_cursor { " › " } else { "   " };
                let label_style = if is_cursor {
                    theme::accent_style()
                } else {
                    theme::body_style()
                };
                let is_enum = field_row_variants(row).is_some();
                let is_model_row = is_model_field(&row.descriptor);
                // Secret fields render a bounded mask so a pasted,
                // realistic-length API key cannot wrap across rows and
                // push later fields and the footer out of view.
                let raw_display = if row.descriptor.is_secret {
                    masked_secret(&row.buf)
                } else {
                    row.buf.clone()
                };
                let is_empty_buf = raw_display.is_empty();
                let show_ghost = is_empty_buf && !is_cursor && !is_enum;
                let (display, value_style) = if is_model_row
                    && matches!(
                        f.model_catalog_state,
                        ModelCatalogState::Pending | ModelCatalogState::Retrying
                    ) {
                    (
                        if f.model_catalog_state == ModelCatalogState::Retrying {
                            crate::i18n::t_args(
                                "zc-quickstart-model-retrying",
                                &[("provider", f.type_key.as_str())],
                            )
                        } else {
                            crate::i18n::t_args(
                                "zc-quickstart-model-loading",
                                &[("provider", f.type_key.as_str())],
                            )
                        },
                        theme::dim_style().add_modifier(Modifier::ITALIC),
                    )
                } else if is_model_row
                    && f.model_catalog_state == ModelCatalogState::Empty
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
                            if c.fields.is_empty() {
                                ""
                            } else {
                                "  (configured)"
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
            if let Some(editor) = &a.editor {
                let mut cursor_lines = Vec::with_capacity(editor.lines.len());
                let mut lines = Vec::with_capacity(editor.lines.len().max(1));
                for (i, line) in editor.lines.iter().enumerate() {
                    cursor_lines.push(lines.len());
                    if i == editor.cursor_row {
                        let split = byte_index_at_char(line, editor.cursor_col);
                        let (before, after) = line.split_at(split);
                        lines.push(Line::from(vec![
                            Span::styled(before.to_string(), theme::body_style()),
                            Span::styled("█", theme::accent_style()),
                            Span::styled(after.to_string(), theme::body_style()),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            line.to_string(),
                            theme::body_style(),
                        )));
                    }
                }
                if lines.is_empty() {
                    cursor_lines.push(0);
                    lines.push(Line::from(Span::styled("█", theme::accent_style())));
                }
                (
                    format!(" Edit {} ", editor.filename),
                    Vec::new(),
                    lines,
                    "Ctrl+S save & close   Esc cancel/quit".to_string(),
                    cursor_lines,
                )
            } else {
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
                    Span::styled(
                        format!("{:14}", crate::i18n::t("zc-quickstart-agent-name-field")),
                        name_style,
                    ),
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
                    let content = a.files.get(filename).map(String::as_str).unwrap_or("");
                    let status = if content.trim().is_empty() {
                        "—".to_string()
                    } else {
                        crate::i18n::t_args(
                            "zc-quickstart-file-bytes",
                            &[("bytes", &content.len().to_string())],
                        )
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
        }
    };

    let box_w = area.width.saturating_sub(8).min(80);
    let block = theme::modal_block(&title).padding(Padding::horizontal(1));
    // Width left for wrapped text inside the block (its borders plus the
    // horizontal padding). Measured off the block so it tracks any future
    // border/padding change rather than hard-coding `box_w - 4`.
    let inner_width = block
        .inner(Rect::new(area.x, area.y, box_w, area.height))
        .width;

    let body_heights = wrapped_row_heights(&body_lines, inner_width);
    let header_rows = wrapped_total(&header_lines, inner_width);
    // Prefix sums: where each logical body line begins in wrapped-row
    // space. `row_starts[i]` is line `i`'s first row; the trailing entry
    // is the total wrapped-row count.
    let mut row_starts: Vec<u16> = Vec::with_capacity(body_heights.len() + 1);
    let mut acc = 0u16;
    for h in &body_heights {
        row_starts.push(acc);
        acc = acc.saturating_add(*h);
    }
    row_starts.push(acc);
    let body_rows = acc;
    // content rows + top/bottom border + footer row (+1 slack).
    let box_h = (header_rows.saturating_add(body_rows).saturating_add(4))
        .min(area.height.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(box_w) / 2;
    let y = area.y + area.height.saturating_sub(box_h) / 2;
    let rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, rect);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Footer occupies the last line of `inner`. The remaining vertical
    // space is split between an optional header band (per-field help)
    // and the body (form rows / picker entries).
    let inner_content_h = inner.height.saturating_sub(1);
    let effective_header_h = header_rows.min(inner_content_h);
    let header_rect = Rect::new(inner.x, inner.y, inner.width, effective_header_h);
    let body_rect = Rect::new(
        inner.x,
        inner.y + effective_header_h,
        inner.width,
        inner_content_h.saturating_sub(effective_header_h),
    );

    let body_h = body_rect.height;
    // Which cursor row must stay visible. TextInput has no row cursor, so
    // its body just top-aligns; everything else keeps the selected row in
    // view. `selected_line` is a logical body-line index; `row_starts`
    // maps it into wrapped-row space so the scroll math survives wrapping.
    let selected_line = match modal {
        Modal::Picker(p) => cursor_lines.get(p.cursor).copied(),
        Modal::FieldForm(f) => cursor_lines
            .get(f.cursor)
            .copied()
            .filter(|line| *line != usize::MAX),
        Modal::ChannelList(cl) => cursor_lines.get(cl.cursor).copied(),
        Modal::PeerGroupList(pl) => cursor_lines.get(pl.cursor).copied(),
        Modal::Agent(a) => {
            if let Some(editor) = &a.editor {
                cursor_lines.get(editor.cursor_row).copied()
            } else {
                cursor_lines.get(a.cursor).copied()
            }
        }
        Modal::TextInput(_) => None,
    };
    let scroll_offset: u16 = if body_rows > body_h && body_h > 0 {
        match selected_line {
            Some(line) => {
                let start = row_starts.get(line).copied().unwrap_or(0);
                let end = row_starts.get(line + 1).copied().unwrap_or(body_rows);
                if end <= body_h {
                    // Selected row ends within the first screenful — no scroll.
                    0
                } else {
                    // Bring the selected row's bottom to the viewport bottom,
                    // but never past its top (handles a row taller than the
                    // viewport, e.g. a long pasted secret rendered as bullets).
                    (end - body_h).min(start)
                }
            }
            None => 0,
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
            if line_idx == usize::MAX {
                return Rect::new(0, 0, 0, 0);
            }
            let start = row_starts.get(line_idx).copied().unwrap_or(0);
            let height = body_heights.get(line_idx).copied().unwrap_or(1).max(1);
            match start.checked_sub(scroll_offset) {
                Some(dy) if dy < body_rect.height => {
                    // Span the row's full wrapped height (clipped to the
                    // viewport) so a click on a wrapped continuation row
                    // still resolves to the right cursor.
                    let visible = height.min(body_rect.height - dy);
                    Rect::new(
                        body_rect.x,
                        body_rect.y + dy,
                        body_rect.width,
                        visible.max(1),
                    )
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

    fn field_row(key: &str, value: &str) -> FieldFormRow {
        FieldFormRow {
            descriptor: QuickstartFieldDescriptor {
                key: key.to_string(),
                label: key.to_string(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: key == "bot_token",
                enum_variants: None,
                required: true,
                default: None,
            },
            buf: value.to_string(),
        }
    }

    /// A FormState with every real selector satisfied.
    fn complete_form() -> FormState {
        let mut f = FormState::default_form();
        f.provider_type = "anthropic".into();
        f.provider_alias = "default".into();
        f.model = "claude-3-5-haiku-20241022".into();
        f.risk = "balanced".into();
        f.runtime = "balanced".into();
        f.memory_chosen = true;
        f.agent_name = "bob".into();
        f
    }

    fn model_provider_snapshot(existing: Vec<&str>) -> QuickstartStateResult {
        QuickstartStateResult {
            quickstart_completed: false,
            agents: Vec::new(),
            risk_profiles: Vec::new(),
            runtime_profiles: Vec::new(),
            default_runtime_profile: Some("unbounded".into()),
            model_providers: existing.into_iter().map(str::to_string).collect(),
            channels: Vec::new(),
            unassigned_channels: Vec::new(),
            storage: Vec::new(),
            model_provider_types: vec![
                crate::client::QuickstartTypeOption {
                    kind: "openrouter".into(),
                    display_name: "OpenRouter".into(),
                    local: false,
                    default_runtime_profile: Some("unbounded".into()),
                },
                crate::client::QuickstartTypeOption {
                    kind: "anthropic".into(),
                    display_name: "Anthropic".into(),
                    local: false,
                    default_runtime_profile: Some("unbounded".into()),
                },
                crate::client::QuickstartTypeOption {
                    kind: "lmstudio".into(),
                    display_name: "LM Studio".into(),
                    local: true,
                    default_runtime_profile: Some("local_small".into()),
                },
            ],
            channel_types: Vec::new(),
            risk_presets: Vec::new(),
            runtime_presets: Vec::new(),
            memory_kinds: Vec::new(),
            personality_files: Vec::new(),
        }
    }

    fn selectable_labels(options: &[PickerOption]) -> Vec<&str> {
        options
            .iter()
            .filter(|option| option.is_selectable())
            .map(|option| option.label.as_str())
            .collect()
    }

    #[test]
    fn model_provider_picker_groups_existing_first_without_hiding_new_provider() {
        let snap = model_provider_snapshot(vec!["openrouter.default", "openai.gpt5"]);

        let options = model_provider_picker_options(Some(&snap), false);
        let labels = selectable_labels(&options);

        assert_eq!(labels[0], "openrouter.default");
        assert_eq!(labels[1], "openai.gpt5");
        assert!(
            labels.contains(&"OpenRouter"),
            "create-new provider rows must remain visible"
        );
    }

    #[test]
    fn model_provider_picker_collapses_existing_provider_overflow() {
        let snap = model_provider_snapshot(vec![
            "openrouter.default",
            "openai.gpt5",
            "anthropic.sonnet",
            "anthropic.haiku",
            "ollama.local",
            "groq.default",
            "xai.grok",
        ]);

        let collapsed = model_provider_picker_options(Some(&snap), false);
        let collapsed_labels = selectable_labels(&collapsed);
        assert!(collapsed_labels.contains(&"  [+ Show 2 more existing providers]"));
        assert!(!collapsed_labels.contains(&"xai.grok"));

        let expanded = model_provider_picker_options(Some(&snap), true);
        let expanded_labels = selectable_labels(&expanded);
        assert!(expanded_labels.contains(&"xai.grok"));
        assert!(expanded_labels.contains(&"  [- Show fewer existing providers]"));
        assert!(expanded_labels.contains(&"OpenRouter"));
    }

    #[test]
    fn existing_local_provider_adoption_uses_daemon_runtime_default() {
        let snapshot = model_provider_snapshot(vec!["lmstudio.default"]);
        let mut form = FormState::default_form();

        apply_existing_provider_choice(&mut form, Some(&snapshot), "lmstudio.default");

        assert_eq!(form.provider_type, "lmstudio");
        assert_eq!(form.provider_alias, "default");
        assert_eq!(form.provider_mode, SelectorMode::Existing);
        assert_eq!(form.runtime, "local_small");
        assert!(form.runtime_auto_defaulted);
    }

    #[test]
    fn runtime_picker_includes_daemon_advertised_local_small_preset() {
        let mut snapshot = model_provider_snapshot(Vec::new());
        snapshot.runtime_presets = vec![crate::client::QuickstartPresetMirror {
            preset_name: "local_small".into(),
            label: "Local small".into(),
            help: "Safe defaults for local models.".into(),
        }];

        let options = runtime_picker_options(Some(&snapshot));
        let local_small = options
            .iter()
            .find(|option| option.value == "local_small")
            .expect("daemon-advertised runtime preset should be selectable");

        assert!(!local_small.use_existing());
    }

    #[test]
    fn runtime_picker_localizes_known_presets_and_preserves_unknown_daemon_text() {
        let mut snapshot = model_provider_snapshot(Vec::new());
        snapshot.runtime_presets = vec![
            crate::client::QuickstartPresetMirror {
                preset_name: "tight".into(),
                label: "Daemon Tight".into(),
                help: "Daemon tight help.".into(),
            },
            crate::client::QuickstartPresetMirror {
                preset_name: "future_runtime".into(),
                label: "Future Runtime".into(),
                help: "Future runtime help.".into(),
            },
        ];

        let options = runtime_picker_options(Some(&snapshot));

        assert_eq!(
            options[0].label,
            crate::i18n::t("zc-quickstart-runtime-tight")
        );
        assert_eq!(
            options[0].help,
            crate::i18n::t("zc-quickstart-runtime-tight-desc")
        );
        assert_eq!(options[1].label, "Future Runtime");
        assert_eq!(options[1].help, "Future runtime help.");
    }

    #[tokio::test]
    async fn runtime_picker_does_not_open_without_advertised_presets() {
        let (writer_tx, _writer_rx) = tokio::sync::mpsc::channel(1);
        let rpc = std::sync::Arc::new(crate::client::RpcClient::with_rpc(std::sync::Arc::new(
            crate::jsonrpc::RpcOutbound::new(writer_tx),
        )));
        let reconnect_state = std::sync::Arc::new(std::sync::Mutex::new(
            crate::app::CrossReconnectState::default(),
        ));
        let mut pane = QuickstartPane::new(rpc, reconnect_state);

        pane.open_picker_modal(Selector::RuntimeProfile);

        assert!(pane.active_modal.is_none());
    }

    #[test]
    fn model_provider_alias_duplicate_is_caught_from_snapshot() {
        let snap = model_provider_snapshot(vec!["openrouter.default"]);
        let mut form = FieldFormModal {
            selector: Selector::ModelProvider,
            type_key: "openrouter".into(),
            alias: "default".into(),
            model_catalog_state: ModelCatalogState::NotApplicable,
            model_catalog_attempts: 0,
            fields: vec![model_provider_alias_row()],
            cursor: 0,
        };
        form.fields[0].buf = "default".into();

        let error = model_provider_alias_duplicate_error(Some(&snap), &form).expect("duplicate");

        assert_eq!(error.step, QuickstartStep::ModelProvider);
        assert_eq!(error.field, "alias");
        assert!(error.message.contains("openrouter.default"));
    }

    #[test]
    fn model_provider_alias_duplicate_check_allows_new_aliases() {
        let snap = model_provider_snapshot(vec!["openrouter.default"]);
        let mut form = FieldFormModal {
            selector: Selector::ModelProvider,
            type_key: "openrouter".into(),
            alias: "default".into(),
            model_catalog_state: ModelCatalogState::NotApplicable,
            model_catalog_attempts: 0,
            fields: vec![model_provider_alias_row()],
            cursor: 0,
        };
        form.fields[0].buf = "work".into();

        assert!(model_provider_alias_duplicate_error(Some(&snap), &form).is_none());
    }

    #[test]
    fn channel_form_preserves_all_schema_field_keys() {
        let rows = vec![
            field_row("bot_token", " 123:ABC "),
            field_row("allowed_users", "[\"42\"]"),
            field_row("unused", UNSET_DISPLAY),
        ];

        let fields = channel_fields_from_rows(&rows);

        assert_eq!(fields["bot_token"], "123:ABC");
        assert_eq!(fields["allowed_users"], "[\"42\"]");
        assert!(!fields.contains_key("unused"));
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

    #[test]
    fn incomplete_submit_reports_first_missing_required_selector() {
        let mut f = complete_form();
        f.channels.clear();
        f.peer_groups.clear();
        f.agent_name.clear();

        let error = incomplete_submit_error(&f).expect("missing required selector");

        assert_eq!(error.step, QuickstartStep::Agent);
        assert_eq!(error.field, String::new());
        assert!(error.message.contains("Name the agent"));
    }

    #[test]
    fn optional_channel_and_peer_group_rows_do_not_block_submit() {
        let f = complete_form();
        assert!(f.channels.is_empty());
        assert!(f.peer_groups.is_empty());
        assert!(!f.is_satisfied(Selector::Channels));
        assert!(!f.is_satisfied(Selector::PeerGroups));
        assert!(f.all_selectors_satisfied());
    }

    #[test]
    fn optional_rows_complete_only_when_configured() {
        let mut f = complete_form();

        assert!(!f.is_satisfied(Selector::Channels));
        assert!(!f.is_satisfied(Selector::PeerGroups));

        f.channels.push(ChannelDraft {
            channel_type: "telegram".into(),
            alias: "chat".into(),
            fields: std::collections::HashMap::new(),
            mode: SelectorMode::Fresh,
        });
        f.peer_groups.push(crate::wire::QuickstartPeerGroup {
            name: "crew".into(),
            channel: "telegram.chat".into(),
            external_peers: vec!["123".into()],
            ignore: Vec::new(),
        });

        assert!(f.is_satisfied(Selector::Channels));
        assert!(f.is_satisfied(Selector::PeerGroups));
        assert!(f.all_selectors_satisfied());
    }

    #[test]
    fn submission_preserves_selected_fresh_runtime_profile() {
        let mut f = complete_form();
        f.runtime = "local_small".into();
        f.runtime_mode = SelectorMode::Fresh;

        let submission = f.to_submission();

        assert_eq!(
            submission.runtime_profile,
            SelectorChoice::Fresh("local_small".into())
        );
    }

    #[test]
    fn submission_preserves_selected_existing_runtime_profile() {
        let mut f = complete_form();
        f.runtime = "small-laptop".into();
        f.runtime_mode = SelectorMode::Existing;

        let submission = f.to_submission();

        assert_eq!(
            submission.runtime_profile,
            SelectorChoice::Existing("small-laptop".into())
        );
    }

    #[test]
    fn local_provider_defaults_to_local_small_runtime_profile() {
        let mut f = FormState::default_form();

        f.apply_provider_runtime_default(Some("local_small"));

        assert_eq!(f.runtime, "local_small");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("local_small".into())
        );
    }

    #[test]
    fn cloud_provider_defaults_to_unbounded_runtime_profile() {
        let mut f = FormState::default_form();

        f.apply_provider_runtime_default(Some("unbounded"));

        assert_eq!(f.runtime, "unbounded");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("unbounded".into())
        );
    }

    #[test]
    fn missing_provider_override_uses_daemon_state_fallback() {
        let mut snapshot = model_provider_snapshot(Vec::new());
        snapshot.model_provider_types[0].default_runtime_profile = None;

        assert_eq!(
            provider_runtime_default(Some(&snapshot), "openrouter"),
            Some("unbounded"),
        );
    }

    #[test]
    fn missing_provider_and_state_defaults_leave_runtime_incomplete() {
        let mut f = FormState::default_form();

        f.apply_provider_runtime_default(None);

        assert!(f.runtime.is_empty());
        assert!(!f.selector_is_complete(Selector::RuntimeProfile, &[]));
    }

    #[test]
    fn provider_runtime_default_preserves_explicit_runtime_choice() {
        let mut f = FormState::default_form();
        f.runtime = "tight".into();
        f.runtime_mode = SelectorMode::Fresh;

        f.apply_provider_runtime_default(Some("local_small"));

        assert_eq!(f.runtime, "tight");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("tight".into())
        );
    }

    #[test]
    fn provider_runtime_default_preserves_explicit_unbounded_choice() {
        let mut f = FormState::default_form();
        f.runtime = "unbounded".into();
        f.runtime_mode = SelectorMode::Fresh;

        f.apply_provider_runtime_default(Some("local_small"));

        assert_eq!(f.runtime, "unbounded");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("unbounded".into())
        );
    }

    #[test]
    fn provider_runtime_default_preserves_explicit_local_small_choice() {
        let mut f = FormState::default_form();
        f.runtime = "local_small".into();
        f.runtime_mode = SelectorMode::Fresh;

        f.apply_provider_runtime_default(Some("unbounded"));

        assert_eq!(f.runtime, "local_small");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("local_small".into())
        );
    }

    #[test]
    fn explicit_runtime_choice_stops_provider_default_rewrites() {
        let mut f = FormState::default_form();

        f.apply_provider_runtime_default(Some("local_small"));
        assert_eq!(f.runtime, "local_small");
        f.runtime = "unbounded".into();
        f.runtime_mode = SelectorMode::Fresh;
        f.runtime_auto_defaulted = false;
        f.apply_provider_runtime_default(Some("local_small"));

        assert_eq!(f.runtime, "unbounded");
        assert_eq!(
            f.to_submission().runtime_profile,
            SelectorChoice::Fresh("unbounded".into())
        );
    }

    #[test]
    fn apply_handoff_waits_for_reconnect_when_reload_was_signalled() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(
            crate::app::CrossReconnectState::default(),
        ));

        let applied_alias = queue_apply_handoff(&state, "agent-a".into(), true);
        let guard = state.lock().unwrap();

        assert_eq!(applied_alias.as_deref(), Some("agent-a"));
        assert_eq!(
            guard.pending_quickstart_chat,
            Some(crate::app::PendingQuickstartChat::AfterReconnect(
                "agent-a".into()
            ))
        );
    }

    #[test]
    fn apply_handoff_starts_chat_immediately_without_reload_signal() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(
            crate::app::CrossReconnectState::default(),
        ));

        let applied_alias = queue_apply_handoff(&state, "agent-a".into(), false);
        let guard = state.lock().unwrap();

        assert!(applied_alias.is_none());
        assert_eq!(
            guard.pending_quickstart_chat,
            Some(crate::app::PendingQuickstartChat::Immediate(
                "agent-a".into()
            ))
        );
    }

    #[test]
    fn live_model_catalog_turns_model_row_into_picker() {
        let mut rows = vec![
            model_provider_alias_row(),
            FieldFormRow {
                descriptor: QuickstartFieldDescriptor {
                    key: "model".into(),
                    label: "model".into(),
                    help: String::new(),
                    kind: crate::client::QuickstartFieldKind::String,
                    is_secret: false,
                    enum_variants: None,
                    required: true,
                    default: None,
                },
                buf: String::new(),
            },
            FieldFormRow {
                descriptor: QuickstartFieldDescriptor {
                    key: "api_key".into(),
                    label: "api_key".into(),
                    help: String::new(),
                    kind: crate::client::QuickstartFieldKind::String,
                    is_secret: true,
                    enum_variants: None,
                    required: false,
                    default: None,
                },
                buf: String::new(),
            },
        ];
        let models = vec!["gpt-5".to_string(), "gpt-5.1".to_string()];

        apply_model_catalog_to_rows(&mut rows, Some(&models));

        assert_eq!(
            rows[1].descriptor.kind,
            crate::client::QuickstartFieldKind::Enum
        );
        assert_eq!(
            rows[1].descriptor.enum_variants.as_deref(),
            Some(models.as_slice())
        );
        assert_eq!(
            rows[2].descriptor.kind,
            crate::client::QuickstartFieldKind::String
        );
        assert!(rows[2].descriptor.enum_variants.is_none());
    }

    #[test]
    fn model_provider_rows_use_live_catalog_for_model_picker() {
        let fields = vec![
            QuickstartFieldDescriptor {
                key: "model".into(),
                label: "model".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: false,
                enum_variants: None,
                required: true,
                default: None,
            },
            QuickstartFieldDescriptor {
                key: "api_key".into(),
                label: "api_key".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: true,
                enum_variants: None,
                required: false,
                default: None,
            },
        ];
        let models = vec!["gpt-5".to_string(), "gpt-5.1".to_string()];

        let rows =
            build_field_form_rows(QuickstartFieldSection::ModelProvider, fields, Some(&models));

        assert_eq!(rows[0].descriptor.key, "alias");
        assert_eq!(rows[1].descriptor.key, "model");
        assert_eq!(
            rows[1].descriptor.kind,
            crate::client::QuickstartFieldKind::Enum
        );
        assert_eq!(
            rows[1].descriptor.enum_variants.as_deref(),
            Some(models.as_slice())
        );
        assert_eq!(rows[1].buf, "gpt-5");
    }

    #[test]
    fn model_provider_picker_reopens_on_current_provider() {
        let options = vec![
            opt("anthropic", "Anthropic", ""),
            opt("openai", "OpenAI", ""),
            existing_opt("openai.default".into()),
        ];
        let mut form = FormState::default_form();

        form.provider_type = "openai".into();
        form.provider_mode = SelectorMode::Fresh;
        assert_eq!(current_provider_picker_cursor(&options, &form), 1);

        form.provider_alias = "default".into();
        form.provider_mode = SelectorMode::Existing;
        assert_eq!(current_provider_picker_cursor(&options, &form), 2);
    }

    #[test]
    fn model_provider_form_reopens_with_staged_values() {
        let fields = vec![
            QuickstartFieldDescriptor {
                key: "model".into(),
                label: "model".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: false,
                enum_variants: None,
                required: true,
                default: None,
            },
            QuickstartFieldDescriptor {
                key: "api_key".into(),
                label: "api_key".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: true,
                enum_variants: None,
                required: false,
                default: None,
            },
        ];
        let mut form = FormState::default_form();
        form.provider_type = "openai".into();
        form.provider_alias = "work".into();
        form.provider_mode = SelectorMode::Fresh;
        form.model = "gpt-5".into();
        form.provider_fields.insert(
            "api_key".into(),
            "sk-test-value-that-is-long-enough-to-wrap".into(),
        );

        let mut rows = build_field_form_rows(QuickstartFieldSection::ModelProvider, fields, None);
        restore_field_form_rows_from_form(
            QuickstartFieldSection::ModelProvider,
            "openai",
            &mut rows,
            &form,
        );

        assert_eq!(rows[0].descriptor.key, "alias");
        assert_eq!(rows[0].buf, "work");
        assert_eq!(rows[1].descriptor.key, "model");
        assert_eq!(rows[1].buf, "gpt-5");
        assert_eq!(rows[2].descriptor.key, "api_key");
        assert_eq!(rows[2].buf, "sk-test-value-that-is-long-enough-to-wrap");
        assert!(masked_secret(&rows[2].buf).contains("(+"));
    }

    #[test]
    fn openai_codex_auth_hides_api_key_row_in_tui_form() {
        let fields = vec![
            QuickstartFieldDescriptor {
                key: "model".into(),
                label: "model".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: false,
                enum_variants: None,
                required: true,
                default: None,
            },
            QuickstartFieldDescriptor {
                key: "auth_mode".into(),
                label: "Authentication".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::Enum,
                is_secret: false,
                enum_variants: Some(vec!["api_key".into(), "codex".into()]),
                required: true,
                default: Some("codex".into()),
            },
            QuickstartFieldDescriptor {
                key: "api_key".into(),
                label: "api_key".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::String,
                is_secret: true,
                enum_variants: None,
                required: false,
                default: None,
            },
        ];
        let mut form = FieldFormModal {
            selector: Selector::ModelProvider,
            type_key: "openai".into(),
            alias: "default".into(),
            model_catalog_state: ModelCatalogState::Empty,
            model_catalog_attempts: 0,
            fields: build_field_form_rows(QuickstartFieldSection::ModelProvider, fields, None),
            cursor: 2,
        };

        let keys: Vec<&str> = visible_field_form_indices(&form)
            .map(|index| form.fields[index].descriptor.key.as_str())
            .collect();
        assert_eq!(keys, vec!["alias", "model", "auth_mode"]);

        move_field_form_cursor(&mut form, 1);
        assert_eq!(
            form.fields[form.cursor].descriptor.key, "alias",
            "next from auth_mode must skip the hidden api_key row"
        );

        let auth_mode = form
            .fields
            .iter_mut()
            .find(|row| row.descriptor.key == "auth_mode")
            .expect("auth_mode row");
        auth_mode.buf = "api_key".into();
        let keys: Vec<&str> = visible_field_form_indices(&form)
            .map(|index| form.fields[index].descriptor.key.as_str())
            .collect();
        assert_eq!(keys, vec!["alias", "model", "auth_mode", "api_key"]);
    }

    #[test]
    fn transient_model_catalog_miss_retries_before_manual_fallback() {
        let mut form = FieldFormModal {
            selector: Selector::ModelProvider,
            type_key: "openai".into(),
            alias: "default".into(),
            model_catalog_state: ModelCatalogState::Pending,
            model_catalog_attempts: 0,
            fields: build_field_form_rows(
                QuickstartFieldSection::ModelProvider,
                vec![QuickstartFieldDescriptor {
                    key: "model".into(),
                    label: "model".into(),
                    help: String::new(),
                    kind: crate::client::QuickstartFieldKind::String,
                    is_secret: false,
                    enum_variants: None,
                    required: true,
                    default: None,
                }],
                None,
            ),
            cursor: 0,
        };

        apply_model_catalog_result(&mut form, None);
        assert_eq!(form.model_catalog_state, ModelCatalogState::Retrying);

        apply_model_catalog_result(&mut form, None);
        assert_eq!(form.model_catalog_state, ModelCatalogState::Empty);
    }

    #[test]
    fn successful_catalog_retry_still_turns_model_row_into_picker() {
        let mut form = FieldFormModal {
            selector: Selector::ModelProvider,
            type_key: "openai".into(),
            alias: "default".into(),
            model_catalog_state: ModelCatalogState::Retrying,
            model_catalog_attempts: 1,
            fields: build_field_form_rows(
                QuickstartFieldSection::ModelProvider,
                vec![QuickstartFieldDescriptor {
                    key: "model".into(),
                    label: "model".into(),
                    help: String::new(),
                    kind: crate::client::QuickstartFieldKind::String,
                    is_secret: false,
                    enum_variants: None,
                    required: true,
                    default: None,
                }],
                None,
            ),
            cursor: 0,
        };
        let models = vec!["gpt-5".to_string(), "gpt-5.1".to_string()];

        apply_model_catalog_result(&mut form, Some(models.clone()));

        assert_eq!(form.model_catalog_state, ModelCatalogState::Loaded);
        let model = form
            .fields
            .iter()
            .find(|row| row.descriptor.key == "model")
            .expect("model row");
        assert_eq!(
            model.descriptor.enum_variants.as_deref(),
            Some(models.as_slice())
        );
        assert_eq!(model.buf, "gpt-5");
    }

    #[test]
    fn completed_selector_advances_to_next_row() {
        let form = FormState::default_form();
        assert_eq!(
            Selector::ALL,
            [
                Selector::ModelProvider,
                Selector::RiskProfile,
                Selector::RuntimeProfile,
                Selector::Memory,
                Selector::Channels,
                Selector::PeerGroups,
                Selector::Agent,
                Selector::Submit,
            ],
        );
        assert_eq!(
            next_selector_index_after(Selector::ModelProvider, &form),
            Some(1)
        );
        assert_eq!(Selector::ALL[1], Selector::RiskProfile);
        let submit_index = Selector::ALL.len() - 1;
        assert_eq!(
            next_selector_index_after(Selector::Agent, &complete_form()),
            Some(submit_index)
        );
        assert_eq!(Selector::ALL[submit_index], Selector::Submit);
        assert_eq!(
            next_selector_index_after(Selector::Submit, &complete_form()),
            Some(submit_index)
        );
    }

    #[test]
    fn completed_selector_visits_optional_rows() {
        let mut form = complete_form();
        form.channels.clear();
        form.peer_groups.clear();

        assert_eq!(
            next_selector_index_after(Selector::ModelProvider, &form).map(|idx| Selector::ALL[idx]),
            Some(Selector::Channels)
        );
        assert_eq!(
            next_selector_index_after(Selector::Channels, &form).map(|idx| Selector::ALL[idx]),
            Some(Selector::PeerGroups)
        );
        assert_eq!(
            next_selector_index_after(Selector::PeerGroups, &form).map(|idx| Selector::ALL[idx]),
            Some(Selector::Submit)
        );
    }

    #[test]
    fn completed_selector_keeps_first_unfilled_required_row() {
        let mut form = complete_form();
        form.risk.clear();

        assert_eq!(
            next_selector_index_after(Selector::ModelProvider, &form).map(|idx| Selector::ALL[idx]),
            Some(Selector::RiskProfile)
        );
    }

    #[test]
    fn bool_fields_render_as_enum_toggles() {
        let rows = build_field_form_rows(
            QuickstartFieldSection::ModelProvider,
            vec![QuickstartFieldDescriptor {
                key: "requires_openai_auth".into(),
                label: "requires_openai_auth".into(),
                help: String::new(),
                kind: crate::client::QuickstartFieldKind::Bool,
                is_secret: false,
                enum_variants: None,
                required: false,
                default: None,
            }],
            None,
        );

        let row = rows
            .iter()
            .find(|row| row.descriptor.key == "requires_openai_auth")
            .expect("bool field row");
        assert_eq!(
            row.descriptor.enum_variants.as_deref(),
            Some(["false".to_string(), "true".to_string()].as_slice())
        );
        assert_eq!(row.buf, "false");
    }

    #[test]
    fn model_provider_alias_prefill_is_not_ghost_text() {
        let mut row = model_provider_alias_row();

        assert_eq!(row.buf, "default");
        assert_eq!(row.descriptor.key, "alias");
        assert!(row.descriptor.required);
        assert_eq!(row.descriptor.default, None);

        row.buf.clear();
        let ghost_display = row.descriptor.default.clone().unwrap_or_default();
        assert!(
            ghost_display.is_empty(),
            "clearing alias must not redraw `default` as non-editable ghost text"
        );
    }

    #[test]
    fn missing_personality_template_reports_selected_file() {
        let err = missing_template_error("MEMORY.md");

        assert_eq!(err.step, QuickstartStep::Agent);
        assert_eq!(err.field, "MEMORY.md");
        assert!(err.message.contains("MEMORY.md"));
    }

    fn test_agent_modal() -> AgentModal {
        AgentModal {
            cursor: 1,
            name: "scout".into(),
            files: std::collections::BTreeMap::new(),
            filenames: vec!["MEMORY.md".into(), "SKILL.md".into()],
            editor: None,
        }
    }

    #[test]
    fn agent_file_enter_loads_template_when_unset() {
        let agent = test_agent_modal();

        assert!(personality_file_needs_template(&agent, "MEMORY.md"));
    }

    #[test]
    fn agent_file_enter_advances_when_content_already_set() {
        let mut agent = test_agent_modal();
        agent.files.insert("MEMORY.md".into(), "custom".into());

        assert!(!personality_file_needs_template(&agent, "MEMORY.md"));
        advance_agent_modal_after_file(&mut agent);
        assert_eq!(agent.cursor, 2);
    }

    #[test]
    fn last_agent_file_enter_advances_to_save_row() {
        let mut agent = test_agent_modal();
        agent.cursor = 2;
        agent.files.insert("SKILL.md".into(), "custom".into());

        advance_agent_modal_after_file(&mut agent);
        assert_eq!(agent.cursor, 3);
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
    fn selector_with_validation_error_does_not_render_complete() {
        let mut f = FormState::default_form();
        f.provider_type = "anthropic".into();
        f.provider_alias = "default".into();
        f.model = "claude-3-5-haiku-20241022".into();
        assert!(f.is_satisfied(Selector::ModelProvider));

        let errors = vec![err(QuickstartStep::ModelProvider)];

        assert!(!f.selector_is_complete(Selector::ModelProvider, &errors));
        assert!(errors_include_selector(&errors, Selector::ModelProvider));
    }

    #[test]
    fn name_field_accepts_hotkey_letters() {
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
        assert_eq!(UNSET_DISPLAY, "<unset>");
        let seeded = Some(UNSET_DISPLAY.to_string())
            .filter(|v| v != UNSET_DISPLAY && !v.is_empty())
            .unwrap_or_default();
        assert!(seeded.is_empty());
    }

    #[test]
    fn file_editor_edits_and_saves_multiline_content() {
        let mut editor = FileEditorState::new("TOOLS.md".into(), "one".into());

        editor.move_right();
        editor.move_right();
        editor.move_right();
        editor.insert_newline();
        editor.insert_text("two");

        assert_eq!(editor.content(), "one\ntwo");
        assert_eq!(editor.cursor_row, 1);
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn file_editor_backspace_joins_lines() {
        let mut editor = FileEditorState::new("TOOLS.md".into(), "one\ntwo".into());
        editor.cursor_row = 1;
        editor.cursor_col = 0;

        editor.backspace();

        assert_eq!(editor.content(), "onetwo");
        assert_eq!(editor.cursor_row, 0);
        assert_eq!(editor.cursor_col, 3);
    }

    #[test]
    fn file_editor_scroll_clamps_at_edges() {
        let mut editor = FileEditorState::new("TOOLS.md".into(), "one\ntwo\nthree".into());

        editor.scroll_lines(-1);
        assert_eq!(editor.cursor_row, 0);

        editor.scroll_lines(10);
        assert_eq!(editor.cursor_row, 2);

        editor.scroll_lines(1);
        assert_eq!(editor.cursor_row, 2);
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

    #[test]
    fn wrapped_total_counts_soft_wrapped_rows() {
        let long = Line::from("a".repeat(40));
        assert_eq!(wrapped_total(std::slice::from_ref(&long), 10), 4);
        // A blank line still occupies one row.
        let blank = Line::from("");
        assert_eq!(wrapped_total(std::slice::from_ref(&blank), 10), 1);
    }

    #[test]
    fn wrapped_row_heights_are_measured_per_line() {
        // Each logical line wraps independently; the per-line heights feed
        // the prefix sums that keep scroll + click hit-testing aligned
        // when an earlier row (e.g. a long api_key) wraps.
        let lines = vec![
            Line::from("short"),
            Line::from("x".repeat(25)), // 25 / 10 -> 3 rows
            Line::from("ok"),
        ];
        assert_eq!(wrapped_row_heights(&lines, 10), vec![1, 3, 1]);
        assert_eq!(wrapped_total(&lines, 10), 5);
    }

    fn render_modal_rects(area: Rect, modal: &Modal) -> (Rect, Vec<Rect>) {
        use ratatui::{Terminal, backend::TestBackend};
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut out = None;
        terminal
            .draw(|frame| {
                out = Some(draw_modal(frame, area, modal, &[], &[]));
            })
            .expect("draw");
        out.expect("draw_modal ran")
    }

    fn risk_picker(cursor: usize, help: &str) -> Modal {
        Modal::Picker(PickerModal {
            selector: Selector::RiskProfile,
            purpose: PickerPurpose::DirectChoice,
            options: vec![
                opt("locked_down", "Locked Down", help),
                opt("balanced", "Balanced", help),
                opt("yolo", "YOLO", help),
            ],
            cursor,
        })
    }

    #[test]
    fn picker_keeps_every_option_visible_when_blurbs_wrap() {
        let help = "Applies specific filesystem and approval defaults for day-to-day operation.";
        let modal = risk_picker(2, help);
        let area = Rect::new(0, 0, 60, 24);
        let (rect, rects) = render_modal_rects(area, &modal);
        assert_eq!(rects.len(), 3);
        for (i, r) in rects.iter().enumerate() {
            assert!(r.height > 0, "option {i} must be visible, got {r:?}");
            assert!(
                in_rect(r.x, r.y, rect),
                "option {i} must sit inside the modal box {rect:?}, got {r:?}"
            );
        }
        assert!(
            rects[1].y >= rects[0].y + 2 && rects[2].y >= rects[1].y + 2,
            "hit-rects must be spaced by wrapped height, not logical lines: {rects:?}"
        );
    }

    #[test]
    fn picker_scrolls_to_keep_selected_option_visible() {
        let help = "Applies specific filesystem and approval defaults, with extra \
                    explanation to force several wrapped rows inside a narrow modal box.";
        let modal = risk_picker(2, help);
        let area = Rect::new(0, 0, 40, 10);
        let (_rect, rects) = render_modal_rects(area, &modal);
        assert_eq!(rects.len(), 3);
        assert!(
            rects[2].height > 0,
            "selected option must scroll into view, got {:?}",
            rects[2]
        );
        assert_eq!(
            rects[0].height, 0,
            "first option must scroll off the top, got {:?}",
            rects[0]
        );
    }
}
